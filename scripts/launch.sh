#!/usr/bin/env bash
# Kill any running Cobalt, (re)build, and launch with the agent channel on. Usage: scripts/launch.sh [--no-build]
# The GUI is started fully detached (stdio -> /dev/null) so callers may pipe this script's output.
set -u
cd "$(dirname "$0")/.."
taskkill //IM cobalt.exe //F >/dev/null 2>&1; sleep 1
if [ "${1:-}" != "--no-build" ]; then
  cargo build -p cobalt-app 2>&1 | grep -E "^error|^warning: unused|Finished" -A6 | head -40
fi
LOG_LEVEL="${COBALT_LOG:-info}"
powershell -NoProfile -Command "\$env:COBALT_AGENT='1'; \$env:COBALT_LOG='$LOG_LEVEL'; Start-Process -FilePath 'C:\LocalData\projects\sqlworks\target\debug\cobalt.exe' -RedirectStandardOutput 'C:\LocalData\projects\sqlworks\target\run_out.log' -RedirectStandardError 'C:\LocalData\projects\sqlworks\target\run_err.log'" > /dev/null 2>&1 < /dev/null
sleep 7
egui-agent-cli --pipe cobalt.agent invoke run_state --quiet >/dev/null 2>&1 && echo "app up" || echo "app NOT reachable"
