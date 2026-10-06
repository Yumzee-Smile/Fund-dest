#![cfg(test)]
extern crate std;

use super::*;
use ed25519_dalek::{Signer, SigningKey};
use soroban_sdk::{
    auth::{ContractContext, CreateContractHostFnContext, ContractExecutable},
    testutils::{Address as _, AuthorizedFunction, AuthorizedInvocation, MockAuth, MockAuthInvoke},
    vec,
    xdr::{self, WriteXdr},
    Address, Bytes, BytesN, Env, IntoVal, Symbol, TryFromVal, Val, Vec,
};

struct Keys {
    ta1: SigningKey,
    ta2: SigningKey,
    admin: SigningKey,
}

fn keys() -> Keys {
    Keys {
        ta1: SigningKey::from_bytes(&[11u8; 32]),
        ta2: SigningKey::from_bytes(&[12u8; 32]),
        admin: SigningKey::from_bytes(&[21u8; 32]),
    }
}

fn pk(env: &Env, k: &SigningKey) -> BytesN<32> {
    BytesN::from_array(env, &k.verifying_key().to_bytes())
}

struct Fx {
    ops: Address,
    compliance: Address,
    vault: Address,
    oracle: Address,
    keys: Keys,
}

/// The seed policy table from data/seed/fund.json, one representative row of
/// every class (TA, ADMIN, TA+ADMIN, any one signer).
fn setup(env: &Env) -> Fx {
    let k = keys();
    let signers = vec![
        env,
        (pk(env, &k.ta1), Role::Ta),
        (pk(env, &k.ta2), Role::Ta),
        (pk(env, &k.admin), Role::Admin),
    ];
    let ops = env.register(OpsAccount, (signers,));
    let compliance = Address::generate(env);
    let vault = Address::generate(env);
    let oracle = Address::generate(env);
    let client = OpsAccountClient::new(env, &ops);
    env.mock_all_auths();
    let rows: [(&Address, &str, u32, u32, u32); 8] = [
        (&compliance, "set_investor", 1, 0, 1),
        (&compliance, "set_frozen", 1, 0, 1),
        (&compliance, "forced_transfer", 1, 1, 2),
        (&vault, "settle", 1, 0, 1),
        (&vault, "strike_nav", 0, 1, 1),
        (&vault, "strike_nav_override", 1, 1, 2),
        (&vault, "pause", 0, 0, 1),
        (&oracle, "publish", 0, 1, 1),
    ];
    for (c, f, ta, admin, total) in rows {
        client.set_policy(c, &Symbol::new(env, f), &Some(Policy { ta, admin, total }));
    }
    env.set_auths(&[]);
    Fx {
        ops,
        compliance,
        vault,
        oracle,
        keys: k,
    }
}

fn ctx(env: &Env, contract: &Address, f: &str) -> Vec<Context> {
    vec![
        env,
        Context::Contract(ContractContext {
            contract: contract.clone(),
            fn_name: Symbol::new(env, f),
            args: vec![env],
        }),
    ]
}

/// Sign `payload` with the given keys and return the signature vector sorted
/// by public key, as the app's `ops-auth.ts` does.
fn sign_sorted(env: &Env, payload: &BytesN<32>, ks: &[&SigningKey]) -> Vec<Sig> {
    let mut v: std::vec::Vec<(std::vec::Vec<u8>, Sig)> = ks
        .iter()
        .map(|k| {
            let key = k.verifying_key().to_bytes();
            (
                key.to_vec(),
                Sig {
                    key: BytesN::from_array(env, &key),
                    sig: BytesN::from_array(env, &k.sign(&payload.to_array()).to_bytes()),
                },
            )
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = Vec::new(env);
    for (_, s) in v {
        out.push_back(s);
    }
    out
}

fn check(
    env: &Env,
    fx: &Fx,
    sigs: Vec<Sig>,
    contexts: &Vec<Context>,
    payload: &BytesN<32>,
) -> Result<(), Result<OpsError, soroban_sdk::InvokeError>> {
    env.try_invoke_contract_check_auth::<OpsError>(&fx.ops, payload, sigs.into_val(env), contexts)
}

fn payload(env: &Env, n: u8) -> BytesN<32> {
    BytesN::from_array(env, &[n; 32])
}

// ---------------------------------------------------------------------------
// constructor, views, administration
// ---------------------------------------------------------------------------

#[test]
fn constructor_requires_one_signer_per_role() {
    let env = Env::default();
    let k = keys();
    let only_ta = vec![&env, (pk(&env, &k.ta1), Role::Ta)];
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.register(OpsAccount, (only_ta,));
    }));
    assert!(res.is_err());
}

#[test]
fn signers_and_policy_views() {
    let env = Env::default();
    let fx = setup(&env);
    let c = OpsAccountClient::new(&env, &fx.ops);
    let s = c.signers();
    assert_eq!(s.len(), 3);
    assert_eq!(s.get(pk(&env, &fx.keys.admin)), Some(Role::Admin));
    assert_eq!(
        c.policy(&fx.compliance, &Symbol::new(&env, "forced_transfer")),
        Some(Policy { ta: 1, admin: 1, total: 2 })
    );
    assert_eq!(c.policy(&fx.compliance, &Symbol::new(&env, "nope")), None);
    // Self-administration is hard-coded.
    assert_eq!(c.policy(&fx.ops, &Symbol::new(&env, "set_policy")), Some(SELF_POLICY));
}

#[test]
fn set_policy_requires_the_accounts_own_auth() {
    let env = Env::default();
    let fx = setup(&env);
    let c = OpsAccountClient::new(&env, &fx.ops);
    let target = Address::generate(&env);
    let p = Some(Policy { ta: 1, admin: 0, total: 1 });
    // Without any auth the call fails.
    assert!(c
        .try_set_policy(&target, &Symbol::new(&env, "declare"), &p)
        .is_err());
    // With the ops account's auth it succeeds, and the recorded auth is exactly it.
    env.mock_all_auths();
    c.set_policy(&target, &Symbol::new(&env, "declare"), &p);
    assert_eq!(
        env.auths(),
        std::vec![(
            fx.ops.clone(),
            AuthorizedInvocation {
                function: AuthorizedFunction::Contract((
                    fx.ops.clone(),
                    Symbol::new(&env, "set_policy"),
                    (target.clone(), Symbol::new(&env, "declare"), p).into_val(&env),
                )),
                sub_invocations: std::vec![],
            }
        )]
    );
}

#[test]
fn set_policy_rejects_unmeetable_rows_and_self_rows() {
    let env = Env::default();
    let fx = setup(&env);
    env.mock_all_auths();
    let c = OpsAccountClient::new(&env, &fx.ops);
    let f = Symbol::new(&env, "x");
    let t = Address::generate(&env);
    assert_eq!(
        c.try_set_policy(&t, &f, &Some(Policy { ta: 0, admin: 0, total: 0 })),
        Err(Ok(OpsError::InvalidPolicy))
    );
    assert_eq!(
        c.try_set_policy(&t, &f, &Some(Policy { ta: 1, admin: 1, total: 1 })),
        Err(Ok(OpsError::InvalidPolicy))
    );
    assert_eq!(
        c.try_set_policy(&fx.ops, &f, &Some(Policy { ta: 1, admin: 0, total: 1 })),
        Err(Ok(OpsError::InvalidPolicy))
    );
}

#[test]
fn set_policy_none_removes_the_row() {
    let env = Env::default();
    let fx = setup(&env);
    env.mock_all_auths();
    let c = OpsAccountClient::new(&env, &fx.ops);
    let f = Symbol::new(&env, "settle");
    c.set_policy(&fx.vault, &f, &None);
    assert_eq!(c.policy(&fx.vault, &f), None);
    env.set_auths(&[]);
    let p = payload(&env, 1);
    let sigs = sign_sorted(&env, &p, &[&fx.keys.ta1]);
    assert_eq!(
        check(&env, &fx, sigs, &ctx(&env, &fx.vault, "settle"), &p),
        Err(Ok(OpsError::NoPolicy))
    );
}

#[test]
fn set_signer_adds_rotates_and_protects_the_last_of_a_role() {
    let env = Env::default();
    let fx = setup(&env);
    env.mock_all_auths();
    let c = OpsAccountClient::new(&env, &fx.ops);
    // Removing the only ADMIN fails.
    assert_eq!(
        c.try_set_signer(&pk(&env, &fx.keys.admin), &None),
        Err(Ok(OpsError::LastSignerOfRole))
    );
    // Re-roling the only ADMIN to TA fails too.
    assert_eq!(
        c.try_set_signer(&pk(&env, &fx.keys.admin), &Some(Role::Ta)),
        Err(Ok(OpsError::LastSignerOfRole))
    );
    // Add a second admin, then the first can be removed (rotation).
    let admin2 = SigningKey::from_bytes(&[22u8; 32]);
    c.set_signer(&pk(&env, &admin2), &Some(Role::Admin));
    c.set_signer(&pk(&env, &fx.keys.admin), &None);
    assert_eq!(c.signers().len(), 3);
    // One TA can go, the last TA cannot.
    c.set_signer(&pk(&env, &fx.keys.ta2), &None);
    assert_eq!(
        c.try_set_signer(&pk(&env, &fx.keys.ta1), &None),
        Err(Ok(OpsError::LastSignerOfRole))
    );
}

#[test]
fn set_signer_without_auth_fails() {
    let env = Env::default();
    let fx = setup(&env);
    let c = OpsAccountClient::new(&env, &fx.ops);
    let stranger = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &stranger,
        invoke: &MockAuthInvoke {
            contract: &fx.ops,
            fn_name: "set_signer",
            args: (pk(&env, &fx.keys.ta1), Option::<Role>::None).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert!(c.try_set_signer(&pk(&env, &fx.keys.ta1), &None).is_err());
}

// ---------------------------------------------------------------------------
// __check_auth with real Ed25519 signatures, one test per policy class
// ---------------------------------------------------------------------------

#[test]
fn ta_row_accepts_one_ta_and_rejects_admin_only() {
    let env = Env::default();
    let fx = setup(&env);
    let p = payload(&env, 2);
    let c = ctx(&env, &fx.compliance, "set_investor");
    assert_eq!(check(&env, &fx, sign_sorted(&env, &p, &[&fx.keys.ta2]), &c, &p), Ok(()));
    assert_eq!(
        check(&env, &fx, sign_sorted(&env, &p, &[&fx.keys.admin]), &c, &p),
        Err(Ok(OpsError::InsufficientTa))
    );
}

#[test]
fn admin_row_accepts_admin_and_rejects_ta_only() {
    let env = Env::default();
    let fx = setup(&env);
    let p = payload(&env, 3);
    let c = ctx(&env, &fx.vault, "strike_nav");
    assert_eq!(check(&env, &fx, sign_sorted(&env, &p, &[&fx.keys.admin]), &c, &p), Ok(()));
    assert_eq!(
        check(&env, &fx, sign_sorted(&env, &p, &[&fx.keys.ta1, &fx.keys.ta2]), &c, &p),
        Err(Ok(OpsError::InsufficientAdmin))
    );
    let c2 = ctx(&env, &fx.oracle, "publish");
    assert_eq!(check(&env, &fx, sign_sorted(&env, &p, &[&fx.keys.admin]), &c2, &p), Ok(()));
}

#[test]
fn forced_transfer_needs_both_roles() {
    let env = Env::default();
    let fx = setup(&env);
    let p = payload(&env, 4);
    let c = ctx(&env, &fx.compliance, "forced_transfer");
    assert_eq!(
        check(&env, &fx, sign_sorted(&env, &p, &[&fx.keys.ta1]), &c, &p),
        Err(Ok(OpsError::InsufficientAdmin))
    );
    assert_eq!(
        check(&env, &fx, sign_sorted(&env, &p, &[&fx.keys.admin]), &c, &p),
        Err(Ok(OpsError::InsufficientTa))
    );
    // Two TA keys are still not an ADMIN.
    assert_eq!(
        check(&env, &fx, sign_sorted(&env, &p, &[&fx.keys.ta1, &fx.keys.ta2]), &c, &p),
        Err(Ok(OpsError::InsufficientAdmin))
    );
    assert_eq!(
        check(&env, &fx, sign_sorted(&env, &p, &[&fx.keys.ta1, &fx.keys.admin]), &c, &p),
        Ok(())
    );
}

#[test]
fn nav_override_needs_both_roles() {
    let env = Env::default();
    let fx = setup(&env);
    let p = payload(&env, 5);
    let c = ctx(&env, &fx.vault, "strike_nav_override");
    assert_eq!(
        check(&env, &fx, sign_sorted(&env, &p, &[&fx.keys.admin]), &c, &p),
        Err(Ok(OpsError::InsufficientTa))
    );
    assert_eq!(
        check(&env, &fx, sign_sorted(&env, &p, &[&fx.keys.ta2, &fx.keys.admin]), &c, &p),
        Ok(())
    );
}

#[test]
fn any_one_signer_row_accepts_either_role() {
    let env = Env::default();
    let fx = setup(&env);
    let p = payload(&env, 6);
    let c = ctx(&env, &fx.vault, "pause");
    assert_eq!(check(&env, &fx, sign_sorted(&env, &p, &[&fx.keys.ta1]), &c, &p), Ok(()));
    assert_eq!(check(&env, &fx, sign_sorted(&env, &p, &[&fx.keys.admin]), &c, &p), Ok(()));
}

#[test]
fn total_threshold_is_enforced() {
    let env = Env::default();
    let fx = setup(&env);
    env.mock_all_auths();
    let t = Address::generate(&env);
    OpsAccountClient::new(&env, &fx.ops).set_policy(
        &t,
        &Symbol::new(&env, "declare"),
        &Some(Policy { ta: 0, admin: 0, total: 2 }),
    );
    env.set_auths(&[]);
    let p = payload(&env, 7);
    let c = ctx(&env, &t, "declare");
    assert_eq!(
        check(&env, &fx, sign_sorted(&env, &p, &[&fx.keys.ta1]), &c, &p),
        Err(Ok(OpsError::InsufficientTotal))
    );
    assert_eq!(
        check(&env, &fx, sign_sorted(&env, &p, &[&fx.keys.ta1, &fx.keys.ta2]), &c, &p),
        Ok(())
    );
}

#[test]
fn every_context_must_pass_its_own_row() {
    let env = Env::default();
    let fx = setup(&env);
    let p = payload(&env, 8);
    let mut c = ctx(&env, &fx.compliance, "set_investor");
    c.push_back(Context::Contract(ContractContext {
        contract: fx.vault.clone(),
        fn_name: Symbol::new(&env, "strike_nav"),
        args: vec![&env],
    }));
    assert_eq!(
        check(&env, &fx, sign_sorted(&env, &p, &[&fx.keys.ta1]), &c, &p),
        Err(Ok(OpsError::InsufficientAdmin))
    );
    assert_eq!(
        check(&env, &fx, sign_sorted(&env, &p, &[&fx.keys.ta1, &fx.keys.admin]), &c, &p),
        Ok(())
    );
}

#[test]
fn self_administration_with_one_role_is_rejected() {
    let env = Env::default();
    let fx = setup(&env);
    let p = payload(&env, 9);
    let c = ctx(&env, &fx.ops, "set_policy");
    assert_eq!(
        check(&env, &fx, sign_sorted(&env, &p, &[&fx.keys.ta1, &fx.keys.ta2]), &c, &p),
        Err(Ok(OpsError::InsufficientAdmin))
    );
    assert_eq!(
        check(&env, &fx, sign_sorted(&env, &p, &[&fx.keys.admin]), &c, &p),
        Err(Ok(OpsError::InsufficientTa))
    );
    let c2 = ctx(&env, &fx.ops, "set_signer");
    assert_eq!(
        check(&env, &fx, sign_sorted(&env, &p, &[&fx.keys.ta1, &fx.keys.admin]), &c2, &p),
        Ok(())
    );
}

#[test]
fn duplicate_key_is_rejected_as_unsorted() {
    let env = Env::default();
    let fx = setup(&env);
    let p = payload(&env, 10);
    let one = sign_sorted(&env, &p, &[&fx.keys.ta1]).get(0).unwrap();
    let dup = vec![&env, one.clone(), one];
    assert_eq!(
        check(&env, &fx, dup, &ctx(&env, &fx.vault, "pause"), &p),
        Err(Ok(OpsError::SignersNotSorted))
    );
}

#[test]
fn descending_keys_are_rejected() {
    let env = Env::default();
    let fx = setup(&env);
    let p = payload(&env, 11);
    let sorted = sign_sorted(&env, &p, &[&fx.keys.ta1, &fx.keys.admin]);
    let reversed = vec![&env, sorted.get(1).unwrap(), sorted.get(0).unwrap()];
    assert_eq!(
        check(&env, &fx, reversed, &ctx(&env, &fx.compliance, "forced_transfer"), &p),
        Err(Ok(OpsError::SignersNotSorted))
    );
}

#[test]
fn unknown_key_is_rejected() {
    let env = Env::default();
    let fx = setup(&env);
    let p = payload(&env, 12);
    let stranger = SigningKey::from_bytes(&[99u8; 32]);
    assert_eq!(
        check(&env, &fx, sign_sorted(&env, &p, &[&stranger]), &ctx(&env, &fx.vault, "pause"), &p),
        Err(Ok(OpsError::UnknownSigner))
    );
}

#[test]
fn bad_signature_is_a_host_error() {
    let env = Env::default();
    let fx = setup(&env);
    let p = payload(&env, 13);
    let other = payload(&env, 14);
    // TA signs a different payload.
    let sigs = sign_sorted(&env, &other, &[&fx.keys.ta1]);
    let res = check(&env, &fx, sigs, &ctx(&env, &fx.vault, "pause"), &p);
    assert!(matches!(res, Err(Err(_))), "expected a host error, got {res:?}");
}

#[test]
fn empty_signature_vector_is_rejected() {
    let env = Env::default();
    let fx = setup(&env);
    let p = payload(&env, 15);
    assert_eq!(
        check(&env, &fx, Vec::new(&env), &ctx(&env, &fx.vault, "pause"), &p),
        Err(Ok(OpsError::EmptySignatures))
    );
}

#[test]
fn missing_policy_row_is_rejected_so_tokens_cannot_move() {
    let env = Env::default();
    let fx = setup(&env);
    let p = payload(&env, 16);
    let token = Address::generate(&env);
    let sigs = sign_sorted(&env, &p, &[&fx.keys.ta1, &fx.keys.ta2, &fx.keys.admin]);
    assert_eq!(
        check(&env, &fx, sigs, &ctx(&env, &token, "transfer"), &p),
        Err(Ok(OpsError::NoPolicy))
    );
}

#[test]
fn create_contract_context_is_foreign() {
    let env = Env::default();
    let fx = setup(&env);
    let p = payload(&env, 17);
    let c = vec![
        &env,
        Context::CreateContractHostFn(CreateContractHostFnContext {
            executable: ContractExecutable::Wasm(BytesN::from_array(&env, &[1u8; 32])),
            salt: BytesN::from_array(&env, &[2u8; 32]),
        }),
    ];
    let sigs = sign_sorted(&env, &p, &[&fx.keys.ta1, &fx.keys.admin]);
    assert_eq!(check(&env, &fx, sigs, &c, &p), Err(Ok(OpsError::ForeignContext)));
}

// ---------------------------------------------------------------------------
// end to end through the host with a signed SorobanAuthorizationEntry
// ---------------------------------------------------------------------------

fn signed_entry(
    env: &Env,
    ops: &Address,
    ks: &[&SigningKey],
    fn_name: &str,
    args: &[Val],
    nonce: i64,
) -> xdr::SorobanAuthorizationEntry {
    let sc_args: std::vec::Vec<xdr::ScVal> = args.iter().map(|v| v.into_val(env)).collect();
    let invocation = xdr::SorobanAuthorizedInvocation {
        function: xdr::SorobanAuthorizedFunction::ContractFn(xdr::InvokeContractArgs {
            contract_address: ops.into(),
            function_name: xdr::ScSymbol(fn_name.try_into().unwrap()),
            args: sc_args.try_into().unwrap(),
        }),
        sub_invocations: Default::default(),
    };
    let expiration = env.ledger().sequence() + 100;
    let preimage = xdr::HashIdPreimage::SorobanAuthorization(xdr::HashIdPreimageSorobanAuthorization {
        network_id: xdr::Hash(env.ledger().network_id().to_array()),
        nonce,
        signature_expiration_ledger: expiration,
        invocation: invocation.clone(),
    });
    let bytes = preimage.to_xdr(xdr::Limits::none()).unwrap();
    let digest = env.crypto().sha256(&Bytes::from_slice(env, &bytes));
    let digest = BytesN::from_array(env, &digest.to_array());
    let sigs = sign_sorted(env, &digest, ks);
    let val: Val = sigs.into_val(env);
    let sc = xdr::ScVal::try_from_val(env, &val).unwrap();
    xdr::SorobanAuthorizationEntry {
        credentials: xdr::SorobanCredentials::Address(xdr::SorobanAddressCredentials {
            address: ops.into(),
            nonce,
            signature_expiration_ledger: expiration,
            signature: sc,
        }),
        root_invocation: invocation,
    }
}

#[test]
fn signed_entry_with_both_roles_changes_policy_through_the_host() {
    let env = Env::default();
    let fx = setup(&env);
    let c = OpsAccountClient::new(&env, &fx.ops);
    let target = Address::generate(&env);
    let f = Symbol::new(&env, "declare");
    let pol = Some(Policy { ta: 0, admin: 1, total: 1 });
    let args: std::vec::Vec<Val> = std::vec![target.into_val(&env), f.into_val(&env), pol.into_val(&env)];
    // TA alone: rejected by __check_auth.
    let one = signed_entry(&env, &fx.ops, &[&fx.keys.ta1], "set_policy", &args, 1);
    assert!(c.set_auths(&[one]).try_set_policy(&target, &f, &pol).is_err());
    // TA + ADMIN: accepted.
    let both = signed_entry(&env, &fx.ops, &[&fx.keys.ta1, &fx.keys.admin], "set_policy", &args, 2);
    c.set_auths(&[both.clone()]).set_policy(&target, &f, &pol);
    assert_eq!(c.policy(&target, &f), pol);
    // The same entry cannot be replayed (nonce consumed).
    assert!(c.set_auths(&[both]).try_set_policy(&target, &f, &pol).is_err());
}
