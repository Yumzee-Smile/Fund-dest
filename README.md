# Fund Desk

A servicing kit for tokenised-fund issuers on Stellar. Subscriptions and redemptions run
as asynchronous requests that settle at the NAV struck for their epoch, share transfers
are gated on KYC expiry and jurisdiction at the ledger time of every move, and income
distributions accrue per share on-chain. A tier-2 or new issuer, or the fund administrator
or transfer agent (TA) that services it, gets the ERC-7540 pattern on Soroban plus a
transfer-restriction registry, instead of hand-rolling both in its own token contract.

## Problem

A tokenised money-market fund's shares already live on Stellar (about $4bn of RWAs, the
largest share of tokenised non-US government debt of any public chain; research/01 §4),
but the servicing around them does not. The TA KYCs investors and whitelists wallets by
hand, orders are queued off-chain, the administrator strikes NAV daily and the TA mints at
that NAV; KYC refresh and distributions live in back-office tools and spreadsheets
(research/11-R2; securities.io; ECB Macroprudential Bulletin, April 2026). Every issuer
writes its own allowlist and queue logic. SDF lists "no turnkey transfer-restriction /
attestation registry standard for RWAs on Soroban" as an ecosystem gap (research/01 §13.8),
and a search for a Soroban async subscription/redemption vault returned nothing (11-R2).

**Caveat carried from the selection panel:** the evidence shows market size and a
standards gap. It does not show buyer pain. Nobody has told us that servicing costs them
money or causes incidents. See [VALIDATION.md](VALIDATION.md).

## User

Operations staff at a TA or fund administrator who whitelist investors, queue and cancel
orders before cut-off, post NAV, run settlement and pay distributions for a tokenised
money-market or short-treasury fund. The problem owner is the issuer's COO or head of
operations; the intended buyer is a tier-2 or new issuer (or its administrator), not the
top five issuers who run their own stacks.

## What the MVP does

Five Soroban contracts (soroban-sdk 28) and a TypeScript console, `funddesk`:

| Contract | Role |
|---|---|
| `ops_account` | Custom account shared by TA and ADMIN keys. `__check_auth` enforces a per-function role policy (e.g. forced transfer and NAV override need one TA **and** one ADMIN signature; any contract without a policy row, such as a token, is refused). |
| `nav_oracle` | Minimal SEP-40 feed the administrator publishes NAV to. Replaceable by a RedStone or Reflector SEP-40 contract (the vault only uses `decimals` + `lastprice`). |
| `compliance` | Investor register and **SAC admin of the share asset**. The asset is issued with AUTH_REQUIRED, AUTH_REVOCABLE and AUTH_CLAWBACK_ENABLED, so every balance is deauthorised at rest; every move goes through the registrar, which checks KYC expiry, jurisdiction and freeze at the ledger time and authorises, moves, deauthorises. Expired or out-of-scope holders cannot receive but can still redeem to a pre-registered cash address. |
| `distribution` | Per-share accumulator (scale 10^18, remainder carried). The registrar notifies it before every balance change, so entitlements survive transfers, burns and forced transfers. |
| `async_vault` | Epochs with cut-off: `request_subscribe` / `request_redeem` / `cancel`, `strike_nav` with staleness, lateness and move-band checks, paged `settle` with deterministic rounding and dust, liquidity top-up, `claim` by investor or TA, `abort_epoch`, `pause`. |

The console builds every transaction offline from the embedded contract specs and prints
it (arguments, required signer, `stellar contract invoke` equivalent, unsigned XDR). It
imports and validates the investor register CSV, assembles ops-account signatures (sorted,
deduplicated, refused locally when the policy is not met), renders holder statements and
the TA register with a three-way reconciliation, keeps a local event journal, and runs an
optional AI triage of KYC documents and subscription forms (deterministic heuristic by
default; the model provider is cross-checked against it and never signs anything).

## Quickstart

Requirements: Rust 1.94 with `wasm32v1-none`, stellar-cli 28, Node 22 (see
`../../TOOLCHAIN.md`). From this directory:

```sh
cargo test                       # 5 contracts in the Soroban host, incl. the 3-epoch seed scenario
stellar contract build           # 5 wasm files in target/wasm32v1-none/release/
scripts/gen-specs.sh             # embed contract specs into the app
cd app && npm install && npm test && npm run demo
```

A few commands (all offline; add `--submit` with `SOROBAN_RPC_URL` set to send):

```sh
cd app
node dist/src/cli.js kyc import ../data/seed/investors.csv --fix ../data/seed/investors.fix.csv --out /tmp/register.json
node dist/src/cli.js kyc triage --docs fixtures/kyc --now 2026-10-05T00:00:00Z
node dist/src/cli.js kyc extract --forms fixtures/forms
node dist/src/cli.js subscribe --investor inv_01 --amount "25,000"
node dist/src/cli.js force --from inv_05 --to inv_01 --shares 12000 --reason "lost keys" --signers ta_ops_1   # refused: InsufficientAdmin
node dist/src/cli.js statement --register
node dist/src/cli.js init --fund ../data/seed/fund.json
```

[DEMO.md](DEMO.md) has the exact commands and output. [ARCHITECTURE.md](ARCHITECTURE.md)
covers interfaces, the deployment order, the sandwich rule, trust assumptions, the ERC-7540
mapping and a threat table.

## What the tests prove

- **Contracts (Rust):** every function, every auth and role check (real Ed25519 signatures
  through `__check_auth`, `env.auths()` assertions), every cut-off, strike-window and
  abort rule; the deauthorised-at-rest gate (a direct SAC transfer by a holder fails after
  a registrar transfer); the flag-order deployment precondition; distribution accrual
  across transfers, forced transfers, burns and carried remainders; property tests for the
  settlement maths (3 x 10,000 cases) and the accumulator (64 random sequences); a
  `settle(epoch, 20)` budget check; and `scenario_three_epochs_seed`, which runs the whole
  seed journey (two share classes, 51 order rows, three KYC expiries, a US investor, stale
  and fat-fingered NAVs, a liquidity top-up, a distribution, holder transfers and a forced
  transfer) with reconciliation asserted after every step, and writes
  `data/seed/expected-scenario.json`.
- **App (TypeScript, offline):** CLI parsing, register import (24 valid + 1 checksum error,
  codes normalised), the TS NAV and distribution mirrors reproducing the Rust golden file
  figure for figure, ops-account signature assembly, heuristic KYC triage and form
  extraction against labelled synthetic fixtures, the model provider's retry and fallback
  with a mocked `fetch`, transaction encoding round trips, and statement/register snapshots.

## Status

**functional locally.** All five contracts execute in the Soroban host through
`cargo test` and build to wasm; the app and its tests run offline. `scripts/deploy-testnet.sh`
is **testnet-ready** but was **not executed** (no route to Stellar testnet from the build
environment). No users, no issuer, no administrator, no interviews, no pilot: see
[VALIDATION.md](VALIDATION.md).
