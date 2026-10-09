//! Client for local-spark-mcp's worker process. Cobalt listens on a loopback port, spawns
//! `python -m local_spark_mcp.worker --port N [--control-port M]`, and exchanges length-prefixed
//! JSON frames: requests `{"id", "method", "params"}`, responses `{"id", "ok", "result"}` or
//! `{"id", "ok": false, "error", "traceback", "fatal"}`.
//!
//! Protocol version 2 (local-spark-mcp 0.4.0) keeps the framing and adds, on the data socket,
//! event frames `{"id", "event": "stdout"|"stderr", "text"}` before a streamed `run_code` reply
//! and `"binary": [sizes]` on a reply that is followed by that many raw blobs (Arrow IPC
//! streams); and a second, control socket served by its own thread in the worker, where
//! `interrupt`, `ping`, `status` and `preload_status` answer while a cell runs.
//!
//! The data socket is synchronous: one request at a time. The worker's stdout/stderr (Spark and
//! py4j chatter, Ivy progress, tracebacks) is pumped to a `log` callback while calls wait.

use crate::{Result, RuntimeError};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub type LogFn = Arc<dyn Fn(String) + Send + Sync>;
/// `(event, text)` for streamed cell output.
pub type EventFn<'a> = &'a mut dyn FnMut(&str, &str);
/// Every event frame of a streamed call, with the binary blobs it announced (`run_sql` batches).
pub type FrameFn<'a> = &'a mut dyn FnMut(&Value, Vec<Vec<u8>>);

#[derive(Clone, Debug)]
pub struct WorkerConfig {
    /// The environment's interpreter.
    pub python: PathBuf,
    pub env: Vec<(String, String)>,
    /// `SparkEngine` keyword arguments for `init`.
    pub init: Value,
    pub startup_timeout: Duration,
    /// Open the control socket (`--control-port`; local-spark-mcp 0.4.0+). An older worker
    /// rejects the argument, so this follows the installed package version.
    pub control: bool,
    /// The module run with `-m`: `local_spark_mcp.worker` or Cobalt's Sail worker.
    pub module: String,
}

/// The control connection: usable from any thread while a data call is in flight.
#[derive(Clone)]
pub struct ControlHandle {
    stream: Arc<Mutex<TcpStream>>,
    next_id: Arc<AtomicU64>,
}

impl ControlHandle {
    fn new(stream: TcpStream) -> Self {
        Self { stream: Arc::new(Mutex::new(stream)), next_id: Arc::new(AtomicU64::new(1_000_000)) }
    }

    /// A request on the control socket (`interrupt`, `ping`, `status`, `preload_status`).
    pub fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut s = self.stream.lock().map_err(|_| RuntimeError::Worker("control socket poisoned".into()))?;
        send_frame(&mut s, &json!({"id": id, "method": method, "params": params}))?;
        let deadline = Instant::now() + timeout;
        let resp = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(RuntimeError::Worker(format!("{method} on the control socket timed out after {timeout:?}")));
            }
            s.set_read_timeout(Some(left))?;
            match recv_frame(&mut s) {
                Ok(Some(v)) => {
                    let got = v.get("id").and_then(Value::as_u64);
                    if got == Some(id) {
                        break v;
                    }
                    // a late duplicate for an earlier control request is dropped; a reply from
                    // the future means the socket is out of step
                    if got.map(|g| g < id).unwrap_or(false) {
                        continue;
                    }
                    return Err(RuntimeError::WorkerFatal(format!("control reply id mismatch (expected {id}, got {})", v.get("id").cloned().unwrap_or(Value::Null))));
                }
                Ok(None) => return Err(RuntimeError::Worker(format!("worker closed the control socket during {method}"))),
                Err(RuntimeError::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                    return Err(RuntimeError::Worker(format!("{method} on the control socket timed out after {timeout:?}")));
                }
                Err(e) => return Err(e),
            }
        };
        unwrap_reply(resp)
    }

    /// The worker answers as soon as its `cancelAllJobs` returns; the wait is generous because a
    /// cancel can take a few seconds when a job is being planned.
    pub fn interrupt(&self) -> Result<Value> {
        self.call("interrupt", json!({}), Duration::from_secs(30))
    }
}

pub struct Worker {
    child: Child,
    stream: TcpStream,
    control: Option<ControlHandle>,
    next_id: u64,
    log_rx: Receiver<String>,
    log: LogFn,
    tail: VecDeque<String>,
    /// The `init` result (versions, profile, databases…).
    pub info: Value,
    /// From `init` (1 when the worker does not say).
    pub protocol_version: u64,
    /// Raw blobs that followed the last reply (`"binary": [sizes]`), in order.
    pub last_blobs: Vec<Vec<u8>>,
    pub started: Instant,
}

pub fn send_frame(stream: &mut TcpStream, v: &Value) -> Result<()> {
    let data = serde_json::to_vec(v)?;
    let len = (data.len() as u32).to_be_bytes();
    stream.write_all(&len)?;
    stream.write_all(&data)?;
    stream.flush()?;
    Ok(())
}

pub fn recv_frame(stream: &mut TcpStream) -> Result<Option<Value>> {
    let mut len = [0u8; 4];
    match stream.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let n = u32::from_be_bytes(len) as usize;
    let mut body = vec![0u8; n];
    stream.read_exact(&mut body)?;
    Ok(Some(serde_json::from_slice(&body)?))
}

/// The sizes a reply announces with `"binary": [...]`.
pub fn blob_sizes(v: &Value) -> Vec<usize> {
    v.get("binary").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).map(|n| n as usize).collect()).unwrap_or_default()
}

/// Read the raw blobs that follow a reply (the stream must be blocking).
pub fn recv_blobs(stream: &mut TcpStream, sizes: &[usize]) -> Result<Vec<Vec<u8>>> {
    let mut out = Vec::with_capacity(sizes.len());
    for &n in sizes {
        let mut b = vec![0u8; n];
        stream.read_exact(&mut b)?;
        out.push(b);
    }
    Ok(out)
}

/// `result` of an `ok` reply, or the error it carries.
fn unwrap_reply(resp: Value) -> Result<Value> {
    if resp.get("ok").and_then(Value::as_bool) != Some(true) {
        let err = resp.get("error").and_then(Value::as_str).unwrap_or("unknown worker error");
        let tb = resp.get("traceback").and_then(Value::as_str).unwrap_or("");
        let fatal = resp.get("fatal").and_then(Value::as_bool).unwrap_or(false);
        let text = if tb.is_empty() { err.to_string() } else { format!("{err}\n{tb}") };
        return Err(if fatal { RuntimeError::WorkerFatal(text) } else { RuntimeError::Worker(text) });
    }
    if resp.get("fatal").and_then(Value::as_bool) == Some(true) {
        let r = resp.get("result").cloned().unwrap_or(Value::Null);
        return Err(RuntimeError::WorkerFatal(r.get("error").and_then(Value::as_str).unwrap_or("the Spark driver is no longer reachable").to_string()));
    }
    Ok(resp.get("result").cloned().unwrap_or(Value::Null))
}

impl Worker {
    /// Spawn and complete the `init` handshake. `cancel` aborts the wait (the process is killed).
    pub fn start(cfg: &WorkerConfig, log: LogFn, cancel: &AtomicBool) -> Result<Worker> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;
        let control_listener = if cfg.control {
            let l = TcpListener::bind(("127.0.0.1", 0))?;
            l.set_nonblocking(true)?;
            Some(l)
        } else {
            None
        };
        let mut cmd = Command::new(&cfg.python);
        cmd.args(["-m", &cfg.module, "--port", &port.to_string()]);
        if let Some(l) = &control_listener {
            cmd.args(["--control-port", &l.local_addr()?.port().to_string()]);
        }
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        for (k, v) in &cfg.env {
            cmd.env(k, v);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = cmd.spawn().map_err(|e| RuntimeError::Worker(format!("could not start {}: {e}", cfg.python.display())))?;
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        for pipe in [child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>), child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>)].into_iter().flatten() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                use std::io::BufRead;
                for line in std::io::BufReader::new(pipe).lines().map_while(std::result::Result::ok) {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        let mut w = Worker { child, stream: placeholder_stream(), control: None, next_id: 0, log_rx: rx, log, tail: VecDeque::new(), info: Value::Null, protocol_version: 1, last_blobs: Vec::new(), started: Instant::now() };
        let deadline = Instant::now() + cfg.startup_timeout;
        // the worker connects to the data port first, then to the control port
        let mut data: Option<TcpStream> = None;
        let mut control: Option<TcpStream> = None;
        loop {
            w.pump();
            if cancel.load(Ordering::Relaxed) {
                let _ = w.child.kill();
                return Err(RuntimeError::Cancelled);
            }
            if data.is_none() {
                match listener.accept() {
                    Ok((s, _)) => data = Some(s),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => {
                        let _ = w.child.kill();
                        return Err(e.into());
                    }
                }
            }
            if let (Some(l), true) = (&control_listener, data.is_some() && control.is_none()) {
                match l.accept() {
                    Ok((s, _)) => control = Some(s),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => {
                        let _ = w.child.kill();
                        return Err(e.into());
                    }
                }
            }
            if data.is_some() && (control_listener.is_none() || control.is_some()) {
                break;
            }
            if let Ok(Some(status)) = w.child.try_wait() {
                w.pump();
                return Err(RuntimeError::Worker(format!("worker exited with {status} before connecting:\n{}", w.tail_text())));
            }
            if Instant::now() > deadline {
                let _ = w.child.kill();
                return Err(RuntimeError::Worker(format!("worker did not connect within {:?}", cfg.startup_timeout)));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let stream = data.expect("data stream");
        // the listeners are non-blocking for the accept loop and, on Windows, an accepted socket
        // inherits that: reads must block (with the timeouts the callers set) or every wait
        // spins and the control socket reports a timeout at once
        stream.set_nonblocking(false)?;
        stream.set_nodelay(true)?;
        w.stream = stream;
        if let Some(c) = control {
            c.set_nonblocking(false)?;
            c.set_nodelay(true)?;
            w.control = Some(ControlHandle::new(c));
        }
        let info = w.call_cancellable("init", cfg.init.clone(), cfg.startup_timeout, cancel)?;
        w.pump();
        w.protocol_version = info.get("protocol_version").and_then(Value::as_u64).unwrap_or(1);
        w.info = info;
        Ok(w)
    }

    /// The control connection (protocol 2), shareable with other threads.
    pub fn control(&self) -> Option<ControlHandle> {
        self.control.clone()
    }

    /// Forward buffered worker output to the log.
    pub fn pump(&mut self) {
        while let Ok(line) = self.log_rx.try_recv() {
            self.tail.push_back(line.clone());
            if self.tail.len() > 60 {
                self.tail.pop_front();
            }
            (self.log)(line);
        }
    }

    /// The last lines the worker printed (for error reports).
    pub fn tail_text(&self) -> String {
        self.tail.iter().cloned().collect::<Vec<_>>().join("\n")
    }

    /// A request that waits for its reply while pumping logs; `cancel` aborts the wait, after
    /// which the connection is out of step and the worker must be killed.
    pub fn call_cancellable(&mut self, method: &str, params: Value, timeout: Duration, cancel: &AtomicBool) -> Result<Value> {
        self.call_streaming(method, params, timeout, cancel, &mut |_, _| {})
    }

    /// Like `call_cancellable`, and event frames (`{"id", "event", "text"}`, protocol 2 with
    /// `stream: true`) go to `on_event` as they arrive. A reply whose id is not this request's
    /// is fatal: the socket is out of step and the worker must be respawned.
    pub fn call_streaming(&mut self, method: &str, params: Value, timeout: Duration, cancel: &AtomicBool, on_event: EventFn<'_>) -> Result<Value> {
        let mut on_frame = |v: &Value, _blobs: Vec<Vec<u8>>| {
            let ev = v.get("event").and_then(Value::as_str).unwrap_or("").to_string();
            let text = v.get("text").and_then(Value::as_str).unwrap_or("").to_string();
            on_event(&ev, &text);
        };
        self.call_streaming_frames(method, params, timeout, cancel, &mut on_frame)
    }

    /// A streamed call whose event frames may carry binary blobs (`run_sql` with `stream`:
    /// one Arrow IPC stream per `batch` event). `on_frame` sees every event frame with its
    /// blobs; the final reply is returned.
    pub fn call_streaming_frames(&mut self, method: &str, params: Value, timeout: Duration, cancel: &AtomicBool, on_frame: FrameFn<'_>) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.last_blobs.clear();
        send_frame(&mut self.stream, &json!({"id": id, "method": method, "params": params}))?;
        let deadline = Instant::now() + timeout;
        self.stream.set_read_timeout(Some(Duration::from_millis(200)))?;
        let resp = loop {
            self.pump();
            if cancel.load(Ordering::Relaxed) {
                return Err(RuntimeError::Cancelled);
            }
            match recv_frame(&mut self.stream) {
                Ok(Some(v)) => {
                    if v.get("event").is_some() && v.get("ok").is_none() {
                        let sizes = blob_sizes(&v);
                        let blobs = if sizes.is_empty() {
                            Vec::new()
                        } else {
                            self.stream.set_read_timeout(None)?;
                            let b = recv_blobs(&mut self.stream, &sizes)?;
                            self.stream.set_read_timeout(Some(Duration::from_millis(200)))?;
                            b
                        };
                        on_frame(&v, blobs);
                        continue;
                    }
                    if v.get("id").and_then(Value::as_u64) != Some(id) {
                        return Err(RuntimeError::WorkerFatal(format!("reply id mismatch during {method} (expected {id}, got {}); the worker socket is out of step", v.get("id").cloned().unwrap_or(Value::Null))));
                    }
                    let sizes = blob_sizes(&v);
                    if !sizes.is_empty() {
                        self.stream.set_read_timeout(None)?;
                        self.last_blobs = recv_blobs(&mut self.stream, &sizes)?;
                    }
                    break v;
                }
                Ok(None) => return Err(RuntimeError::Worker(format!("worker closed the connection during {method}:\n{}", self.tail_text()))),
                Err(RuntimeError::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                    if let Ok(Some(status)) = self.child.try_wait() {
                        self.pump();
                        return Err(RuntimeError::Worker(format!("worker exited with {status} during {method}:\n{}", self.tail_text())));
                    }
                    if Instant::now() > deadline {
                        return Err(RuntimeError::Worker(format!("{method} timed out after {timeout:?}")));
                    }
                }
                Err(e) => return Err(e),
            }
        };
        self.stream.set_read_timeout(None)?;
        self.pump();
        unwrap_reply(resp)
    }

    /// A plain request with a timeout (no cancel).
    pub fn call(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let never = AtomicBool::new(false);
        self.call_cancellable(method, params, timeout, &never)
    }

    pub fn run_sql(&mut self, sql: &str, limit: Option<u64>) -> Result<Value> {
        self.call("run_sql", json!({"sql": sql, "limit": limit}), Duration::from_secs(60 * 60))
    }
    pub fn run_code(&mut self, code: &str) -> Result<Value> {
        self.call("run_code", json!({"code": code}), Duration::from_secs(60 * 60 * 24))
    }
    pub fn get_info(&mut self) -> Result<Value> {
        self.call("info", json!({}), Duration::from_secs(60))
    }
    pub fn ping(&mut self) -> Result<bool> {
        Ok(self.call("ping", json!({}), Duration::from_secs(10))?.get("pong").and_then(Value::as_bool).unwrap_or(false))
    }
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Ask the worker to stop, then make sure the process is gone.
    pub fn shutdown(mut self) {
        self.next_id += 1;
        let _ = send_frame(&mut self.stream, &json!({"id": self.next_id, "method": "shutdown", "params": {}}));
        let _ = self.stream.set_read_timeout(Some(Duration::from_secs(10)));
        let _ = recv_frame(&mut self.stream);
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Kill the process outright (after a cancelled call, the protocol is out of step).
    pub fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A connected-to-nothing stream used only until `start` accepts the real one.
fn placeholder_stream() -> TcpStream {
    let l = TcpListener::bind(("127.0.0.1", 0)).expect("loopback");
    let addr = l.local_addr().expect("addr");
    let c = TcpStream::connect(addr).expect("connect");
    let _ = l.accept();
    c
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_roundtrip() {
        let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let req = recv_frame(&mut s).unwrap().unwrap();
            assert_eq!(req["method"], "ping");
            send_frame(&mut s, &json!({"id": req["id"], "ok": true, "result": {"pong": true}})).unwrap();
        });
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        send_frame(&mut c, &json!({"id": 1, "method": "ping", "params": {}})).unwrap();
        let resp = recv_frame(&mut c).unwrap().unwrap();
        assert_eq!(resp["result"]["pong"], true);
        assert!(recv_frame(&mut c).unwrap().is_none());
        t.join().unwrap();
    }

    #[test]
    fn blobs_follow_a_reply() {
        let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            send_frame(&mut s, &json!({"id": 7, "ok": true, "result": {}, "binary": [3, 2]})).unwrap();
            s.write_all(b"abcde").unwrap();
        });
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let v = recv_frame(&mut c).unwrap().unwrap();
        let sizes = blob_sizes(&v);
        assert_eq!(sizes, vec![3, 2]);
        let blobs = recv_blobs(&mut c, &sizes).unwrap();
        assert_eq!(blobs, vec![b"abc".to_vec(), b"de".to_vec()]);
        t.join().unwrap();
    }

    #[test]
    fn control_handle_checks_ids() {
        let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let req = recv_frame(&mut s).unwrap().unwrap();
            // a stale duplicate first, then the real reply
            send_frame(&mut s, &json!({"id": 1, "ok": true, "result": {"stale": true}})).unwrap();
            send_frame(&mut s, &json!({"id": req["id"], "ok": true, "result": {"interrupted": true, "state": "interrupting"}})).unwrap();
            let _ = recv_frame(&mut s).unwrap().unwrap();
            send_frame(&mut s, &json!({"id": 99_999_999, "ok": true, "result": {}})).unwrap();
        });
        let h = ControlHandle::new(TcpStream::connect(("127.0.0.1", port)).unwrap());
        let r = h.interrupt().unwrap();
        assert_eq!(r["state"], "interrupting");
        assert!(matches!(h.call("ping", json!({}), Duration::from_secs(5)), Err(RuntimeError::WorkerFatal(_))));
        t.join().unwrap();
    }
}
