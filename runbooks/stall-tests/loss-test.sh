#!/usr/bin/env bash
# Random packet loss on the stream's incoming packets (netem via ifb).
# Moderate loss should be invisible (TCP retransmits); heavy loss
# approaches the silent-break behavior without being a hard cut.
#
# usage: loss-test.sh start [PERCENT]   (default 10%)
#        loss-test.sh stop | status
set -euo pipefail
cd "$(dirname "$0")"; . ./common.sh
case "${1:-}" in
    start)  shape_start loss "${2:-10%}"; watch_hint ;;
    stop)   shape_stop ;;
    status) shape_status ;;
    *)      echo "usage: $0 start [PERCENT] | stop | status"; exit 2 ;;
esac
