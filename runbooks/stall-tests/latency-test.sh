#!/usr/bin/env bash
# Latency/jitter on the stream's incoming packets (netem via ifb).
# TCP should ride this out; watch whether the read ages in /debug grow
# and whether FFmpeg (with --debug) has anything to say.
#
# usage: latency-test.sh start [DELAY [JITTER]]   (defaults 300ms 100ms)
#        latency-test.sh stop | status
set -euo pipefail
cd "$(dirname "$0")"; . ./common.sh
case "${1:-}" in
    start)  shape_start delay "${2:-300ms}" "${3:-100ms}"; watch_hint ;;
    stop)   shape_stop ;;
    status) shape_status ;;
    *)      echo "usage: $0 start [DELAY [JITTER]] | stop | status"; exit 2 ;;
esac
