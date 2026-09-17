#!/usr/bin/env bash
# UI exploration via egui-agent: tree, dialogs, theme, history, settings.
# Usage: scripts/agent-ui.sh <shots windows path like C:/temp>
set -u
SHOTS="${1:-C:/temp}"
A() { egui-agent-cli --pipe cobalt.agent "$@"; }
J() { local verb="$1"; local args; args="$(cat)"; A invoke "$verb" --quiet --args "$args"; }
shot() { A screenshot | python "$(dirname "$0")/save_shot.py" "$SHOTS/$1.png"; }
labels() { A snapshot | python "$(dirname "$0")/labels.py"; }
cmd() { A invoke command --quiet --args "{\"id\":\"$1\"}" >/dev/null; }
click() { A invoke click --target "{\"label\":\"$1\"}" --quiet | head -3; }

echo "### labels"; labels
echo "### expand server 'local'"; click "server: local"; sleep 3
echo "### expand database cobalt_test"; click "database: cobalt_test"; sleep 3
echo "### expand Tables"; click "folder: Tables"; sleep 1
echo "### expand dbo.big"; click "Table: dbo.big"; sleep 1
echo "### expand Columns"; click "folder: Columns"; sleep 2
shot 06_tree
echo "### dark theme"; cmd view.toggle_theme; sleep 1; shot 07_dark
echo "### history sidebar"; cmd view.history; sleep 1; shot 08_history; sleep 1; shot 08b_history
echo "### new connection dialog"; cmd connection.new; sleep 2; shot 09_conn_dialog
A invoke dismiss_dialog --quiet >/dev/null
echo "### settings"; cmd view.settings; sleep 1; shot 10_settings
A invoke click --target '{"label":"Cancel"}' --quiet >/dev/null 2>&1
echo "### palette"; cmd view.palette; sleep 1; shot 11_palette
cmd view.palette
echo "### back to light + servers"; cmd view.toggle_theme; cmd view.servers; sleep 1
echo "### stacked results + all_types"
J set_query <<'J'
{"text":"SELECT * FROM dbo.all_types;\nSELECT TOP 5 id, category, amount, created, note FROM dbo.big ORDER BY id;"}
J
A invoke run --quiet >/dev/null; sleep 4; shot 12_stacked
echo "### sort by column c_int"; click "column c_int"; sleep 1; shot 13_sorted
echo "### filter popup on category"; click "filter c_varchar"; sleep 1; shot 14_filter
A invoke dismiss_dialog --quiet >/dev/null

echo "### close agent tabs"; for i in 1 2 3 4 5 6; do A invoke close_tab --quiet >/dev/null 2>&1; done
