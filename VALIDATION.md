# Fund Desk validation

What we know, what we assume, what we built, and what would prove or kill the idea.
Nothing in this file is a claim of adoption: no issuer, administrator, TA, investor,
interview, pilot or partner exists yet. Proof level today: **0** (idea backed by researched
market-size and standards-gap evidence; no buyer-pain evidence).

**Panel caveat, carried into every later field:** the evidence shows market size (about $4bn of RWAs on Stellar) and a standards gap. It does not show buyer pain. Nobody has yet told us that servicing costs them money or causes incidents.

## Evidence tiers

### Researched evidence

- Stellar tokenised RWAs about $3.996bn on 2026-08-29 (Spiko $1.55bn, Realiz $559M,
  Tradable $548M, BENJI $546M, Ondo USDY $535M), and the largest share (about $490M) of
  tokenised non-US government debt of any public chain; figures conflict by date (Messari
  Q1 about $1.2bn, SDF May ">$2B") (`research/01-stellar-ecosystem.md` §4).
- For fiat flows, "investors interact off-chain with the fund manager or transfer agent,
  who subsequently maintains the on-chain DLT records" (ECB Macroprudential Bulletin, April
  2026, via `research/11-round2-crosscutting-problems.md` §R2).
- Workflow KYC → whitelist → cash → TA mints; NAV struck daily; redemptions paid the same
  business day, often in a regulated stablecoin (securities.io; Spiko via everstake.one;
  11-R2). Spiko has more than 1,100 investors from €1,000 minimums (11-R2).
- BENJI's TA "creates and controls blockchain wallets on Stellar" and keeps the official
  share record (eco.com, 11-R2): Franklin is not a buyer.
- SDF gap list: "no turnkey transfer-restriction / attestation registry standard for RWAs
  on Soroban" (`research/01` §13.8).
- Prior art: OpenZeppelin `stellar-tokens` RWA and vault modules (pinned to SDK 26/27, no
  async queue), RWA-ToolKit (testnet, weak maintenance signal), jarik2014/compliance-primitives
  ("more audit surface and more chances for compliance incidents" as its motivation), zero
  Soroban async subscription/redemption vaults found (`research/01` §2; 11-R2).
- ERC-7540 is the EVM async standard; Centrifuge runs more than $1.6bn through it (11-R2).
- SAC honours classic issuer flags; SEP-8 is Final; SEP-40 is Draft; Reflector is a SEP-40
  oracle; RedStone prices Stellar RWAs (`research/01` §2, §4, §5, §7).
- In soroban-env-host 28.0.2, SAC `clawback` spends without an authorisation check, and the
  clawback-ability of a contract balance is fixed when the balance is created (read in the
  host source in the local cargo registry; the precondition test reproduces it).
- SDF 2026 targets: 15 new enterprise signings, ≥ 5 in production, +$1B network asset value
  (`research/01` §13.3). New RWA issuers enter through SCF, for example Bando (SCF #42).

### Observed facts (this repository only)

- The five contracts execute in the Soroban host: the Rust suite passes (`cargo test`,
  counts in DEMO.md), including `scenario_three_epochs_seed` with reconciliation asserted
  after every step, and all five build to wasm (sizes in DEMO.md).
- After a registrar transfer both holders' balances are deauthorised and a direct SAC
  `transfer` by the holder fails: the gate cannot be bypassed through the token.
- The TypeScript model reproduces every figure of `data/seed/expected-scenario.json`
  (written by the Rust scenario): 0 differences.
- Lab metrics on the seed (Simulated data, measured in this repo): every seeded transfer or
  subscription to an ineligible holder was blocked (3 transfer/subscription rows, plus the
  random property runs), 0 bypasses through direct SAC calls; 0-stroop difference between
  registrar supply and the sum of SAC balances after every step; distribution rounding left
  8 stroops in the contract for 21 holders (bound: holders + 1 per declaration); the
  16-request USD-D epoch 1 settled in 1 `settle` call (batch 20), the epoch needing a
  liquidity top-up in 2.
- `settle(epoch, 20)` used about 11.5M CPU instructions natively; wasm execution costs more
  and was not measured on a network.
- The heuristic KYC triage read 48/48 labelled fields correctly and the form extractor
  72/72 on **synthetic fixtures written by us**. This is Simulated and says nothing about
  real documents.
- Testnet deployment was **not** performed; the sandbox had no route to Stellar testnet,
  Horizon or Friendbot.

### Team assumptions

- The buyer set is about five issuers and the large ones run their own TA/administrator
  stack (derived from the researched list and BENJI's TA model); Spiko's in-house stack
  probably rules it out too.
- Distributions for income share classes are monthly (typical for MMF income classes; not
  sourced).
- Proof of address stays valid 90 days after issue and a registry extract 3 months: both
  configurable in `fund.json`, flagged in every triage report. A consequence we noticed while
  labelling fixtures: with a 90-day proof-of-address rule, no individual can ever sit in the
  `ok` (≥ 90 days) bucket, so the rule may be stricter than how TAs actually run refresh.
- NAV at 14 decimals follows Reflector's convention; `nav_oracle` stores what it is given.
- The entry point for validation is a fund administrator, not an issuer (11-R2).

### Hypotheses

- H1 (buyer): tier-2 or new issuers on or entering Stellar would adopt a shared, audited
  servicing kit instead of writing their own token contract and back office.
- H2 (time): cut-off to investor-visible settlement falls from "same business day" to under
  one hour after the NAV is published.
- H3 (touches): manual TA touches per subscription fall by at least 50%.
- H4 (compliance): transactions by holders with expired KYC fall to 0 because the gate is
  evaluated at ledger time.
- H5 (distributions): preparing a monthly distribution takes minutes instead of a
  spreadsheet cycle.
- H6 (AI): the triage saves TA time on KYC refresh; value unmeasured and not a reason to
  fund the project.

### Simulated / demo data

`data/seed/` describes the fictional "Seedfund Short Treasury MMF" (USD-D distributing in
USDC, EUR-A accumulating in EURC), 25 investors with messy codes (`fr`, `De `, `ESP`), mixed
date formats, one wallet with a bad StrKey checksum (corrected in `investors.fix.csv`), two
investors with the same name, an entity with two cash addresses, a US investor, three KYC
expiries timed around the epochs, three epochs of orders, three NAV series (with a stale and
a fat-fingered publish), one distribution of 12,418.5531907 USDC and a lost-keys forced
transfer. The AI fixtures (12 KYC documents, 8 forms) are synthetic texts. Every address was
generated locally from a fixed seed and holds nothing.

### Actual validation

None yet. No interviews, no pilot, no issuer, no administrator, no measurement outside the
lab metrics above.

## Baseline and success metric

| Metric | Baseline | Target | How measured |
|---|---|---|---|
| Transfers to ineligible holders | unknown (not researched) | 0, with 0 bypasses via the SAC | lab: seed + property tests (done, Simulated); pilot: blocked-transfer events vs attempts |
| Registrar supply vs Σ balances | n/a | 0-stroop difference | lab: asserted every step (done); pilot: `statement --register` reconciliation daily |
| Distribution rounding loss | n/a | ≤ holders + 1 stroops per declaration | lab: 8 stroops for 21 holders (done) |
| Cut-off to investor-visible settlement | "same business day" (E5, researched) | < 1 h after NAV publication | `Struck` and `Claimed` event timestamps in shadow mode |
| Manual TA touches per subscription | to be measured | −50% | administrator's own count before/after in the shadow trial |
| Transactions by expired-KYC holders | to be measured | 0 | registrar rejections vs the administrator's incident log |
| Time to prepare a monthly distribution | to be measured | minutes | stopwatch, administrator's current process vs `distribute declare` |

## Experiment plan

- **Weeks 1-2: interviews.** Reach 8 targets through SDF's RWA/enterprise team and the
  RedStone and Reflector teams: 2 fund administrators or TAs servicing tokenised funds,
  3 tier-2 or new issuers on or entering Stellar (for example SCF-funded RWA issuers), and
  1 larger issuer as a control that expects "we built it". Questions: how orders are queued
  and cancelled today; manual steps per subscription; how KYC expiries are caught and how
  many were missed last quarter; how distributions are computed and paid; what the issuer's
  own token contract and its audit cost.
- **Weeks 3-4: shadow trial.** One administrator replays an **anonymised** register and a
  real NAV series through `funddesk` on testnet; we compare its register, claimables and
  distribution figures with theirs, line by line.
- **Pass:** ≥ 2 of 6 tier-2/new-issuer/administrator conversations name a servicing cost or
  incident they would outsource, and ≥ 1 runs the shadow trial.
- **Kill or pivot:** 0 of 6. Publish the contracts as an open-source reference (SCF Public
  Goods, or offered upstream to OpenZeppelin `stellar-tokens`) and stop pursuing a product.
- **Willingness to pay:** ask the administrator about a flat annual licence vs basis points
  on serviced AUM. No price point is researched.

## Killer questions

1. **Would the user care if it disappeared?** Unknown, and this is the weakest point. The
   gap is researched (no Soroban async vault, no turnkey transfer-restriction registry);
   the pain is not. No TA or administrator has told us servicing costs them money or causes
   incidents (panel caveat). Nobody has used it.
2. **Did anyone outside the team use it?** No.
3. **Before/after measured?** No. Only lab metrics on simulated data exist (blocked
   transfers, reconciliation, rounding). Pilot baselines (touches per order, missed
   expiries, settlement latency) are defined above and unmeasured.
4. **Value lost without AI?** Nothing on-chain and some TA time off-chain (Hypothesis H6).
   The contracts, register, queue and distributions work unchanged without it. If the model
   is wrong, the cost is a wrong suggested expiry that a human reviews; the chain only ever
   sees the date the TA approves with `kyc approve`. The model output is schema-checked,
   cross-checked against the deterministic heuristic, and every disagreement goes to a
   person. This is not a reason to fund the project.
5. **Reason to keep using after demo?** Hypothesis: NAV is struck daily, so the queue,
   settlement and reconciliation run every business day, KYC refresh never stops, and
   distributions recur monthly. Once a fund's share SAC admin is the registrar, the kit is
   on the transfer path of every holder. That is also a lock-in risk the buyer will weigh.
6. **Would someone pay?** Hypothesis: SaaS per issuer plus basis points on serviced AUM,
   or an annual licence to an administrator servicing several issuers (model proposed in
   11-R2; no price researched). Risk: about five issuers exist and the large ones built
   their own; tier-2 issuers may prefer a full-service TA to a kit.
7. **Does the chain create value?** Partly, and the claim is "standardise and make
   visible", not "remove a trusted party". The queue status, the NAV applied per epoch, the
   eligibility rule that blocks a transfer and each holder's distribution entitlement become
   shared state that investor and TA both read, and the transfer rule applies to every
   wallet and contract because balances are deauthorised at rest, with no issuer server on
   the transfer path (unlike SEP-8). The NAV is still struck off-chain, KYC is still done
   off-chain and the legal register of record stays with the TA.
8. **Why this chain?** The issuers are here (about $4bn of RWAs, the largest share of
   non-US government debt), SDF named this gap and targets RWA growth, classic issuer flags
   reach Soroban through the SAC (the protocol-level gate and clawback this kit relies on),
   USDC/EURC settle natively, custom accounts express TA/ADMIN role policies, and SEP-40
   oracles already price Stellar RWAs. On Arbitrum the same issuers already have ERC-7540
   (Centrifuge, OpenZeppelin) plus Securitize and Ondo, so a kit there would be a me-too.

## Main risks

Buyer scarcity (about five issuers, large ones in-house); regulatory (using the kit does
not make anyone a registered TA); OpenZeppelin or SDF shipping an async vault (mitigation:
keep the ERC-7540 shape and offer upstream); deauthorised-at-rest breaks composability;
NAV trust stays off-chain; SDK/protocol churn and state archival; audit cost before any
real money (SCF Audit Bank only after an award).

## Proof level achievable

1 during the MVP (functional locally, testnet-ready scripts): reached locally, testnet
scripts not executed. 2 if one administrator or tier-2 issuer runs an anonymised register
and a real NAV series through the kit in shadow mode. 3 (real subscriptions) is not
realistic inside the MVP window: no regulated fund will move its register onto an
unaudited kit.
