# Fund Desk demo

Everything below was run locally on 2026-09-27 (toolchain: Rust 1.94, stellar-cli 28.0.0,
Node 22, soroban-sdk 28.0.0, @stellar/stellar-sdk 17.1.0). All data is **Simulated**: the
fictional "Seedfund Short Treasury MMF" in `data/seed/` and synthetic KYC/form fixtures in
`app/fixtures/`. Nothing was deployed: the build environment had no route to Stellar
testnet, so `scripts/deploy-testnet.sh` is **not executed**.

## 1. Contracts: `cargo test`

```
$ cargo test
     Running unittests src/lib.rs (target/debug/deps/async_vault-...)
test test_scenario::scenario_three_epochs_seed ... ok
test result: ok. 36 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 25.08s
     Running unittests src/lib.rs (target/debug/deps/compliance-...)
test test::deployment_precondition_clawback_flag_must_precede_balances ... ok
test test::issue_mints_deauthorised_and_is_vault_only ... ok
test test::redeem_burn_works_for_an_expired_deauthorised_holder ... ok
test test::transfer_between_eligible_holders_leaves_both_deauthorised ... ok
test result: ok. 22 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.31s
     Running unittests src/lib.rs (target/debug/deps/distribution-...)
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 31.88s
     Running unittests src/lib.rs (target/debug/deps/nav_oracle-...)
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.20s
     Running unittests src/lib.rs (target/debug/deps/ops_account-...)
test result: ok. 23 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.40s
```

101 tests: async_vault 36, ops_account 23, compliance 22, distribution 13, nav_oracle 7.
The distribution suite includes a 64-case proptest over random transfer/declare
sequences, and async_vault's `math.rs` property tests (3 × 10,000 cases) run in the 36.
The settle budget check prints:

```
$ cargo test -p async_vault settle_batch_of_20 -- --nocapture
settle(epoch, 20): cpu_insns=11536269 mem_bytes=4103402 (native; wasm costs more)
test test::settle_batch_of_20_fits_the_transaction_budget ... ok
```

`FD_WRITE_GOLDEN=1 cargo test -p async_vault scenario_three_epochs_seed` rewrites
`data/seed/expected-scenario.json`; the committed file is the one the app is checked against.

## 2. Wasm: `stellar contract build`

```
$ stellar contract build
✅ Build Complete
$ ls -l target/wasm32v1-none/release/*.wasm
29313  async_vault.wasm
14008  compliance.wasm
10322  distribution.wasm
 6358  nav_oracle.wasm
10987  ops_account.wasm
$ scripts/gen-specs.sh
wrote app/src/chain/specs.ts (23888 bytes)
```

## 3. App: `npm test`

```
$ cd app && npm test
# heuristic KYC field accuracy on synthetic fixtures (Simulated, not a claim about real documents): 48/48
# heuristic form field accuracy on synthetic fixtures (Simulated): 72/72
# tests 35
# suites 0
# pass 35
# fail 0
# cancelled 0
```

The accuracy lines are **Simulated**: we wrote both the fixtures and the labels, so they
say nothing about real documents.

## 4. Offline replay: `npm run demo`

The seed replayed through the TypeScript model (bigint mirrors of the contract maths),
then compared figure for figure with the golden file the Rust scenario wrote. Exit code 0.

```
$ npm run demo
Fund Desk demo - "Seedfund Short Treasury MMF" (fictional, simulated data)
register: 25 investors; 51 order rows; 8 NAV rows; 5 distribution rows; 2 forced-transfer rows

== USD-D
  epoch 1: Settled NAV 1.00000000 | subs 1379583.82 | redeemed shares 0.00 | claimable cash 0.00 | liquidity 0.00 | settle calls 1
  epoch 2: Settled NAV 1.00000412 | subs 48000.01 | redeemed shares 7000.49 | claimable cash 12000.52 | liquidity 0.00 | settle calls 1
  epoch 3: Settled NAV 0.99999870 | subs 12777.77 | redeemed shares 1054000.00 | claimable cash 1053998.63 | liquidity 1050000.00 | settle calls 2
  rejections as designed: 10 (subscribe:BelowMinimum, subscribe:JurisdictionBlocked, cancel:CutoffPassed, redeem:CashAddressNotAllowed, transfer:KycExpired, transfer:KycExpired, distribution.claim:CashAddressNotAllowed, forced_transfer:InsufficientAdmin, strike:StalePrice, strike:NavMoveTooLarge)
  supply 374,360.9527849 = balances 374,360.9527849 = journal 374,360.9527849: reconciled
  vault cash 0.0000000 = pending + claimable 0.0000000: reconciled
  distribution declared 12418.5531907, claimed 12418.5531899, rounding left in contract 8 stroop(s)

== EUR-A
  epoch 1: Settled NAV 1.00000000 | subs 69750.25 | redeemed shares 0.00 | claimable cash 0.00 | liquidity 0.00 | settle calls 1
  epoch 2: Settled NAV 1.00010959 | subs 101500.00 | redeemed shares 0.00 | claimable cash 0.00 | liquidity 0.00 | settle calls 1
  epoch 3: Settled NAV 1.00021918 | subs 20000.00 | redeemed shares 10000.00 | claimable cash 10002.19 | liquidity 0.00 | settle calls 1
  rejections as designed: 1 (subscribe:BelowMinimum)
  supply 181,234.7453300 = balances 181,234.7453300 = journal 181,234.7453300: reconciled
  vault cash 0.0000000 = pending + claimable 0.0000000: reconciled

statement inv_12: shares 0.0000000, KYC EXPIRED, requests #11 Subscribe Claimed, #29 Redeem Claimed
TS model == Rust scenario golden file (data/seed/expected-scenario.json): every figure matches
```

Reading the output:
- USD-D epoch 3 needs a 1,050,000 USDC treasury top-up; its first final `settle` reverts
  with `InsufficientLiquidity`, so it takes 2 settle calls.
- The 10 USD-D rejections are the seeded ones: a sub-minimum subscription, a US investor,
  a cancel after cut-off, a redemption to an unregistered cash address, two transfers by
  holders whose KYC had expired, a distribution claim to an unregistered address, a
  single-role forced transfer, a stale NAV and a NAV outside the 25 bps band.
- Distribution rounding left 8 stroops for 21 holders (bound: holders + 1).

## 5. Console commands (offline)

Register import: 25 rows → 24 valid + 1 checksum error; with the corrected row the output is
byte-identical to the committed `data/seed/register.json`.

```
$ node dist/src/cli.js kyc import ../data/seed/investors.csv --out /tmp/reg1.json
register: 24 valid row(s), 1 error(s), 11 normalisation note(s)
ERROR line 22 inv_20 wallet: invalid StrKey (checksum or format): GCVNL7RHZY4TAJJQOIPBAQUIXGHL6UT42Q2IUAWDXOPYKQE3NVGUSPSA
note  line 4 inv_02 jurisdiction: "De " normalised to DE
note  line 6 inv_04 jurisdiction: alpha-3 "ESP" mapped to ES
note  line 15 inv_13 legal_name: same legal name as inv_08 with a different wallet; confirm they are different people
...
$ node dist/src/cli.js kyc import ../data/seed/investors.csv --fix ../data/seed/investors.fix.csv --out /tmp/reg2.json
$ cmp /tmp/reg2.json ../data/seed/register.json && echo identical
identical
```

KYC triage (heuristic provider, suggestions only):

```
$ node dist/src/cli.js kyc triage --docs fixtures/kyc --now 2026-10-05T00:00:00Z
| Investor | Bucket | Effective expiry | On-chain expiry | Mismatch (days) | Why |
|---|---|---|---|---|---|
| inv_12 | expired | 2026-09-30 | 2026-10-07 | -7 | - |
| inv_05 | needs_human | - | 2027-09-09 | - | inv_05__passport: MRZ check digit failed; missing a readable proof_of_address |
| inv_22 | needs_human | - | 2027-04-30 | - | inv_22__idcard: ambiguous date; missing a readable proof_of_address |
| inv_24 | needs_human | - | 2027-11-11 | - | inv_24__scan: unreadable or unrecognised document; ... |
| inv_02 | lt30 | 2026-10-18 | 2027-03-15 | -148 | - |
| inv_07 | lt30 | 2026-10-06 | 2026-10-06 | 0 | - |
| inv_01 | lt90 | 2026-11-18 | 2027-06-30 | -224 | - |
| inv_10 | lt90 | 2026-11-12 | 2028-03-31 | -505 | - |
```

The negative mismatches show the on-chain expiry is later than the documents support,
mostly because of the 90-day proof-of-address **Assumption**. A TA reviews that; nothing is
written on-chain.

Form extraction:

```
$ node dist/src/cli.js kyc extract --forms fixtures/forms
| Form | Name | Class | Amount | Jurisdiction | Signed | Flags |
|---|---|---|---|---|---|---|
| f01_clean | Amélie Laurent | USD-D/USDC | 25000.0000000 | FR | yes | - |
| f02_wallet_typo | Ana Costa | USD-D/USDC | 15000.0000000 | PT | yes | wallet_invalid_strkey |
| f03_words_conflict | Helvetia Reserve AG | USD-D/USDC | 150000.0000000 | CH | yes | amount_words_mismatch |
| f04_unsigned | Tiago Almeida | USD-D/USDC | 3333.3300000 | PT | NO | not_signed |
| f05_class_currency | Jan de Vries | EUR-A/USDC | 100000.0000000 | NL | yes | class_currency_mismatch |
| f06_below_minimum | Jonas Weber | USD-D/USDC | 999.9900000 | DE | yes | below_minimum |
| f07_entity_two_cash | Bramwell Treasury SARL | USD-D/USDC | 1000000.0000001 | LU | yes | - |
| f08_us_person | Robert Miller | USD-D/USDC | 10000.0000000 | US | yes | jurisdiction_not_allowed |
```

Transactions are composed offline and never sent without `--submit` and a reachable RPC:

```
$ node dist/src/cli.js subscribe --investor inv_01 --amount "25,000"
async_vault.request_subscribe  (auth: investor)
  args: {"investor":"GBKKC7KJDN733KFBYAAXBTQPPBDGR7OZUET7MA7KKM7ELRCGFTWVZ4WL","amount":"250000000000"}
  cli:  stellar contract invoke --id CBQXG6LO... --network testnet --source-account <key> -- request_subscribe --investor GBKK... --amount 250000000000
  xdr:  AAAAAgAAAAA...
(not sent: add --submit with SOROBAN_RPC_URL set to simulate, sign and send)

$ SOROBAN_RPC_URL=http://127.0.0.1:9/ node dist/src/cli.js epoch open --cutoff 2026-10-08T13:00:00Z --submit
--submit: RPC http://127.0.0.1:9/ is not reachable; nothing was sent          [exit 2]
```

A forced transfer with one role is refused before anything is signed; with TA + ADMIN it is
composed (the replacement wallet is passed by address because it is registered during the
scenario, not in `register.json`):

```
$ node dist/src/cli.js force --from inv_05 --to inv_05b --shares 12000 --reason "lost keys" --signers ta_ops_1
refused locally by the ops policy mirror: InsufficientAdmin (compliance.forced_transfer needs 1 ADMIN signature(s), got 0); nothing was signed   [exit 3]

$ node dist/src/cli.js force --from inv_05 --to GCV5V53BNO2ZSIH7FKNK2B7AOMUN2BACU2KZCIMKO5B6VXFQEWLHKFVT --shares 12000 --reason "lost keys; ticket TA-2026-1187"
compliance.forced_transfer  (auth: ops (TA>=1, ADMIN>=1, total>=2) signed by ta_ops_1+fund_admin_1)
```

A NAV outside the band needs the co-signed override:

```
$ node dist/src/cli.js strike --epoch 3 --publish 1.0270 --as-of 2026-10-07T14:00:00Z --override
nav_oracle.publish  (auth: ops (TA>=0, ADMIN>=1, total>=1))
  args: {"asset":{"tag":"Other","values":["USD_D"]},"price":"102700000000000","timestamp":"1791381600"}
async_vault.strike_nav_override  (auth: ops (TA>=1, ADMIN>=1, total>=2))
```

Statements and the TA register (from the offline replay; snapshots in `app/test/snapshots/`):

```
$ node dist/src/cli.js statement --register
# TA register - class USD-D
- registrar total_shares 374,360.9527849 | sum of SAC balances 374,360.9527849 | journal 374,360.9527849 -> RECONCILED
- vault cash 0.0000000 | pending + claimable 0.0000000 -> RECONCILED
## KYC expiring or expired
- inv_07: 2026-10-06T12:00:00Z (expired 1 day(s) ago)
- inv_12: 2026-10-07T00:00:00Z (expired <1 day ago)
- inv_19: 2026-10-06T20:00:00Z (expired <1 day ago)
## Exceptions for a person to act on
- 2026-10-05T08:15:40Z subscribe inv_02: BelowMinimum
- 2026-10-05T12:02:47Z subscribe inv_23: JurisdictionBlocked
...
- 2026-10-07T14:05:00Z strike : NavMoveTooLarge
```

`node dist/src/cli.js init --fund ../data/seed/fund.json` prints the 38-step deployment plan
(issuer flags → SAC → five contracts → `set_admin(compliance)` → `bind` → 16 policy rows →
13 jurisdictions).

## Not shown

- Testnet deployment (`scripts/deploy-testnet.sh`): written, **not executed**.
- `funddesk journal` (needs RPC) and `--provider llm` against the real API (needs a key; the
  retry, fallback and disagreement paths are tested with a mocked `fetch`).
