# Stall testing (reproducing the silent streaming stall)

Sometimes the audio stops while the UI and `GET /status` still say
`playing` (see `plans/20260829-01-streaming-stall-debug.md`). This
runbook reproduces that class of failure on demand by impairing the
network under a live stream, so the always-on `/debug` heartbeat can be
watched reacting to each cause.

**Where:** the dev Pi (or the Debian PC) — **not muzak** unless you keep
the volume conservative; a stall is silent, but recovery is not. Needs
`sudo` and a kernel with `sch_netem` + `ifb` (stock on Raspberry Pi OS
and Debian). The scripts live in `runbooks/stall-tests/`.

## 1. Start a stream

Run radiod (the debug build is enough; `--debug` adds FFmpeg's own log
lines) and play a station — the scripts target **whatever is playing
now**, reading the live TCP peer and falling back to the `stream_url`
in `/debug`:

```
cargo run --features ffmpeg-next/rpi -- --config <dev-config>   # on the Pi
curl -X POST localhost:8080/play -d '{"playlist_url": "https://somafm.com/groovesalad.pls"}'
```

(The `ffmpeg-next/rpi` feature is required on Raspberry Pi OS, whose
patched FFmpeg headers carry extra pixel formats.)

## 2. Watch the heartbeat

In its own terminal:

```
cd runbooks/stall-tests
./watch-debug.sh                 # refreshes every 2s
# RADIO=http://raspberry.local:8080 ./watch-debug.sh   # remote daemon
```

or open `http://<host>:8080/debug` in a browser (same page), or
`curl http://<host>:8080/debug` for the JSON.

## 3. Run a scenario

Each script takes `start` / `stop` / `status`. `stop` always removes the
impairment. Run one, watch the signature appear, then `stop`.

| Script | Simulates | Expected `/debug` signature |
|---|---|---|
| `silent-break-test.sh` | half-open connection (NAT/AP dies, no FIN/RST) — **the prime suspect** | `stage: reading`, ages grow without bound, `stalled` after ~10s, `/stop` returns 200 but never takes effect |
| `clean-break-test.sh` | connection reset — the reconnect loop should cope | `connect_attempts` climbing, a fresh `last_error`, recovery after `stop` |
| `latency-test.sh [delay [jitter]]` | high RTT / jitter (default 400ms/150ms) | usually rides it out; read ages grow |
| `loss-test.sh [percent]` | random packet loss (default 10%) | low loss invisible; heavy loss (≈15%+) can stall, then recover slowly |
| `starve-test.sh [rate]` | icecast underrun — rate below bitrate (default 64kbit) | loop alive but sample counters advance well below real time |

The shaping scenarios (latency/loss/starve) share one netem qdisc, so
only one runs at a time; the two break scenarios use separate nft
tables and are independent.

## 4. Read the signature

Map what `/debug` shows to the hypothesis (full table in the plan):

- `stage: reading`, `stage_ms_ago` huge, no new events → **blocked read**
  (missing `rw_timeout`) — hypothesis 1.
- `stage: backoff`/`connecting`, `connect_attempts` climbing, recent
  errors → **reconnect loop failing** — hypothesis 2 (the error text
  says why).
- `stage: writing` stuck → **ALSA/DAC** — hypothesis 3.
- loop alive but sample counters well below `rate × channels`/s →
  **starving stream** — hypothesis 4.

`journalctl -u radiod --since …` has the stall-monitor lines (one per
transition) and, with `--debug`, the FFmpeg chatter around the moment.

## 5. Clean up

`stop` on the scenario you ran undoes it. If a script was interrupted
mid-run, remove everything by hand:

```
sudo nft delete table inet stall_silent 2>/dev/null
sudo nft delete table inet stall_clean  2>/dev/null
sudo tc qdisc del dev eth0 ingress 2>/dev/null
sudo tc qdisc del dev ifb0 root    2>/dev/null
```

A silent-break test can leave radiod wedged (that is the finding, not a
bug in the test); restart the daemon to clear it.

## Findings so far (2026-08-30, dev Pi)

- **silent break** reproduces the field wedge exactly: the read blocks
  forever, and `/stop` is accepted (200) but never processed until
  radiod is restarted.
- **loss at 15%** stalled playback for ~50-70s, then self-healed once
  FFmpeg's *internal* read finally errored — a slow-recovery version of
  the same symptom, and the clearest argument for putting an
  `rw_timeout` on the FFmpeg open so radiod's own reconnect loop takes
  over promptly.
- **clean break** rejects on the nft **output** hook (an input reject
  only RSTs the server and leaves our socket blocked — that *is* the
  silent break). The instant-kill `ss -K` needs
  `CONFIG_INET_DIAG_DESTROY`, absent on the Pi's kernel, so the current
  read errors on TCP's retransmit timeout (~10-20s), not immediately.
- **ifb**: plain `modprobe ifb` created no interface on the Pi;
  `common.sh` loads it with `numifbs=1`.
