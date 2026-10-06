#![cfg(test)]
extern crate std;

use super::*;
use compliance::{Compliance, ComplianceClient, Investor};
use distribution::Distribution;
use nav_oracle::{Asset as OAsset, NavOracle, NavOracleClient};
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Events as _, IssuerFlags, Ledger as _},
    token::{StellarAssetClient, TokenClient},
    vec, Address, BytesN, Env, Symbol,
};

pub const ONE: i128 = 10_000_000; // 7 decimals
pub const NAV1: i128 = 100_000_000_000_000; // 1.0 at 14 decimals
pub const CUTOFF: u64 = 1_791_205_200; // 2026-10-05T13:00:00Z
pub const HOUR: u64 = 3_600;
pub const YEAR: u64 = 365 * 86_400;

pub struct F {
    pub ops: Address,
    pub treasury: Address,
    pub usdc: Address,
    pub share: Address,
    pub reg: Address,
    pub oracle: Address,
    pub vault: Address,
}

impl F {
    pub fn v<'a>(&self, env: &'a Env) -> AsyncVaultClient<'a> {
        AsyncVaultClient::new(env, &self.vault)
    }
    pub fn r<'a>(&self, env: &'a Env) -> ComplianceClient<'a> {
        ComplianceClient::new(env, &self.reg)
    }
    pub fn usdc_of(&self, env: &Env, a: &Address) -> i128 {
        TokenClient::new(env, &self.usdc).balance(a)
    }
    /// Publish a NAV (as the administrator) with timestamp `ts`, moving the
    /// ledger clock forward to `ts` if needed.
    pub fn publish(&self, env: &Env, nav: i128, ts: u64) {
        if env.ledger().timestamp() < ts {
            env.ledger().set_timestamp(ts);
        }
        NavOracleClient::new(env, &self.oracle).publish(&OAsset::Other(Symbol::new(env, "USD_D")), &nav, &ts);
    }
    /// Register an investor valid for a year in FR, with 1 000 000 USDC.
    pub fn investor(&self, env: &Env) -> (Address, Address) {
        self.investor_with(env, CUTOFF + YEAR, "FR")
    }
    pub fn investor_with(&self, env: &Env, expiry: u64, code: &str) -> (Address, Address) {
        let h = Address::generate(env);
        let cash = Address::generate(env);
        self.r(env).set_investor(
            &h,
            &Investor {
                kyc_expiry: expiry,
                jurisdiction: Symbol::new(env, code),
                investor_type: 0,
                cash_addresses: vec![env, cash.clone()],
                frozen: false,
            },
        );
        StellarAssetClient::new(env, &self.usdc).mint(&h, &(1_000_000 * ONE));
        (h, cash)
    }
}

pub fn config(f_ops: &Address, reg: &Address, share: &Address, usdc: &Address, treasury: &Address, oracle: &Address, env: &Env, max_req: u32) -> Config {
    Config {
        ops: f_ops.clone(),
        compliance: reg.clone(),
        share: share.clone(),
        cash: usdc.clone(),
        treasury: treasury.clone(),
        oracle: oracle.clone(),
        oracle_asset: Asset::Other(Symbol::new(env, "USD_D")),
        nav_decimals: 14,
        initial_nav: NAV1,
        min_subscription: 1_000 * ONE,
        max_strike_delay: 8 * HOUR,
        max_nav_move_bps: 25,
        max_requests_per_epoch: max_req,
    }
}

pub fn setup_with(env: &Env, max_req: u32) -> F {
    env.mock_all_auths();
    env.ledger().set_timestamp(CUTOFF - 6 * HOUR);
    let ops = Address::generate(env);
    let treasury = Address::generate(env);
    let usdc = env.register_stellar_asset_contract_v2(Address::generate(env)).address();
    let sac = env.register_stellar_asset_contract_v2(Address::generate(env));
    for fl in [IssuerFlags::RequiredFlag, IssuerFlags::RevocableFlag, IssuerFlags::ClawbackEnabledFlag] {
        sac.issuer().set_flag(fl);
    }
    let share = sac.address();
    let oracle = env.register(
        NavOracle,
        (ops.clone(), OAsset::Stellar(usdc.clone()), 14u32, 86_400u32),
    );
    let reg = env.register(Compliance, (ops.clone(), share.clone()));
    StellarAssetClient::new(env, &share).set_admin(&reg);
    let dist = env.register(Distribution, (ops.clone(), reg.clone(), usdc.clone(), treasury.clone()));
    let vault = env.register(
        AsyncVault,
        (config(&ops, &reg, &share, &usdc, &treasury, &oracle, env, max_req),),
    );
    let r = ComplianceClient::new(env, &reg);
    r.bind(&vault, &dist);
    for code in ["FR", "DE", "LU"] {
        r.set_jurisdiction(&Symbol::new(env, code), &true);
    }
    AsyncVaultClient::new(env, &vault).open_epoch(&CUTOFF);
    F {
        ops,
        treasury,
        usdc,
        share,
        reg,
        oracle,
        vault,
    }
}

pub fn setup(env: &Env) -> F {
    setup_with(env, 400)
}

fn reason(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[9u8; 32])
}

/// Subscribe, strike at `nav`, settle and claim, returning the shares issued.
fn hold(env: &Env, f: &F, who: &Address, cash: i128, nav: i128) -> i128 {
    let id = f.v(env).request_subscribe(who, &cash);
    let ep = f.v(env).request(&id).epoch;
    let e = f.v(env).epoch(&ep);
    f.publish(env, nav, e.cutoff + 60);
    f.v(env).strike_nav(&ep);
    f.v(env).settle(&ep, &50);
    f.v(env).claim(who, &id);
    f.r(env).balance(who)
}

// ---------------------------------------------------------------------------
// constructor and epochs
// ---------------------------------------------------------------------------

#[contract]
pub struct BadOracle;

#[contractimpl]
impl BadOracle {
    pub fn decimals(_env: Env) -> u32 {
        8
    }
    pub fn lastprice(env: Env, _asset: Asset) -> Option<PriceData> {
        Some(PriceData {
            price: 0,
            timestamp: env.ledger().timestamp(),
        })
    }
}

#[test]
fn constructor_checks_oracle_decimals() {
    let env = Env::default();
    let f = setup(&env);
    let bad = env.register(BadOracle, ());
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.register(
            AsyncVault,
            (config(&f.ops, &f.reg, &f.share, &f.usdc, &f.treasury, &bad, &env, 10),),
        );
    }));
    assert!(res.is_err());
    assert_eq!(f.v(&env).config().nav_decimals, 14);
    assert_eq!(f.v(&env).last_nav(), NAV1);
}

#[test]
fn open_epoch_rules_and_auth() {
    let env = Env::default();
    let f = setup(&env);
    let v = f.v(&env);
    assert_eq!(v.current_epoch(), 1);
    assert_eq!(v.try_open_epoch(&(CUTOFF + 86_400)), Err(Ok(VaultError::PreviousEpochOpen)));
    env.ledger().set_timestamp(CUTOFF);
    assert_eq!(v.try_open_epoch(&CUTOFF), Err(Ok(VaultError::CutoffInPast)));
    env.set_auths(&[]);
    assert!(v.try_open_epoch(&(CUTOFF + 86_400)).is_err());
    env.mock_all_auths();
    assert_eq!(v.open_epoch(&(CUTOFF + 86_400)), 2);
    assert_eq!(env.auths()[0].0, f.ops);
    assert_eq!(v.epoch(&2).status, EpochStatus::Open);
}

// ---------------------------------------------------------------------------
// requests
// ---------------------------------------------------------------------------

#[test]
fn subscribe_escrows_cash_and_queues() {
    let env = Env::default();
    let f = setup(&env);
    let (a, _) = f.investor(&env);
    let id = f.v(&env).request_subscribe(&a, &(2_499 * ONE + 9_900_000));
    assert_eq!(env.auths()[0].0, a);
    assert_eq!(env.events().all().filter_by_contract(&f.vault).events().len(), 1);
    assert_eq!(f.usdc_of(&env, &f.vault), 2_499 * ONE + 9_900_000);
    let r = f.v(&env).request(&id);
    assert_eq!(r.status, ReqStatus::Pending);
    assert_eq!(r.kind, Kind::Subscribe);
    assert_eq!(f.v(&env).epoch(&1).sub_total, r.amount);
    assert_eq!(f.v(&env).queue(&1, &0, &10).len(), 1);
    assert_eq!(f.v(&env).queue_len(&1), 1);
}

#[test]
fn subscribe_rejections() {
    let env = Env::default();
    let f = setup(&env);
    let v = f.v(&env);
    let (a, _) = f.investor(&env);
    assert_eq!(v.try_request_subscribe(&a, &(999 * ONE + 9_900_000)), Err(Ok(VaultError::BelowMinimum)));
    let (us, _) = f.investor_with(&env, CUTOFF + YEAR, "US");
    assert_eq!(v.try_request_subscribe(&us, &(10_000 * ONE)), Err(Ok(VaultError::JurisdictionBlocked)));
    let (old, _) = f.investor_with(&env, CUTOFF - 7 * HOUR, "FR");
    assert_eq!(v.try_request_subscribe(&old, &(10_000 * ONE)), Err(Ok(VaultError::KycExpired)));
    let (fz, _) = f.investor(&env);
    f.r(&env).set_frozen(&fz, &true, &reason(&env));
    assert_eq!(v.try_request_subscribe(&fz, &(10_000 * ONE)), Err(Ok(VaultError::Frozen)));
    let nobody = Address::generate(&env);
    assert_eq!(v.try_request_subscribe(&nobody, &(10_000 * ONE)), Err(Ok(VaultError::NotRegistered)));
    // at the cut-off exactly
    env.ledger().set_timestamp(CUTOFF);
    assert_eq!(v.try_request_subscribe(&a, &(10_000 * ONE)), Err(Ok(VaultError::CutoffPassed)));
    assert_eq!(f.usdc_of(&env, &f.vault), 0);
}

#[test]
fn subscribe_needs_investor_auth() {
    let env = Env::default();
    let f = setup(&env);
    let (a, _) = f.investor(&env);
    env.set_auths(&[]);
    assert!(f.v(&env).try_request_subscribe(&a, &(1_000 * ONE)).is_err());
}

#[test]
fn no_open_epoch() {
    let env = Env::default();
    let f = setup(&env);
    let (a, _) = f.investor(&env);
    let other = env.register(
        AsyncVault,
        (config(&f.ops, &f.reg, &f.share, &f.usdc, &f.treasury, &f.oracle, &env, 10),),
    );
    assert_eq!(
        AsyncVaultClient::new(&env, &other).try_request_subscribe(&a, &(1_000 * ONE)),
        Err(Ok(VaultError::NoOpenEpoch))
    );
}

#[test]
fn epoch_full() {
    let env = Env::default();
    let f = setup_with(&env, 2);
    let (a, _) = f.investor(&env);
    f.v(&env).request_subscribe(&a, &(1_000 * ONE));
    f.v(&env).request_subscribe(&a, &(1_000 * ONE));
    assert_eq!(f.v(&env).try_request_subscribe(&a, &(1_000 * ONE)), Err(Ok(VaultError::EpochFull)));
}

#[test]
fn redeem_locks_and_allows_expired_but_not_frozen() {
    let env = Env::default();
    let f = setup(&env);
    let (a, cash) = f.investor_with(&env, CUTOFF + 20 * HOUR, "FR");
    assert_eq!(hold(&env, &f, &a, 5_000 * ONE, NAV1), 5_000 * ONE);
    let e2 = f.v(&env).open_epoch(&(CUTOFF + 86_400));
    env.ledger().set_timestamp(CUTOFF + 21 * HOUR); // KYC expired now
    assert!(!f.r(&env).status(&a).kyc_valid);
    let v = f.v(&env);
    let stranger = Address::generate(&env);
    assert_eq!(v.try_request_redeem(&a, &ONE, &stranger), Err(Ok(VaultError::CashAddressNotAllowed)));
    assert_eq!(v.try_request_redeem(&a, &(5_001 * ONE), &cash), Err(Ok(VaultError::InsufficientShares)));
    assert_eq!(v.try_request_redeem(&a, &0, &cash), Err(Ok(VaultError::InvalidAmount)));
    let nobody = Address::generate(&env);
    assert_eq!(v.try_request_redeem(&nobody, &ONE, &cash), Err(Ok(VaultError::NotRegistered)));
    let id = v.request_redeem(&a, &(2_000 * ONE), &cash);
    assert_eq!(f.r(&env).locked(&a), 2_000 * ONE);
    assert_eq!(v.epoch(&e2).redeem_shares_total, 2_000 * ONE);
    assert_eq!(v.request(&id).cash_to, Some(cash.clone()));
    // Locked shares cannot be requested twice.
    assert_eq!(v.try_request_redeem(&a, &(3_001 * ONE), &cash), Err(Ok(VaultError::InsufficientShares)));
    f.r(&env).set_frozen(&a, &true, &reason(&env));
    assert_eq!(v.try_request_redeem(&a, &ONE, &cash), Err(Ok(VaultError::Frozen)));
}

// ---------------------------------------------------------------------------
// cancel
// ---------------------------------------------------------------------------

#[test]
fn cancel_window() {
    let env = Env::default();
    let f = setup(&env);
    let (a, _) = f.investor(&env);
    let v = f.v(&env);
    let before = f.usdc_of(&env, &a);
    let id1 = v.request_subscribe(&a, &(10_000 * ONE));
    let id2 = v.request_subscribe(&a, &(25_000 * ONE));
    env.ledger().set_timestamp(CUTOFF - 1);
    v.cancel(&a, &id1);
    assert_eq!(f.usdc_of(&env, &a), before - 25_000 * ONE);
    assert_eq!(v.epoch(&1).sub_total, 25_000 * ONE);
    assert_eq!(v.request(&id1).status, ReqStatus::Cancelled);
    assert_eq!(v.try_cancel(&a, &id1), Err(Ok(VaultError::NotPending)));
    env.ledger().set_timestamp(CUTOFF);
    assert_eq!(v.try_cancel(&a, &id2), Err(Ok(VaultError::CutoffPassed)));
    assert_eq!(v.request(&id2).status, ReqStatus::Pending);
}

#[test]
fn cancel_redemption_unlocks() {
    let env = Env::default();
    let f = setup(&env);
    let (a, cash) = f.investor(&env);
    hold(&env, &f, &a, 3_000 * ONE, NAV1);
    f.v(&env).open_epoch(&(CUTOFF + 86_400));
    let id = f.v(&env).request_redeem(&a, &(3_000 * ONE), &cash);
    f.v(&env).cancel(&a, &id);
    assert_eq!(f.r(&env).locked(&a), 0);
    assert_eq!(f.v(&env).epoch(&2).redeem_shares_total, 0);
}

#[test]
fn cancel_owner_and_auth() {
    let env = Env::default();
    let f = setup(&env);
    let (a, _) = f.investor(&env);
    let (b, _) = f.investor(&env);
    let id = f.v(&env).request_subscribe(&a, &(1_000 * ONE));
    assert_eq!(f.v(&env).try_cancel(&b, &id), Err(Ok(VaultError::NotRequestOwner)));
    assert_eq!(f.v(&env).try_cancel(&a, &999), Err(Ok(VaultError::RequestNotFound)));
    env.set_auths(&[]);
    assert!(f.v(&env).try_cancel(&a, &id).is_err());
}

// ---------------------------------------------------------------------------
// strike
// ---------------------------------------------------------------------------

#[test]
fn strike_before_cutoff_and_missing_price() {
    let env = Env::default();
    let f = setup(&env);
    assert_eq!(f.v(&env).try_strike_nav(&1), Err(Ok(VaultError::EpochNotClosed)));
    env.ledger().set_timestamp(CUTOFF + HOUR);
    assert_eq!(f.v(&env).try_strike_nav(&1), Err(Ok(VaultError::PriceMissing)));
    assert_eq!(f.v(&env).try_strike_nav(&7), Err(Ok(VaultError::WrongEpochState)));
}

#[test]
fn strike_rejects_stale_price() {
    let env = Env::default();
    let f = setup(&env);
    f.publish(&env, NAV1, CUTOFF - HOUR); // struck before this epoch's cut-off
    env.ledger().set_timestamp(CUTOFF + 10 * 60);
    assert_eq!(f.v(&env).try_strike_nav(&1), Err(Ok(VaultError::StalePrice)));
}

#[test]
fn strike_rejects_late_price() {
    let env = Env::default();
    let f = setup(&env);
    f.publish(&env, NAV1, CUTOFF + 8 * HOUR + 1);
    assert_eq!(f.v(&env).try_strike_nav(&1), Err(Ok(VaultError::PriceTooLate)));
}

#[test]
fn strike_band_and_override() {
    let env = Env::default();
    let f = setup(&env);
    f.publish(&env, 102_700_000_000_000, CUTOFF + HOUR); // fat-fingered 1.0270
    assert_eq!(f.v(&env).try_strike_nav(&1), Err(Ok(VaultError::NavMoveTooLarge)));
    // The co-signed override accepts it and emits NavOverride + Struck.
    assert_eq!(f.v(&env).strike_nav_override(&1), 102_700_000_000_000);
    assert_eq!(env.events().all().filter_by_contract(&f.vault).events().len(), 2);
    assert_eq!(f.v(&env).epoch(&1).status, EpochStatus::Struck);
    assert_eq!(f.v(&env).try_strike_nav(&1), Err(Ok(VaultError::WrongEpochState)));
}

#[test]
fn strike_within_band_and_auth() {
    let env = Env::default();
    let f = setup(&env);
    f.publish(&env, 100_000_412_000_000, CUTOFF + HOUR);
    env.set_auths(&[]);
    assert!(f.v(&env).try_strike_nav(&1).is_err());
    env.mock_all_auths();
    assert_eq!(f.v(&env).strike_nav(&1), 100_000_412_000_000);
    assert_eq!(env.auths()[0].0, f.ops);
    let e = f.v(&env).epoch(&1);
    assert_eq!((e.nav, e.nav_ts), (100_000_412_000_000, CUTOFF + HOUR));
}

#[test]
fn strike_rejects_non_positive_nav_from_a_foreign_oracle() {
    let env = Env::default();
    let f = setup(&env);
    let bad = env.register(BadOracle, ());
    let mut c = config(&f.ops, &f.reg, &f.share, &f.usdc, &f.treasury, &bad, &env, 10);
    c.nav_decimals = 8;
    let v2 = env.register(AsyncVault, (c,));
    let v2c = AsyncVaultClient::new(&env, &v2);
    v2c.open_epoch(&CUTOFF);
    env.ledger().set_timestamp(CUTOFF + HOUR);
    assert_eq!(v2c.try_strike_nav(&1), Err(Ok(VaultError::NavNonPositive)));
    assert_eq!(v2c.try_strike_nav_override(&1), Err(Ok(VaultError::NavNonPositive)));
}

// ---------------------------------------------------------------------------
// settle
// ---------------------------------------------------------------------------

#[test]
fn settle_paginates_25_requests_in_3_calls() {
    let env = Env::default();
    let f = setup(&env);
    let mut ids = std::vec::Vec::new();
    for i in 0..25 {
        let (a, _) = f.investor(&env);
        ids.push((a.clone(), f.v(&env).request_subscribe(&a, &((1_000 + i) * ONE))));
    }
    f.publish(&env, NAV1, CUTOFF + HOUR);
    f.v(&env).strike_nav(&1);
    assert_eq!(f.v(&env).settle(&1, &10), (10, 15));
    assert_eq!(f.v(&env).epoch(&1).status, EpochStatus::Settling);
    // A claim in the middle of settlement is refused.
    assert_eq!(
        f.v(&env).try_claim(&ids[0].0, &ids[0].1),
        Err(Ok(VaultError::EpochNotSettled))
    );
    assert_eq!(f.v(&env).settle(&1, &10), (10, 5));
    let t0 = f.usdc_of(&env, &f.treasury);
    assert_eq!(f.v(&env).settle(&1, &10), (5, 0));
    let e = f.v(&env).epoch(&1);
    assert_eq!(e.status, EpochStatus::Settled);
    assert_eq!(e.cursor, 25);
    // NAV 1.0: all cash swept to the treasury, no dust.
    assert_eq!(f.usdc_of(&env, &f.treasury) - t0, e.sub_total);
    assert_eq!(f.v(&env).try_settle(&1, &10), Err(Ok(VaultError::WrongEpochState)));
    for (a, id) in ids {
        f.v(&env).claim(&a, &id);
    }
    assert_eq!(f.r(&env).total_shares(), e.sub_total);
    assert_eq!(f.usdc_of(&env, &f.vault), 0);
}

#[test]
fn settle_refunds_subscription_whose_kyc_expired_after_request() {
    let env = Env::default();
    let f = setup(&env);
    let (a, _) = f.investor_with(&env, CUTOFF - HOUR, "FR"); // valid now (cut-off - 6h)
    let (b, _) = f.investor(&env);
    let before = f.usdc_of(&env, &a);
    let ida = f.v(&env).request_subscribe(&a, &(5_000 * ONE));
    let idb = f.v(&env).request_subscribe(&b, &(5_000 * ONE));
    f.publish(&env, 100_000_412_000_000, CUTOFF + HOUR);
    f.v(&env).strike_nav(&1);
    f.v(&env).settle(&1, &20);
    let ra = f.v(&env).request(&ida);
    assert_eq!(ra.status, ReqStatus::Claimable);
    assert_eq!(ra.reject, REJECT_KYC_EXPIRED);
    assert_eq!((ra.shares_out, ra.cash_out), (0, 5_000 * ONE));
    let rb = f.v(&env).request(&idb);
    assert_eq!(rb.reject, REJECT_NONE);
    assert!(rb.shares_out > 0 && rb.shares_out < 5_000 * ONE);
    // treasury got b's cash minus b's dust; a's cash stays for the refund
    f.v(&env).claim(&a, &ida);
    assert_eq!(f.usdc_of(&env, &a), before);
    assert_eq!(f.r(&env).balance(&a), 0);
    f.v(&env).claim(&b, &idb);
    assert_eq!(f.r(&env).balance(&b), rb.shares_out);
    assert_eq!(f.usdc_of(&env, &f.vault), 0);
}

#[test]
fn settle_rejects_frozen_and_blocked_subscribers() {
    let env = Env::default();
    let f = setup(&env);
    let (a, _) = f.investor(&env);
    let (b, _) = f.investor_with(&env, CUTOFF + YEAR, "DE");
    let ida = f.v(&env).request_subscribe(&a, &(1_000 * ONE));
    let idb = f.v(&env).request_subscribe(&b, &(1_000 * ONE));
    f.r(&env).set_frozen(&a, &true, &reason(&env));
    f.r(&env).set_jurisdiction(&Symbol::new(&env, "DE"), &false);
    f.publish(&env, NAV1, CUTOFF + HOUR);
    f.v(&env).strike_nav(&1);
    f.v(&env).settle(&1, &20);
    assert_eq!(f.v(&env).request(&ida).reject, REJECT_FROZEN);
    assert_eq!(f.v(&env).request(&idb).reject, REJECT_JURISDICTION);
}

#[test]
fn insufficient_liquidity_then_top_up_then_settle() {
    let env = Env::default();
    let f = setup(&env);
    let (a, cash) = f.investor(&env);
    let (b, _) = f.investor(&env);
    hold(&env, &f, &a, 100_000 * ONE, NAV1);
    // Epoch 2: a redeems everything, b subscribes a little.
    f.v(&env).open_epoch(&(CUTOFF + 86_400));
    let idr = f.v(&env).request_redeem(&a, &(100_000 * ONE), &cash);
    let ids = f.v(&env).request_subscribe(&b, &(10_000 * ONE));
    f.publish(&env, 100_000_412_000_000, CUTOFF + 86_400 + HOUR);
    f.v(&env).strike_nav(&2);
    assert_eq!(f.v(&env).try_settle(&2, &20), Err(Ok(VaultError::InsufficientLiquidity)));
    // The failed call left nothing behind.
    assert_eq!(f.v(&env).epoch(&2).status, EpochStatus::Struck);
    assert_eq!(f.r(&env).balance(&a), 100_000 * ONE);
    // Treasury tops up; wrong state and missing auth are refused.
    assert_eq!(f.v(&env).try_deposit_liquidity(&1, &ONE), Err(Ok(VaultError::WrongEpochState)));
    assert_eq!(f.v(&env).try_deposit_liquidity(&2, &0), Err(Ok(VaultError::InvalidAmount)));
    f.v(&env).deposit_liquidity(&2, &(95_000 * ONE));
    assert_eq!(env.auths()[0].0, f.treasury);
    let t0 = f.usdc_of(&env, &f.treasury);
    assert_eq!(f.v(&env).settle(&2, &20), (2, 0));
    let e = f.v(&env).epoch(&2);
    let redeem_cash = f.v(&env).request(&idr).cash_out;
    assert_eq!(redeem_cash, 1_000_004_120_000); // 100,000 shares x 1.00000412
    let sub_dust = f.v(&env).request(&ids).cash_out;
    let surplus = e.sub_total + e.liquidity - e.claimable_cash;
    assert_eq!(surplus, 10_000 * ONE + 95_000 * ONE - redeem_cash - sub_dust);
    assert_eq!(f.usdc_of(&env, &f.treasury) - t0, surplus);
    f.v(&env).claim(&a, &idr);
    assert_eq!(f.usdc_of(&env, &cash), redeem_cash);
    assert_eq!(f.r(&env).balance(&a), 0);
    f.v(&env).claim(&f.ops, &ids);
    assert_eq!(f.usdc_of(&env, &f.vault), 0);
}

#[test]
fn deposit_liquidity_needs_treasury() {
    let env = Env::default();
    let f = setup(&env);
    f.publish(&env, NAV1, CUTOFF + HOUR);
    f.v(&env).strike_nav(&1);
    env.set_auths(&[]);
    assert!(f.v(&env).try_deposit_liquidity(&1, &ONE).is_err());
}

#[test]
fn settle_needs_ops_and_a_struck_epoch() {
    let env = Env::default();
    let f = setup(&env);
    assert_eq!(f.v(&env).try_settle(&1, &10), Err(Ok(VaultError::WrongEpochState)));
    f.publish(&env, NAV1, CUTOFF + HOUR);
    f.v(&env).strike_nav(&1);
    assert_eq!(f.v(&env).try_settle(&1, &0), Err(Ok(VaultError::InvalidAmount)));
    env.set_auths(&[]);
    assert!(f.v(&env).try_settle(&1, &10).is_err());
    env.mock_all_auths();
    assert_eq!(f.v(&env).settle(&1, &10), (0, 0));
    assert_eq!(env.auths()[0].0, f.ops);
}

// ---------------------------------------------------------------------------
// claim
// ---------------------------------------------------------------------------

#[test]
fn claim_rules() {
    let env = Env::default();
    let f = setup(&env);
    let (a, _) = f.investor(&env);
    let (b, _) = f.investor(&env);
    let ida = f.v(&env).request_subscribe(&a, &(1_000 * ONE));
    let idb = f.v(&env).request_subscribe(&b, &(1_000 * ONE));
    let idc = f.v(&env).request_subscribe(&b, &(1_000 * ONE));
    f.v(&env).cancel(&b, &idc);
    assert_eq!(f.v(&env).try_claim(&a, &ida), Err(Ok(VaultError::EpochNotSettled)));
    f.publish(&env, NAV1, CUTOFF + HOUR);
    f.v(&env).strike_nav(&1);
    f.v(&env).settle(&1, &10);
    let stranger = Address::generate(&env);
    assert_eq!(f.v(&env).try_claim(&stranger, &ida), Err(Ok(VaultError::NotRequestOwner)));
    assert_eq!(f.v(&env).try_claim(&b, &ida), Err(Ok(VaultError::NotRequestOwner)));
    f.v(&env).claim(&a, &ida);
    assert_eq!(f.v(&env).try_claim(&a, &ida), Err(Ok(VaultError::AlreadyClaimed)));
    assert_eq!(f.v(&env).try_claim(&b, &idc), Err(Ok(VaultError::NotClaimable)));
    // The TA pushes b's claim through the ops account.
    f.v(&env).claim(&f.ops, &idb);
    assert_eq!(env.auths()[0].0, f.ops);
    assert_eq!(f.r(&env).balance(&b), 1_000 * ONE);
    assert_eq!(f.v(&env).request(&idb).status, ReqStatus::Claimed);
    // Claim needs the caller's signature.
    env.set_auths(&[]);
    assert!(f.v(&env).try_claim(&a, &ida).is_err());
}

#[test]
fn claim_pays_dust_with_shares_at_a_fractional_nav() {
    let env = Env::default();
    let f = setup(&env);
    let (a, _) = f.investor(&env);
    let before = f.usdc_of(&env, &a);
    let amount = 3_333 * ONE + 3_333_333;
    let id = f.v(&env).request_subscribe(&a, &amount);
    let nav = 100_000_412_000_000;
    f.publish(&env, nav, CUTOFF + HOUR);
    f.v(&env).strike_nav(&1);
    f.v(&env).settle(&1, &10);
    f.v(&env).claim(&a, &id);
    let (shares, dust) = math::shares_for_cash(amount, nav, 14).unwrap();
    assert_eq!(f.r(&env).balance(&a), shares);
    assert_eq!(f.usdc_of(&env, &a), before - amount + dust);
    assert_eq!(f.v(&env).last_nav(), nav);
}

// ---------------------------------------------------------------------------
// abort and pause
// ---------------------------------------------------------------------------

#[test]
fn abort_after_the_strike_window_refunds_everything() {
    let env = Env::default();
    let f = setup(&env);
    let (a, cash) = f.investor(&env);
    hold(&env, &f, &a, 10_000 * ONE, NAV1);
    f.v(&env).open_epoch(&(CUTOFF + 86_400));
    let (b, _) = f.investor(&env);
    let before_b = f.usdc_of(&env, &b);
    let idr = f.v(&env).request_redeem(&a, &(4_000 * ONE), &cash);
    let ids = f.v(&env).request_subscribe(&b, &(2_000 * ONE));
    let ids2 = f.v(&env).request_subscribe(&b, &(1_000 * ONE));
    env.ledger().set_timestamp(CUTOFF + 86_400 + 8 * HOUR);
    assert_eq!(f.v(&env).try_abort_epoch(&2, &reason(&env), &10), Err(Ok(VaultError::AbortTooEarly)));
    env.ledger().set_timestamp(CUTOFF + 86_400 + 8 * HOUR + 1);
    env.set_auths(&[]);
    assert!(f.v(&env).try_abort_epoch(&2, &reason(&env), &10).is_err());
    env.mock_all_auths();
    // Paged: two items, then the last one.
    assert_eq!(f.v(&env).abort_epoch(&2, &reason(&env), &2), (2, 1));
    assert_eq!(f.v(&env).epoch(&2).status, EpochStatus::Aborted);
    assert_eq!(f.r(&env).locked(&a), 0);
    assert_eq!(f.v(&env).request(&idr).status, ReqStatus::Cancelled);
    f.v(&env).claim(&b, &ids);
    assert_eq!(f.v(&env).try_claim(&b, &ids2), Err(Ok(VaultError::NotClaimable)));
    assert_eq!(f.v(&env).abort_epoch(&2, &reason(&env), &2), (1, 0));
    assert_eq!(f.v(&env).try_abort_epoch(&2, &reason(&env), &2), Err(Ok(VaultError::WrongEpochState)));
    f.v(&env).claim(&f.ops, &ids2);
    assert_eq!(f.usdc_of(&env, &b), before_b);
    assert_eq!(f.r(&env).balance(&b), 0);
    // A late price can no longer be struck on an aborted epoch.
    assert_eq!(f.v(&env).try_strike_nav(&2), Err(Ok(VaultError::WrongEpochState)));
}

#[test]
fn pause_blocks_requests_but_not_cancel_settle_or_claim() {
    let env = Env::default();
    let f = setup(&env);
    let (a, _) = f.investor(&env);
    let id = f.v(&env).request_subscribe(&a, &(1_000 * ONE));
    let id2 = f.v(&env).request_subscribe(&a, &(1_000 * ONE));
    f.v(&env).pause();
    assert!(f.v(&env).paused());
    assert_eq!(f.v(&env).try_request_subscribe(&a, &(1_000 * ONE)), Err(Ok(VaultError::Paused)));
    let cash = Address::generate(&env);
    assert_eq!(f.v(&env).try_request_redeem(&a, &ONE, &cash), Err(Ok(VaultError::Paused)));
    f.v(&env).cancel(&a, &id2);
    f.publish(&env, NAV1, CUTOFF + HOUR);
    f.v(&env).strike_nav(&1);
    f.v(&env).settle(&1, &10);
    f.v(&env).claim(&a, &id);
    assert_eq!(f.r(&env).balance(&a), 1_000 * ONE);
    env.set_auths(&[]);
    assert!(f.v(&env).try_unpause().is_err());
    assert!(f.v(&env).try_pause().is_err());
    env.mock_all_auths();
    f.v(&env).unpause();
    assert!(!f.v(&env).paused());
}

// ---------------------------------------------------------------------------
// resources
// ---------------------------------------------------------------------------

/// Per-transaction instruction limit on Stellar (research/01 §14.1); re-read
/// it with `stellar network settings` before mainnet.
const TX_INSTRUCTION_LIMIT: u64 = 100_000_000;

#[test]
fn settle_batch_of_20_fits_the_transaction_budget() {
    let env = Env::default();
    let f = setup(&env);
    // 10 subscriptions and 10 redemptions in one epoch.
    let mut holders = std::vec::Vec::new();
    for _ in 0..10 {
        let (a, cash) = f.investor(&env);
        f.v(&env).request_subscribe(&a, &(2_000 * ONE));
        holders.push((a, cash));
    }
    f.publish(&env, NAV1, CUTOFF + HOUR);
    f.v(&env).strike_nav(&1);
    f.v(&env).settle(&1, &20);
    for id in 1..=10u64 {
        f.v(&env).claim(&f.ops, &id);
    }
    f.v(&env).open_epoch(&(CUTOFF + 86_400));
    for (a, cash) in &holders {
        f.v(&env).request_redeem(a, &(500 * ONE), cash);
        let (n, _) = f.investor(&env);
        f.v(&env).request_subscribe(&n, &(1_500 * ONE));
    }
    f.publish(&env, 100_000_412_000_000, CUTOFF + 86_400 + HOUR);
    f.v(&env).strike_nav(&2);
    env.cost_estimate().budget().reset_unlimited();
    assert_eq!(f.v(&env).settle(&2, &20), (20, 0));
    let cpu = env.cost_estimate().budget().cpu_instruction_cost();
    let mem = env.cost_estimate().budget().memory_bytes_cost();
    std::println!("settle(epoch, 20): cpu_insns={cpu} mem_bytes={mem} (native; wasm costs more)");
    // Native execution underestimates wasm execution, so keep a 4x margin.
    assert!(cpu * 4 < TX_INSTRUCTION_LIMIT, "settle(20) used {cpu} instructions");
}
