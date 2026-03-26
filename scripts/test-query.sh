#!/usr/bin/env bash
# Copyright (c) The Nexus-Node Contributors
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail

WALLET=./target/release/nexus-wallet
KEYDIR=./devnet-n7s/validator-0/keys
COUNTER=./contracts/examples/counter
RPC=http://localhost:8080

ADDR=$($WALLET address --key-file $KEYDIR/dilithium-secret.json 2>&1 | grep -oE '[0-9a-f]{64}')
echo "Deployer: $ADDR"

echo "--- Faucet ---"
curl -sf -X POST "$RPC/v2/faucet" \
  -H "Content-Type: application/json" \
  -d "{\"address\":\"$ADDR\",\"amount\":1000000}" || echo "faucet failed"

echo ""
echo "--- Build counter ---"
$WALLET move build \
  --package-dir $COUNTER \
  --named-addresses "counter_addr=0x$ADDR" 2>&1 | tail -2

echo "--- Deploy counter ---"
$WALLET move deploy \
  --package-dir $COUNTER \
  --rpc-url $RPC \
  --key-file $KEYDIR/dilithium-secret.json 2>&1 | tail -2

echo "--- Initialize ---"
$WALLET move call \
  --contract "0x$ADDR" \
  --function "counter::initialize" \
  --rpc-url $RPC \
  --key-file $KEYDIR/dilithium-secret.json 2>&1 | tail -2

echo "--- Increment ---"
$WALLET move call \
  --contract "0x$ADDR" \
  --function "counter::increment" \
  --rpc-url $RPC \
  --key-file $KEYDIR/dilithium-secret.json 2>&1 | tail -2

sleep 3
echo ""
echo "--- Query get_count ---"
curl -s -w "
HTTP %{http_code}
" \
  -X POST "$RPC/v2/contract/query" \
  -H "Content-Type: application/json" \
  -d "{\"contract\":\"$ADDR\",\"function\":\"counter::get_count\",\"type_args\":[],\"args\":[\"$ADDR\"]}"

echo ""
echo "--- Docker logs for diagnostics ---"
docker logs nexus-node-0 2>&1 | grep -i "query_view\|load_function" | tail -10
