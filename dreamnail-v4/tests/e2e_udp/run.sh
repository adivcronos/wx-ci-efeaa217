#!/bin/sh
# Runs INSIDE a pasta user+net namespace (root there, real internet via pasta).
H=$1; OUT=$2; SP=$3
ip link set lo up
$SP/cli connect "$DN_TEST_USER" "$DN_TEST_PASS" 200 > $OUT.cli 2>&1 &
CLI=$!
for i in $(seq 1 40); do grep -q "CONNECTED\|failed" $OUT.cli && break; sleep 1; done
grep -E "CONNECTED|failed" $OUT.cli
rm -f $OUT.ready $OUT.stop
DREAMNAIL_TUN_LOG=info $H --proxy 127.0.0.1:11080 --bypass 66.226.144.136 --mtu 1500 --mss 1400 --udpgw 127.0.0.1:10730 --ready-file $OUT.ready --stop-file $OUT.stop --log-file $OUT.tun > $OUT.tunerr 2>&1 &
TH=$!
for i in $(seq 1 20); do [ -e $OUT.ready ] && break; sleep 0.5; done
[ -e $OUT.ready ] && echo "TUN ready" || { echo "TUN NOT READY"; cat $OUT.tunerr | tail -5; }
ip route | head -5
python3 -I $SP/e2e/game_client.py 40 100 2>&1
touch $OUT.stop; sleep 2; kill $TH $CLI 2>/dev/null; wait 2>/dev/null
echo "  helper log: streams ended by recv error = $(grep -c "recv_udpgw_packet" $OUT.tunerr)  | udpgw streams begun = $(grep -c "Beginning stream" $OUT.tunerr)"
