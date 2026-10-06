//! Async vault: epoch-based subscription and redemption queue for a
//! tokenised fund share class (the ERC-7540 pattern on Soroban).
//!
//! * Investors `request_subscribe` (cash is escrowed here) or
//!   `request_redeem` (shares are locked by the registrar) before the epoch
//!   cut-off, and may `cancel` before the cut-off.
//! * After the cut-off the administrator strikes the NAV (`strike_nav`),
//!   read from a SEP-40 oracle and checked for staleness, lateness and a
//!   maximum move against the last settled NAV.
//! * `settle` (paged) turns every pending request into a claimable amount at
//!   that NAV, burning redeemed shares, re-checking subscriber eligibility
//!   and sweeping surplus subscription cash to the fund treasury.
//! * `claim` mints shares or pays cash; the investor or the TA (ops) may
//!   call it.
//!
//! Epoch states: `Open` -> [now >= cutoff] `strike_nav` -> `Struck` ->
//! `settle`... -> `Settling` -> `Settled`; or `Open` past the strike window
//! -> `abort_epoch` -> `Aborted` (everything refunded or unlocked).
#![no_std]

pub mod math;

use soroban_sdk::{
    contract, contractclient, contracterror, contractevent, contractimpl, contracttype,
    token::TokenClient, Address, BytesN, Env, Symbol, Vec,
};

pub const BPS: i128 = 10_000;

const LEDGERS_PER_DAY: u32 = 17_280;
const INSTANCE_TTL_THRESHOLD: u32 = LEDGERS_PER_DAY * 30;
const INSTANCE_TTL_EXTEND_TO: u32 = LEDGERS_PER_DAY * 120;
const RECORD_TTL_THRESHOLD: u32 = LEDGERS_PER_DAY * 60;
const RECORD_TTL_EXTEND_TO: u32 = LEDGERS_PER_DAY * 365;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum VaultError {
    Paused = 1,
    NoOpenEpoch = 2,
    CutoffPassed = 3,
    BelowMinimum = 4,
    KycExpired = 5,
    JurisdictionBlocked = 6,
    Frozen = 7,
    NotRegistered = 8,
    RequestNotFound = 9,
    NotRequestOwner = 10,
    NotPending = 11,
    EpochNotClosed = 12,
    PriceMissing = 13,
    StalePrice = 14,
    PriceTooLate = 15,
    NavMoveTooLarge = 16,
    NavNonPositive = 17,
    WrongEpochState = 18,
    InsufficientLiquidity = 19,
    EpochNotSettled = 20,
    NotClaimable = 21,
    AlreadyClaimed = 22,
    AbortTooEarly = 23,
    EpochFull = 24,
    CutoffInPast = 25,
    PreviousEpochOpen = 26,
    MathOverflow = 27,
    // Added beyond the specification so the vault reports these cases in
    // its own error space instead of surfacing a registrar error code.
    InvalidAmount = 28,
    CashAddressNotAllowed = 29,
    InsufficientShares = 30,
    OracleDecimalsMismatch = 31,
}

// ----- SEP-40 subset (same XDR shape as nav_oracle / Reflector) -----

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Asset {
    Stellar(Address),
    Other(Symbol),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceData {
    pub price: i128,
    pub timestamp: u64,
}

#[contractclient(name = "Sep40Client")]
pub trait Sep40 {
    fn decimals(env: Env) -> u32;
    fn lastprice(env: Env, asset: Asset) -> Option<PriceData>;
}

// ----- registrar interface (same XDR shape as compliance) -----

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Eligibility {
    pub registered: bool,
    pub kyc_valid: bool,
    pub jurisdiction_ok: bool,
    pub frozen: bool,
    pub can_receive: bool,
    pub can_send: bool,
    pub can_redeem: bool,
}

#[contractclient(name = "RegistrarClient")]
pub trait Registrar {
    fn status(env: Env, addr: Address) -> Eligibility;
    fn balance(env: Env, addr: Address) -> i128;
    fn locked(env: Env, addr: Address) -> i128;
    fn is_cash_address(env: Env, holder: Address, addr: Address) -> bool;
    fn issue(env: Env, to: Address, amount: i128);
    fn lock(env: Env, holder: Address, amount: i128, cash_to: Address);
    fn unlock(env: Env, holder: Address, amount: i128);
    fn redeem_burn(env: Env, holder: Address, amount: i128);
}

// ----- vault types -----

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    pub ops: Address,
    pub compliance: Address,
    pub share: Address,
    pub cash: Address,
    pub treasury: Address,
    pub oracle: Address,
    pub oracle_asset: Asset,
    pub nav_decimals: u32,
    pub initial_nav: i128,
    pub min_subscription: i128,
    pub max_strike_delay: u64,
    pub max_nav_move_bps: u32,
    pub max_requests_per_epoch: u32,
}

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EpochStatus {
    Open,
    Struck,
    Settling,
    Settled,
    Aborted,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Epoch {
    pub id: u32,
    pub cutoff: u64,
    pub status: EpochStatus,
    pub nav: i128,
    pub nav_ts: u64,
    pub sub_total: i128,
    pub redeem_shares_total: i128,
    pub liquidity: i128,
    pub cursor: u32,
    pub claimable_cash: i128,
}

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Kind {
    Subscribe,
    Redeem,
}

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ReqStatus {
    Pending,
    Cancelled,
    Claimable,
    Claimed,
}

/// Rejection codes stored on a request refunded at settlement.
pub const REJECT_NONE: u32 = 0;
pub const REJECT_KYC_EXPIRED: u32 = 4;
pub const REJECT_JURISDICTION: u32 = 5;
pub const REJECT_FROZEN: u32 = 6;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Request {
    pub id: u64,
    pub epoch: u32,
    pub investor: Address,
    pub kind: Kind,
    /// Cash for a subscription, shares for a redemption.
    pub amount: i128,
    pub cash_to: Option<Address>,
    pub status: ReqStatus,
    pub shares_out: i128,
    pub cash_out: i128,
    pub reject: u32,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Config,
    CurrentEpoch,
    NextReqId,
    Paused,
    LastNav,
    Epoch(u32),
    Req(u64),
    Queue(u32),
}

// ----- events -----

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Requested {
    #[topic]
    pub id: u64,
    #[topic]
    pub epoch: u32,
    pub investor: Address,
    pub kind: Kind,
    pub amount: i128,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cancelled {
    #[topic]
    pub id: u64,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Struck {
    #[topic]
    pub epoch: u32,
    pub nav: i128,
    pub nav_ts: u64,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NavOverride {
    #[topic]
    pub epoch: u32,
    pub nav: i128,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Settled {
    #[topic]
    pub epoch: u32,
    pub processed: u32,
    pub surplus: i128,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Claimed {
    #[topic]
    pub id: u64,
    pub shares_out: i128,
    pub cash_out: i128,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Aborted {
    #[topic]
    pub epoch: u32,
    pub reason: BytesN<32>,
}

#[contract]
pub struct AsyncVault;

// ---------------------------------------------------------------------------
// storage helpers
// ---------------------------------------------------------------------------

fn bump(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_EXTEND_TO);
}

fn cfg(env: &Env) -> Config {
    env.storage().instance().get(&DataKey::Config).unwrap()
}

fn put<V: soroban_sdk::IntoVal<Env, soroban_sdk::Val>>(env: &Env, key: &DataKey, v: &V) {
    let p = env.storage().persistent();
    p.set(key, v);
    p.extend_ttl(key, RECORD_TTL_THRESHOLD, RECORD_TTL_EXTEND_TO);
}

fn load_epoch(env: &Env, id: u32) -> Result<Epoch, VaultError> {
    env.storage()
        .persistent()
        .get(&DataKey::Epoch(id))
        .ok_or(VaultError::WrongEpochState)
}

fn load_req(env: &Env, id: u64) -> Result<Request, VaultError> {
    env.storage()
        .persistent()
        .get(&DataKey::Req(id))
        .ok_or(VaultError::RequestNotFound)
}

fn load_queue(env: &Env, epoch: u32) -> Vec<u64> {
    env.storage()
        .persistent()
        .get(&DataKey::Queue(epoch))
        .unwrap_or(Vec::new(env))
}

fn is_paused(env: &Env) -> bool {
    env.storage().instance().get(&DataKey::Paused).unwrap_or(false)
}

fn last_nav(env: &Env) -> i128 {
    env.storage().instance().get(&DataKey::LastNav).unwrap()
}

fn add(a: i128, b: i128) -> Result<i128, VaultError> {
    a.checked_add(b).ok_or(VaultError::MathOverflow)
}

fn sub(a: i128, b: i128) -> Result<i128, VaultError> {
    a.checked_sub(b).ok_or(VaultError::MathOverflow)
}

/// The epoch currently accepting requests, checked against cut-off.
fn open_epoch_for_requests(env: &Env) -> Result<Epoch, VaultError> {
    if is_paused(env) {
        return Err(VaultError::Paused);
    }
    let id: u32 = env.storage().instance().get(&DataKey::CurrentEpoch).unwrap_or(0);
    if id == 0 {
        return Err(VaultError::NoOpenEpoch);
    }
    let e = load_epoch(env, id)?;
    if e.status != EpochStatus::Open {
        return Err(VaultError::NoOpenEpoch);
    }
    if env.ledger().timestamp() >= e.cutoff {
        return Err(VaultError::CutoffPassed);
    }
    Ok(e)
}

fn enqueue(env: &Env, c: &Config, e: &Epoch, req_id: u64) -> Result<(), VaultError> {
    let mut q = load_queue(env, e.id);
    if q.len() >= c.max_requests_per_epoch {
        return Err(VaultError::EpochFull);
    }
    q.push_back(req_id);
    put(env, &DataKey::Queue(e.id), &q);
    Ok(())
}

fn next_req_id(env: &Env) -> u64 {
    let id: u64 = env.storage().instance().get(&DataKey::NextReqId).unwrap_or(1);
    env.storage().instance().set(&DataKey::NextReqId, &(id + 1));
    id
}

fn reject_code(st: &Eligibility) -> u32 {
    if st.frozen {
        REJECT_FROZEN
    } else if !st.registered || !st.kyc_valid {
        REJECT_KYC_EXPIRED
    } else if !st.jurisdiction_ok {
        REJECT_JURISDICTION
    } else {
        REJECT_NONE
    }
}

/// Shared strike logic; `band` is false for the co-signed override.
fn strike(env: &Env, epoch: u32, band: bool) -> Result<Epoch, VaultError> {
    let c = cfg(env);
    c.ops.require_auth();
    let mut e = load_epoch(env, epoch)?;
    if e.status != EpochStatus::Open {
        return Err(VaultError::WrongEpochState);
    }
    if env.ledger().timestamp() < e.cutoff {
        return Err(VaultError::EpochNotClosed);
    }
    let p = Sep40Client::new(env, &c.oracle)
        .lastprice(&c.oracle_asset)
        .ok_or(VaultError::PriceMissing)?;
    if p.timestamp < e.cutoff {
        return Err(VaultError::StalePrice);
    }
    if p.timestamp > e.cutoff.checked_add(c.max_strike_delay).ok_or(VaultError::MathOverflow)? {
        return Err(VaultError::PriceTooLate);
    }
    if p.price <= 0 {
        return Err(VaultError::NavNonPositive);
    }
    if band
        && math::move_exceeds_band(last_nav(env), p.price, c.max_nav_move_bps)
            .ok_or(VaultError::MathOverflow)?
    {
        return Err(VaultError::NavMoveTooLarge);
    }
    e.nav = p.price;
    e.nav_ts = p.timestamp;
    e.status = EpochStatus::Struck;
    put(env, &DataKey::Epoch(epoch), &e);
    bump(env);
    Struck {
        epoch,
        nav: e.nav,
        nav_ts: e.nav_ts,
    }
    .publish(env);
    Ok(e)
}

// ---------------------------------------------------------------------------
// contract
// ---------------------------------------------------------------------------

#[contractimpl]
impl AsyncVault {
    pub fn __constructor(env: Env, cfg: Config) -> Result<(), VaultError> {
        let d = Sep40Client::new(&env, &cfg.oracle).decimals();
        if d != cfg.nav_decimals {
            return Err(VaultError::OracleDecimalsMismatch);
        }
        if cfg.initial_nav <= 0 {
            return Err(VaultError::NavNonPositive);
        }
        if cfg.min_subscription <= 0 || cfg.max_requests_per_epoch == 0 {
            return Err(VaultError::InvalidAmount);
        }
        let s = env.storage().instance();
        s.set(&DataKey::LastNav, &cfg.initial_nav);
        s.set(&DataKey::Config, &cfg);
        s.set(&DataKey::CurrentEpoch, &0u32);
        s.set(&DataKey::NextReqId, &1u64);
        s.set(&DataKey::Paused, &false);
        bump(&env);
        Ok(())
    }

    /// Open the next epoch. Ops (TA policy).
    pub fn open_epoch(env: Env, cutoff: u64) -> Result<u32, VaultError> {
        let c = cfg(&env);
        c.ops.require_auth();
        let now = env.ledger().timestamp();
        if cutoff <= now {
            return Err(VaultError::CutoffInPast);
        }
        let cur: u32 = env.storage().instance().get(&DataKey::CurrentEpoch).unwrap_or(0);
        if cur > 0 {
            let prev = load_epoch(&env, cur)?;
            if now < prev.cutoff {
                return Err(VaultError::PreviousEpochOpen);
            }
        }
        let id = cur + 1;
        let e = Epoch {
            id,
            cutoff,
            status: EpochStatus::Open,
            nav: 0,
            nav_ts: 0,
            sub_total: 0,
            redeem_shares_total: 0,
            liquidity: 0,
            cursor: 0,
            claimable_cash: 0,
        };
        put(&env, &DataKey::Epoch(id), &e);
        put(&env, &DataKey::Queue(id), &Vec::<u64>::new(&env));
        env.storage().instance().set(&DataKey::CurrentEpoch, &id);
        bump(&env);
        Ok(id)
    }

    /// Queue a subscription and escrow its cash. Investor auth.
    pub fn request_subscribe(env: Env, investor: Address, amount: i128) -> Result<u64, VaultError> {
        investor.require_auth();
        let c = cfg(&env);
        let mut e = open_epoch_for_requests(&env)?;
        if amount < c.min_subscription {
            return Err(VaultError::BelowMinimum);
        }
        let st = RegistrarClient::new(&env, &c.compliance).status(&investor);
        if !st.registered {
            return Err(VaultError::NotRegistered);
        }
        if st.frozen {
            return Err(VaultError::Frozen);
        }
        if !st.kyc_valid {
            return Err(VaultError::KycExpired);
        }
        if !st.jurisdiction_ok {
            return Err(VaultError::JurisdictionBlocked);
        }
        let id = next_req_id(&env);
        enqueue(&env, &c, &e, id)?;
        TokenClient::new(&env, &c.cash).transfer(&investor, &env.current_contract_address(), &amount);
        e.sub_total = add(e.sub_total, amount)?;
        put(&env, &DataKey::Epoch(e.id), &e);
        put(
            &env,
            &DataKey::Req(id),
            &Request {
                id,
                epoch: e.id,
                investor: investor.clone(),
                kind: Kind::Subscribe,
                amount,
                cash_to: None,
                status: ReqStatus::Pending,
                shares_out: 0,
                cash_out: 0,
                reject: REJECT_NONE,
            },
        );
        bump(&env);
        Requested {
            id,
            epoch: e.id,
            investor,
            kind: Kind::Subscribe,
            amount,
        }
        .publish(&env);
        Ok(id)
    }

    /// Queue a redemption and lock the shares. Investor auth. Allowed for a
    /// holder whose KYC expired or whose jurisdiction is blocked; not for a
    /// frozen holder. Cash goes to one of the holder's cash addresses.
    pub fn request_redeem(
        env: Env,
        investor: Address,
        shares: i128,
        cash_to: Address,
    ) -> Result<u64, VaultError> {
        investor.require_auth();
        let c = cfg(&env);
        let mut e = open_epoch_for_requests(&env)?;
        if shares <= 0 {
            return Err(VaultError::InvalidAmount);
        }
        let reg = RegistrarClient::new(&env, &c.compliance);
        let st = reg.status(&investor);
        if !st.registered {
            return Err(VaultError::NotRegistered);
        }
        if st.frozen {
            return Err(VaultError::Frozen);
        }
        if !reg.is_cash_address(&investor, &cash_to) {
            return Err(VaultError::CashAddressNotAllowed);
        }
        let free = sub(reg.balance(&investor), reg.locked(&investor))?;
        if shares > free {
            return Err(VaultError::InsufficientShares);
        }
        let id = next_req_id(&env);
        enqueue(&env, &c, &e, id)?;
        reg.lock(&investor, &shares, &cash_to);
        e.redeem_shares_total = add(e.redeem_shares_total, shares)?;
        put(&env, &DataKey::Epoch(e.id), &e);
        put(
            &env,
            &DataKey::Req(id),
            &Request {
                id,
                epoch: e.id,
                investor: investor.clone(),
                kind: Kind::Redeem,
                amount: shares,
                cash_to: Some(cash_to),
                status: ReqStatus::Pending,
                shares_out: 0,
                cash_out: 0,
                reject: REJECT_NONE,
            },
        );
        bump(&env);
        Requested {
            id,
            epoch: e.id,
            investor,
            kind: Kind::Redeem,
            amount: shares,
        }
        .publish(&env);
        Ok(id)
    }

    /// Cancel a pending request before its epoch's cut-off. Investor auth.
    pub fn cancel(env: Env, investor: Address, request_id: u64) -> Result<(), VaultError> {
        investor.require_auth();
        let c = cfg(&env);
        let mut r = load_req(&env, request_id)?;
        if r.investor != investor {
            return Err(VaultError::NotRequestOwner);
        }
        if r.status != ReqStatus::Pending {
            return Err(VaultError::NotPending);
        }
        let mut e = load_epoch(&env, r.epoch)?;
        if env.ledger().timestamp() >= e.cutoff {
            return Err(VaultError::CutoffPassed);
        }
        match r.kind {
            Kind::Subscribe => {
                TokenClient::new(&env, &c.cash).transfer(&env.current_contract_address(), &investor, &r.amount);
                e.sub_total = sub(e.sub_total, r.amount)?;
            }
            Kind::Redeem => {
                RegistrarClient::new(&env, &c.compliance).unlock(&investor, &r.amount);
                e.redeem_shares_total = sub(e.redeem_shares_total, r.amount)?;
            }
        }
        r.status = ReqStatus::Cancelled;
        put(&env, &DataKey::Req(request_id), &r);
        put(&env, &DataKey::Epoch(e.id), &e);
        bump(&env);
        Cancelled { id: request_id }.publish(&env);
        Ok(())
    }

    /// Strike the epoch's NAV from the oracle. Ops (ADMIN policy).
    pub fn strike_nav(env: Env, epoch: u32) -> Result<i128, VaultError> {
        Ok(strike(&env, epoch, true)?.nav)
    }

    /// Strike without the move band. Ops (TA + ADMIN policy).
    pub fn strike_nav_override(env: Env, epoch: u32) -> Result<i128, VaultError> {
        let e = strike(&env, epoch, false)?;
        NavOverride { epoch, nav: e.nav }.publish(&env);
        Ok(e.nav)
    }

    /// Treasury tops up redemption liquidity for a struck epoch.
    pub fn deposit_liquidity(env: Env, epoch: u32, amount: i128) -> Result<(), VaultError> {
        let c = cfg(&env);
        c.treasury.require_auth();
        if amount <= 0 {
            return Err(VaultError::InvalidAmount);
        }
        let mut e = load_epoch(&env, epoch)?;
        if e.status != EpochStatus::Struck && e.status != EpochStatus::Settling {
            return Err(VaultError::WrongEpochState);
        }
        TokenClient::new(&env, &c.cash).transfer(&c.treasury, &env.current_contract_address(), &amount);
        e.liquidity = add(e.liquidity, amount)?;
        put(&env, &DataKey::Epoch(epoch), &e);
        bump(&env);
        Ok(())
    }

    /// Settle up to `max_items` queued requests at the struck NAV. Ops (TA
    /// policy). Returns (processed, remaining). The call that exhausts the
    /// queue sweeps the surplus to the treasury, or reverts with
    /// `InsufficientLiquidity` if the cash on hand cannot cover redemptions.
    pub fn settle(env: Env, epoch: u32, max_items: u32) -> Result<(u32, u32), VaultError> {
        let c = cfg(&env);
        c.ops.require_auth();
        if max_items == 0 {
            return Err(VaultError::InvalidAmount);
        }
        let mut e = load_epoch(&env, epoch)?;
        if e.status != EpochStatus::Struck && e.status != EpochStatus::Settling {
            return Err(VaultError::WrongEpochState);
        }
        let q = load_queue(&env, epoch);
        let len = q.len();
        let end = core::cmp::min(len, e.cursor.saturating_add(max_items));
        let reg = RegistrarClient::new(&env, &c.compliance);
        let mut processed = 0u32;
        for i in e.cursor..end {
            let id = q.get(i).unwrap();
            let mut r = load_req(&env, id)?;
            processed += 1;
            if r.status != ReqStatus::Pending {
                continue;
            }
            match r.kind {
                Kind::Subscribe => {
                    let st = reg.status(&r.investor);
                    if !st.can_receive {
                        r.cash_out = r.amount;
                        r.reject = reject_code(&st);
                    } else {
                        let (shares, dust) = math::shares_for_cash(r.amount, e.nav, c.nav_decimals)
                            .ok_or(VaultError::MathOverflow)?;
                        r.shares_out = shares;
                        r.cash_out = dust;
                    }
                }
                Kind::Redeem => {
                    r.cash_out = math::cash_for_shares(r.amount, e.nav, c.nav_decimals)
                        .ok_or(VaultError::MathOverflow)?;
                    reg.redeem_burn(&r.investor, &r.amount);
                }
            }
            e.claimable_cash = add(e.claimable_cash, r.cash_out)?;
            r.status = ReqStatus::Claimable;
            put(&env, &DataKey::Req(id), &r);
        }
        e.cursor = end;
        let remaining = len - end;
        if remaining == 0 {
            let surplus = sub(add(e.sub_total, e.liquidity)?, e.claimable_cash)?;
            if surplus < 0 {
                return Err(VaultError::InsufficientLiquidity);
            }
            if surplus > 0 {
                TokenClient::new(&env, &c.cash).transfer(&env.current_contract_address(), &c.treasury, &surplus);
            }
            env.storage().instance().set(&DataKey::LastNav, &e.nav);
            e.status = EpochStatus::Settled;
            put(&env, &DataKey::Epoch(epoch), &e);
            Settled {
                epoch,
                processed,
                surplus,
            }
            .publish(&env);
        } else {
            e.status = EpochStatus::Settling;
            put(&env, &DataKey::Epoch(epoch), &e);
        }
        bump(&env);
        Ok((processed, remaining))
    }

    /// Claim a settled (or aborted) request: mint shares and/or pay cash.
    /// `caller` is the investor, or the ops account pushing on their behalf.
    pub fn claim(env: Env, caller: Address, request_id: u64) -> Result<(), VaultError> {
        caller.require_auth();
        let c = cfg(&env);
        let mut r = load_req(&env, request_id)?;
        if caller != r.investor && caller != c.ops {
            return Err(VaultError::NotRequestOwner);
        }
        let e = load_epoch(&env, r.epoch)?;
        if e.status != EpochStatus::Settled && e.status != EpochStatus::Aborted {
            return Err(VaultError::EpochNotSettled);
        }
        match r.status {
            ReqStatus::Claimed => return Err(VaultError::AlreadyClaimed),
            ReqStatus::Claimable => {}
            _ => return Err(VaultError::NotClaimable),
        }
        r.status = ReqStatus::Claimed;
        put(&env, &DataKey::Req(request_id), &r);
        if r.shares_out > 0 {
            RegistrarClient::new(&env, &c.compliance).issue(&r.investor, &r.shares_out);
        }
        if r.cash_out > 0 {
            let to = match (&r.kind, &r.cash_to) {
                (Kind::Redeem, Some(a)) => a.clone(),
                _ => r.investor.clone(),
            };
            TokenClient::new(&env, &c.cash).transfer(&env.current_contract_address(), &to, &r.cash_out);
        }
        bump(&env);
        Claimed {
            id: request_id,
            shares_out: r.shares_out,
            cash_out: r.cash_out,
        }
        .publish(&env);
        Ok(())
    }

    /// Abort an epoch whose NAV was never struck within the strike window:
    /// subscriptions become fully refundable, redemptions are unlocked.
    /// Paged like `settle`. Ops (TA + ADMIN policy).
    pub fn abort_epoch(
        env: Env,
        epoch: u32,
        reason: BytesN<32>,
        max_items: u32,
    ) -> Result<(u32, u32), VaultError> {
        let c = cfg(&env);
        c.ops.require_auth();
        if max_items == 0 {
            return Err(VaultError::InvalidAmount);
        }
        let mut e = load_epoch(&env, epoch)?;
        let q = load_queue(&env, epoch);
        let len = q.len();
        match e.status {
            EpochStatus::Open => {
                let window_end = e
                    .cutoff
                    .checked_add(c.max_strike_delay)
                    .ok_or(VaultError::MathOverflow)?;
                if env.ledger().timestamp() <= window_end {
                    return Err(VaultError::AbortTooEarly);
                }
                e.status = EpochStatus::Aborted;
                Aborted {
                    epoch,
                    reason: reason.clone(),
                }
                .publish(&env);
            }
            EpochStatus::Aborted if e.cursor < len => {}
            _ => return Err(VaultError::WrongEpochState),
        }
        let end = core::cmp::min(len, e.cursor.saturating_add(max_items));
        let reg = RegistrarClient::new(&env, &c.compliance);
        let mut processed = 0u32;
        for i in e.cursor..end {
            let id = q.get(i).unwrap();
            let mut r = load_req(&env, id)?;
            processed += 1;
            if r.status != ReqStatus::Pending {
                continue;
            }
            match r.kind {
                Kind::Subscribe => {
                    r.cash_out = r.amount;
                    r.status = ReqStatus::Claimable;
                    e.claimable_cash = add(e.claimable_cash, r.amount)?;
                }
                Kind::Redeem => {
                    reg.unlock(&r.investor, &r.amount);
                    r.status = ReqStatus::Cancelled;
                }
            }
            put(&env, &DataKey::Req(id), &r);
        }
        e.cursor = end;
        put(&env, &DataKey::Epoch(epoch), &e);
        bump(&env);
        Ok((processed, len - end))
    }

    /// Block new requests (cancel, settle and claim keep working). Ops.
    pub fn pause(env: Env) {
        cfg(&env).ops.require_auth();
        env.storage().instance().set(&DataKey::Paused, &true);
        bump(&env);
    }

    /// Resume requests. Ops (TA + ADMIN policy).
    pub fn unpause(env: Env) {
        cfg(&env).ops.require_auth();
        env.storage().instance().set(&DataKey::Paused, &false);
        bump(&env);
    }

    // ----- views -----

    pub fn config(env: Env) -> Config {
        cfg(&env)
    }

    pub fn epoch(env: Env, id: u32) -> Result<Epoch, VaultError> {
        load_epoch(&env, id)
    }

    pub fn request(env: Env, id: u64) -> Result<Request, VaultError> {
        load_req(&env, id)
    }

    pub fn queue(env: Env, epoch: u32, start: u32, limit: u32) -> Vec<Request> {
        let q = load_queue(&env, epoch);
        let mut out = Vec::new(&env);
        let end = core::cmp::min(q.len(), start.saturating_add(limit));
        for i in start..end {
            if let Some(r) = env.storage().persistent().get(&DataKey::Req(q.get(i).unwrap())) {
                out.push_back(r);
            }
        }
        out
    }

    pub fn queue_len(env: Env, epoch: u32) -> u32 {
        load_queue(&env, epoch).len()
    }

    pub fn current_epoch(env: Env) -> u32 {
        env.storage().instance().get(&DataKey::CurrentEpoch).unwrap_or(0)
    }

    pub fn last_nav(env: Env) -> i128 {
        last_nav(&env)
    }

    pub fn paused(env: Env) -> bool {
        is_paused(&env)
    }
}

#[cfg(test)]
mod test;
#[cfg(test)]
mod test_scenario;
