#!/usr/bin/env bash
# Live view of the /debug heartbeat: refreshes every 2s, heartbeat on
# top, most recent events underneath. RADIO=... overrides the address.
set -euo pipefail
RADIO="${RADIO:-http://127.0.0.1:8080}"
render() {
python3 - "$@" << 'PY'
import json, sys
d = json.loads(sys.argv[1])
def age(ms):
    if ms is None: return "-"
    s = ms / 1000
    return f"{s:.1f}s" if s < 60 else f"{int(s//60)}m{int(s%60):02d}s"
print(f"stage: {d['stage']} (for {age(d['stage_ms_ago'])})   stalled: {d['stalled']}")
print(f"session: started {age(d['session_started_ms_ago'])} ago   stream: {d['stream_url']}")
bo = age(d['current_backoff_ms']) if d['current_backoff_ms'] else "-"
print(f"connects: {d['connect_attempts']}   backoff: {bo}")
print(f"read: {age(d['last_read_ms_ago'])} ago ({d['samples_read']})   "
      f"write: {age(d['last_write_ms_ago'])} ago ({d['samples_written']})")
e = d['last_error']
print(f"last error: {e['message']} ({age(e['ms_ago'])} ago)" if e else "last error: -")
print()
for ev in d['events'][:12]:
    print(f"  {ev['kind']:<16} {ev['detail']}")
PY
}
while true; do
    out=$(curl -s --max-time 2 "$RADIO/debug" || true)
    clear
    echo "== $RADIO/debug — $(date '+%H:%M:%S') =="
    if [ -z "$out" ]; then echo "(radiod unreachable)"; else render "$out" || echo "(bad response)"; fi
    sleep 2
done
