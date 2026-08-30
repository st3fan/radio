#!/usr/bin/env bash
# Scenario 1 — silent break (half-open connection): drop everything the
# stream server sends, say nothing. TCP reports no error, so with no
# rw_timeout the player's read should block forever.
#
# Expected /debug signature: stage "reading" with ever-growing ages, no
# new events, "! STALLED" after ~10s and the stall line in the journal —
# and /stop accepted (200) but never processed. This is the prime
# suspect for the field failure.
set -euo pipefail
cd "$(dirname "$0")"; . ./common.sh
case "${1:-}" in
    start)  nft_break_start stall_silent drop
            echo "silent break active — audio should die within ~0.5s + buffered data"
            watch_hint ;;
    stop)   nft_break_stop stall_silent
            echo "note whether playback resumes by itself — it tells us if the socket ever wakes" ;;
    status) nft_break_status stall_silent ;;
    *)      echo "usage: $0 start|stop|status"; exit 2 ;;
esac
