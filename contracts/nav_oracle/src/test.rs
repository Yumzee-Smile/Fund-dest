#![cfg(test)]
extern crate std;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, AuthorizedFunction, AuthorizedInvocation, Events as _, Ledger as _, MockAuth, MockAuthInvoke},
    Address, Env, IntoVal, Symbol,
};

/// 14 decimals, Reflector's convention (seed: data/seed/fund.json).
const D14: i128 = 100_000_000_000_000;
const CUTOFF_E1: u64 = 1_791_205_200; // 2026-10-05T13:00:00Z

fn setup(env: &Env) -> (Address, Address, Asset) {
    let publisher = Address::generate(env);
    let usdc = Address::generate(env);
    let oracle = env.register(
        NavOracle,
        (publisher.clone(), Asset::Stellar(usdc), 14u32, 86_400u32),
    );
    env.ledger().set_timestamp(CUTOFF_E1 + 3_600);
    (oracle, publisher, Asset::Other(Symbol::new(env, "USD_D")))
}

#[test]
fn constructor_sets_sep40_metadata() {
    let env = Env::default();
    let (oracle, publisher, _) = setup(&env);
    let c = NavOracleClient::new(&env, &oracle);
    assert_eq!(c.decimals(), 14);
    assert_eq!(c.resolution(), 86_400);
    assert!(matches!(c.base(), Asset::Stellar(_)));
    assert_eq!(c.assets().len(), 0);
    assert_eq!(c.publisher(), publisher);
}

#[test]
fn publish_then_lastprice_and_price() {
    let env = Env::default();
    let (oracle, _, asset) = setup(&env);
    env.mock_all_auths();
    let c = NavOracleClient::new(&env, &oracle);
    assert_eq!(c.lastprice(&asset), None);
    c.publish(&asset, &D14, &(CUTOFF_E1 + 1_800));
    env.ledger().set_timestamp(CUTOFF_E1 + 86_400 + 3_600);
    let nav2 = 100_000_412_000_000i128; // 1.00000412
    c.publish(&asset, &nav2, &(CUTOFF_E1 + 86_400 + 3_600));
    // One Published event per call, emitted by the oracle.
    assert_eq!(env.events().all().filter_by_contract(&oracle).events().len(), 1);
    assert_eq!(
        c.lastprice(&asset),
        Some(PriceData { price: nav2, timestamp: CUTOFF_E1 + 86_400 + 3_600 })
    );
    assert_eq!(
        c.price(&asset, &(CUTOFF_E1 + 1_800)),
        Some(PriceData { price: D14, timestamp: CUTOFF_E1 + 1_800 })
    );
    assert_eq!(c.price(&asset, &(CUTOFF_E1 + 1_801)), None);
    assert_eq!(c.assets().len(), 1);
}

#[test]
fn publish_is_authorised_by_the_publisher_only() {
    let env = Env::default();
    let (oracle, publisher, asset) = setup(&env);
    let c = NavOracleClient::new(&env, &oracle);
    env.mock_all_auths();
    c.publish(&asset, &D14, &CUTOFF_E1);
    assert_eq!(
        env.auths(),
        std::vec![(
            publisher.clone(),
            AuthorizedInvocation {
                function: AuthorizedFunction::Contract((
                    oracle.clone(),
                    Symbol::new(&env, "publish"),
                    (asset.clone(), D14, CUTOFF_E1).into_val(&env),
                )),
                sub_invocations: std::vec![],
            }
        )]
    );
}

#[test]
fn publish_by_a_stranger_fails() {
    let env = Env::default();
    let (oracle, _, asset) = setup(&env);
    let c = NavOracleClient::new(&env, &oracle);
    let stranger = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &stranger,
        invoke: &MockAuthInvoke {
            contract: &oracle,
            fn_name: "publish",
            args: (asset.clone(), D14, CUTOFF_E1).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert!(c.try_publish(&asset, &D14, &CUTOFF_E1).is_err());
    assert_eq!(c.lastprice(&asset), None);
}

#[test]
fn non_positive_price_is_rejected() {
    let env = Env::default();
    let (oracle, _, asset) = setup(&env);
    env.mock_all_auths();
    let c = NavOracleClient::new(&env, &oracle);
    assert_eq!(c.try_publish(&asset, &0, &CUTOFF_E1), Err(Ok(OracleError::NonPositivePrice)));
    assert_eq!(c.try_publish(&asset, &-1, &CUTOFF_E1), Err(Ok(OracleError::NonPositivePrice)));
}

#[test]
fn future_timestamp_is_rejected() {
    let env = Env::default();
    let (oracle, _, asset) = setup(&env);
    env.mock_all_auths();
    let c = NavOracleClient::new(&env, &oracle);
    let now = env.ledger().timestamp();
    assert_eq!(c.try_publish(&asset, &D14, &(now + 1)), Err(Ok(OracleError::TimestampInFuture)));
    c.publish(&asset, &D14, &now);
}

#[test]
fn non_increasing_timestamp_is_rejected() {
    let env = Env::default();
    let (oracle, _, asset) = setup(&env);
    env.mock_all_auths();
    let c = NavOracleClient::new(&env, &oracle);
    c.publish(&asset, &D14, &CUTOFF_E1);
    assert_eq!(
        c.try_publish(&asset, &D14, &CUTOFF_E1),
        Err(Ok(OracleError::TimestampNotIncreasing))
    );
    assert_eq!(
        c.try_publish(&asset, &D14, &(CUTOFF_E1 - 60)),
        Err(Ok(OracleError::TimestampNotIncreasing))
    );
    // A different asset has its own sequence.
    let other = Asset::Other(Symbol::new(&env, "EUR_A"));
    c.publish(&other, &D14, &(CUTOFF_E1 - 60));
    assert_eq!(c.assets().len(), 2);
}
