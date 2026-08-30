#!/usr/bin/env bash
# Scenario 3 — clean break: make radiod's own socket ERROR (unlike the
# silent break, where it blocks forever). A reject on the OUTPUT hook
# means every packet radiod sends to the stream server fails locally:
#  - each reconnect attempt (the SYN) errors immediately, and
#  - the CURRENT connection errors as soon as its TCP tries to ACK the
#    next inbound data (~10-20s, on TCP's retransmit timing).
#
# The immediate-kill trick (`ss -K`) needs CONFIG_INET_DIAG_DESTROY,
# which this Pi's kernel does NOT set (verified 2026-08-30) — so the
# current read errors on the retransmit timeout, not instantly. Be
# patient, or run 'stop' and re-'start' to force a fresh SYN.
#
# Expected /debug signature: connect_attempts climbing, stage
# "backoff"/"connecting", a fresh last_error, "reconnecting to …"
# journal lines, and recovery shortly after 'stop'.
set -euo pipefail
cd "$(dirname "$0")"; . ./common.sh
case "${1:-}" in
    start)
        sudo nft list table inet stall_clean >/dev/null 2>&1 && die "stall_clean already active (run 'stop' first)"
        ips=$(stream_ips)
        echo "breaking stream to:"; echo "$ips" | sed 's/^/  /'
        sudo nft add table inet stall_clean
        sudo nft 'add chain inet stall_clean out { type filter hook output priority 0; }'
        for ip in $ips; do
            ip=${ip#[}; ip=${ip%]}
            case $ip in
                *:*) sudo nft add rule inet stall_clean out ip6 daddr "$ip" reject with tcp reset ;;
                *)   sudo nft add rule inet stall_clean out ip daddr "$ip" reject with tcp reset ;;
            esac
        done
        echo "clean break active — reconnect attempts should start climbing within ~20s"
        watch_hint ;;
    stop)   nft_break_stop stall_clean
            echo "the next reconnect attempt should succeed" ;;
    status) nft_break_status stall_clean ;;
    *)      echo "usage: $0 start|stop|status"; exit 2 ;;
esac
