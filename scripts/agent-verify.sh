#!/usr/bin/env bash
# Deeper behaviour checks via egui-agent: snapshot stability, cancel, actual plan, copy, viewer.
set -u
SHOTS="${1:-C:/temp}"
A() { egui-agent-cli --pipe cobalt.agent "$@"; }
J() { local verb="$1"; local args; args="$(cat)"; A invoke "$verb" --quiet --args "$args"; }
shot() { A screenshot | python "$(dirname "$0")/save_shot.py" "$SHOTS/$1.png"; }
count_nodes() { A snapshot | python -c "
import sys,json
d=json.load(sys.stdin); n=0
def walk(x):
    global n
    if isinstance(x,dict):
        n+=1
        for k in ('children','nodes'):
            for c in x.get(k,[]) or []: walk(c)
    elif isinstance(x,list):
        for c in x: walk(c)
walk(d); print(n)"; }
wait_state() { local n=0; while [ $n -lt "${1:-30}" ]; do st=$(A invoke run_state --quiet 2>/dev/null | python -c "import sys,json; print(json.load(sys.stdin)['invoked']['result']['state'])" 2>/dev/null); case "$st" in done|failed|cancelled|paused) echo "state=$st after ${n}s"; return;; esac; sleep 1; n=$((n+1)); done; echo "timeout state=$st"; }

echo "### snapshot node counts: idle, idle again, after a click"
count_nodes; count_nodes; A invoke click --target '{"label":"tab SQLQuery_1"}' --quiet >/dev/null 2>&1; count_nodes

echo "### connect + cancel test (WAITFOR 20s, cancel after 2s)"
J connect <<'J'
{"profile":"local"}
J
sleep 4
J set_query <<'J'
{"text":"WAITFOR DELAY '00:00:20'; SELECT 1 AS after_wait;"}
J
A invoke run --quiet >/dev/null; sleep 2
t0=$(date +%s); A invoke cancel --quiet >/dev/null; wait_state 15; t1=$(date +%s); echo "cancel took $((t1-t0))s"
A invoke messages --quiet | python -c "import sys,json; [print(' *', m['text'].replace(chr(10),' | ')) for m in json.load(sys.stdin)['invoked']['result']]"
echo "--- connection still usable?"
J set_query <<'J'
{"text":"SELECT 42 AS answer"}
J
A invoke run --quiet >/dev/null; wait_state 10
J results <<'J'
{"limit":1}
J

echo "### actual plan"
J set_query <<'J'
{"text":"SELECT category, COUNT(*) AS n FROM dbo.big WHERE id < 50000 GROUP BY category ORDER BY n DESC"}
J
J run <<'J'
{"mode":"all","actual_plan":true}
J
wait_state 20
A invoke plan --quiet | python -c "
import sys,json; r=json.load(sys.stdin)['invoked']['result']
for p in r:
    for s in p.get('statements',[]):
        print('actual:', s.get('actual'), 'nodes:', len(s.get('nodes',[])), [(n['op'], n.get('actual_rows')) for n in s.get('nodes',[])][:4])"
A invoke command --quiet --args '{"id":"plan.zoom_fit"}' >/dev/null
J command <<'J'
{"id":"results.toggle"}
J
sleep 1; shot 22_actual_plan
J command <<'J'
{"id":"results.toggle"}
J

echo "### copy as markdown -> clipboard"
J set_query <<'J'
{"text":"SELECT TOP 3 id, category, amount FROM dbo.big ORDER BY id"}
J
A invoke run --quiet >/dev/null; wait_state 10
J copy <<'J'
{"kind":"markdown"}
J
sleep 1; powershell -NoProfile -Command "Get-Clipboard" | head -6

echo "### cell viewer via select_cell"
J set_query <<'J'
{"text":"SELECT c_json, c_xml, c_nvarcharmax FROM dbo.all_types WHERE id = 1"}
J
A invoke run --quiet >/dev/null; wait_state 10
J select_cell <<'J'
{"row":0,"col":0}
J
J command <<'J'
{"id":"results.cell_viewer"}
J
sleep 1; shot 23_viewer
echo "### done"
