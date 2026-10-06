//! Distribution: a per-share accumulator for income distributions paid in
//! the fund's cash asset (USDC for the distributing class).
//!
//! `declare(amount)` pulls cash from the treasury and raises the accumulator
//! `Acc` (scaled by 10^18) by `amount * SCALE / total_shares`, carrying the
//! division remainder into the next declaration so nothing is lost to
//! rounding across declarations. Each holder keeps a snapshot of `Acc` and a
//! scaled accrued amount. The registrar calls `on_change(holder, old_balance)`
//! **before** every balance change (issue, transfer, forced transfer,
//! redemption burn), which books `old_balance * (Acc - snap)` for the holder,
//! so entitlements follow the balance a holder actually had when each
//! distribution was declared.
#![no_std]

use soroban_sdk::{
    contract, contractclient, contracterror, contractevent, contractimpl, contracttype, token::TokenClient,
    Address, BytesN, Env, Symbol, Vec,
};

/// Fixed-point scale of the accumulator.
pub const SCALE: i128 = 1_000_000_000_000_000_000;
/// Maximum holders per `claim_for` call.
pub const MAX_BATCH: u32 = 25;

const LEDGERS_PER_DAY: u32 = 17_280;
const INSTANCE_TTL_THRESHOLD: u32 = LEDGERS_PER_DAY * 30;
const INSTANCE_TTL_EXTEND_TO: u32 = LEDGERS_PER_DAY * 120;
const RECORD_TTL_THRESHOLD: u32 = LEDGERS_PER_DAY * 60;
const RECORD_TTL_EXTEND_TO: u32 = LEDGERS_PER_DAY * 365;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum DistError {
    NoSupply = 1,
    InvalidAmount = 2,
    NothingToClaim = 3,
    CashAddressNotAllowed = 4,
    Frozen = 5,
    MathOverflow = 6,
    BatchTooLarge = 7,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HolderState {
    pub snap: i128,
    pub accrued_scaled: i128,
}

// Mirrors of the registrar's types (same XDR shape), so this crate does not
// link the registrar's contract code.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Investor {
    pub kyc_expiry: u64,
    pub jurisdiction: Symbol,
    pub investor_type: u32,
    pub cash_addresses: Vec<Address>,
    pub frozen: bool,
}

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

/// The registrar views this contract reads.
#[contractclient(name = "RegistrarClient")]
pub trait Registrar {
    fn total_shares(env: Env) -> i128;
    fn balance(env: Env, addr: Address) -> i128;
    fn is_cash_address(env: Env, holder: Address, addr: Address) -> bool;
    fn status(env: Env, addr: Address) -> Eligibility;
    fn investor(env: Env, addr: Address) -> Option<Investor>;
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Ops,
    Compliance,
    Cash,
    Treasury,
    Acc,
    CarryScaled,
    Declared,
    Claimed,
    Holder(Address),
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Declared {
    pub amount: i128,
    pub per_share: i128,
    pub memo: BytesN<32>,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DistClaimed {
    #[topic]
    pub holder: Address,
    pub to: Address,
    pub amount: i128,
}

#[contract]
pub struct Distribution;

fn bump(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_EXTEND_TO);
}

fn get_addr(env: &Env, k: &DataKey) -> Address {
    env.storage().instance().get(k).unwrap()
}

fn get_i(env: &Env, k: &DataKey) -> i128 {
    env.storage().instance().get(k).unwrap_or(0)
}

fn read_holder(env: &Env, h: &Address) -> HolderState {
    let key = DataKey::Holder(h.clone());
    let p = env.storage().persistent();
    match p.get(&key) {
        Some(s) => {
            p.extend_ttl(&key, RECORD_TTL_THRESHOLD, RECORD_TTL_EXTEND_TO);
            s
        }
        None => HolderState {
            snap: 0,
            accrued_scaled: 0,
        },
    }
}

fn write_holder(env: &Env, h: &Address, s: &HolderState) {
    let key = DataKey::Holder(h.clone());
    let p = env.storage().persistent();
    p.set(&key, s);
    p.extend_ttl(&key, RECORD_TTL_THRESHOLD, RECORD_TTL_EXTEND_TO);
}

/// Scaled entitlement of a holder with balance `bal`, including what has not
/// been booked yet.
fn total_scaled(acc: i128, st: &HolderState, bal: i128) -> Result<i128, DistError> {
    let delta = acc.checked_sub(st.snap).ok_or(DistError::MathOverflow)?;
    bal.checked_mul(delta)
        .and_then(|x| x.checked_add(st.accrued_scaled))
        .ok_or(DistError::MathOverflow)
}

fn registrar(env: &Env) -> RegistrarClient<'_> {
    RegistrarClient::new(env, &get_addr(env, &DataKey::Compliance))
}

/// Book and pay a holder's whole-stroop entitlement to `to`. Returns the
/// amount paid (0 when nothing whole is owed).
fn pay_out(env: &Env, holder: &Address, to: &Address) -> Result<i128, DistError> {
    let acc = get_i(env, &DataKey::Acc);
    let st = read_holder(env, holder);
    let bal = registrar(env).balance(holder);
    let tot = total_scaled(acc, &st, bal)?;
    let payout = tot / SCALE;
    if payout == 0 {
        return Ok(0);
    }
    let rest = tot - payout * SCALE;
    write_holder(
        env,
        holder,
        &HolderState {
            snap: acc,
            accrued_scaled: rest,
        },
    );
    let claimed = get_i(env, &DataKey::Claimed)
        .checked_add(payout)
        .ok_or(DistError::MathOverflow)?;
    env.storage().instance().set(&DataKey::Claimed, &claimed);
    TokenClient::new(env, &get_addr(env, &DataKey::Cash)).transfer(
        &env.current_contract_address(),
        to,
        &payout,
    );
    DistClaimed {
        holder: holder.clone(),
        to: to.clone(),
        amount: payout,
    }
    .publish(env);
    Ok(payout)
}

#[contractimpl]
impl Distribution {
    pub fn __constructor(env: Env, ops: Address, compliance: Address, cash: Address, treasury: Address) {
        let s = env.storage().instance();
        s.set(&DataKey::Ops, &ops);
        s.set(&DataKey::Compliance, &compliance);
        s.set(&DataKey::Cash, &cash);
        s.set(&DataKey::Treasury, &treasury);
        s.set(&DataKey::Acc, &0i128);
        s.set(&DataKey::CarryScaled, &0i128);
        s.set(&DataKey::Declared, &0i128);
        s.set(&DataKey::Claimed, &0i128);
        bump(&env);
    }

    /// Called by the registrar before a holder's balance changes.
    pub fn on_change(env: Env, holder: Address, old_balance: i128) -> Result<(), DistError> {
        get_addr(&env, &DataKey::Compliance).require_auth();
        let acc = get_i(&env, &DataKey::Acc);
        let st = read_holder(&env, &holder);
        let tot = total_scaled(acc, &st, old_balance)?;
        write_holder(
            &env,
            &holder,
            &HolderState {
                snap: acc,
                accrued_scaled: tot,
            },
        );
        Ok(())
    }

    /// Declare a distribution of `amount` cash stroops. Ops (ADMIN policy)
    /// plus the treasury, which pays. Returns the scaled per-share increment.
    pub fn declare(env: Env, amount: i128, memo: BytesN<32>) -> Result<i128, DistError> {
        get_addr(&env, &DataKey::Ops).require_auth();
        let treasury = get_addr(&env, &DataKey::Treasury);
        treasury.require_auth();
        if amount <= 0 {
            return Err(DistError::InvalidAmount);
        }
        let t = registrar(&env).total_shares();
        if t <= 0 {
            return Err(DistError::NoSupply);
        }
        TokenClient::new(&env, &get_addr(&env, &DataKey::Cash)).transfer(
            &treasury,
            &env.current_contract_address(),
            &amount,
        );
        let num = amount
            .checked_mul(SCALE)
            .and_then(|x| x.checked_add(get_i(&env, &DataKey::CarryScaled)))
            .ok_or(DistError::MathOverflow)?;
        let per_share = num / t;
        let carry = num % t;
        let acc = get_i(&env, &DataKey::Acc)
            .checked_add(per_share)
            .ok_or(DistError::MathOverflow)?;
        let declared = get_i(&env, &DataKey::Declared)
            .checked_add(amount)
            .ok_or(DistError::MathOverflow)?;
        let s = env.storage().instance();
        s.set(&DataKey::Acc, &acc);
        s.set(&DataKey::CarryScaled, &carry);
        s.set(&DataKey::Declared, &declared);
        bump(&env);
        Declared {
            amount,
            per_share,
            memo,
        }
        .publish(&env);
        Ok(per_share)
    }

    /// Whole cash stroops a holder could claim now.
    pub fn accrued(env: Env, holder: Address) -> Result<i128, DistError> {
        let acc = get_i(&env, &DataKey::Acc);
        let st = read_holder(&env, &holder);
        let bal = registrar(&env).balance(&holder);
        Ok(total_scaled(acc, &st, bal)? / SCALE)
    }

    /// Claim to one of the holder's registered cash addresses. Holder auth.
    /// Expired KYC does not block a claim; a freeze does.
    pub fn claim(env: Env, holder: Address, to: Address) -> Result<i128, DistError> {
        holder.require_auth();
        let reg = registrar(&env);
        if !reg.is_cash_address(&holder, &to) {
            return Err(DistError::CashAddressNotAllowed);
        }
        if reg.status(&holder).frozen {
            return Err(DistError::Frozen);
        }
        let paid = pay_out(&env, &holder, &to)?;
        if paid == 0 {
            return Err(DistError::NothingToClaim);
        }
        bump(&env);
        Ok(paid)
    }

    /// TA push: pay up to 25 holders to their first cash address. Holders that
    /// are frozen, unregistered or owed nothing are skipped. Ops (TA policy).
    pub fn claim_for(env: Env, holders: Vec<Address>) -> Result<i128, DistError> {
        get_addr(&env, &DataKey::Ops).require_auth();
        if holders.len() > MAX_BATCH {
            return Err(DistError::BatchTooLarge);
        }
        let reg = registrar(&env);
        let mut total = 0i128;
        for h in holders.iter() {
            let inv = match reg.investor(&h) {
                Some(i) => i,
                None => continue,
            };
            if inv.frozen {
                continue;
            }
            let to = match inv.cash_addresses.get(0) {
                Some(a) => a,
                None => continue,
            };
            let paid = pay_out(&env, &h, &to)?;
            total = total.checked_add(paid).ok_or(DistError::MathOverflow)?;
        }
        bump(&env);
        Ok(total)
    }

    // ----- views -----

    pub fn acc(env: Env) -> i128 {
        get_i(&env, &DataKey::Acc)
    }

    pub fn carry_scaled(env: Env) -> i128 {
        get_i(&env, &DataKey::CarryScaled)
    }

    pub fn declared(env: Env) -> i128 {
        get_i(&env, &DataKey::Declared)
    }

    pub fn claimed(env: Env) -> i128 {
        get_i(&env, &DataKey::Claimed)
    }

    pub fn holder(env: Env, holder: Address) -> HolderState {
        read_holder(&env, &holder)
    }
}

#[cfg(test)]
mod test;
