#![cfg(test)]
extern crate std;

use super::*;
use soroban_sdk::{
    contract, contractimpl, symbol_short,
    testutils::{
        Address as _, AuthorizedFunction, AuthorizedInvocation, Events as _, IssuerFlags, Ledger as _,
    },
    token::{StellarAssetClient, TokenClient},
    vec, Address, BytesN, Env, IntoVal, Symbol,
};

/// 1 share = 10^7 stroops (7 decimals, classic asset).
const ONE: i128 = 10_000_000;
const NOW: u64 = 1_791_205_200; // 2026-10-05T13:00:00Z (seed epoch-1 cut-off)
const YEAR: u64 = 365 * 86_400;

/// Stand-in for `distribution`: counts `on_change` calls and remembers the
/// last reported balance, so the registrar's notifications can be asserted.
#[contract]
pub struct Hook;

#[contractimpl]
impl Hook {
    pub fn on_change(env: Env, holder: Address, old_balance: i128) {
        let n: u32 = env.storage().instance().get(&symbol_short!("n")).unwrap_or(0);
        env.storage().instance().set(&symbol_short!("n"), &(n + 1));
        env.storage().instance().set(&holder, &old_balance);
    }
    pub fn calls(env: Env) -> u32 {
        env.storage().instance().get(&symbol_short!("n")).unwrap_or(0)
    }
    pub fn last(env: Env, holder: Address) -> i128 {
        env.storage().instance().get(&holder).unwrap_or(-1)
    }
}

struct Fx {
    ops: Address,
    share: Address,
    reg: Address,
    vault: Address,
    hook: Address,
}

impl Fx {
    fn c<'a>(&self, env: &'a Env) -> ComplianceClient<'a> {
        ComplianceClient::new(env, &self.reg)
    }
}

/// Deployment order from ARCHITECTURE.md: issuer flags first, then SAC
/// admin handed to the registrar, then bind.
fn setup_with_flags(env: &Env, clawback: bool) -> Fx {
    env.mock_all_auths();
    env.ledger().set_timestamp(NOW);
    let ops = Address::generate(env);
    let issuer_admin = Address::generate(env);
    let sac = env.register_stellar_asset_contract_v2(issuer_admin);
    sac.issuer().set_flag(IssuerFlags::RequiredFlag);
    sac.issuer().set_flag(IssuerFlags::RevocableFlag);
    if clawback {
        sac.issuer().set_flag(IssuerFlags::ClawbackEnabledFlag);
    }
    let share = sac.address();
    let reg = env.register(Compliance, (ops.clone(), share.clone()));
    StellarAssetClient::new(env, &share).set_admin(&reg);
    let vault = Address::generate(env);
    let hook = env.register(Hook, ());
    ComplianceClient::new(env, &reg).bind(&vault, &hook);
    let c = ComplianceClient::new(env, &reg);
    for code in ["FR", "DE", "LU"] {
        c.set_jurisdiction(&Symbol::new(env, code), &true);
    }
    Fx {
        ops,
        share,
        reg,
        vault,
        hook,
    }
}

fn setup(env: &Env) -> Fx {
    setup_with_flags(env, true)
}

fn inv(env: &Env, expiry: u64, code: &str, cash: &[&Address]) -> Investor {
    let mut v = Vec::new(env);
    for a in cash {
        v.push_back((*a).clone());
    }
    Investor {
        kyc_expiry: expiry,
        jurisdiction: Symbol::new(env, code),
        investor_type: 0,
        cash_addresses: v,
        frozen: false,
    }
}

/// Register a holder valid for a year in `code`, return (holder, cash address).
fn holder(env: &Env, fx: &Fx, code: &str) -> (Address, Address) {
    let h = Address::generate(env);
    let cash = Address::generate(env);
    fx.c(env).set_investor(&h, &inv(env, NOW + YEAR, code, &[&cash]));
    (h, cash)
}

fn reason(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[7u8; 32])
}

fn is_deauthorised(env: &Env, fx: &Fx, a: &Address) -> bool {
    !StellarAssetClient::new(env, &fx.share).authorized(a)
}

// ---------------------------------------------------------------------------
// set_investor / set_jurisdiction / set_frozen / bind
// ---------------------------------------------------------------------------

#[test]
fn set_investor_stores_and_emits() {
    let env = Env::default();
    let fx = setup(&env);
    let h = Address::generate(&env);
    let cash = Address::generate(&env);
    let rec = inv(&env, NOW + YEAR, "FR", &[&cash]);
    fx.c(&env).set_investor(&h, &rec);
    assert_eq!(env.events().all().filter_by_contract(&fx.reg).events().len(), 1);
    assert_eq!(fx.c(&env).investor(&h), Some(rec));
    assert!(fx.c(&env).is_cash_address(&h, &cash));
    assert!(!fx.c(&env).is_cash_address(&h, &h));
    assert_eq!(fx.c(&env).investor(&cash), None);
}

#[test]
fn set_investor_validates_cash_addresses_and_code() {
    let env = Env::default();
    let fx = setup(&env);
    let h = Address::generate(&env);
    let a = Address::generate(&env);
    let c = fx.c(&env);
    assert_eq!(
        c.try_set_investor(&h, &inv(&env, NOW + YEAR, "FR", &[])),
        Err(Ok(ComplianceError::TooManyCashAddresses))
    );
    assert_eq!(
        c.try_set_investor(&h, &inv(&env, NOW + YEAR, "FR", &[&a, &a, &a, &a])),
        Err(Ok(ComplianceError::TooManyCashAddresses))
    );
    for bad in ["fr", "FRA", "F", "F1", "De"] {
        assert_eq!(
            c.try_set_investor(&h, &inv(&env, NOW + YEAR, bad, &[&a])),
            Err(Ok(ComplianceError::BadJurisdiction)),
            "{bad}"
        );
    }
    // Three cash addresses (the entity case) are fine.
    let b = Address::generate(&env);
    let d = Address::generate(&env);
    c.set_investor(&h, &inv(&env, NOW + YEAR, "LU", &[&a, &b, &d]));
}

#[test]
fn set_investor_requires_ops() {
    let env = Env::default();
    let fx = setup(&env);
    env.set_auths(&[]);
    let h = Address::generate(&env);
    let rec = inv(&env, NOW + YEAR, "FR", &[&h]);
    assert!(fx.c(&env).try_set_investor(&h, &rec).is_err());
    env.mock_all_auths();
    fx.c(&env).set_investor(&h, &rec);
    assert_eq!(env.auths()[0].0, fx.ops);
}

#[test]
fn set_jurisdiction_validates_and_requires_ops() {
    let env = Env::default();
    let fx = setup(&env);
    let c = fx.c(&env);
    assert_eq!(
        c.try_set_jurisdiction(&Symbol::new(&env, "usa"), &true),
        Err(Ok(ComplianceError::BadJurisdiction))
    );
    assert!(!c.jurisdiction(&Symbol::new(&env, "US")));
    env.set_auths(&[]);
    assert!(c.try_set_jurisdiction(&Symbol::new(&env, "US"), &true).is_err());
    assert!(!c.jurisdiction(&Symbol::new(&env, "US")));
}

#[test]
fn set_frozen_requires_registration_and_ops() {
    let env = Env::default();
    let fx = setup(&env);
    let c = fx.c(&env);
    let stranger = Address::generate(&env);
    assert_eq!(
        c.try_set_frozen(&stranger, &true, &reason(&env)),
        Err(Ok(ComplianceError::NotRegistered))
    );
    let (h, _) = holder(&env, &fx, "FR");
    c.set_frozen(&h, &true, &reason(&env));
    assert!(c.status(&h).frozen);
    env.set_auths(&[]);
    assert!(c.try_set_frozen(&h, &false, &reason(&env)).is_err());
    assert!(c.status(&h).frozen);
}

#[test]
fn bind_is_once_and_ops_only() {
    let env = Env::default();
    let fx = setup(&env);
    let c = fx.c(&env);
    assert_eq!(
        c.try_bind(&fx.vault, &fx.hook),
        Err(Ok(ComplianceError::AlreadyBound))
    );
    // A fresh registrar cannot be bound without ops auth.
    let reg2 = env.register(Compliance, (fx.ops.clone(), fx.share.clone()));
    env.set_auths(&[]);
    assert!(ComplianceClient::new(&env, &reg2).try_bind(&fx.vault, &fx.hook).is_err());
}

#[test]
fn unbound_registrar_cannot_issue() {
    let env = Env::default();
    let fx = setup(&env);
    let reg2 = env.register(Compliance, (fx.ops.clone(), fx.share.clone()));
    let h = Address::generate(&env);
    assert_eq!(
        ComplianceClient::new(&env, &reg2).try_issue(&h, &ONE),
        Err(Ok(ComplianceError::NotBound))
    );
}

// ---------------------------------------------------------------------------
// eligibility matrix
// ---------------------------------------------------------------------------

#[test]
fn eligibility_matrix() {
    let env = Env::default();
    let fx = setup(&env);
    let c = fx.c(&env);
    let unregistered = Address::generate(&env);
    let st = c.status(&unregistered);
    assert!(!st.registered && !st.can_receive && !st.can_send && !st.can_redeem);

    for expired in [false, true] {
        for allowed in [false, true] {
            for frozen in [false, true] {
                let h = Address::generate(&env);
                let expiry = if expired { NOW } else { NOW + 1 }; // expiry must be > now
                let mut rec = inv(&env, expiry, if allowed { "DE" } else { "US" }, &[&h]);
                rec.frozen = frozen;
                c.set_investor(&h, &rec);
                let st = c.status(&h);
                assert!(st.registered);
                assert_eq!(st.kyc_valid, !expired);
                assert_eq!(st.jurisdiction_ok, allowed);
                assert_eq!(st.frozen, frozen);
                assert_eq!(st.can_receive, !expired && allowed && !frozen, "{expired} {allowed} {frozen}");
                assert_eq!(st.can_send, !expired && !frozen);
                assert_eq!(st.can_redeem, !frozen);
            }
        }
    }
}

#[test]
fn eligibility_changes_with_ledger_time() {
    let env = Env::default();
    let fx = setup(&env);
    let h = Address::generate(&env);
    fx.c(&env).set_investor(&h, &inv(&env, NOW + 3_600, "FR", &[&h]));
    assert!(fx.c(&env).status(&h).can_receive);
    env.ledger().set_timestamp(NOW + 3_600);
    let st = fx.c(&env).status(&h);
    assert!(!st.can_receive && !st.can_send && st.can_redeem);
}

// ---------------------------------------------------------------------------
// transfer
// ---------------------------------------------------------------------------

#[test]
fn transfer_between_eligible_holders_leaves_both_deauthorised() {
    let env = Env::default();
    let fx = setup(&env);
    let c = fx.c(&env);
    let (a, _) = holder(&env, &fx, "FR");
    let (b, _) = holder(&env, &fx, "DE");
    c.issue(&a, &(100 * ONE));
    let calls_before = HookClient::new(&env, &fx.hook).calls();
    c.transfer(&a, &b, &(40 * ONE));
    assert_eq!(c.balance(&a), 60 * ONE);
    assert_eq!(c.balance(&b), 40 * ONE);
    assert_eq!(c.total_shares(), 100 * ONE);
    // Both holders were reported with their balances before the move.
    let hook = HookClient::new(&env, &fx.hook);
    assert_eq!(hook.calls(), calls_before + 2);
    assert_eq!(hook.last(&a), 100 * ONE);
    assert_eq!(hook.last(&b), 0);
    // The proof the gate cannot be bypassed: both balances are deauthorised
    // and a direct SAC transfer fails even with the holder's signature.
    assert!(is_deauthorised(&env, &fx, &a));
    assert!(is_deauthorised(&env, &fx, &b));
    let token = TokenClient::new(&env, &fx.share);
    assert!(token.try_transfer(&a, &b, &ONE).is_err());
    assert!(token.try_transfer(&b, &a, &ONE).is_err());
    assert_eq!(c.balance(&a), 60 * ONE);
}

#[test]
fn transfer_is_authorised_by_the_sender() {
    let env = Env::default();
    let fx = setup(&env);
    let c = fx.c(&env);
    let (a, _) = holder(&env, &fx, "FR");
    let (b, _) = holder(&env, &fx, "DE");
    c.issue(&a, &(10 * ONE));
    c.transfer(&a, &b, &ONE);
    let auths = env.auths();
    assert_eq!(auths.len(), 1);
    assert_eq!(auths[0].0, a);
    match &auths[0].1.function {
        AuthorizedFunction::Contract((addr, f, args)) => {
            assert_eq!(addr, &fx.reg);
            assert_eq!(f, &Symbol::new(&env, "transfer"));
            assert_eq!(args, &(a.clone(), b.clone(), ONE).into_val(&env));
        }
        _ => panic!("unexpected auth"),
    }
    // Without the sender's signature it fails.
    env.set_auths(&[]);
    assert!(c.try_transfer(&a, &b, &ONE).is_err());
}

#[test]
fn transfer_rejections() {
    let env = Env::default();
    let fx = setup(&env);
    let c = fx.c(&env);
    let (a, _) = holder(&env, &fx, "FR");
    let (ok, _) = holder(&env, &fx, "DE");
    c.issue(&a, &(10 * ONE));
    // to an expired holder
    let expired = Address::generate(&env);
    c.set_investor(&expired, &inv(&env, NOW - 1, "FR", &[&expired]));
    assert_eq!(c.try_transfer(&a, &expired, &ONE), Err(Ok(ComplianceError::KycExpired)));
    // to a blocked jurisdiction
    let (us, _) = holder(&env, &fx, "US");
    assert_eq!(c.try_transfer(&a, &us, &ONE), Err(Ok(ComplianceError::JurisdictionBlocked)));
    // to an unregistered address
    let nobody = Address::generate(&env);
    assert_eq!(c.try_transfer(&a, &nobody, &ONE), Err(Ok(ComplianceError::NotRegistered)));
    // to a frozen holder
    let (fz, _) = holder(&env, &fx, "FR");
    c.set_frozen(&fz, &true, &reason(&env));
    assert_eq!(c.try_transfer(&a, &fz, &ONE), Err(Ok(ComplianceError::Frozen)));
    // same address, zero amount
    assert_eq!(c.try_transfer(&a, &a, &ONE), Err(Ok(ComplianceError::SameAddress)));
    assert_eq!(c.try_transfer(&a, &ok, &0), Err(Ok(ComplianceError::InvalidAmount)));
    // more than the balance
    assert_eq!(
        c.try_transfer(&a, &ok, &(11 * ONE)),
        Err(Ok(ComplianceError::InsufficientUnlocked))
    );
    // from a frozen holder
    c.set_frozen(&a, &true, &reason(&env));
    assert_eq!(c.try_transfer(&a, &ok, &ONE), Err(Ok(ComplianceError::Frozen)));
    c.set_frozen(&a, &false, &reason(&env));
    // from an expired holder (time passes)
    let (late, _) = holder(&env, &fx, "FR");
    c.issue(&late, &ONE);
    c.set_investor(&late, &inv(&env, NOW + 60, "FR", &[&late]));
    env.ledger().set_timestamp(NOW + 60);
    assert_eq!(c.try_transfer(&late, &ok, &ONE), Err(Ok(ComplianceError::KycExpired)));
    // a jurisdiction blocked after onboarding stops incoming transfers
    c.set_jurisdiction(&Symbol::new(&env, "DE"), &false);
    assert_eq!(c.try_transfer(&a, &ok, &ONE), Err(Ok(ComplianceError::JurisdictionBlocked)));
    assert_eq!(c.balance(&a), 10 * ONE);
}

#[test]
fn locked_shares_cannot_be_transferred() {
    let env = Env::default();
    let fx = setup(&env);
    let c = fx.c(&env);
    let (a, cash) = holder(&env, &fx, "FR");
    let (b, _) = holder(&env, &fx, "DE");
    c.issue(&a, &(10 * ONE));
    c.lock(&a, &(7 * ONE), &cash);
    assert_eq!(c.locked(&a), 7 * ONE);
    assert_eq!(
        c.try_transfer(&a, &b, &(4 * ONE)),
        Err(Ok(ComplianceError::InsufficientUnlocked))
    );
    c.transfer(&a, &b, &(3 * ONE));
    c.unlock(&a, &(7 * ONE));
    c.transfer(&a, &b, &(4 * ONE));
    assert_eq!(c.balance(&b), 7 * ONE);
}

// ---------------------------------------------------------------------------
// vault-only functions
// ---------------------------------------------------------------------------

#[test]
fn issue_mints_deauthorised_and_is_vault_only() {
    let env = Env::default();
    let fx = setup(&env);
    let c = fx.c(&env);
    let (a, _) = holder(&env, &fx, "FR");
    c.issue(&a, &(5 * ONE));
    assert_eq!(env.auths()[0].0, fx.vault);
    assert_eq!(c.total_shares(), 5 * ONE);
    assert!(is_deauthorised(&env, &fx, &a));
    env.set_auths(&[]);
    assert!(c.try_issue(&a, &ONE).is_err());
}

#[test]
fn issue_requires_registered_unfrozen_holder() {
    let env = Env::default();
    let fx = setup(&env);
    let c = fx.c(&env);
    let nobody = Address::generate(&env);
    assert_eq!(c.try_issue(&nobody, &ONE), Err(Ok(ComplianceError::NotRegistered)));
    let (a, _) = holder(&env, &fx, "FR");
    c.set_frozen(&a, &true, &reason(&env));
    assert_eq!(c.try_issue(&a, &ONE), Err(Ok(ComplianceError::Frozen)));
    assert_eq!(c.try_issue(&nobody, &0), Err(Ok(ComplianceError::InvalidAmount)));
}

#[test]
fn lock_checks_cash_address_freeze_and_balance() {
    let env = Env::default();
    let fx = setup(&env);
    let c = fx.c(&env);
    let (a, cash) = holder(&env, &fx, "FR");
    c.issue(&a, &(10 * ONE));
    let other = Address::generate(&env);
    assert_eq!(
        c.try_lock(&a, &ONE, &other),
        Err(Ok(ComplianceError::CashAddressNotAllowed))
    );
    assert_eq!(
        c.try_lock(&a, &(11 * ONE), &cash),
        Err(Ok(ComplianceError::InsufficientUnlocked))
    );
    c.set_frozen(&a, &true, &reason(&env));
    assert_eq!(c.try_lock(&a, &ONE, &cash), Err(Ok(ComplianceError::Frozen)));
    c.set_frozen(&a, &false, &reason(&env));
    // Expired KYC does not block a redemption lock.
    env.ledger().set_timestamp(NOW + 2 * YEAR);
    c.lock(&a, &(10 * ONE), &cash);
    assert_eq!(c.try_unlock(&a, &(11 * ONE)), Err(Ok(ComplianceError::InvalidAmount)));
}

#[test]
fn lock_unlock_and_burn_are_vault_only() {
    let env = Env::default();
    let fx = setup(&env);
    let c = fx.c(&env);
    let (a, cash) = holder(&env, &fx, "FR");
    c.issue(&a, &(10 * ONE));
    env.set_auths(&[]);
    assert!(c.try_lock(&a, &ONE, &cash).is_err());
    env.mock_all_auths();
    c.lock(&a, &(2 * ONE), &cash);
    assert_eq!(env.auths()[0].0, fx.vault);
    env.set_auths(&[]);
    assert!(c.try_unlock(&a, &ONE).is_err());
    assert!(c.try_redeem_burn(&a, &ONE).is_err());
    assert_eq!(c.locked(&a), 2 * ONE);
}

#[test]
fn redeem_burn_works_for_an_expired_deauthorised_holder() {
    let env = Env::default();
    let fx = setup(&env);
    let c = fx.c(&env);
    let a = Address::generate(&env);
    let cash = Address::generate(&env);
    c.set_investor(&a, &inv(&env, NOW + 3_600, "FR", &[&cash]));
    c.issue(&a, &(10 * ONE));
    env.ledger().set_timestamp(NOW + 7_200); // KYC now expired
    assert!(!c.status(&a).kyc_valid);
    c.lock(&a, &(10 * ONE), &cash);
    assert!(is_deauthorised(&env, &fx, &a));
    c.redeem_burn(&a, &(10 * ONE));
    assert_eq!(c.balance(&a), 0);
    assert_eq!(c.locked(&a), 0);
    assert_eq!(c.total_shares(), 0);
    assert_eq!(c.try_redeem_burn(&a, &ONE), Err(Ok(ComplianceError::InvalidAmount)));
}

#[test]
fn deployment_precondition_clawback_flag_must_precede_balances() {
    // If the issuer enables clawback only after a balance exists, that
    // balance is not clawback-able (the flag is fixed when the balance entry
    // is created), so redemptions and forced transfers fail for it. This is
    // why the deployment order sets all three flags before any holder exists.
    let env = Env::default();
    let fx = setup_with_flags(&env, false);
    let c = fx.c(&env);
    let (a, cash) = holder(&env, &fx, "FR");
    c.issue(&a, &(10 * ONE));
    c.lock(&a, &(10 * ONE), &cash);
    assert!(c.try_redeem_burn(&a, &(10 * ONE)).is_err());
    // A balance created after the flag is set would work; see the scenario.
    assert_eq!(c.balance(&a), 10 * ONE);
}

// ---------------------------------------------------------------------------
// forced transfer
// ---------------------------------------------------------------------------

#[test]
fn forced_transfer_preserves_supply_and_records_ops_auth() {
    let env = Env::default();
    let fx = setup(&env);
    let c = fx.c(&env);
    let (lost, _) = holder(&env, &fx, "FR");
    let (fresh, _) = holder(&env, &fx, "FR");
    c.issue(&lost, &(12_000 * ONE));
    let r = reason(&env);
    c.forced_transfer(&lost, &fresh, &(12_000 * ONE), &r);
    assert_eq!(
        env.auths(),
        std::vec![(
            fx.ops.clone(),
            AuthorizedInvocation {
                function: AuthorizedFunction::Contract((
                    fx.reg.clone(),
                    Symbol::new(&env, "forced_transfer"),
                    (lost.clone(), fresh.clone(), 12_000 * ONE, r.clone()).into_val(&env),
                )),
                sub_invocations: std::vec![],
            }
        )]
    );
    assert_eq!(c.balance(&lost), 0);
    assert_eq!(c.balance(&fresh), 12_000 * ONE);
    assert_eq!(c.total_shares(), 12_000 * ONE);
    assert!(is_deauthorised(&env, &fx, &fresh));
}

#[test]
fn forced_transfer_rejections() {
    let env = Env::default();
    let fx = setup(&env);
    let c = fx.c(&env);
    let (lost, cash) = holder(&env, &fx, "FR");
    let (us, _) = holder(&env, &fx, "US");
    let (fresh, _) = holder(&env, &fx, "FR");
    c.issue(&lost, &(10 * ONE));
    let r = reason(&env);
    assert_eq!(
        c.try_forced_transfer(&lost, &us, &ONE, &r),
        Err(Ok(ComplianceError::JurisdictionBlocked))
    );
    c.lock(&lost, &(6 * ONE), &cash);
    assert_eq!(
        c.try_forced_transfer(&lost, &fresh, &(5 * ONE), &r),
        Err(Ok(ComplianceError::InsufficientUnlocked))
    );
    // A stranger cannot force a transfer.
    env.set_auths(&[]);
    assert!(c.try_forced_transfer(&lost, &fresh, &ONE, &r).is_err());
}

// ---------------------------------------------------------------------------
// housekeeping
// ---------------------------------------------------------------------------

#[test]
fn extend_is_permissionless() {
    let env = Env::default();
    let fx = setup(&env);
    let (a, _) = holder(&env, &fx, "FR");
    env.set_auths(&[]);
    fx.c(&env).extend(&vec![&env, a.clone(), Address::generate(&env)]);
    assert!(env.auths().is_empty());
    assert_eq!(fx.c(&env).ops(), fx.ops);
    assert_eq!(fx.c(&env).share(), fx.share);
}
