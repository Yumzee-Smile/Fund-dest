#![cfg(test)]
extern crate std;

use super::*;
use compliance::{Compliance, ComplianceClient, Investor as RegInvestor};
use proptest::prelude::*;
use soroban_sdk::{
    testutils::{Address as _, IssuerFlags, Ledger as _},
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env, Symbol, Vec,
};

const ONE: i128 = 10_000_000;
const NOW: u64 = 1_791_291_600; // 2026-10-06T13:00:00Z
const YEAR: u64 = 365 * 86_400;

struct Fx {
    reg: Address,
    dist: Address,
    usdc: Address,
    treasury: Address,
}

impl Fx {
    fn r<'a>(&self, env: &'a Env) -> ComplianceClient<'a> {
        ComplianceClient::new(env, &self.reg)
    }
    fn d<'a>(&self, env: &'a Env) -> DistributionClient<'a> {
        DistributionClient::new(env, &self.dist)
    }
    fn usdc_of(&self, env: &Env, a: &Address) -> i128 {
        TokenClient::new(env, &self.usdc).balance(a)
    }
}

fn setup(env: &Env) -> Fx {
    env.mock_all_auths();
    env.ledger().set_timestamp(NOW);
    let ops = Address::generate(env);
    let sac = env.register_stellar_asset_contract_v2(Address::generate(env));
    for f in [IssuerFlags::RequiredFlag, IssuerFlags::RevocableFlag, IssuerFlags::ClawbackEnabledFlag] {
        sac.issuer().set_flag(f);
    }
    let share = sac.address();
    let usdc = env.register_stellar_asset_contract_v2(Address::generate(env)).address();
    let treasury = Address::generate(env);
    StellarAssetClient::new(env, &usdc).mint(&treasury, &(10_000_000 * ONE));
    let reg = env.register(Compliance, (ops.clone(), share.clone()));
    StellarAssetClient::new(env, &share).set_admin(&reg);
    let dist = env.register(Distribution, (ops.clone(), reg.clone(), usdc.clone(), treasury.clone()));
    let vault = Address::generate(env);
    let r = ComplianceClient::new(env, &reg);
    r.bind(&vault, &dist);
    r.set_jurisdiction(&Symbol::new(env, "FR"), &true);
    Fx {
        reg,
        dist,
        usdc,
        treasury,
    }
}

/// Register a holder with one cash address; returns (holder, cash).
fn holder(env: &Env, fx: &Fx, expiry: u64) -> (Address, Address) {
    let h = Address::generate(env);
    let cash = Address::generate(env);
    let mut v = Vec::new(env);
    v.push_back(cash.clone());
    fx.r(env).set_investor(
        &h,
        &RegInvestor {
            kyc_expiry: expiry,
            jurisdiction: Symbol::new(env, "FR"),
            investor_type: 0,
            cash_addresses: v,
            frozen: false,
        },
    );
    (h, cash)
}

fn memo(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[3u8; 32])
}

#[test]
fn accrual_is_proportional() {
    let env = Env::default();
    let fx = setup(&env);
    let (a, ca) = holder(&env, &fx, NOW + YEAR);
    let (b, _) = holder(&env, &fx, NOW + YEAR);
    fx.r(&env).issue(&a, &(30 * ONE));
    fx.r(&env).issue(&b, &(10 * ONE));
    let before = fx.usdc_of(&env, &fx.treasury);
    let per_share = fx.d(&env).declare(&(100 * ONE), &memo(&env));
    assert_eq!(per_share, 100 * ONE * SCALE / (40 * ONE));
    assert_eq!(fx.usdc_of(&env, &fx.treasury), before - 100 * ONE);
    assert_eq!(fx.d(&env).accrued(&a), 75 * ONE);
    assert_eq!(fx.d(&env).accrued(&b), 25 * ONE);
    assert_eq!(fx.d(&env).claim(&a, &ca), 75 * ONE);
    assert_eq!(fx.usdc_of(&env, &ca), 75 * ONE);
    assert_eq!(fx.d(&env).accrued(&a), 0);
    assert_eq!(fx.d(&env).claimed(), 75 * ONE);
    assert_eq!(fx.d(&env).declared(), 100 * ONE);
}

#[test]
fn accrual_follows_the_holder_across_a_transfer() {
    let env = Env::default();
    let fx = setup(&env);
    let (a, _) = holder(&env, &fx, NOW + YEAR);
    let (b, _) = holder(&env, &fx, NOW + YEAR);
    fx.r(&env).issue(&a, &(50 * ONE));
    fx.r(&env).issue(&b, &(50 * ONE));
    fx.d(&env).declare(&(10 * ONE), &memo(&env));
    // A sends everything to B after the declaration: A keeps its entitlement.
    fx.r(&env).transfer(&a, &b, &(50 * ONE));
    assert_eq!(fx.d(&env).accrued(&a), 5 * ONE);
    assert_eq!(fx.d(&env).accrued(&b), 5 * ONE);
    // B accrues on the full 100 only from the next declaration.
    fx.d(&env).declare(&(10 * ONE), &memo(&env));
    assert_eq!(fx.d(&env).accrued(&a), 5 * ONE);
    assert_eq!(fx.d(&env).accrued(&b), 15 * ONE);
}

#[test]
fn accrual_survives_forced_transfer_and_redemption_burn() {
    let env = Env::default();
    let fx = setup(&env);
    let (lost, _) = holder(&env, &fx, NOW + YEAR);
    let (fresh, _) = holder(&env, &fx, NOW + YEAR);
    let (red, red_cash) = holder(&env, &fx, NOW + YEAR);
    fx.r(&env).issue(&lost, &(20 * ONE));
    fx.r(&env).issue(&red, &(20 * ONE));
    fx.d(&env).declare(&(4 * ONE), &memo(&env));
    fx.r(&env).forced_transfer(&lost, &fresh, &(20 * ONE), &memo(&env));
    fx.r(&env).lock(&red, &(20 * ONE), &red_cash);
    fx.r(&env).redeem_burn(&red, &(20 * ONE));
    assert_eq!(fx.r(&env).total_shares(), 20 * ONE);
    assert_eq!(fx.d(&env).accrued(&lost), 2 * ONE);
    assert_eq!(fx.d(&env).accrued(&red), 2 * ONE);
    assert_eq!(fx.d(&env).accrued(&fresh), 0);
    fx.d(&env).declare(&(4 * ONE), &memo(&env));
    assert_eq!(fx.d(&env).accrued(&fresh), 4 * ONE);
    assert_eq!(fx.d(&env).accrued(&red), 2 * ONE);
    // A fully redeemed holder can still claim what it earned while holding.
    assert_eq!(fx.d(&env).claim(&red, &red_cash), 2 * ONE);
}

#[test]
fn remainder_is_carried_across_declarations() {
    let env = Env::default();
    let fx = setup(&env);
    let mut hs = std::vec::Vec::new();
    for _ in 0..3 {
        let (h, _) = holder(&env, &fx, NOW + YEAR);
        fx.r(&env).issue(&h, &ONE);
        hs.push(h);
    }
    // 1 stroop over 3 shares: per-share floors, the rest is carried.
    fx.d(&env).declare(&1, &memo(&env));
    let carry1 = fx.d(&env).carry_scaled();
    assert!(carry1 > 0);
    assert_eq!(fx.d(&env).acc() * 3 * ONE + carry1, SCALE);
    fx.d(&env).declare(&2, &memo(&env));
    // After 3 stroops over 3 equal holders, each is owed exactly 1.
    for h in &hs {
        assert_eq!(fx.d(&env).accrued(h), 1);
    }
    assert_eq!(fx.d(&env).acc() * 3 * ONE + fx.d(&env).carry_scaled(), 3 * SCALE);
}

#[test]
fn claim_rejections() {
    let env = Env::default();
    let fx = setup(&env);
    let (a, ca) = holder(&env, &fx, NOW + YEAR);
    fx.r(&env).issue(&a, &(10 * ONE));
    assert_eq!(fx.d(&env).try_claim(&a, &ca), Err(Ok(DistError::NothingToClaim)));
    fx.d(&env).declare(&ONE, &memo(&env));
    let stranger = Address::generate(&env);
    assert_eq!(
        fx.d(&env).try_claim(&a, &stranger),
        Err(Ok(DistError::CashAddressNotAllowed))
    );
    // Claiming to the holder's own share wallet is not allowed either.
    assert_eq!(fx.d(&env).try_claim(&a, &a), Err(Ok(DistError::CashAddressNotAllowed)));
    fx.r(&env).set_frozen(&a, &true, &memo(&env));
    assert_eq!(fx.d(&env).try_claim(&a, &ca), Err(Ok(DistError::Frozen)));
    fx.r(&env).set_frozen(&a, &false, &memo(&env));
    assert_eq!(fx.d(&env).claim(&a, &ca), ONE);
}

#[test]
fn expired_holder_can_still_claim_to_its_cash_address() {
    let env = Env::default();
    let fx = setup(&env);
    let (a, ca) = holder(&env, &fx, NOW + 60);
    fx.r(&env).issue(&a, &(10 * ONE));
    fx.d(&env).declare(&ONE, &memo(&env));
    env.ledger().set_timestamp(NOW + 3_600);
    assert!(!fx.r(&env).status(&a).kyc_valid);
    assert_eq!(fx.d(&env).claim(&a, &ca), ONE);
}

#[test]
fn declare_rejections() {
    let env = Env::default();
    let fx = setup(&env);
    assert_eq!(fx.d(&env).try_declare(&ONE, &memo(&env)), Err(Ok(DistError::NoSupply)));
    let (a, _) = holder(&env, &fx, NOW + YEAR);
    fx.r(&env).issue(&a, &ONE);
    assert_eq!(fx.d(&env).try_declare(&0, &memo(&env)), Err(Ok(DistError::InvalidAmount)));
}

#[test]
fn declare_needs_ops_and_treasury() {
    let env = Env::default();
    let fx = setup(&env);
    let (a, _) = holder(&env, &fx, NOW + YEAR);
    fx.r(&env).issue(&a, &ONE);
    fx.d(&env).declare(&ONE, &memo(&env));
    let auths = env.auths();
    let who: std::vec::Vec<Address> = auths.iter().map(|(a, _)| a.clone()).collect();
    let ops = fx.r(&env).ops();
    assert!(who.contains(&ops));
    assert!(who.contains(&fx.treasury));
    env.set_auths(&[]);
    assert!(fx.d(&env).try_declare(&ONE, &memo(&env)).is_err());
}

#[test]
fn claim_needs_holder_auth() {
    let env = Env::default();
    let fx = setup(&env);
    let (a, ca) = holder(&env, &fx, NOW + YEAR);
    fx.r(&env).issue(&a, &ONE);
    fx.d(&env).declare(&ONE, &memo(&env));
    env.set_auths(&[]);
    assert!(fx.d(&env).try_claim(&a, &ca).is_err());
    env.mock_all_auths();
    fx.d(&env).claim(&a, &ca);
    assert_eq!(env.auths()[0].0, a);
}

#[test]
fn on_change_is_registrar_only() {
    let env = Env::default();
    let fx = setup(&env);
    let h = Address::generate(&env);
    env.set_auths(&[]);
    assert!(fx.d(&env).try_on_change(&h, &(1_000 * ONE)).is_err());
    env.mock_all_auths();
    fx.d(&env).on_change(&h, &0);
    assert_eq!(env.auths()[0].0, fx.reg);
}

#[test]
fn claim_for_pays_first_cash_address_and_skips() {
    let env = Env::default();
    let fx = setup(&env);
    let (a, ca) = holder(&env, &fx, NOW + YEAR);
    let (b, cb) = holder(&env, &fx, NOW + YEAR);
    let (c, cc) = holder(&env, &fx, NOW + YEAR);
    for h in [&a, &b, &c] {
        fx.r(&env).issue(h, &(10 * ONE));
    }
    fx.d(&env).declare(&(3 * ONE), &memo(&env));
    fx.r(&env).set_frozen(&c, &true, &memo(&env));
    let nobody = Address::generate(&env);
    let paid = fx
        .d(&env)
        .claim_for(&soroban_sdk::vec![&env, a.clone(), b.clone(), c.clone(), nobody]);
    assert_eq!(paid, 2 * ONE);
    assert_eq!(fx.usdc_of(&env, &ca), ONE);
    assert_eq!(fx.usdc_of(&env, &cb), ONE);
    assert_eq!(fx.usdc_of(&env, &cc), 0);
    assert_eq!(fx.d(&env).accrued(&c), ONE);
}

#[test]
fn claim_for_batch_limit_and_auth() {
    let env = Env::default();
    let fx = setup(&env);
    let mut v = Vec::new(&env);
    for _ in 0..26 {
        v.push_back(Address::generate(&env));
    }
    assert_eq!(fx.d(&env).try_claim_for(&v), Err(Ok(DistError::BatchTooLarge)));
    v.pop_back();
    fx.d(&env).claim_for(&v);
    env.set_auths(&[]);
    assert!(fx.d(&env).try_claim_for(&v).is_err());
}

// ---------------------------------------------------------------------------
// property test: random issue / transfer / forced / burn / declare / claim
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Op {
    Issue(usize, i128),
    Transfer(usize, usize, i128),
    Forced(usize, usize, i128),
    Burn(usize, i128),
    Declare(i128),
    Claim(usize),
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0usize..8, 1i128..5_000_000_000_000).prop_map(|(h, a)| Op::Issue(h, a)),
        (0usize..8, 0usize..8, 1i128..3_000_000_000_000).prop_map(|(a, b, x)| Op::Transfer(a, b, x)),
        (0usize..8, 0usize..8, 1i128..3_000_000_000_000).prop_map(|(a, b, x)| Op::Forced(a, b, x)),
        (0usize..8, 1i128..3_000_000_000_000).prop_map(|(h, a)| Op::Burn(h, a)),
        (1i128..200_000_000_000).prop_map(Op::Declare),
        (0usize..8).prop_map(Op::Claim),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn accumulator_invariants(n in 2usize..=8, ops in proptest::collection::vec(op_strategy(), 4..40)) {
        let env = Env::default();
        let fx = setup(&env);
        let hs: std::vec::Vec<(Address, Address)> = (0..n).map(|_| holder(&env, &fx, NOW + YEAR)).collect();
        let r = fx.r(&env);
        let d = fx.d(&env);
        let share = TokenClient::new(&env, &r.share());
        for op in ops {
            match op {
                Op::Issue(h, a) => { let _ = r.try_issue(&hs[h % n].0, &a); }
                Op::Transfer(a, b, x) => { let _ = r.try_transfer(&hs[a % n].0, &hs[b % n].0, &x); }
                Op::Forced(a, b, x) => { let _ = r.try_forced_transfer(&hs[a % n].0, &hs[b % n].0, &x, &memo(&env)); }
                Op::Burn(h, a) => {
                    let (who, cash) = &hs[h % n];
                    if r.try_lock(who, &a, cash).is_ok() {
                        r.redeem_burn(who, &a);
                    }
                }
                Op::Declare(a) => { let _ = d.try_declare(&a, &memo(&env)); }
                Op::Claim(h) => { let (who, cash) = &hs[h % n]; let _ = d.try_claim(who, cash); }
            }
        }
        let declared = d.declared();
        let claimed = d.claimed();
        let accrued: i128 = hs.iter().map(|(h, _)| d.accrued(h)).sum();
        prop_assert!(claimed + accrued <= declared);
        prop_assert!(declared - (claimed + accrued) <= n as i128 + 1);
        // The contract holds exactly what is still owed plus rounding dust.
        prop_assert_eq!(fx.usdc_of(&env, &fx.dist), declared - claimed);
        let sum_bal: i128 = hs.iter().map(|(h, _)| share.balance(h)).sum();
        prop_assert_eq!(r.total_shares(), sum_bal);
    }
}
