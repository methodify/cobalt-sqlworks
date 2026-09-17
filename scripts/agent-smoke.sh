#!/usr/bin/env bash
# Drive the running Cobalt app through egui-agent-cli. Usage: scripts/agent-smoke.sh <shots_dir_windows_path>
set -u
SHOTS="${1:-C:/temp}"
A() { egui-agent-cli --pipe cobalt.agent "$@"; }
J() { # invoke verb with args from a heredoc on stdin
  local verb="$1"; local args; args="$(cat)"; A invoke "$verb" --quiet --args "$args"; }
shot() { A screenshot | python -c "
import sys,json,base64
d=json.load(sys.stdin); img=d.get('image') or d.get('result',{}).get('image') or d
open(r'$SHOTS/$1.png','wb').write(base64.b64decode(img['data'])); print('saved $1', img['width'], img['height'])"; }
wait_done() { # wait until active run finishes (max $1 s)
  local n=0; while [ $n -lt "${1:-30}" ]; do
    st=$(A invoke run_state --quiet 2>/dev/null | python -c "import sys,json; print(json.load(sys.stdin)['invoked']['result']['state'])" 2>/dev/null)
    case "$st" in done|failed|cancelled|paused) echo "run_state=$st after ${n}s"; return;; esac
    sleep 1; n=$((n+1)); done; echo "timeout waiting (state=$st)"; }
echo "### connect"; J connect <<'J'
{"profile":"local"}
J
sleep 4
echo "### query 1: top 100 + print + group by"; J set_query <<'J'
{"text":"SELECT TOP 100 * FROM dbo.big ORDER BY id;\nPRINT 'hello from cobalt';\nSELECT category, COUNT(*) AS n, SUM(amount) AS total FROM dbo.big GROUP BY category ORDER BY category;"}
J
A invoke run --quiet >/dev/null; wait_done 30
echo "--- results set 0 (2 rows)"; J results <<'J'
{"set":0,"limit":2}
J
echo "--- results set 1 (3 rows)"; J results <<'J'
{"set":1,"limit":3}
J
echo "--- messages"; A invoke messages --quiet | python -c "import sys,json; [print(' *', m['text'].replace(chr(10),' | ')) for m in json.load(sys.stdin)['invoked']['result']]"
shot 02_results
echo "### estimated plan"; J run <<'J'
{"mode":"estimated_plan"}
J
wait_done 30; sleep 1
A invoke plan --quiet | python -c "import sys,json; r=json.load(sys.stdin)['invoked']['result']; print(json.dumps(r)[:700])"
shot 03_plan
echo "### exports"; A invoke run --quiet >/dev/null; wait_done 30
for f in csv parquet delta xlsx json; do
  J export <<J
{"format":"$f","path":"$SHOTS/export_test.$f","set":1}
J
  sleep 2
done
sleep 3
ls -la "$(cygpath -u "$SHOTS")" | grep export_test
ls "$(cygpath -u "$SHOTS")/export_test.delta" 2>/dev/null | head
A invoke dismiss_dialog --quiet >/dev/null
echo "### 2M rows with cap"; J set_query <<'J'
{"text":"SELECT * FROM dbo.big"}
J
A invoke run --quiet >/dev/null; wait_done 60
A invoke state --quiet | python -c "import sys,json; t=[t for t in json.load(sys.stdin)['invoked']['result']['tabs'] if t['active']][0]; print('rows so far:', t['run']['result_sets'][0]['rows'], 'state', t['run']['state'], 'paused', t['run']['paused'])"
shot 04_paused
echo "### fetch all"; J fetch_more <<'J'
{}
J
t0=$(date +%s); wait_done 120; t1=$(date +%s)
A invoke state --quiet | python -c "import sys,json; t=[t for t in json.load(sys.stdin)['invoked']['result']['tabs'] if t['active']][0]; print('rows:', t['run']['result_sets'][0]['rows'], 'state', t['run']['state'], 'elapsed_ms', t['run']['elapsed_ms'])"
echo "fetch-all wall time: $((t1-t0))s"
shot 05_2m_rows
