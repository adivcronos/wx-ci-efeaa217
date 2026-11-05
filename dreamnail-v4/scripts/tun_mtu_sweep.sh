#!/usr/bin/env bash
# TUN-mode MTU sweep — find the MTU that maximizes throughput while keeping
# latency low. For each MTU it: connects via dreamnail-cli in TUN mode, measures
# download speed + ping THROUGH the tunnel, tears down, then prints a summary.
#
# TUN needs root (CAP_NET_ADMIN to make the interface + edit routes), so run as
# root. From the dreamnail-v4 dir:
#
#   cargo build -p dreamnail-cli
#   sudo -E ./scripts/tun_mtu_sweep.sh <username> <password> [mtu ...]
#
# Example:
#   sudo -E ./scripts/tun_mtu_sweep.sh portkey-good 'yes'
#
# Defaults sweep 1360 1400 1420 1440 1460. Override the xray path with
# DREAMNAIL_XRAY=... . Needs curl + ping (standard on Linux).

set -u

USER_NAME="${1:-}"
PASS="${2:-}"
if [ -z "$USER_NAME" ] || [ -z "$PASS" ]; then
  echo "usage: sudo -E $0 <username> <password> [mtu ...]" >&2
  exit 1
fi
shift 2
MTUS=("$@")
[ ${#MTUS[@]} -eq 0 ] && MTUS=(1360 1400 1420 1440 1460)

BIN="target/debug/dreamnail-cli"
# sudo often resets HOME to /root, so resolve the invoking user's real home.
REAL_HOME="$HOME"
if [ -n "${SUDO_USER:-}" ]; then
  _h="$(getent passwd "$SUDO_USER" | cut -d: -f6)"
  [ -n "$_h" ] && REAL_HOME="$_h"
fi
XRAY="${DREAMNAIL_XRAY:-$REAL_HOME/dreamnail/xray-linux-x64}"
DLURL="https://speed.cloudflare.com/__down?bytes=50000000"  # 50 MB
PINGHOST="1.1.1.1"
HOLD=60  # seconds to hold each connection (must exceed ping+download time)

if [ ! -x "$BIN" ]; then
  echo "error: $BIN not found — run 'cargo build -p dreamnail-cli' first" >&2
  exit 1
fi
if [ ! -x "$XRAY" ]; then
  echo "error: xray not found at $XRAY — set DREAMNAIL_XRAY=/path/to/xray" >&2
  exit 1
fi

declare -a RESULTS
echo "sweeping MTUs: ${MTUS[*]}"

for MTU in "${MTUS[@]}"; do
  echo
  echo "===== MTU $MTU ====="
  LOG="/tmp/tun_sweep_${MTU}.log"

  DREAMNAIL_TUN=1 DREAMNAIL_TUN_MTU="$MTU" DREAMNAIL_XRAY="$XRAY" \
    "$BIN" connect "$USER_NAME" "$PASS" "$HOLD" >"$LOG" 2>&1 &
  CLIPID=$!

  # Wait up to 20s for CONNECTED (or an early failure).
  connected=0
  for _ in $(seq 1 40); do
    if grep -q "CONNECTED" "$LOG"; then connected=1; break; fi
    if grep -q "connect failed" "$LOG"; then break; fi
    sleep 0.5
  done

  if [ "$connected" -eq 1 ]; then
    echo "  connected — measuring latency + download through the tunnel…"
    # ipstack tunnels TCP/UDP but not ICMP, so measure latency as the TCP connect
    # time to a host (≈ one round-trip) instead of ping.
    PMS=$(curl -o /dev/null -s --max-time 10 -w '%{time_connect}' "https://$PINGHOST/" 2>/dev/null \
            | awk '{printf "%.0f", $1*1000}')
    SPEED=$(curl -o /dev/null -s --max-time 40 -w '%{speed_download}' "$DLURL" 2>/dev/null)
    MBPS=$(awk "BEGIN{printf \"%.1f\", ($SPEED*8)/1000000}")
    echo "  download: ${MBPS} Mbps   ping: ${PMS:-?} ms"
    RESULTS+=("MTU ${MTU}:  ${MBPS} Mbps   ${PMS:-?} ms")
  else
    echo "  connect FAILED — see $LOG"
    tail -3 "$LOG" | sed 's/^/    /'
    RESULTS+=("MTU ${MTU}:  FAILED")
  fi

  # Let the timed hold finish so it tears down cleanly before the next run.
  wait "$CLIPID" 2>/dev/null
  sleep 2
done

echo
echo "================= SUMMARY ================="
for r in "${RESULTS[@]}"; do echo "  $r"; done
echo "==========================================="
echo "pick the highest Mbps whose ping stays low (~24ms). tell Claude the table."
