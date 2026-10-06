//! Ops account: a Soroban custom account shared by the transfer agent (TA)
//! and the fund administrator (ADMIN).
//!
//! Every Fund Desk contract names this account as its `ops` address and calls
//! `ops.require_auth()` for privileged functions. The host then runs
//! `__check_auth` below, which enforces a per-function role policy:
//!
//! * signatures arrive as `Vec<Sig>` sorted strictly by public key (this also
//!   rejects duplicates);
//! * every key must be a registered signer and its Ed25519 signature over the
//!   auth payload must verify;
//! * valid signatures are counted per role and compared with the policy row
//!   for `(contract, fn_name)` of every authorised context;
//! * a context with no policy row fails with `NoPolicy`, so the account can
//!   never authorise a token transfer or anything else nobody configured;
//! * changes to this account itself (`set_policy`, `set_signer`) always need
//!   one TA and one ADMIN signature; that rule is hard-coded.
#![no_std]

use soroban_sdk::{
    auth::{Context, CustomAccountInterface},
    contract, contracterror, contractevent, contractimpl, contracttype,
    crypto::Hash,
    Address, BytesN, Env, Map, Symbol, Vec,
};

const LEDGERS_PER_DAY: u32 = 17_280;
const INSTANCE_TTL_THRESHOLD: u32 = LEDGERS_PER_DAY * 30;
const INSTANCE_TTL_EXTEND_TO: u32 = LEDGERS_PER_DAY * 120;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum OpsError {
    NoPolicy = 1,
    InsufficientTa = 2,
    InsufficientAdmin = 3,
    InsufficientTotal = 4,
    UnknownSigner = 5,
    SignersNotSorted = 6,
    ForeignContext = 7,
    LastSignerOfRole = 8,
    EmptySignatures = 9,
    /// Added beyond the specification: a policy row that can never be met
    /// (`total == 0`, `ta + admin > total`) or that targets this account.
    InvalidPolicy = 10,
}

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Role {
    Ta,
    Admin,
}

/// Minimum number of valid signatures per role and in total.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Policy {
    pub ta: u32,
    pub admin: u32,
    pub total: u32,
}

/// One signature in the custom-account signature vector.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Sig {
    pub key: BytesN<32>,
    pub sig: BytesN<64>,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Signers,
    Policies,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicySet {
    #[topic]
    pub contract: Address,
    #[topic]
    pub fn_name: Symbol,
    pub ta: u32,
    pub admin: u32,
    pub total: u32,
    pub removed: bool,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignerSet {
    #[topic]
    pub key: BytesN<32>,
    pub role: u32, // 0 TA, 1 ADMIN, 2 removed
}

/// The hard-coded policy for administering this account itself.
pub const SELF_POLICY: Policy = Policy {
    ta: 1,
    admin: 1,
    total: 2,
};

#[contract]
pub struct OpsAccount;

fn bump(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_EXTEND_TO);
}

fn read_signers(env: &Env) -> Map<BytesN<32>, Role> {
    env.storage()
        .instance()
        .get(&DataKey::Signers)
        .unwrap_or(Map::new(env))
}

fn read_policies(env: &Env) -> Map<(Address, Symbol), Policy> {
    env.storage()
        .instance()
        .get(&DataKey::Policies)
        .unwrap_or(Map::new(env))
}

fn count_role(signers: &Map<BytesN<32>, Role>, role: Role) -> u32 {
    signers.values().iter().filter(|r| *r == role).count() as u32
}

fn check_policy(p: &Policy, ta: u32, admin: u32, total: u32) -> Result<(), OpsError> {
    if ta < p.ta {
        return Err(OpsError::InsufficientTa);
    }
    if admin < p.admin {
        return Err(OpsError::InsufficientAdmin);
    }
    if total < p.total {
        return Err(OpsError::InsufficientTotal);
    }
    Ok(())
}

#[contractimpl]
impl OpsAccount {
    /// Register the initial signer keys. At least one TA and one ADMIN key.
    pub fn __constructor(env: Env, signers: Vec<(BytesN<32>, Role)>) -> Result<(), OpsError> {
        let mut map: Map<BytesN<32>, Role> = Map::new(&env);
        for (k, r) in signers.iter() {
            map.set(k, r);
        }
        if count_role(&map, Role::Ta) == 0 || count_role(&map, Role::Admin) == 0 {
            return Err(OpsError::LastSignerOfRole);
        }
        env.storage().instance().set(&DataKey::Signers, &map);
        env.storage()
            .instance()
            .set(&DataKey::Policies, &Map::<(Address, Symbol), Policy>::new(&env));
        bump(&env);
        Ok(())
    }

    /// Add, change or remove (`None`) the policy row for `contract.fn_name`.
    /// Needs this account's own authorisation: one TA and one ADMIN signature.
    pub fn set_policy(
        env: Env,
        contract: Address,
        fn_name: Symbol,
        policy: Option<Policy>,
    ) -> Result<(), OpsError> {
        env.current_contract_address().require_auth();
        if contract == env.current_contract_address() {
            return Err(OpsError::InvalidPolicy);
        }
        let mut policies = read_policies(&env);
        let key = (contract.clone(), fn_name.clone());
        let (ta, admin, total, removed) = match policy {
            Some(p) => {
                let floor = p.ta.checked_add(p.admin).ok_or(OpsError::InvalidPolicy)?;
                if p.total == 0 || p.total < floor {
                    return Err(OpsError::InvalidPolicy);
                }
                policies.set(key, p);
                (p.ta, p.admin, p.total, false)
            }
            None => {
                policies.remove(key);
                (0, 0, 0, true)
            }
        };
        env.storage().instance().set(&DataKey::Policies, &policies);
        bump(&env);
        PolicySet {
            contract,
            fn_name,
            ta,
            admin,
            total,
            removed,
        }
        .publish(&env);
        Ok(())
    }

    /// Add, re-role or remove (`None`) a signer key. Needs TA + ADMIN.
    /// Removing or re-roling the last key of a role fails.
    pub fn set_signer(env: Env, key: BytesN<32>, role: Option<Role>) -> Result<(), OpsError> {
        env.current_contract_address().require_auth();
        let mut signers = read_signers(&env);
        match role {
            Some(r) => {
                signers.set(key.clone(), r);
            }
            None => {
                signers.remove(key.clone());
            }
        }
        if count_role(&signers, Role::Ta) == 0 || count_role(&signers, Role::Admin) == 0 {
            return Err(OpsError::LastSignerOfRole);
        }
        env.storage().instance().set(&DataKey::Signers, &signers);
        bump(&env);
        SignerSet {
            key,
            role: match role {
                Some(Role::Ta) => 0,
                Some(Role::Admin) => 1,
                None => 2,
            },
        }
        .publish(&env);
        Ok(())
    }

    pub fn policy(env: Env, contract: Address, fn_name: Symbol) -> Option<Policy> {
        if contract == env.current_contract_address() {
            return Some(SELF_POLICY);
        }
        read_policies(&env).get((contract, fn_name))
    }

    pub fn signers(env: Env) -> Map<BytesN<32>, Role> {
        read_signers(&env)
    }
}

#[contractimpl]
impl CustomAccountInterface for OpsAccount {
    type Signature = Vec<Sig>;
    type Error = OpsError;

    fn __check_auth(
        env: Env,
        signature_payload: Hash<32>,
        signatures: Vec<Sig>,
        auth_contexts: Vec<Context>,
    ) -> Result<(), OpsError> {
        if signatures.is_empty() {
            return Err(OpsError::EmptySignatures);
        }
        let signers = read_signers(&env);
        let payload: BytesN<32> = signature_payload.into();
        let mut ta = 0u32;
        let mut admin = 0u32;
        let mut prev: Option<BytesN<32>> = None;
        for s in signatures.iter() {
            if let Some(p) = &prev {
                if s.key <= *p {
                    return Err(OpsError::SignersNotSorted);
                }
            }
            let role = signers.get(s.key.clone()).ok_or(OpsError::UnknownSigner)?;
            // Traps (host error) when the signature does not verify.
            env.crypto()
                .ed25519_verify(&s.key, &payload.clone().into(), &s.sig);
            match role {
                Role::Ta => ta += 1,
                Role::Admin => admin += 1,
            }
            prev = Some(s.key);
        }
        let total = ta + admin;
        let this = env.current_contract_address();
        let policies = read_policies(&env);
        for ctx in auth_contexts.iter() {
            match ctx {
                Context::Contract(c) => {
                    let p = if c.contract == this {
                        SELF_POLICY
                    } else {
                        policies
                            .get((c.contract, c.fn_name))
                            .ok_or(OpsError::NoPolicy)?
                    };
                    check_policy(&p, ta, admin, total)?;
                }
                Context::CreateContractHostFn(_) | Context::CreateContractWithCtorHostFn(_) => {
                    return Err(OpsError::ForeignContext)
                }
            }
        }
        bump(&env);
        Ok(())
    }
}

#[cfg(test)]
mod test;
