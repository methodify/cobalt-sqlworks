"""Cobalt's LakeSail worker: Sail's Spark Connect server in-process, local-spark-mcp's socket
protocol towards Cobalt.

Launched by Cobalt as ``python -m cobalt_sail_worker --port N --control-port M`` from the Sail
environment (pysail + pyspark-client + ipython). The worker connects back to Cobalt's two
loopback listeners and serves the same length-prefixed JSON protocol (version 2) as
local-spark-mcp's worker, so the kernel actor, contexts, Arrow results, streamed SQL and the
interrupt all work unchanged:

- ``init`` starts ``pysail.spark.SparkConnectServer`` on a random loopback port, with OneLake
  credentials taken from Cobalt's token endpoint through object_store's Fabric token provider
  (``AZURE_FABRIC_TOKEN_SERVICE_URL`` + ``AZURE_FABRIC_SESSION_TOKEN``) and a memory catalog
  ``spark_catalog`` for scratch tables;
- a context is one Spark Connect session (``builder.remote(url).create()``: its own temp views,
  current catalog / database, conf) plus one IPython namespace, swapped into the single shell
  per cell exactly as the JVM worker does;
- every lakehouse is a Unity catalog served by Cobalt's loopback endpoint over Fabric's table
  API (``init.catalog.endpoint``): Sail asks for schemas, tables and columns as statements name
  them, so nothing is mounted and a lakehouse with thousands of tables costs one listing;
- ``run_sql`` streams the client's Arrow batches as ``batch`` events; DML reports Sail's
  ``count`` frame as ``metrics``; ``interrupt`` on the control socket is
  ``session.interruptAll()`` plus a KeyboardInterrupt for Python code.

Nothing here needs Java. Methods the JVM worker has for shadows, preload and the Files mirror
are not served; ``features`` says so.
"""

from __future__ import annotations

import argparse
import io
import json
import os
import re
import socket
import struct
import sys
import threading
import time
import traceback as _tb
import types
import urllib.parse
import urllib.request
from dataclasses import asdict, dataclass, field

PROTOCOL_VERSION = 2
FEATURES = ["arrow", "streaming", "interrupt", "capture_result", "job_description", "register_lakehouse", "contexts",
            "sql_stream", "commit_metrics", "catalog_listing", "engine_sail"]
ENGINE = "sail"
DEFAULT_CATALOG = "spark_catalog"
ONELAKE_DFS = "https://onelake.dfs.fabric.microsoft.com"
MAX_VALUE_LEN = 4000
_HEADER = struct.Struct(">I")


# ---- framing (local-spark-mcp protocol.py) ----

def send_msg(sock: socket.socket, obj: dict) -> None:
    data = json.dumps(obj).encode("utf-8")
    sock.sendall(_HEADER.pack(len(data)) + data)


def _recv_exactly(sock: socket.socket, n: int) -> bytes | None:
    chunks = []
    remaining = n
    while remaining:
        chunk = sock.recv(remaining)
        if not chunk:
            return None
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def recv_msg(sock: socket.socket) -> dict | None:
    header = _recv_exactly(sock, _HEADER.size)
    if header is None:
        return None
    (length,) = _HEADER.unpack(header)
    body = _recv_exactly(sock, length)
    if body is None:
        return None
    return json.loads(body)


def send_reply(sock: socket.socket, obj: dict, blobs: list[bytes] | None = None) -> None:
    if blobs:
        obj = dict(obj, binary=[len(b) for b in blobs])
    send_msg(sock, obj)
    for b in blobs or []:
        sock.sendall(b)


# ---- results ----

@dataclass
class ExecResult:
    ok: bool
    stdout: str = ""
    stderr: str = ""
    error: str | None = None
    traceback: str | None = None
    execution_count: int | None = None
    notices: list[str] = field(default_factory=list)
    interrupted: bool = False
    displays: list[dict] = field(default_factory=list)

    def to_dict(self) -> dict:
        return asdict(self)


@dataclass
class SqlResult:
    columns: list[str] = field(default_factory=list)
    rows: list[list] = field(default_factory=list)
    row_count: int = 0
    truncated: bool = False
    limit: int = 0
    notices: list[str] = field(default_factory=list)
    arrow: dict | None = None
    metrics: dict | None = None
    batches: int | None = None
    elapsed_s: float | None = None

    def to_dict(self) -> dict:
        return asdict(self)


class InterruptedQuery(RuntimeError):
    """run_sql was stopped by `interrupt`; reported with interrupted=true."""


def _truncate(text: str | None, n: int = 200_000) -> str:
    if not text:
        return ""
    return text if len(text) <= n else text[:n] + f"\n… [{len(text) - n} more characters]"


def _jsonify(value):
    if value is None or isinstance(value, (bool, int, float, str)):
        if isinstance(value, str) and len(value) > MAX_VALUE_LEN:
            return value[:MAX_VALUE_LEN] + "…"
        return value
    if isinstance(value, (bytes, bytearray)):
        return value.hex()
    if isinstance(value, dict):
        return {str(k): _jsonify(v) for k, v in value.items()}
    if isinstance(value, (list, tuple, set)):
        return [_jsonify(v) for v in value]
    if hasattr(value, "asDict"):
        return _jsonify(value.asDict())
    return str(value)


@dataclass
class Lakehouse:
    name: str
    id: str
    workspace_id: str
    # None until listed; then the schema folders (empty for a plain lakehouse)
    schemas: list[str] | None = None
    # top-level tables (plain lakehouse, or the legacy tables of a schema-enabled one)
    tables: list[str] | None = None
    # schema -> tables
    schema_tables: dict = field(default_factory=dict)
    listed_at: float | None = None

    @property
    def schema_enabled(self) -> bool:
        return bool(self.schemas)

    def path(self, schema: str | None, table: str) -> str:
        base = f"abfss://{self.workspace_id}@onelake.dfs.fabric.microsoft.com/{self.id}/Tables"
        return f"{base}/{schema}/{table}" if schema else f"{base}/{table}"


@dataclass
class Context:
    id: str
    session: object
    module: object
    default_lakehouse: str | None = None
    default_schema: str | None = None
    name: str | None = None
    created_at: float = field(default_factory=time.time)
    cells: int = 0
    seeded: bool = False
    last_activity: float | None = None
    pending_drop: bool = False
    # tables mounted in this session, as "lh.schema.t" / "lh.t"
    mounted: set = field(default_factory=set)
    prepared: bool = False
    # a rewritten CREATE TABLE in flight: (scratch name, lakehouse, schema, table)
    pending_create: object = None

    @property
    def ns(self) -> dict:
        return self.module.__dict__


class _Tee(io.TextIOBase):
    """Keeps everything written and forwards complete lines (or 4 KB chunks) to a callback."""

    def __init__(self, name: str, on_output):
        self._name, self._cb = name, on_output
        self._all: list[str] = []
        self._pending = ""

    def writable(self) -> bool:
        return True

    def write(self, text: str) -> int:
        if not text:
            return 0
        self._all.append(text)
        self._pending += text
        if "\n" in self._pending or len(self._pending) >= 4096:
            self._emit()
        return len(text)

    def _emit(self) -> None:
        if self._pending:
            chunk, self._pending = self._pending, ""
            try:
                self._cb(self._name, chunk)
            except Exception:
                pass

    def flush(self) -> None:
        self._emit()

    def close(self) -> None:
        self._emit()

    def getvalue(self) -> str:
        return "".join(self._all)


_WRITE_TARGET = re.compile(
    r"^\s*(?:INSERT\s+(?:INTO|OVERWRITE)(?:\s+TABLE)?|MERGE\s+INTO|UPDATE|DELETE\s+FROM|TRUNCATE\s+TABLE"
    r"|ALTER\s+TABLE|DROP\s+TABLE(?:\s+IF\s+EXISTS)?"
    r"|CREATE\s+(?:OR\s+REPLACE\s+)?TABLE(?:\s+IF\s+NOT\s+EXISTS)?)\s+([`\w.]+)",
    re.IGNORECASE,
)
_WRITE_VERB = re.compile(r"^\s*(INSERT\s+OVERWRITE|INSERT|MERGE|UPDATE|DELETE|TRUNCATE|ALTER|DROP|CREATE)", re.IGNORECASE)
_NOT_FOUND = (
    re.compile(r"Table or view not found:\s*([`\w.]+)"),
    re.compile(r"The table or view\s+((?:`[^`]+`\.?)+)\s+cannot be found"),
    re.compile(r"table\s+'?([`\w.]+)'?\s+not found", re.IGNORECASE),
)
_USE_PLAIN = re.compile(r"^\s*USE\s+(?!CATALOG\b|DATABASE\b|SCHEMA\b|NAMESPACE\b)", re.IGNORECASE)
_CREATE_HEAD = re.compile(r"^\s*CREATE\s+(?:OR\s+REPLACE\s+)?TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?[`\w.]+\s*", re.IGNORECASE)
_SHOW_TABLES = re.compile(r"^\s*SHOW\s+TABLES(?:\s+(?:IN|FROM)\s+([`\w.]+))?\s*;?\s*$", re.IGNORECASE)


def _safe(fn, default=None):
    try:
        return fn()
    except Exception:
        return default


def _sql_write_target(sql: str) -> str | None:
    m = _WRITE_TARGET.match(sql)
    return m.group(1) if m else None


def _write_verb(sql: str) -> str | None:
    m = _WRITE_VERB.match(sql)
    return m.group(1).upper().replace("  ", " ") if m else None


def _parts(name: str) -> list[str]:
    """`a`.`b`.c -> [a, b, c]"""
    out, cur, quoted = [], "", False
    for ch in name:
        if ch == "`":
            quoted = not quoted
        elif ch == "." and not quoted:
            out.append(cur)
            cur = ""
        else:
            cur += ch
    out.append(cur)
    return [p for p in out if p != ""]


def _q(name: str) -> str:
    return "`" + name.replace("`", "``") + "`"


def _missing_table(exc: BaseException) -> list[str] | None:
    text = str(exc)
    if "TABLE_OR_VIEW_NOT_FOUND" not in text and "not found" not in text.lower():
        return None
    for rx in _NOT_FOUND:
        m = rx.search(text)
        if m:
            return _parts(m.group(1))
    return None


class SailEngine:
    """The Sail server, its sessions and the lakehouse registry."""

    def __init__(self, onelake: dict | None = None, lakehouses=(), default_lakehouse: str | None = None,
                 write_mode: str = "readonly", default_sql_limit: int = 1000, app_name: str = "cobalt-sqlworks",
                 state_root: str | None = None, sail_options: dict | None = None, catalog: dict | None = None, **_ignored):
        self.started_at = time.time()
        self.onelake = onelake or {}
        self.catalog = catalog or {}
        self.write_mode = "writethrough" if write_mode == "writethrough" else "readonly"
        self.requested_write_mode = write_mode
        self.default_sql_limit = int(default_sql_limit or 1000)
        self.app_name = app_name
        self.state_root = state_root
        self.lakehouses: dict[str, Lakehouse] = {}
        self.default_lakehouse: str | None = None
        self.contexts: dict[str, Context] = {}
        self._active: Context | None = None
        self._cell_running = False
        self._cell_started: float | None = None
        self._cell_method: str | None = None
        self._cell_context: str | None = None
        self._interrupt_requested = False
        self._last_activity = time.time()
        self._displays: list[dict] = []
        self._blobs: list[bytes] = []
        self.blobs_out: list[bytes] = []
        self._notices: list[str] = []
        self._lock = threading.RLock()
        self.server = None
        self.url: str | None = None
        self.sail_options = dict(sail_options or {})
        self.warnings: list[str] = []
        for lh in lakehouses or ():
            self._add_lakehouse(lh)
        if default_lakehouse:
            info = self._resolve_lakehouse(default_lakehouse)
            if info is None:
                raise ValueError(f"unknown default lakehouse {default_lakehouse!r}; known: {sorted(self.lakehouses)}")
            self.default_lakehouse = info.name
        self._start_server()
        from IPython.core.interactiveshell import InteractiveShell

        self.shell = InteractiveShell.instance()
        self.shell.colors = "nocolor"
        self.shell.ast_node_interactivity = "last_expr"
        self._root_session = self._new_session()
        self._prepare_session(self._root_session, self.default_lakehouse, None)
        module = types.ModuleType("__main__")
        module, _ = self.shell.prepare_user_module(module)
        self._seed_namespace(module.__dict__, self._root_session)
        self.contexts["default"] = Context("default", self._root_session, module, self.default_lakehouse,
                                           self._default_schema(self.default_lakehouse), name="default", prepared=True)
        self._bind_proxy(self.contexts["default"])
        self._activate(self.contexts["default"])

    # ---- lakehouses and OneLake listings ----

    def _add_lakehouse(self, lh: dict) -> Lakehouse:
        name, lid, ws = lh.get("name"), lh.get("id"), lh.get("workspace_id")
        if not (name and lid and ws):
            raise ValueError(f"lakehouse needs name, id and workspace_id: {lh!r}")
        info = Lakehouse(name=str(name), id=str(lid), workspace_id=str(ws))
        schemas = lh.get("schemas")
        if isinstance(schemas, list):
            info.schemas = [str(s) for s in schemas]
        self.lakehouses[info.name] = info
        return info

    def _resolve_lakehouse(self, name: str | None) -> Lakehouse | None:
        if not name:
            return None
        if name in self.lakehouses:
            return self.lakehouses[name]
        for lh in self.lakehouses.values():
            if lh.name.lower() == name.lower() or lh.id == name:
                return lh
        return None

    def _schemas_of(self, lh: Lakehouse, sess=None) -> list[str]:
        """The lakehouse's schemas from the catalog (Fabric reports `dbo` for a plain lakehouse
        too); cached on the lakehouse record."""
        if lh.schemas is None:
            sess = sess or self._root_session
            try:
                rows = sess.sql(f"SHOW DATABASES IN {_q(lh.name)}").collect()
                names = []
                for r in rows:
                    n = str(r[0])
                    names.append(n.split(".")[-1] if n.startswith(lh.name + ".") else n)
                lh.schemas = names
            except Exception as exc:
                self.warnings.append(f"could not list the schemas of {lh.name}: {type(exc).__name__}: {str(exc).splitlines()[0][:200]}")
                lh.schemas = ["dbo"]
        return lh.schemas

    def _default_schema(self, lh_name: str | None, sess=None) -> str | None:
        lh = self._resolve_lakehouse(lh_name)
        if lh is None:
            return None
        schemas = self._schemas_of(lh, sess)
        return "dbo" if "dbo" in schemas or not schemas else schemas[0]

    def list_tables(self, lakehouse: str) -> list[str]:
        lh = self._resolve_lakehouse(lakehouse)
        if lh is None:
            raise ValueError(f"unknown lakehouse {lakehouse!r}; known: {sorted(self.lakehouses)}")
        lh.schemas = None
        out: list[str] = []
        for s in self._schemas_of(lh):
            try:
                for r in self._root_session.sql(f"SHOW TABLES IN {_q(lh.name)}.{_q(s)}").collect():
                    out.append(f"{s}/{r[1]}")
            except Exception:
                pass
        return out

    def register_lakehouse(self, lh: dict) -> dict:
        existing = self._resolve_lakehouse(lh.get("name")) or self._resolve_lakehouse(lh.get("id"))
        if existing is not None and existing.id == lh.get("id"):
            info = existing
        else:
            if existing is not None:
                raise ValueError(f"lakehouse name {lh.get('name')!r} is already attached from another workspace")
            info = self._add_lakehouse(lh)
            # every lakehouse is a catalog, and the catalog list is server configuration
            self._restart_server()
        return {"name": info.name, "id": info.id, "workspace_id": info.workspace_id, "schemas": self._schemas_of(info),
                "lakehouses": sorted(self.lakehouses)}

    def unregister_lakehouse(self, name: str) -> dict:
        lh = self._resolve_lakehouse(name)
        if lh is None:
            raise ValueError(f"unknown lakehouse {name!r}")
        del self.lakehouses[lh.name]
        return {"name": lh.name, "dropped_databases": [], "lakehouses": sorted(self.lakehouses)}

    # ---- the server ----

    def _catalog_list(self) -> str:
        """`spark_catalog` (memory, scratch tables and views) plus one Unity catalog per lakehouse
        served by Cobalt's loopback endpoint over Fabric's table API: no table is ever mounted,
        Sail asks for schemas, tables and columns as statements name them."""
        items = [f'{{type="memory", name="{DEFAULT_CATALOG}", initial_database=["default"]}}']
        endpoint = (self.catalog or {}).get("endpoint")
        if endpoint:
            for lh in self.lakehouses.values():
                items.append(f'{{type="unity", name="{_toml(lh.name)}", uri="{_toml(endpoint)}", default_catalog="{_toml(lh.name)}"}}')
        return "[" + ", ".join(items) + "]"

    def _start_server(self) -> None:
        env = os.environ
        if self.onelake.get("endpoint"):
            # object_store's Fabric token provider: GET <url>?resource=… with x-ms-partner-token
            env["AZURE_FABRIC_TOKEN_SERVICE_URL"] = self.onelake["endpoint"]
            env["AZURE_FABRIC_SESSION_TOKEN"] = self.onelake.get("secret") or ""
            env["AZURE_FABRIC_WORKLOAD_HOST"] = "cobalt"
            env["AZURE_FABRIC_CLUSTER_IDENTIFIER"] = "cobalt"
            env["AZURE_ALLOW_HTTP"] = "true"
        env["SAIL_CATALOG__LIST"] = self._catalog_list()
        env["SAIL_CATALOG__DEFAULT_CATALOG"] = DEFAULT_CATALOG
        env["UNITY_ALLOW_HTTP_URL"] = "true"  # Cobalt's catalog endpoint is plain HTTP on loopback
        for k, v in self.sail_options.items():
            env[str(k)] = str(v)
        from pysail.spark import SparkConnectServer

        server = SparkConnectServer("127.0.0.1", 0)
        server.start(background=True)
        ip, port = server.listening_address
        self.server = server
        self.url = f"sc://{ip}:{port}"

    def _restart_server(self) -> None:
        """Catalogs are server configuration: a new schema-enabled lakehouse means a new server.
        Contexts keep their Python namespaces and get fresh sessions (temp views are gone)."""
        old = self.server
        try:
            for ctx in self.contexts.values():
                try:
                    ctx.session.stop()
                except Exception:
                    pass
        finally:
            try:
                if old is not None:
                    old.stop()
            except Exception:
                pass
        self._start_server()
        for ctx in self.contexts.values():
            ctx.session = self._new_session()
            ctx.mounted.clear()
            ctx.prepared = False
            self._prepare_session(ctx.session, ctx.default_lakehouse, ctx.default_schema)
            ctx.prepared = True
            self._bind_proxy(ctx)
        if self.contexts.get("default") is not None:
            self._root_session = self.contexts["default"].session
        self._notices.append("the Sail server was restarted to attach a schema-enabled lakehouse: DataFrames and temp views from before are gone")

    def _new_session(self):
        from pyspark.sql import SparkSession

        return SparkSession.builder.remote(self.url).create()

    def _prepare_session(self, sess, default_lakehouse: str | None, default_schema: str | None) -> None:
        """The context's default lakehouse is its current catalog, its default schema the
        current database; nothing else to set up (the catalogs are Cobalt's endpoint)."""
        lh = self._resolve_lakehouse(default_lakehouse)
        if lh is None:
            return
        schema = default_schema or self._default_schema(lh.name, sess) or "dbo"
        try:
            sess.sql(f"USE CATALOG {_q(lh.name)}").collect()
            sess.sql(f"USE DATABASE {_q(schema)}").collect()
        except Exception as exc:
            self.warnings.append(f"could not select {lh.name}.{schema}: {type(exc).__name__}: {str(exc).splitlines()[0][:200]}")

    # ---- namespaces and contexts ----

    def _seed_namespace(self, ns: dict, spark) -> None:
        import pyspark.sql.functions as F
        import pyspark.sql.types as T
        from pyspark.sql import Window

        ns.update({"spark": spark, "F": F, "T": T, "Window": Window, "display": self.display,
                   "notebookutils": _NotebookUtilsStub(), "mssparkutils": _NotebookUtilsStub()})

    def _bind_proxy(self, ctx: Context) -> None:
        """`spark` in the context's namespace mounts lakehouse tables on first use."""
        ctx.ns["spark"] = _SessionProxy(self, ctx)

    def _resolve_context(self, context: str | None) -> Context:
        ctx = self.contexts.get(context or "default")
        if ctx is None:
            raise ValueError(f"unknown context {context!r}; known: {sorted(self.contexts)}")
        return ctx

    def _activate(self, ctx: Context) -> None:
        if self._active is not ctx:
            self.shell.user_module = ctx.module
            self.shell.user_ns = ctx.ns
            self._active = ctx
        if not ctx.seeded:
            self.shell.init_user_ns()
            ctx.seeded = True

    def create_context(self, id: str, default_lakehouse: str | None = None, default_schema: str | None = None,
                       name: str | None = None) -> dict:
        if not isinstance(id, str) or not id.strip():
            raise ValueError("create_context: id must be a non-empty string")
        if id in self.contexts:
            raise ValueError(f"context {id!r} already exists")
        sess = self._new_session()
        lh_name = None
        if default_lakehouse:
            info = self._resolve_lakehouse(default_lakehouse)
            if info is None:
                raise ValueError(f"unknown lakehouse {default_lakehouse!r}; known: {sorted(self.lakehouses)}")
            lh_name = info.name
            if default_schema and default_schema not in (info.schemas or []):
                raise ValueError(f"lakehouse {info.name!r} has no schema {default_schema!r}; known: {info.schemas or []}")
        self._prepare_session(sess, lh_name, default_schema)
        module = types.ModuleType("__main__")
        module, _ = self.shell.prepare_user_module(module)
        self._seed_namespace(module.__dict__, sess)
        ctx = Context(id, sess, module, lh_name, default_schema or self._default_schema(lh_name), name=(name or None), prepared=True)
        self._bind_proxy(ctx)
        self.contexts[id] = ctx
        return self._context_info(ctx)

    def drop_context(self, id: str, force: bool = False) -> dict:
        if id == "default":
            raise ValueError("the default context cannot be dropped")
        ctx = self._resolve_context(id)
        if self._cell_running and self._cell_context == id:
            if not force:
                raise RuntimeError(f"context {id!r} is running a cell; interrupt it first (or drop with force)")
            ctx.pending_drop = True
            self.interrupt(context=id)
            return {"id": id, "dropped": False, "scheduled": True, "contexts": sorted(self.contexts)}
        try:
            ctx.session.stop()
        except Exception:
            pass
        del self.contexts[id]
        if self._active is ctx:
            self._activate(self.contexts["default"])
        return {"id": id, "dropped": True, "scheduled": False, "contexts": sorted(self.contexts)}

    def _context_info(self, ctx: Context) -> dict:
        cur_db = cur_cat = None
        try:
            row = ctx.session.sql("SELECT current_catalog() AS c, current_database() AS d").collect()[0]
            cur_cat, cur_db = row[0], row[1]
        except Exception:
            pass
        return {"id": ctx.id, "name": ctx.name, "default_lakehouse": ctx.default_lakehouse, "default_schema": ctx.default_schema,
                "dropping": ctx.pending_drop, "files_fs": None, "current_database": cur_db, "current_catalog": cur_cat,
                "created_at": ctx.created_at, "cells": ctx.cells, "last_activity": ctx.last_activity,
                "idle_s": round(time.time() - (ctx.last_activity or ctx.created_at), 1), "mounted": sorted(ctx.mounted)}

    # ---- running ----

    def _running(self, method: str, job_description: str | None):
        engine = self

        class _Running:
            def __enter__(self_):
                engine._interrupt_requested = False
                engine._cell_method = method
                engine._cell_context = engine._active.id
                engine._active.cells += 1
                engine._cell_started = time.time()
                engine._cell_running = True

            def __exit__(self_, *exc):
                engine._cell_running = False
                engine._cell_started = None
                engine._cell_method = None
                engine._cell_context = None
                engine._last_activity = time.time()
                engine._active.last_activity = engine._last_activity
                if engine._interrupt_requested:
                    engine._drain_pending_interrupt()
                if engine._active.pending_drop:
                    engine._active.pending_drop = False
                    try:
                        engine.drop_context(engine._active.id)
                    except Exception:
                        pass
                return False

        return _Running()

    def _drain_pending_interrupt(self) -> None:
        """A KeyboardInterrupt raised for a cell that ended first lands on the next bytecode:
        give it a few harmless places to land so it never hits the socket writes."""
        for _ in range(10):
            try:
                time.sleep(0.02)
            except KeyboardInterrupt:
                continue

    def _exec(self, code: str, on_output=None, capture_result: bool = False, job_description: str | None = None,
              context: str | None = None) -> ExecResult:
        from IPython.utils.capture import capture_output

        self._displays, self._blobs = [], []
        self._activate(self._resolve_context(context))
        with self._running("run_code", job_description):
            if on_output is None:
                with capture_output() as cap:
                    result = self.shell.run_cell(code, store_history=True)
                cap_stdout, cap_stderr = cap.stdout, cap.stderr
            else:
                out_tee, err_tee = _Tee("stdout", on_output), _Tee("stderr", on_output)
                with capture_output(stdout=False, stderr=False, display=True) as cap:
                    saved = sys.stdout, sys.stderr
                    sys.stdout, sys.stderr = out_tee, err_tee
                    try:
                        result = self.shell.run_cell(code, store_history=True)
                    finally:
                        sys.stdout, sys.stderr = saved
                        out_tee.close()
                        err_tee.close()
                cap_stdout, cap_stderr = out_tee.getvalue(), err_tee.getvalue()
        if capture_result and result.success and result.result is not None:
            try:
                self._capture_result(result.result)
            except Exception as exc:
                cap_stderr += f"\ncobalt-sail: could not capture the result as Arrow: {type(exc).__name__}: {exc}\n"
        error = tb = None
        exc = result.error_before_exec or result.error_in_exec
        if exc is not None:
            error = f"{type(exc).__name__}: {exc}" if str(exc) else repr(exc)
            if result.error_in_exec is not None:
                tb = "".join(_tb.format_exception(type(exc), exc, exc.__traceback__))
        stdout = cap_stdout
        for out in cap.outputs:
            text = out.data.get("text/plain") if hasattr(out, "data") else None
            if text:
                stdout += text + "\n"
        if error and exc is not None and self._interrupt_requested and not isinstance(exc, KeyboardInterrupt):
            error = f"KeyboardInterrupt: interrupted (Spark operations cancelled); underlying {error}"
        return ExecResult(ok=bool(result.success), stdout=_truncate(stdout), stderr=_truncate(cap_stderr), error=error,
                          interrupted=exc is not None and (isinstance(exc, KeyboardInterrupt) or self._interrupt_requested),
                          displays=list(self._displays), traceback=_truncate(tb) if tb else None,
                          execution_count=self.shell.execution_count)

    def run_code(self, code: str, on_output=None, capture_result: bool = False, job_description: str | None = None,
                 context: str | None = None) -> ExecResult:
        res = self._exec(code, on_output=on_output, capture_result=capture_result, job_description=job_description, context=context)
        res.notices.extend(self._drain_notices())
        self.blobs_out = list(self._blobs)
        return res

    def interrupt(self, context: str | None = None) -> dict:
        import _thread

        if not self._cell_running:
            return {"interrupted": False, "state": "idle", "reason": "idle: no cell is running"}
        if context is not None and context != self._cell_context:
            return {"interrupted": False, "state": "idle", "reason": f"idle: context {context!r} is not running (running: {self._cell_context!r})"}
        self._interrupt_requested = True
        t0 = time.time()
        ctx = self.contexts.get(self._cell_context or "default")
        try:
            ids = ctx.session.interruptAll() if ctx is not None else []
            cancel = f"{len(ids)} operation(s) interrupted in {time.time() - t0:.2f}s"
        except Exception as exc:
            cancel = f"interruptAll failed: {type(exc).__name__}: {exc}"
        if self._cell_method == "run_code":
            # Python code may be in a loop of its own; the gRPC call returns on its own
            _thread.interrupt_main()
        return {"interrupted": True, "state": "interrupting", "detail": cancel, "method": self._cell_method,
                "context": self._cell_context, "elapsed_s": round(time.time() - self._cell_started, 1) if self._cell_started else None}

    def status(self, context: str | None = None) -> dict:
        cell = None
        if self._cell_running:
            cell = {"method": self._cell_method, "context": self._cell_context,
                    "elapsed_s": round(time.time() - self._cell_started, 1) if self._cell_started else None, "jobs": []}
        idle = time.time() - self._last_activity
        return {"initialized": True, "cell_running": self._cell_running and (context is None or context == self._cell_context),
                "cell": cell, "idle_s": round(idle, 1), "last_activity": self._last_activity, "contexts": sorted(self.contexts)}

    # ---- Arrow results ----

    def _arrow_from_df(self, df, limit: int) -> tuple[bytes, dict]:
        import pyarrow as pa

        table = df.limit(limit + 1).toArrow()
        truncated = table.num_rows > limit
        if truncated:
            table = table.slice(0, limit)
        sink = pa.BufferOutputStream()
        with pa.ipc.new_stream(sink, table.schema) as writer:
            writer.write_table(table)
        data = sink.getvalue().to_pybytes()
        return data, {"arrow_bytes": len(data), "row_count": table.num_rows, "truncated": truncated, "limit": limit,
                      "columns": list(table.schema.names)}

    def _capture_result(self, value) -> None:
        import pyarrow as pa
        from pyspark.sql import DataFrame

        limit = self.default_sql_limit
        if isinstance(value, _SessionProxy):
            return
        if isinstance(value, DataFrame):
            data, meta = self._arrow_from_df(value, limit)
        else:
            try:
                import pandas as pd
            except ImportError:
                return
            if not isinstance(value, pd.DataFrame):
                return
            table = pa.Table.from_pandas(value.head(limit + 1), preserve_index=False)
            truncated = table.num_rows > limit
            if truncated:
                table = table.slice(0, limit)
            sink = pa.BufferOutputStream()
            with pa.ipc.new_stream(sink, table.schema) as writer:
                writer.write_table(table)
            data = sink.getvalue().to_pybytes()
            meta = {"arrow_bytes": len(data), "row_count": table.num_rows, "truncated": truncated, "limit": limit,
                    "columns": list(table.schema.names)}
        meta.update(kind="arrow", source="result")
        self._displays.append(meta)
        self._blobs.append(data)

    def display(self, obj, limit: int | None = None) -> None:
        from pyspark.sql import DataFrame

        if isinstance(obj, DataFrame):
            data, meta = self._arrow_from_df(obj, limit or self.default_sql_limit)
            meta.update(kind="arrow", source="display")
            self._displays.append(meta)
            self._blobs.append(data)
        else:
            print(repr(obj))

    def _drain_notices(self) -> list[str]:
        out, self._notices = self._notices, []
        return out

    # ---- lakehouse tables: lazy mounts ----

    def _sql_with_automount(self, sql: str, ctx: Context):
        """`spark.sql` plus analysis (a Connect DataFrame is lazy: an unknown table only shows
        when the schema is asked for; raising here keeps the error with the statement)."""
        df = ctx.session.sql(sql)
        df.columns  # noqa: B018 — forces analysis
        return df

    def _lakehouse_target(self, target: str, ctx: Context) -> tuple[Lakehouse, str, str] | None:
        """(lakehouse, schema, table) when a write target names a lakehouse table: three parts
        with a lakehouse first, or one / two parts in a context whose current catalog is a
        lakehouse. `spark_catalog.…` and plain sessions are not lakehouse targets."""
        parts = _parts(target)
        if len(parts) >= 3:
            lh = self._resolve_lakehouse(".".join(parts[:-2]))
            return (lh, parts[-2], parts[-1]) if lh is not None else None
        lh = self._resolve_lakehouse(ctx.default_lakehouse)
        if lh is None:
            return None
        schema = parts[0] if len(parts) == 2 else (ctx.default_schema or self._default_schema(lh.name) or "dbo")
        return lh, schema, parts[-1]

    def _guard_write(self, sql: str, ctx: Context) -> str:
        """Read-only sessions refuse writes that would reach a lakehouse: a target in a lakehouse
        catalog (named, or the context's current one), or a CREATE at an abfss:// location.
        Scratch tables in `spark_catalog` and temp views are fine. In write-through, DROP TABLE
        on a lakehouse table is refused too (Fabric would delete the folder), and CREATE TABLE
        is rewritten to create the Delta table at its lakehouse path (Sail's catalog-managed
        CREATE needs a Unity table id Fabric does not issue). Returns the SQL to run."""
        target = _sql_write_target(sql)
        if not target:
            return sql
        verb = _write_verb(sql) or "write"
        resolved = self._lakehouse_target(target, ctx)
        lakehouse_table = resolved is not None or ("abfss://" in sql.lower() and verb == "CREATE")
        if not lakehouse_table:
            return sql
        if self.write_mode != "writethrough":
            raise RuntimeError(f"write_mode is 'readonly': refusing to {verb} {target} on OneLake. "
                               "Switch the session to write-through (lakehouse button) to write to the lakehouse; "
                               "LakeSail has no sandbox clones.")
        if verb == "DROP":
            raise RuntimeError(f"DROP TABLE {target} is not done through LakeSail (Fabric would delete the lakehouse folder). "
                               "Delete the table in Fabric, or drop it on the Local Spark engine.")
        if verb == "CREATE" and resolved is not None and not re.search(r"\bLOCATION\s+'", sql, re.IGNORECASE):
            return self._rewrite_create(sql, ctx, *resolved)
        return sql

    def _rewrite_create(self, sql: str, ctx: Context, lh: Lakehouse, schema: str, table: str) -> str:
        """`CREATE TABLE [lh.][schema.]t …` → the same statement on a scratch name in
        `spark_catalog` with `USING delta LOCATION '<abfss…/Tables/schema/t>'`, so the data lands
        in the lakehouse and Fabric lists the table; the scratch registration is dropped and the
        catalog listing refreshed afterwards (`ctx.pending_create`)."""
        m = _CREATE_HEAD.match(sql)
        if not m:
            return sql
        rest = sql[m.end():]
        scratch = f"{_q(DEFAULT_CATALOG)}.{_q('default')}.{_q('__cobalt_create_' + table)}"
        location = f"LOCATION '{lh.path(schema, table)}'"
        if re.search(r"\bUSING\s+delta\b", rest, re.IGNORECASE):
            rest = re.sub(r"\bUSING\s+delta\b", lambda mm: f"{mm.group(0)} {location}", rest, count=1, flags=re.IGNORECASE)
        elif re.search(r"\bUSING\s+\w+", rest, re.IGNORECASE):
            raise RuntimeError(f"CREATE TABLE {table}: lakehouse tables are Delta tables; use USING delta (or no USING)")
        else:
            m_as = re.search(r"\bAS\b", rest, re.IGNORECASE)
            rest = (rest[:m_as.start()] + f"USING delta {location} " + rest[m_as.start():]) if m_as else (rest.rstrip().rstrip(";") + f" USING delta {location}")
        ctx.pending_create = (scratch, lh, schema, table)
        return f"CREATE TABLE {scratch} {rest}"

    def _finish_create(self, ctx: Context) -> None:
        """After a rewritten CREATE: forget the scratch registration (the data stays) and tell
        Cobalt's catalog endpoint to list the schema again."""
        pending, ctx.pending_create = ctx.pending_create, None
        if not pending:
            return
        scratch, lh, schema, table = pending
        try:
            ctx.session.sql(f"DROP TABLE IF EXISTS {scratch}").collect()
        except Exception:
            pass
        endpoint = (self.catalog or {}).get("endpoint")
        if endpoint:
            try:
                base = endpoint.split("/api/2.1/unity-catalog")[0]
                req = urllib.request.Request(f"{base}/cobalt/invalidate", data=json.dumps({"lakehouse": lh.name, "schema": schema, "created": table}).encode(), method="POST",
                                             headers={"Content-Type": "application/json"})
                urllib.request.urlopen(req, timeout=10).read()
            except Exception:
                pass
        self._notices.append(f"created {lh.name}.{schema}.{table} in the lakehouse")

    def _dml_metrics(self, sql: str, df) -> dict | None:
        if not _sql_write_target(sql):
            return None
        try:
            cols = list(df.columns)
            if cols == ["count"] or set(cols) <= {"count", "num_affected_rows", "num_inserted_rows", "num_updated_rows", "num_deleted_rows"}:
                row = df.collect()
                if not row:
                    return None
                d = row[0].asDict()
                n = int(d.get("num_affected_rows", d.get("count", 0)) or 0)
                out = {"affected_rows": n, "source": "result", "operation": _write_verb(sql) or "WRITE"}
                for k, name in (("num_inserted_rows", "inserted"), ("num_updated_rows", "updated"), ("num_deleted_rows", "deleted")):
                    if k in d:
                        out[name] = int(d[k] or 0)
                verb = out["operation"]
                if "inserted" not in out and verb in ("INSERT", "INSERT OVERWRITE", "CREATE"):
                    out["inserted"] = n
                elif "updated" not in out and verb == "UPDATE":
                    out["updated"] = n
                elif "deleted" not in out and verb == "DELETE":
                    out["deleted"] = n
                return out
            return None
        except Exception as exc:
            return {"error": f"{type(exc).__name__}: {str(exc).splitlines()[0][:200]}"}

    def _iter_batches(self, df, sess, batch_rows: int | None):
        """The client's Arrow batches, re-chunked to about `batch_rows` rows."""
        import pyarrow as pa

        client = sess.client
        req = client._execute_plan_request_with_metadata()
        req.plan.CopyFrom(df._plan.to_proto(client))
        try:
            it = client._execute_and_fetch_as_iterator(req, {})
        except TypeError:
            it = client._execute_and_fetch_as_iterator(req)

        def chunks(batch):
            if batch_rows and batch.num_rows > batch_rows:
                off = 0
                while off < batch.num_rows:
                    yield batch.slice(off, batch_rows)
                    off += batch_rows
            else:
                yield batch

        for resp in it:
            if isinstance(resp, pa.RecordBatch):
                yield from chunks(resp)

    def _stream(self, df, sess, batch_rows: int | None, on_batch) -> tuple[int, int]:
        import pyarrow as pa

        total = count = 0
        for batch in self._iter_batches(df, sess, batch_rows):
            sink = pa.BufferOutputStream()
            with pa.ipc.new_stream(sink, batch.schema) as w:
                w.write_batch(batch)
            data = sink.getvalue().to_pybytes()
            on_batch({"rows": batch.num_rows, "batch": count, "partition": 0, "arrow_bytes": len(data)}, data)
            total += batch.num_rows
            count += 1
        return total, count

    def run_sql(self, sql: str, limit: int | None = None, arrow: bool = False, job_description: str | None = None,
                context: str | None = None, batch_rows: int | None = None, on_batch=None) -> SqlResult:
        streaming = on_batch is not None
        if limit is None and not streaming:
            limit = self.default_sql_limit
        self.blobs_out = []
        ctx = self._resolve_context(context)
        self._activate(ctx)
        t0 = time.time()
        if _USE_PLAIN.match(sql):
            sql = re.sub(r"^\s*USE\s+", "USE DATABASE ", sql, count=1, flags=re.IGNORECASE)
        sql = self._guard_write(sql, ctx)
        with self._running("run_sql", job_description):
            try:
                df = self._sql_with_automount(sql, ctx)
                self._finish_create(ctx)
                columns = list(df.columns)
                metrics = self._dml_metrics(sql, df)
                if metrics is not None and "error" not in metrics:
                    return SqlResult(columns=columns, rows=[], row_count=0, truncated=False, limit=limit or 0,
                                     notices=self._drain_notices(), metrics=metrics, batches=0 if streaming else None,
                                     elapsed_s=round(time.time() - t0, 3),
                                     arrow={"columns": columns, "row_count": 0, "streamed": True, "batches": 0} if streaming else None)
                if streaming:
                    if limit:
                        df = df.limit(limit)
                    total, nb = self._stream(df, ctx.session, batch_rows, on_batch) if columns else (0, 0)
                    return SqlResult(columns=columns, rows=[], row_count=total, truncated=False, limit=limit or 0,
                                     notices=self._drain_notices(), metrics=metrics, batches=nb, elapsed_s=round(time.time() - t0, 3),
                                     arrow={"columns": columns, "row_count": total, "streamed": True, "batches": nb})
                if arrow and columns:
                    data, meta = self._arrow_from_df(df, limit)
                    self.blobs_out = [data]
                    return SqlResult(columns=meta["columns"], rows=[], row_count=meta["row_count"], truncated=meta["truncated"],
                                     limit=limit, notices=self._drain_notices(), arrow=meta, metrics=metrics,
                                     elapsed_s=round(time.time() - t0, 3))
                collected = df.limit(limit + 1).collect() if columns else []
            except KeyboardInterrupt:
                raise InterruptedQuery("KeyboardInterrupt: interrupted (Spark operations cancelled)") from None
            except Exception as exc:
                if self._interrupt_requested:
                    raise InterruptedQuery(f"KeyboardInterrupt: interrupted (Spark operations cancelled); underlying {type(exc).__name__}: {exc}") from exc
                raise
        truncated = len(collected) > limit
        collected = collected[:limit]
        rows = [[_jsonify(v) for v in row] for row in collected]
        return SqlResult(columns=columns, rows=rows, row_count=len(rows), truncated=truncated, limit=limit,
                         notices=self._drain_notices(), metrics=metrics, elapsed_s=round(time.time() - t0, 3))

    # ---- info and shutdown ----

    def info(self) -> dict:
        import pyspark

        try:
            import importlib.metadata as md

            sail_v = md.version("pysail")
        except Exception:
            sail_v = "?"
        cur = {}
        try:
            cur = self._context_info(self._active) if self._active else {}
        except Exception:
            pass
        return {
            "engine": ENGINE, "engine_version": sail_v, "spark_version": getattr(pyspark, "__version__", "?"),
            "app_id": f"sail-{os.getpid()}", "master": self.url, "profile": "sail",
            "current_database": cur.get("current_database"), "current_catalog": cur.get("current_catalog"),
            "lakehouses": {n: {"id": lh.id, "workspace_id": lh.workspace_id, "schemas": lh.schemas or []} for n, lh in self.lakehouses.items()},
            "lakehouse_schemas": {n: list(lh.schemas or []) for n, lh in self.lakehouses.items()},
            "default_lakehouse": self.default_lakehouse, "write_mode": self.write_mode, "requested_write_mode": self.requested_write_mode,
            "started_at": self.started_at, "uptime_s": round(time.time() - self.started_at, 1), "python": sys.version.split()[0],
            "protocol_version": PROTOCOL_VERSION, "features": list(FEATURES), "default_sql_limit": self.default_sql_limit,
            "execution_count": getattr(self.shell, "execution_count", None), "contexts": sorted(self.contexts),
            "active_context": self._active.id if self._active else None, "warnings": list(self.warnings),
        }

    def alive(self) -> bool:
        try:
            return bool(self.server is not None and self.server.running)
        except Exception:
            return False

    def stop(self) -> None:
        for ctx in list(self.contexts.values()):
            try:
                ctx.session.stop()
            except Exception:
                pass
        try:
            if self.server is not None and self.server.running:
                self.server.stop()
        except Exception:
            pass


def _toml(s: str) -> str:
    return s.replace("\\", "\\\\").replace('"', '\\"')


class _SessionProxy:
    """The `spark` a cell sees: the context's Spark Connect session, with `sql` and `table`
    mounting lakehouse tables on first use (a Connect DataFrame is lazy, so the analysis is
    forced here, where the missing table can still be mounted and the statement retried).
    Everything else is the real session's."""

    def __init__(self, engine: "SailEngine", ctx: "Context"):
        object.__setattr__(self, "_engine", engine)
        object.__setattr__(self, "_ctx", ctx)

    @property
    def _session(self):
        return self._ctx.session

    def sql(self, sqlQuery, args=None, **kwargs):  # noqa: N803 — PySpark's spelling
        if args or kwargs:
            return self._session.sql(sqlQuery, args, **kwargs)
        if _USE_PLAIN.match(sqlQuery):
            sqlQuery = re.sub(r"^\s*USE\s+", "USE DATABASE ", sqlQuery, count=1, flags=re.IGNORECASE)
        sqlQuery = self._engine._guard_write(sqlQuery, self._ctx)
        df = self._engine._sql_with_automount(sqlQuery, self._ctx)
        self._engine._finish_create(self._ctx)
        return df

    def table(self, tableName):  # noqa: N803
        return self._session.table(tableName)

    def __getattr__(self, name):
        return getattr(self._session, name)

    def __setattr__(self, name, value):
        setattr(self._session, name, value)

    def __repr__(self):
        return f"<SparkSession (LakeSail, {self._engine.url})>"


class _NotebookUtilsStub:
    """`notebookutils` is not available on the LakeSail engine yet (slice B)."""

    def __getattr__(self, name):
        raise AttributeError(f"notebookutils.{name} is not available on the LakeSail engine; use abfss:// paths with spark.read / spark.write, or run this notebook on Local Spark (JVM)")


# ---- healthcheck ----

def healthcheck() -> dict:
    problems: list[str] = []
    warnings: list[str] = []
    versions: dict = {"python": sys.version.split()[0]}
    import importlib.metadata as md

    for pkg in ("pysail", "pyspark-client", "pyarrow", "ipython", "pandas", "grpcio"):
        try:
            versions[pkg] = md.version(pkg)
        except Exception:
            if pkg == "pyspark-client":
                try:
                    versions["pyspark"] = md.version("pyspark")
                    continue
                except Exception:
                    pass
            problems.append(f"{pkg} is not installed")
    try:
        import pysail.spark  # noqa: F401
    except Exception as exc:
        problems.append(f"pysail cannot load: {type(exc).__name__}: {exc}")
    try:
        import pyspark.sql.connect.session  # noqa: F401
    except Exception as exc:
        problems.append(f"the PySpark Connect client cannot load: {type(exc).__name__}: {exc}")
    try:
        import pandas

        if int(str(pandas.__version__).split(".")[0]) >= 3:
            warnings.append(f"pandas {pandas.__version__}: PySpark does not fully support pandas 3 yet")
    except Exception:
        pass
    return {"ok": not problems, "problems": problems, "warnings": warnings, "versions": versions, "engine": ENGINE,
            "profile": "sail", "protocol_version": PROTOCOL_VERSION, "features": list(FEATURES)}


# ---- the worker loop (local-spark-mcp worker.py) ----

_FATAL_MARKERS = ("StatusCode.UNAVAILABLE", "failed to connect to all addresses", "Connection refused", "ConnectionRefusedError",
                  "ConnectionResetError", "is not running", "the server is not started")


def _chain(exc):
    seen = set()
    while exc is not None and id(exc) not in seen:
        seen.add(id(exc))
        yield exc
        exc = exc.__cause__ or exc.__context__


def _result_is_fatal(result, engine) -> bool:
    if not isinstance(result, dict) or (result.get("ok", True) and "error" not in result):
        return False
    if result.get("interrupted"):
        return engine is not None and not engine.alive()
    text = f"{result.get('error') or ''}\n{result.get('traceback') or ''}"
    if any(m in text for m in _FATAL_MARKERS):
        return engine is None or not engine.alive()
    return False


def _is_fatal(exc: BaseException, engine) -> bool:
    msgs = " ".join(str(e) for e in _chain(exc))
    if any(m in msgs for m in _FATAL_MARKERS):
        return engine is None or not engine.alive()
    return False


def _handle(engine, method: str, params: dict):
    if method == "healthcheck":
        return healthcheck(), engine
    if method == "init":
        if engine is not None:
            engine.stop()
        params.pop("profile", None)
        engine = SailEngine(**params)
        info = engine.info()
        info["profile_warnings"] = list(engine.warnings)
        info["control"] = True
        return info, engine
    if method == "ping":
        return {"pong": True}, engine
    if engine is None:
        raise RuntimeError("engine not initialized; send 'init' first")
    if method == "create_context":
        return engine.create_context(params["id"], params.get("default_lakehouse"), params.get("default_schema"), name=params.get("name")), engine
    if method == "drop_context":
        return engine.drop_context(params["id"], force=bool(params.get("force"))), engine
    if method == "register_lakehouse":
        return engine.register_lakehouse(params["lakehouse"]), engine
    if method == "unregister_lakehouse":
        return engine.unregister_lakehouse(params["name"]), engine
    if method == "list_tables":
        return engine.list_tables(params["lakehouse"]), engine
    if method == "run_code":
        return engine.run_code(params["code"], on_output=params.get("_on_output"), capture_result=bool(params.get("capture_result")),
                               job_description=params.get("job_description"), context=params.get("context")).to_dict(), engine
    if method == "run_sql":
        return engine.run_sql(params["sql"], params.get("limit"), bool(params.get("arrow")), job_description=params.get("job_description"),
                              context=params.get("context"), batch_rows=params.get("batch_rows"), on_batch=params.get("_on_batch")).to_dict(), engine
    if method == "info":
        return engine.info(), engine
    if method == "status":
        return engine.status(params.get("context")), engine
    raise ValueError(f"unknown method: {method!r} (not available on the LakeSail engine)")


class _Shared:
    engine = None


def _control_loop(port: int, shared: _Shared) -> None:
    try:
        sock = socket.create_connection(("127.0.0.1", port))
    except OSError:
        return
    try:
        while True:
            req = recv_msg(sock)
            if req is None:
                return
            rid, method = req.get("id"), req.get("method")
            cparams = req.get("params") or {}
            eng = shared.engine
            try:
                if method == "ping":
                    result = {}
                elif method == "status":
                    result = eng.status(cparams.get("context")) if eng is not None else {"initialized": False, "cell_running": False, "cell": None}
                elif method == "interrupt":
                    result = eng.interrupt(cparams.get("context")) if eng is not None else {"interrupted": False, "reason": "not initialized"}
                elif method == "preload_status":
                    result = {"state": "idle"}
                elif method == "drop_context":
                    if eng is None:
                        raise ValueError("not initialized")
                    result = eng.drop_context(cparams["id"], force=bool(cparams.get("force")))
                else:
                    raise ValueError(f"unknown control method {method!r}")
                send_msg(sock, {"id": rid, "ok": True, "result": result})
            except Exception as exc:
                send_msg(sock, {"id": rid, "ok": False, "error": f"{type(exc).__name__}: {exc}"})
    except OSError:
        return


def run_worker(port: int, control_port: int | None = None) -> int:
    sock = socket.create_connection(("127.0.0.1", port))
    shared = _Shared()
    if control_port:
        threading.Thread(target=_control_loop, args=(control_port, shared), name="sail-control", daemon=True).start()
    engine = None
    try:
        while True:
            try:
                req = recv_msg(sock)
            except KeyboardInterrupt:
                continue
            if req is None:
                break
            rid = req.get("id")
            method = req.get("method")
            params = req.get("params") or {}
            if method == "shutdown":
                send_msg(sock, {"id": rid, "ok": True, "result": {}})
                break
            stream = params.pop("stream", False)
            if stream and method == "run_code":
                def _on_output(stream_name, text, _rid=rid):
                    send_msg(sock, {"id": _rid, "event": stream_name, "text": text})
                params["_on_output"] = _on_output
            if method == "run_sql" and (stream or params.get("batch_rows")):
                def _on_batch(meta, blob, _rid=rid):
                    send_reply(sock, {"id": _rid, "event": "batch", **meta}, [blob])
                params["_on_batch"] = _on_batch
            try:
                result, engine = _handle(engine, method, params)
                shared.engine = engine
                blobs = list(getattr(engine, "blobs_out", []) or []) if engine is not None else []
                if engine is not None:
                    engine.blobs_out = []
                send_reply(sock, {"id": rid, "ok": True, "result": result, "fatal": _result_is_fatal(result, engine)}, blobs)
            except KeyboardInterrupt:
                send_msg(sock, {"id": rid, "ok": False, "error": "KeyboardInterrupt: interrupted", "traceback": None, "fatal": False})
            except Exception as exc:
                interrupted = isinstance(exc, InterruptedQuery)
                send_msg(sock, {"id": rid, "ok": False, "error": str(exc) if interrupted else f"{type(exc).__name__}: {exc}",
                                "traceback": None if interrupted else _tb.format_exc(), "fatal": False if interrupted else _is_fatal(exc, engine),
                                "interrupted": interrupted})
    finally:
        if engine is not None:
            engine.stop()
        sock.close()
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="cobalt_sail_worker")
    parser.add_argument("--port", type=int, help="Cobalt's listener port")
    parser.add_argument("--control-port", type=int, default=None, help="Cobalt's listener for the control socket")
    parser.add_argument("--healthcheck", action="store_true", help="print the environment's health as JSON and exit")
    args = parser.parse_args(argv)
    if args.healthcheck:
        print(json.dumps(healthcheck()))
        return 0
    if args.port is None:
        parser.error("--port is required")
    return run_worker(args.port, args.control_port)


if __name__ == "__main__":
    sys.exit(main())
