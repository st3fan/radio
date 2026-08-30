#!/usr/bin/env bash
# Shared helpers for the stall test scripts — the test scenarios from
# plans/20260829-01-streaming-stall-debug.md, adapted to this box
# (nftables, tc/netem + ifb for ingress shaping).
#
# All scripts target the stream the radio is CURRENTLY playing: start
# playback first, then run a scenario. RADIO=http://host:8080 overrides
# where radiod is.
set -euo pipefail

RADIO="${RADIO:-http://127.0.0.1:8080}"

die() { echo "error: $*" >&2; exit 1; }

# The interface the default route (and thus the stream) uses.
iface() { ip route show default | awk '{print $5; exit}'; }

# The IPs the stream comes from. Preferred: the live TCP peers of the
# radiod process (the truth, DNS may rotate). Fallback: every address of
# the stream_url host from /debug.
stream_ips() {
    local ips
    ips=$(ss -Htnp state established \
            '( dport = :443 or dport = :80 or dport = :8000 or dport = :8443 )' 2>/dev/null \
        | grep radiod | awk '{print $4}' | sed -E 's/:[0-9]+$//' | sort -u || true)
    if [ -z "$ips" ]; then
        local url host
        url=$(curl -s --max-time 3 "$RADIO/debug" \
            | python3 -c 'import json,sys; print(json.load(sys.stdin).get("stream_url") or "")' \
            2>/dev/null || true)
        [ -n "$url" ] || die "no live stream connection and no stream_url in $RADIO/debug — is the radio playing?"
        host=${url#*://}; host=${host%%[:/]*}
        ips=$(getent ahosts "$host" | awk '{print $1}' | sort -u)
    fi
    [ -n "$ips" ] || die "could not determine the stream server's IP"
    echo "$ips"
}

# Adds one nft drop/reject table for the stream IPs. $1 = table name,
# $2 = rule verdict ("drop" or "reject with tcp reset").
nft_break_start() {
    local table=$1 verdict=$2 ips ip
    sudo nft list table inet "$table" >/dev/null 2>&1 && die "$table already active (run 'stop' first)"
    ips=$(stream_ips)
    echo "breaking stream from:"; echo "$ips" | sed 's/^/  /'
    sudo nft add table inet "$table"
    sudo nft "add chain inet $table in { type filter hook input priority 0; }"
    for ip in $ips; do
        ip=${ip#[}; ip=${ip%]}
        case $ip in
            *:*) sudo nft add rule inet "$table" in ip6 saddr "$ip" $verdict ;;
            *)   sudo nft add rule inet "$table" in ip saddr "$ip" $verdict ;;
        esac
    done
}

nft_break_stop() {
    local table=$1
    sudo nft list table inet "$table" >/dev/null 2>&1 || { echo "$table was not active"; return 0; }
    sudo nft delete table inet "$table"
    echo "$table removed — stream traffic flows again"
}

nft_break_status() {
    local table=$1
    sudo nft list table inet "$table" 2>/dev/null || echo "$table not active"
}

# Ingress shaping: redirect the stream server's incoming packets through
# ifb0 and apply a netem profile there. Scoped to the stream IPs so SSH
# and everything else stay untouched. Only one profile at a time (a new
# 'start' replaces it).
shape_start() {
    local dev ips ip
    dev=$(iface)
    ips=$(stream_ips)
    echo "shaping stream from ($*):"; echo "$ips" | sed 's/^/  /'
    # Plain `modprobe ifb` can create zero interfaces on this kernel, so
    # ask for one explicitly and wait for it to appear.
    ip link show ifb0 >/dev/null 2>&1 || sudo modprobe ifb numifbs=1
    for _ in 1 2 3 4 5; do ip link show ifb0 >/dev/null 2>&1 && break; sleep 0.3; done
    ip link show ifb0 >/dev/null 2>&1 || die "ifb0 did not appear (modprobe ifb numifbs=1 failed)"
    sudo ip link set ifb0 up
    sudo tc qdisc replace dev ifb0 root netem "$@"
    if ! tc qdisc show dev "$dev" ingress | grep -q ingress; then
        sudo tc qdisc add dev "$dev" handle ffff: ingress
    fi
    # Fresh filters each start (the stream IP may have changed).
    sudo tc filter del dev "$dev" parent ffff: 2>/dev/null || true
    for ip in $ips; do
        ip=${ip#[}; ip=${ip%]}
        case $ip in
            *:*) sudo tc filter add dev "$dev" parent ffff: protocol ipv6 u32 \
                     match ip6 src "$ip" action mirred egress redirect dev ifb0 ;;
            *)   sudo tc filter add dev "$dev" parent ffff: protocol ip u32 \
                     match ip src "$ip"/32 action mirred egress redirect dev ifb0 ;;
        esac
    done
}

shape_stop() {
    local dev
    dev=$(iface)
    sudo tc qdisc del dev "$dev" ingress 2>/dev/null && echo "ingress redirect on $dev removed" \
        || echo "no ingress redirect on $dev"
    sudo tc qdisc del dev ifb0 root 2>/dev/null || true
    sudo ip link set ifb0 down 2>/dev/null || true
}

shape_status() {
    local dev
    dev=$(iface)
    echo "== ingress ($dev):"; tc qdisc show dev "$dev" ingress || true
    echo "== ifb0:"; tc qdisc show dev ifb0 2>/dev/null || echo "  (down)"
}

watch_hint() {
    echo
    echo "watch it: $RADIO/debug  (or: ./watch-debug.sh)"
}
