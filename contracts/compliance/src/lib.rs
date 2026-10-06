//! Compliance registrar: the investor register of a tokenised fund and the
//! admin of the share asset's Stellar Asset Contract (SAC).
//!
//! The share asset is a classic asset whose issuer sets AUTH_REQUIRED,
//! AUTH_REVOCABLE and AUTH_CLAWBACK_ENABLED **before any balance exists** and
//! then hands SAC admin to this contract (`set_admin(compliance)`). Every
//! holder balance therefore sits deauthorised at rest: neither the holder nor
//! any other contract can move shares through the SAC directly. The only way
//! shares move is through this registrar, which checks eligibility at the
//! ledger time of the transaction and performs a SEP-8-style sandwich:
//!
//! ```text
//! set_authorized(from, true) -> set_authorized(to, true)
//!   -> transfer(from, to, amount)
//! -> set_authorized(from, false) -> set_authorized(to, false)
//! ```
//!
//! Minting (`issue`), burning (`redeem_burn`, a SAC clawback, which works on
//! deauthorised balances) and the redemption lock are reserved to the bound
//! `async_vault`. Before any balance changes, the registrar notifies the bound
//! `distribution` contract so per-share entitlements stay exact.
#![no_std]

use soroban_sdk::{
    contract, contractclient, contracterror, contractevent, contractimpl, contracttype,
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env, Symbol, Vec,
};

const LEDGERS_PER_DAY: u32 = 17_280;
const INSTANCE_TTL_THRESHOLD: u32 = LEDGERS_PER_DAY * 30;
const INSTANCE_TTL_EXTEND_TO: u32 = LEDGERS_PER_DAY * 120;
const RECORD_TTL_THRESHOLD: u32 = LEDGERS_PER_DAY * 60;
const RECORD_TTL_EXTEND_TO: u32 = LEDGERS_PER_DAY * 365;

/// Tag of a small symbol in a `Val` (soroban-env-common `Tag::SymbolSmall`).
const TAG_SYMBOL_SMALL: u64 = 14;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ComplianceError {
    AlreadyBound = 1,
    NotBound = 2,
    NotRegistered = 3,
    KycExpired = 4,
    JurisdictionBlocked = 5,
    Frozen = 6,
    InsufficientUnlocked = 7,
    CashAddressNotAllowed = 8,
    InvalidAmount = 9,
    TooManyCashAddresses = 10,
    SameAddress = 11,
    BadJurisdiction = 12,
    MathOverflow = 13,
}

/// One investor record. `jurisdiction` is an ISO-3166 alpha-2 code in upper
/// case; `investor_type` is 0 for an individual and 1 for an entity;
/// `cash_addresses` holds 1 to 3 addresses redemption and distribution cash
/// may be paid to.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Investor {
    pub kyc_expiry: u64,
    pub jurisdiction: Symbol,
    pub investor_type: u32,
    pub cash_addresses: Vec<Address>,
    pub frozen: bool,
}

/// Eligibility of an address at the current ledger time.
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

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Ops,
    Share,
    Vault,
    Distribution,
    TotalShares,
    Inv(Address),
    Locked(Address),
    Juris(Symbol),
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvestorSet {
    #[topic]
    pub investor: Address,
    pub kyc_expiry: u64,
    pub jurisdiction: Symbol,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frozen {
    #[topic]
    pub investor: Address,
    pub frozen: bool,
    pub reason: BytesN<32>,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Transferred {
    #[topic]
    pub from: Address,
    #[topic]
    pub to: Address,
    pub amount: i128,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Forced {
    #[topic]
    pub from: Address,
    #[topic]
    pub to: Address,
    pub amount: i128,
    pub reason: BytesN<32>,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Issued {
    #[topic]
    pub to: Address,
    pub amount: i128,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Burned {
    #[topic]
    pub holder: Address,
    pub amount: i128,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JurisdictionSet {
    #[topic]
    pub code: Symbol,
    pub allowed: bool,
}

/// The hook `distribution` exposes to the registrar.
#[contractclient(name = "DistributionHookClient")]
pub trait DistributionHook {
    fn on_change(env: Env, holder: Address, old_balance: i128);
}

#[contract]
pub struct Compliance;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn bump(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_EXTEND_TO);
}

fn touch(env: &Env, key: &DataKey) {
    let p = env.storage().persistent();
    if p.has(key) {
        p.extend_ttl(key, RECORD_TTL_THRESHOLD, RECORD_TTL_EXTEND_TO);
    }
}

fn ops(env: &Env) -> Address {
    env.storage().instance().get(&DataKey::Ops).unwrap()
}

fn share(env: &Env) -> Address {
    env.storage().instance().get(&DataKey::Share).unwrap()
}

fn vault(env: &Env) -> Result<Address, ComplianceError> {
    env.storage()
        .instance()
        .get(&DataKey::Vault)
        .ok_or(ComplianceError::NotBound)
}

fn distribution(env: &Env) -> Result<Address, ComplianceError> {
    env.storage()
        .instance()
        .get(&DataKey::Distribution)
        .ok_or(ComplianceError::NotBound)
}

fn read_investor(env: &Env, addr: &Address) -> Option<Investor> {
    let key = DataKey::Inv(addr.clone());
    let rec = env.storage().persistent().get(&key);
    if rec.is_some() {
        touch(env, &key);
    }
    rec
}

fn read_locked(env: &Env, addr: &Address) -> i128 {
    let key = DataKey::Locked(addr.clone());
    let v = env.storage().persistent().get(&key).unwrap_or(0);
    touch(env, &key);
    v
}

fn write_locked(env: &Env, addr: &Address, v: i128) {
    let key = DataKey::Locked(addr.clone());
    let p = env.storage().persistent();
    p.set(&key, &v);
    p.extend_ttl(&key, RECORD_TTL_THRESHOLD, RECORD_TTL_EXTEND_TO);
}

fn total_shares(env: &Env) -> i128 {
    env.storage().instance().get(&DataKey::TotalShares).unwrap_or(0)
}

fn jurisdiction_allowed(env: &Env, code: &Symbol) -> bool {
    env.storage()
        .persistent()
        .get(&DataKey::Juris(code.clone()))
        .unwrap_or(false)
}

/// True when `code` is exactly two upper-case ASCII letters.
fn is_alpha2(code: &Symbol) -> bool {
    let payload = code.to_val().get_payload();
    if payload & 0xff != TAG_SYMBOL_SMALL {
        return false;
    }
    let body = payload >> 8;
    if body >> 12 != 0 {
        return false;
    }
    let c1 = (body >> 6) & 63;
    let c2 = body & 63;
    // 'A'..='Z' are encoded as 12..=37 in a small symbol.
    (12..=37).contains(&c1) && (12..=37).contains(&c2)
}

fn eligibility(env: &Env, addr: &Address) -> Eligibility {
    match read_investor(env, addr) {
        None => Eligibility {
            registered: false,
            kyc_valid: false,
            jurisdiction_ok: false,
            frozen: false,
            can_receive: false,
            can_send: false,
            can_redeem: false,
        },
        Some(inv) => {
            let now = env.ledger().timestamp();
            let kyc_valid = inv.kyc_expiry > now;
            let jurisdiction_ok = jurisdiction_allowed(env, &inv.jurisdiction);
            let frozen = inv.frozen;
            Eligibility {
                registered: true,
                kyc_valid,
                jurisdiction_ok,
                frozen,
                can_receive: !frozen && kyc_valid && jurisdiction_ok,
                can_send: !frozen && kyc_valid,
                can_redeem: !frozen,
            }
        }
    }
}

fn require_receive(e: &Eligibility) -> Result<(), ComplianceError> {
    if !e.registered {
        return Err(ComplianceError::NotRegistered);
    }
    if e.frozen {
        return Err(ComplianceError::Frozen);
    }
    if !e.kyc_valid {
        return Err(ComplianceError::KycExpired);
    }
    if !e.jurisdiction_ok {
        return Err(ComplianceError::JurisdictionBlocked);
    }
    Ok(())
}

fn require_send(e: &Eligibility) -> Result<(), ComplianceError> {
    if !e.registered {
        return Err(ComplianceError::NotRegistered);
    }
    if e.frozen {
        return Err(ComplianceError::Frozen);
    }
    if !e.kyc_valid {
        return Err(ComplianceError::KycExpired);
    }
    Ok(())
}

fn require_redeem(e: &Eligibility) -> Result<(), ComplianceError> {
    if !e.registered {
        return Err(ComplianceError::NotRegistered);
    }
    if e.frozen {
        return Err(ComplianceError::Frozen);
    }
    Ok(())
}

fn sac_balance(env: &Env, addr: &Address) -> i128 {
    TokenClient::new(env, &share(env)).balance(addr)
}

fn unlocked(env: &Env, addr: &Address) -> Result<i128, ComplianceError> {
    sac_balance(env, addr)
        .checked_sub(read_locked(env, addr))
        .ok_or(ComplianceError::MathOverflow)
}

fn notify(env: &Env, holder: &Address, old_balance: i128) -> Result<(), ComplianceError> {
    DistributionHookClient::new(env, &distribution(env)?).on_change(holder, &old_balance);
    Ok(())
}

/// Authorise, mint, deauthorise.
fn sandwich_mint(env: &Env, to: &Address, amount: i128) {
    let sac = StellarAssetClient::new(env, &share(env));
    sac.set_authorized(to, &true);
    sac.mint(to, &amount);
    sac.set_authorized(to, &false);
}

fn is_cash(inv: &Investor, addr: &Address) -> bool {
    inv.cash_addresses.iter().any(|a| a == *addr)
}

// ---------------------------------------------------------------------------
// contract
// ---------------------------------------------------------------------------

#[contractimpl]
impl Compliance {
    pub fn __constructor(env: Env, ops: Address, share: Address) {
        let s = env.storage().instance();
        s.set(&DataKey::Ops, &ops);
        s.set(&DataKey::Share, &share);
        s.set(&DataKey::TotalShares, &0i128);
        bump(&env);
    }

    /// Bind the vault and distribution contracts. Ops (TA + ADMIN), once.
    pub fn bind(env: Env, vault: Address, distribution: Address) -> Result<(), ComplianceError> {
        ops(&env).require_auth();
        let s = env.storage().instance();
        if s.has(&DataKey::Vault) {
            return Err(ComplianceError::AlreadyBound);
        }
        s.set(&DataKey::Vault, &vault);
        s.set(&DataKey::Distribution, &distribution);
        bump(&env);
        Ok(())
    }

    /// Register or update an investor. Ops (TA policy).
    pub fn set_investor(env: Env, investor: Address, rec: Investor) -> Result<(), ComplianceError> {
        ops(&env).require_auth();
        let n = rec.cash_addresses.len();
        if n == 0 || n > 3 {
            return Err(ComplianceError::TooManyCashAddresses);
        }
        if !is_alpha2(&rec.jurisdiction) {
            return Err(ComplianceError::BadJurisdiction);
        }
        if rec.investor_type > 1 {
            return Err(ComplianceError::InvalidAmount);
        }
        let key = DataKey::Inv(investor.clone());
        let p = env.storage().persistent();
        p.set(&key, &rec);
        p.extend_ttl(&key, RECORD_TTL_THRESHOLD, RECORD_TTL_EXTEND_TO);
        bump(&env);
        InvestorSet {
            investor,
            kyc_expiry: rec.kyc_expiry,
            jurisdiction: rec.jurisdiction,
        }
        .publish(&env);
        Ok(())
    }

    /// Allow or block a jurisdiction (default: blocked). Ops (TA + ADMIN).
    pub fn set_jurisdiction(env: Env, code: Symbol, allowed: bool) -> Result<(), ComplianceError> {
        ops(&env).require_auth();
        if !is_alpha2(&code) {
            return Err(ComplianceError::BadJurisdiction);
        }
        let key = DataKey::Juris(code.clone());
        let p = env.storage().persistent();
        p.set(&key, &allowed);
        p.extend_ttl(&key, RECORD_TTL_THRESHOLD, RECORD_TTL_EXTEND_TO);
        bump(&env);
        JurisdictionSet { code, allowed }.publish(&env);
        Ok(())
    }

    /// Freeze or unfreeze an investor; `reason` is the hash of the TA's note.
    pub fn set_frozen(
        env: Env,
        investor: Address,
        frozen: bool,
        reason: BytesN<32>,
    ) -> Result<(), ComplianceError> {
        ops(&env).require_auth();
        let mut rec = read_investor(&env, &investor).ok_or(ComplianceError::NotRegistered)?;
        rec.frozen = frozen;
        let key = DataKey::Inv(investor.clone());
        env.storage().persistent().set(&key, &rec);
        touch(&env, &key);
        bump(&env);
        Frozen {
            investor,
            frozen,
            reason,
        }
        .publish(&env);
        Ok(())
    }

    /// Holder-to-holder transfer through the registrar.
    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) -> Result<(), ComplianceError> {
        from.require_auth();
        if amount <= 0 {
            return Err(ComplianceError::InvalidAmount);
        }
        if from == to {
            return Err(ComplianceError::SameAddress);
        }
        require_send(&eligibility(&env, &from))?;
        require_receive(&eligibility(&env, &to))?;
        let from_bal = sac_balance(&env, &from);
        let free = from_bal
            .checked_sub(read_locked(&env, &from))
            .ok_or(ComplianceError::MathOverflow)?;
        if amount > free {
            return Err(ComplianceError::InsufficientUnlocked);
        }
        let to_bal = sac_balance(&env, &to);
        notify(&env, &from, from_bal)?;
        notify(&env, &to, to_bal)?;
        let sac_addr = share(&env);
        let sac = StellarAssetClient::new(&env, &sac_addr);
        sac.set_authorized(&from, &true);
        sac.set_authorized(&to, &true);
        TokenClient::new(&env, &sac_addr).transfer(&from, &to, &amount);
        sac.set_authorized(&from, &false);
        sac.set_authorized(&to, &false);
        bump(&env);
        Transferred { from, to, amount }.publish(&env);
        Ok(())
    }

    /// Mint shares to a registered, unfrozen holder. Vault only.
    pub fn issue(env: Env, to: Address, amount: i128) -> Result<(), ComplianceError> {
        vault(&env)?.require_auth();
        if amount <= 0 {
            return Err(ComplianceError::InvalidAmount);
        }
        let e = eligibility(&env, &to);
        require_redeem(&e)?; // registered and not frozen
        notify(&env, &to, sac_balance(&env, &to))?;
        sandwich_mint(&env, &to, amount);
        let total = total_shares(&env)
            .checked_add(amount)
            .ok_or(ComplianceError::MathOverflow)?;
        env.storage().instance().set(&DataKey::TotalShares, &total);
        bump(&env);
        Issued { to, amount }.publish(&env);
        Ok(())
    }

    /// Lock shares for a redemption request. Vault only. Expired KYC and a
    /// blocked jurisdiction do not stop a redemption; a freeze does, and the
    /// cash must go to one of the holder's registered cash addresses.
    pub fn lock(env: Env, holder: Address, amount: i128, cash_to: Address) -> Result<(), ComplianceError> {
        vault(&env)?.require_auth();
        if amount <= 0 {
            return Err(ComplianceError::InvalidAmount);
        }
        let inv = read_investor(&env, &holder).ok_or(ComplianceError::NotRegistered)?;
        require_redeem(&eligibility(&env, &holder))?;
        if !is_cash(&inv, &cash_to) {
            return Err(ComplianceError::CashAddressNotAllowed);
        }
        if amount > unlocked(&env, &holder)? {
            return Err(ComplianceError::InsufficientUnlocked);
        }
        let locked = read_locked(&env, &holder)
            .checked_add(amount)
            .ok_or(ComplianceError::MathOverflow)?;
        write_locked(&env, &holder, locked);
        bump(&env);
        Ok(())
    }

    /// Release a lock (cancelled or aborted redemption). Vault only.
    pub fn unlock(env: Env, holder: Address, amount: i128) -> Result<(), ComplianceError> {
        vault(&env)?.require_auth();
        let locked = read_locked(&env, &holder);
        if amount <= 0 || amount > locked {
            return Err(ComplianceError::InvalidAmount);
        }
        write_locked(&env, &holder, locked - amount);
        bump(&env);
        Ok(())
    }

    /// Burn locked shares at settlement (SAC clawback). Vault only.
    pub fn redeem_burn(env: Env, holder: Address, amount: i128) -> Result<(), ComplianceError> {
        vault(&env)?.require_auth();
        let locked = read_locked(&env, &holder);
        if amount <= 0 || amount > locked {
            return Err(ComplianceError::InvalidAmount);
        }
        notify(&env, &holder, sac_balance(&env, &holder))?;
        StellarAssetClient::new(&env, &share(&env)).clawback(&holder, &amount);
        write_locked(&env, &holder, locked - amount);
        let total = total_shares(&env)
            .checked_sub(amount)
            .ok_or(ComplianceError::MathOverflow)?;
        env.storage().instance().set(&DataKey::TotalShares, &total);
        bump(&env);
        Burned { holder, amount }.publish(&env);
        Ok(())
    }

    /// Move shares without the holder's signature (lost keys, court order,
    /// error correction). Ops under the TA + ADMIN policy. Supply unchanged.
    pub fn forced_transfer(
        env: Env,
        from: Address,
        to: Address,
        amount: i128,
        reason: BytesN<32>,
    ) -> Result<(), ComplianceError> {
        ops(&env).require_auth();
        if amount <= 0 {
            return Err(ComplianceError::InvalidAmount);
        }
        if from == to {
            return Err(ComplianceError::SameAddress);
        }
        require_receive(&eligibility(&env, &to))?;
        let from_bal = sac_balance(&env, &from);
        let free = from_bal
            .checked_sub(read_locked(&env, &from))
            .ok_or(ComplianceError::MathOverflow)?;
        if amount > free {
            return Err(ComplianceError::InsufficientUnlocked);
        }
        notify(&env, &from, from_bal)?;
        notify(&env, &to, sac_balance(&env, &to))?;
        StellarAssetClient::new(&env, &share(&env)).clawback(&from, &amount);
        sandwich_mint(&env, &to, amount);
        bump(&env);
        Forced {
            from,
            to,
            amount,
            reason,
        }
        .publish(&env);
        Ok(())
    }

    /// Extend the TTL of investor records and locks. Permissionless.
    pub fn extend(env: Env, addrs: Vec<Address>) {
        for a in addrs.iter() {
            touch(&env, &DataKey::Inv(a.clone()));
            touch(&env, &DataKey::Locked(a));
        }
        bump(&env);
    }

    // ----- views -----

    pub fn investor(env: Env, addr: Address) -> Option<Investor> {
        read_investor(&env, &addr)
    }

    pub fn status(env: Env, addr: Address) -> Eligibility {
        eligibility(&env, &addr)
    }

    pub fn balance(env: Env, addr: Address) -> i128 {
        sac_balance(&env, &addr)
    }

    pub fn locked(env: Env, addr: Address) -> i128 {
        read_locked(&env, &addr)
    }

    pub fn total_shares(env: Env) -> i128 {
        total_shares(&env)
    }

    pub fn is_cash_address(env: Env, holder: Address, addr: Address) -> bool {
        read_investor(&env, &holder)
            .map(|inv| is_cash(&inv, &addr))
            .unwrap_or(false)
    }

    pub fn jurisdiction(env: Env, code: Symbol) -> bool {
        jurisdiction_allowed(&env, &code)
    }

    pub fn ops(env: Env) -> Address {
        ops(&env)
    }

    pub fn share(env: Env) -> Address {
        share(&env)
    }
}

#[cfg(test)]
mod test;
