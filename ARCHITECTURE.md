# Fund Desk architecture

## Components

```
investor wallet ──request_subscribe / request_redeem / cancel / claim──► async_vault
   │                                                                        │ cash escrow (USDC/EURC SAC)
   │                                                                        │ status / lock / unlock / issue / redeem_burn
   └──transfer (holder-to-holder)──► compliance (registrar, SAC admin) ◄────┘
                                        │ set_authorized → mint/transfer/clawback → set_authorized(false)
                                        ▼
                                   share SAC (AUTH_REQUIRED | AUTH_REVOCABLE | AUTH_CLAWBACK_ENABLED)
                                        │ on_change(holder, old_balance) before every balance change
                                        ▼
                                   distribution (per-share accumulator, pays USDC)

ops_account (TA + ADMIN keys, __check_auth policy table) authorises: open_epoch, strike_nav(_override),
settle, claim (push), abort_epoch, pause/unpause, set_investor, set_frozen, set_jurisdiction, bind,
forced_transfer, nav_oracle.publish, declare, claim_for.

administrator ──publish(asset, NAV, as_of)──► nav_oracle (SEP-40) ──decimals / lastprice──► async_vault
treasury ──deposit_liquidity / declare──► vault / distribution; receives surplus subscription cash.
funddesk (app): builds and signs transactions, journals events, statements, AI triage (off-chain only).
```

Units: cash and shares use 7 decimals (classic assets). NAV uses the oracle's `decimals()`
(14 in the seed, Reflector's convention). `BPS = 10_000`; distribution `SCALE = 10^18`.
All arithmetic is checked and fails with `MathOverflow`.

## Contract interfaces

### ops_account (custom account)

- `__constructor(signers: Vec<(BytesN<32>, Role)>)`: at least one `Ta` and one `Admin`.
- `set_policy(contract, fn_name, Option<Policy>)`, `set_signer(key, Option<Role>)`: both
  need the account's own auth, which is hard-coded to TA >= 1 and ADMIN >= 1. Removing or
  re-roling the last key of a role fails (`LastSignerOfRole`).
- `policy(contract, fn_name)`, `signers()`.
- `__check_auth(payload, Vec<Sig>, contexts)`: keys strictly ascending (duplicates
  rejected), each key registered and its Ed25519 signature verified (a bad signature traps),
  valid signatures counted per role, every `Context::Contract` checked against its row
  (TA, then ADMIN, then total), a missing row is `NoPolicy`, any create-contract context is
  `ForeignContext`. No token has a row, so the ops account can never move tokens.
- Errors 1-9 as specified, plus `InvalidPolicy = 10` (added, see Deviations).

Seed policy table (`data/seed/fund.json`):

| Function | TA | ADMIN | Total |
|---|---|---|---|
| compliance.set_investor, set_frozen | 1 | 0 | 1 |
| compliance.set_jurisdiction, bind, forced_transfer | 1 | 1 | 2 |
| async_vault.open_epoch, settle, claim | 1 | 0 | 1 |
| async_vault.strike_nav | 0 | 1 | 1 |
| async_vault.strike_nav_override, abort_epoch, unpause | 1 | 1 | 2 |
| async_vault.pause | 0 | 0 | 1 |
| nav_oracle.publish, distribution.declare | 0 | 1 | 1 |
| distribution.claim_for | 1 | 0 | 1 |

### nav_oracle (SEP-40 subset)

`__constructor(publisher, base: Asset, decimals, resolution)`; `publish(asset, price,
timestamp)` by the publisher only, rejecting `price <= 0`, `timestamp > now` and a
non-increasing timestamp; reads `base`, `assets`, `decimals`, `resolution`,
`lastprice(asset)`, `price(asset, timestamp)`. Storage `Last(Asset)` and `Hist(Asset, u64)`
(persistent, TTL extended on write).

### compliance (registrar)

- Eligibility at `env.ledger().timestamp()`:
  `can_receive = registered ∧ ¬frozen ∧ kyc_expiry > now ∧ jurisdiction allowed`;
  `can_send = registered ∧ ¬frozen ∧ kyc_expiry > now`; `can_redeem = registered ∧ ¬frozen`.
- `bind(vault, distribution)` once (ops); `set_investor` validates 1-3 cash addresses and a
  two-letter upper-case code (checked on the symbol's encoding); `set_jurisdiction`
  (default deny); `set_frozen(investor, frozen, reason_hash)`.
- `transfer(from, to, amount)`: `from` auth, eligibility of both, unlocked balance, then
  `on_change` for both, then the sandwich.
- Vault only: `issue` (registered and not frozen; full eligibility was checked at
  settle), `lock` (can_redeem, registered cash address, unlocked balance), `unlock`,
  `redeem_burn` (SAC `clawback`, which works on a deauthorised balance).
- `forced_transfer(from, to, amount, reason)`: ops under TA + ADMIN; clawback from `from`,
  sandwich-mint to `to`; supply unchanged.
- Views `investor`, `status`, `balance` (reads the SAC), `locked`, `total_shares`,
  `is_cash_address`; `extend(addrs)` is permissionless.

### distribution

`on_change(holder, old_balance)` (registrar only) books `old_balance × (Acc − snap)`.
`declare(amount, memo)` (ops ADMIN + treasury) pulls cash, `num = amount × SCALE + carry`,
`Acc += num / T`, `carry = num % T`. `accrued(holder)` floors;
`claim(holder, to)` needs the holder, a registered cash address and no freeze, pays the
floor and keeps the remainder; `claim_for(holders ≤ 25)` (ops TA) pays first cash addresses
and skips frozen, unregistered or zero holders.

The registrar passes `old_balance` rather than letting `distribution` read it back, because
Soroban forbids re-entering `compliance` while it is executing.

### async_vault

Epoch state machine: `Open` (requests and cancels while `now < cutoff`) → `strike_nav`
(after cut-off) → `Struck` → `settle` pages → `Settling` → `Settled` (claims allowed); or
`Open` past `cutoff + max_strike_delay` → `abort_epoch` (paged) → `Aborted` (subscriptions
refundable, redemptions unlocked). Request states: `Pending` → `Cancelled` | `Claimable`
(`reject ≠ 0` = full refund) → `Claimed`.

Strike checks, in order: `Open` and `now ≥ cutoff`; price present; `price.timestamp ≥
cutoff` (else `StalePrice`); `≤ cutoff + max_strike_delay` (else `PriceTooLate`);
`price > 0`; `|p − LastNav| × 10,000 ≤ LastNav × max_nav_move_bps` (not for the co-signed
override). Settlement per item: subscriptions re-check `can_receive` (refund with reject
4/5/6 otherwise) and get `shares = floor(cash × 10^d / nav)`, `dust = cash − ceil(shares ×
nav / 10^d)`; redemptions get `floor(shares × nav / 10^d)` and are burned. The call that
exhausts the queue computes `surplus = sub_total + liquidity − claimable_cash`; a negative
surplus reverts that call with `InsufficientLiquidity` (earlier pages stay committed), a
positive one is swept to the treasury. All rounding favours the fund.

### ERC-7540 mapping (for integrators)

| ERC-7540 | Fund Desk |
|---|---|
| `requestDeposit` | `request_subscribe` |
| `requestRedeem` | `request_redeem` |
| `pendingDepositRequest` / `claimableDepositRequest` | `request(id).status` (`Pending` / `Claimable`) |
| `deposit` / `redeem` (claim) | `claim` |
| operator | `ops` via `claim` |

## Deployment order (asserted in tests)

1. The share issuer sets AUTH_REQUIRED, AUTH_REVOCABLE and AUTH_CLAWBACK_ENABLED **before
   any trustline or balance exists**. The clawback flag of a balance is fixed when the
   balance is created (soroban-env-host 28.0.2, `stellar_asset_contract/balance.rs`), so a
   balance created before the flag cannot be burned or force-transferred:
   `deployment_precondition_clawback_flag_must_precede_balances` proves it.
2. Deploy the share SAC, then `ops_account`, `nav_oracle`, `compliance`, `distribution`,
   `async_vault` (the vault constructor asserts the oracle's decimals).
3. The issuer calls SAC `set_admin(compliance)`. From then on only the registrar can
   authorise share balances.
4. Ops (TA + ADMIN) calls `compliance.bind(vault, distribution)`, the `set_policy` rows and
   `set_jurisdiction` rows. `funddesk init` prints this plan; `scripts/deploy-testnet.sh`
   runs it (not executed here).

## The sandwich rule

Every share movement inside the registrar is `set_authorized(x, true)` → SAC operation →
`set_authorized(x, false)` for every holder the operation credits or debits, inside one
contract call. Consequences, all tested:

- a holder, a DEX or any other contract calling the SAC directly fails with the SAC's
  deauthorised-balance error, so the eligibility rule applies to every wallet and contract;
- a holder transfer's SAC `transfer` is a sub-invocation covered by the holder's signed
  auth tree of `compliance.transfer`;
- burns use `clawback`, which spends a deauthorised balance without authorisation;
- mints (`issue`, forced transfer) need the receiver authorised for the duration of `mint`.

## Data flow of one epoch

1. TA `open_epoch(cutoff)` (previous epoch must be past its cut-off).
2. Investors `request_subscribe` (cash escrowed) / `request_redeem` (shares locked, cash
   address checked); `cancel` before cut-off.
3. Administrator publishes NAV to `nav_oracle` and calls `strike_nav` (ADMIN) or, outside
   the band, `strike_nav_override` (TA + ADMIN).
4. TA `settle(epoch, 20)` until remaining is 0; treasury `deposit_liquidity` if the last
   page reports `InsufficientLiquidity`.
5. TA pushes `claim` for every claimable request (or investors claim themselves).
6. The app ingests events into its journal (RPC keeps about 7 days, research/01 §14.7) and
   renders statements and the register reconciliation.

## Trust assumptions

- The administrator's NAV is correct. It is bounded by the move band; overriding the band
  needs both roles. A wrong NAV inside the band settles at the wrong price.
- TA and ADMIN keys are not both compromised. One key alone can open epochs, settle,
  freeze, register investors or strike within the band; it cannot force-transfer, override
  the band, abort, unpause, change policy or signers, or move tokens.
- The issuer handed SAC admin to `compliance`. Whoever controls the registrar controls the
  register; the MVP ships **without** `update_current_contract_wasm`, so an upgrade is a
  redeploy plus migration (and a new `set_admin`, which needs the registrar's cooperation:
  a registrar-side `set_sac_admin` would be needed and is not in the MVP).
- KYC itself happens off-chain; the chain holds addresses, expiry timestamps, jurisdiction
  codes and hashes of freeze/forced-transfer reasons, never documents or names.
- The legal register of record stays with the TA; the chain can be a secondary record.

## Limits

- Holder-to-holder moves only through the registrar: no DEX trading, no Blend/DeFindex
  unless those contracts are registered as holders.
- Queue bounded by `max_requests_per_epoch` (seed 400) to keep the `Queue(epoch)` entry well
  under the 64 KiB entry limit (research/01 §14.1); settle and abort are paged.
- `settle(epoch, 20)` measured about 11.5M CPU instructions natively in the Soroban host
  (test `settle_batch_of_20_fits_the_transaction_budget`, asserted with a 4x margin under the
  100M per-transaction limit). Native execution underestimates wasm execution; re-measure
  with `stellar contract invoke --sim-only` on testnet before choosing a batch size.
- One oracle asset per class; no FX between classes; accumulating class (EUR-A) runs the
  same contracts with no declarations.
- Epoch timing trusts ledger timestamps (seconds, validator-set).

## Deviations from the specification

- `ops_account` adds `InvalidPolicy = 10`: `set_policy` refuses a row with `total = 0`,
  `total < ta + admin`, or a row for the ops account itself (its own administration is
  hard-coded to TA + ADMIN and cannot be weakened by a policy row).
- `async_vault` adds `InvalidAmount = 28`, `CashAddressNotAllowed = 29`,
  `InsufficientShares = 30` and `OracleDecimalsMismatch = 31`, so the vault reports these
  cases in its own error space instead of surfacing registrar error codes; the constructor
  checks that the oracle's `decimals()` equals the configured NAV decimals.
- `nav_oracle` errors are numbered `NonPositivePrice = 1, TimestampInFuture = 2,
  TimestampNotIncreasing = 3` (the specification names them but gives no codes).
- `funddesk statement`, `claim --epoch N --all` (without `--submit`) and the demo read the
  offline replay of the seed. With `--submit`, `settle` and `claim --all` read request ids
  from the vault's `queue` view on the network instead.
- `funddesk journal` (event ingestion from RPC) is an extra command; the specification
  lists the journal module but no command for it.
- The seed's largest epoch has 16 USD-D requests, not 25. It settles in 1 call at batch 20.
  "25 requests in 2 settle transactions" follows from paging (`settle_paginates_25_requests_in_3_calls`
  checks 25 requests at batch 10) and was not run as its own case.

## Threat table (STRIDE-style, for a later audit)

| Threat | Asset | Mitigation in the MVP | Residual |
|---|---|---|---|
| Spoofing an investor to subscribe/redeem/transfer | investor cash and shares | `investor.require_auth()` / `from.require_auth()`; redemption cash only to registered cash addresses | a compromised investor key can redeem to the investor's own cash address |
| Spoofing the TA or administrator | register, NAV | `ops_account` role policy with Ed25519 per key, sorted unique signatures, no policy = no auth | one role key can act within its single-role rows |
| Tampering with NAV | settlement price | staleness (≥ cut-off), lateness (≤ cut-off + 8 h), 25 bps band vs last settled NAV, override needs TA + ADMIN, `NavOverride` event | wrong NAV inside the band; oracle key compromise within the band |
| Tampering with balances outside the registrar | share register | balances deauthorised at rest; SAC admin = registrar; clawback flag set before balances | issuer flags set in the wrong order (precondition test documents it) |
| Repudiation of forced transfers / freezes | legal record | `Forced` / `Frozen` events with a reason hash, TA + ADMIN signatures | reason text lives off-chain |
| Information disclosure | investor PII | only addresses, expiry timestamps, ISO codes and hashes on-chain; fixtures synthetic | addresses and amounts are public by design |
| Denial of service: queue flooding | epoch settlement | minimum subscription, `max_requests_per_epoch`, paged settle | a registered investor can fill an epoch with minimum-size requests |
| Denial of service: liquidity shortfall draining escrow | subscribers' cash | final settle reverts with `InsufficientLiquidity`; claims only after `Settled` | redemptions wait for the treasury |
| Denial of service: missing NAV | pending orders | `abort_epoch` after the strike window refunds and unlocks | investors wait up to cut-off + 8 h |
| Elevation: ops account moving tokens | fund cash | no token policy rows; custom account refuses unknown contexts | a mistaken policy row added with TA + ADMIN |
| Elevation: registrar upgrade | whole register | no upgrade entry point in the MVP | redeploy/migration process is manual |
| State archival of investor records | eligibility | TTL extended on every touch, permissionless `extend`, simulate-before-submit restores | an unattended record could archive after a year without activity |
