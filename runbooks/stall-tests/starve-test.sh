#!/usr/bin/env bash
# Scenario for hypothesis 4 — the starving stream: cap the incoming
# rate below the stream's bitrate (SomaFM mp3/aac ≈ 128 kbps), so reads
# keep succeeding but audio arrives slower than real time.
#
# Expected /debug signature: the loop looks alive (recent reads/writes)
# but the sample counters advance well below rate × channels per
# second; eventually the buffer drains and writes gap out.
#
# usage: starve-test.sh start [RATE]   (default 64kbit)
#        starve-test.sh stop | status
set -euo pipefail
cd "$(dirname "$0")"; . ./common.sh
case "${1:-}" in
    start)  shape_start rate "${2:-64kbit}"; watch_hint ;;
    stop)   shape_stop ;;
    status) shape_status ;;
    *)      echo "usage: $0 start [RATE] | stop | status"; exit 2 ;;
esac
