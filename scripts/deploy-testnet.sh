#!/usr/bin/env bash
# Deploy one Fund Desk share class (USD-D) to Stellar testnet.
#
# NOT EXECUTED in the environment this project was built in: the sandbox could
# not reach soroban-testnet.stellar.org, horizon-testnet.stellar.org or
# friendbot. The script is written against stellar-cli 28 and the contract
# interfaces that `cargo test` exercises; expect to adjust flags if the CLI
# changes. It creates its own test cash asset ("USDC" from a local issuer)
# so the run is self-contained; for Circle's testnet USDC replace CASH_*.
#
# Order matters (ARCHITECTURE.md, "Deployment order"):
#   1. share issuer sets AUTH_REQUIRED, AUTH_REVOCABLE, AUTH_CLAWBACK_ENABLED
#      BEFORE any trustline or balance exists;
#   2. deploy the share SAC; 3. deploy the five contracts;
#   4. SAC set_admin(compliance); 5. ops-authorised bind / policies / jurisdictions.
#
# Prerequisites: stellar-cli 28, node 22, built wasm (scripts/build.sh), app built (npm run build).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
NETWORK="${NETWORK:-testnet}"
WASM="$ROOT/target/wasm32v1-none/release"
ENV_OUT="${ENV_OUT:-$ROOT/app/.env}"
SHARE_CODE="${SHARE_CODE:-FDUSDD}"
CASH_CODE="${CASH_CODE:-USDC}"
inv() { stellar contract invoke --network "$NETWORK" "$@"; }

echo "== keys (generated and funded through friendbot)"
for k in share_issuer cash_issuer treasury ta_ops_1 ta_ops_2 fund_admin_1 investor_demo; do
  stellar keys generate --network "$NETWORK" --fund "$k" 2>/dev/null || echo "key $k exists"
done
addr() { stellar keys address "$1"; }
secret() { stellar keys show "$1"; }
ISSUER=$(addr share_issuer); CASH_ISSUER=$(addr cash_issuer); TREASURY=$(addr treasury); INVESTOR=$(addr investor_demo)
hexkey() { node -e "const {StrKey}=require('$ROOT/app/node_modules/@stellar/stellar-sdk');console.log(StrKey.decodeEd25519PublicKey('$1').toString('hex'))"; }

echo "== 1. share issuer flags before any balance exists"
stellar tx new set-options --network "$NETWORK" --source-account share_issuer \
  --set-required --set-revocable --set-clawback-enabled >/dev/null

echo "== 2. share and cash SACs"
SHARE=$(stellar contract asset deploy --network "$NETWORK" --source-account share_issuer --asset "$SHARE_CODE:$ISSUER" 2>/dev/null \
  || stellar contract id asset --network "$NETWORK" --asset "$SHARE_CODE:$ISSUER")
CASH=$(stellar contract asset deploy --network "$NETWORK" --source-account cash_issuer --asset "$CASH_CODE:$CASH_ISSUER" 2>/dev/null \
  || stellar contract id asset --network "$NETWORK" --asset "$CASH_CODE:$CASH_ISSUER")
echo "share SAC $SHARE, cash SAC $CASH"
# Cash trustlines and balances for the treasury and the demo investor.
for k in treasury investor_demo; do
  stellar tx new change-trust --network "$NETWORK" --source-account "$k" --line "$CASH_CODE:$CASH_ISSUER" >/dev/null
  stellar tx new payment --network "$NETWORK" --source-account cash_issuer --destination "$(addr $k)" \
    --asset "$CASH_CODE:$CASH_ISSUER" --amount 20000000000000 >/dev/null
done
# Share trustline for the demo investor: created deauthorised (AUTH_REQUIRED); only the registrar authorises it.
stellar tx new change-trust --network "$NETWORK" --source-account investor_demo --line "$SHARE_CODE:$ISSUER" >/dev/null

echo "== 3. contracts"
SIGNERS="[[\"$(hexkey "$(addr ta_ops_1)")\",\"Ta\"],[\"$(hexkey "$(addr ta_ops_2)")\",\"Ta\"],[\"$(hexkey "$(addr fund_admin_1)")\",\"Admin\"]]"
OPS=$(stellar contract deploy --network "$NETWORK" --source-account ta_ops_1 --wasm "$WASM/ops_account.wasm" --alias fd_ops -- --signers "$SIGNERS")
ORACLE=$(stellar contract deploy --network "$NETWORK" --source-account fund_admin_1 --wasm "$WASM/nav_oracle.wasm" --alias fd_oracle -- \
  --publisher "$OPS" --base "{\"Stellar\":\"$CASH\"}" --decimals 14 --resolution 86400)
COMPLIANCE=$(stellar contract deploy --network "$NETWORK" --source-account ta_ops_1 --wasm "$WASM/compliance.wasm" --alias fd_compliance -- \
  --ops "$OPS" --share "$SHARE")
DIST=$(stellar contract deploy --network "$NETWORK" --source-account ta_ops_1 --wasm "$WASM/distribution.wasm" --alias fd_distribution -- \
  --ops "$OPS" --compliance "$COMPLIANCE" --cash "$CASH" --treasury "$TREASURY")
CFG=$(cat <<JSON
{"ops":"$OPS","compliance":"$COMPLIANCE","share":"$SHARE","cash":"$CASH","treasury":"$TREASURY","oracle":"$ORACLE",
 "oracle_asset":{"Other":"USD_D"},"nav_decimals":14,"initial_nav":"100000000000000","min_subscription":"10000000000",
 "max_strike_delay":28800,"max_nav_move_bps":25,"max_requests_per_epoch":400}
JSON
)
VAULT=$(stellar contract deploy --network "$NETWORK" --source-account ta_ops_1 --wasm "$WASM/async_vault.wasm" --alias fd_vault -- --cfg "$CFG")
echo "ops $OPS | oracle $ORACLE | compliance $COMPLIANCE | distribution $DIST | vault $VAULT"

echo "== 4. hand SAC admin to the registrar (after this only compliance can authorise share balances)"
inv --id "$SHARE" --source-account share_issuer -- set_admin --new_admin "$COMPLIANCE"

cat > "$ENV_OUT" <<ENV
SOROBAN_RPC_URL=https://soroban-testnet.stellar.org
NETWORK_PASSPHRASE=Test SDF Network ; September 2015
FD_OPS_ID=$OPS
FD_ORACLE_ID=$ORACLE
FD_COMPLIANCE_ID=$COMPLIANCE
FD_DISTRIBUTION_ID=$DIST
FD_VAULT_ID=$VAULT
FD_SHARE_ID=$SHARE
FD_CASH_ID=$CASH
FD_TA_SECRET_1=$(secret ta_ops_1)
FD_TA_SECRET_2=$(secret ta_ops_2)
FD_ADMIN_SECRET=$(secret fund_admin_1)
FD_TREASURY_SECRET=$(secret treasury)
FD_JOURNAL=funddesk-journal.json
LLM_MODEL=claude-sonnet-5
ENV
echo "wrote $ENV_OUT"

echo "== 5. ops-authorised setup (custom-account auth entries signed by TA + ADMIN in the app)"
cd "$ROOT/app"
node --env-file="$ENV_OUT" dist/src/cli.js init --fund ../data/seed/fund.json --submit

echo "== 6. demo: register the demo investor, open an epoch, subscribe"
node --env-file="$ENV_OUT" dist/src/cli.js kyc approve "$INVESTOR" --expiry 2027-12-31 --jurisdiction FR --cash "$INVESTOR" --submit
CUTOFF=$(date -u -d "+2 hours" +%Y-%m-%dT%H:%M:%SZ)
node --env-file="$ENV_OUT" dist/src/cli.js epoch open --cutoff "$CUTOFF" --submit
echo "Next: the investor signs request_subscribe from a wallet (Freighter / Stellar Wallets Kit):"
echo "  stellar contract invoke --network $NETWORK --id $VAULT --source-account investor_demo -- request_subscribe --investor $INVESTOR --amount 10000000000"
echo "After $CUTOFF: funddesk strike --epoch 1 --publish 1.0 --as-of <iso> --submit; funddesk settle --epoch 1 --submit"
