//! `scenario_three_epochs_seed`: the whole Fund Desk journey on the seed
//! data in `data/seed/`, with all five contracts registered in one Soroban
//! host.
//!
//! The runner reads `fund.json`, `register.json` (the normalised output of
//! `funddesk kyc import investors.csv + investors.fix.csv`), `requests.csv`,
//! `nav.csv`, `distribution.csv` and `forced.csv`, merges them into one
//! timeline, executes every row at its timestamp and checks each row's
//! `expect` column. After every step it asserts:
//!
//! * vault cash == sum of pending subscriptions + unclaimed claimable cash;
//! * registrar supply == sum of SAC share balances;
//! * treasury == accepted subscription cash - redemption cash - distributions.
//!
//! With `FD_WRITE_GOLDEN=1` it writes `data/seed/expected-scenario.json`,
//! which the TypeScript model (`npm run demo`, `npm test`) must reproduce;
//! otherwise it compares its result with the committed file.
#![cfg(test)]
extern crate std;

use super::*;
use compliance::{Compliance, ComplianceClient, ComplianceError, Investor};
use distribution::{DistError, Distribution, DistributionClient};
use ed25519_dalek::{Signer, SigningKey};
use nav_oracle::{Asset as OAsset, NavOracle, NavOracleClient, OracleError};
use ops_account::{OpsAccount, OpsAccountClient, OpsError, Policy, Role, Sig};
use serde_json::{json, Map as JMap, Value};
use soroban_sdk::{
    auth::{Context, ContractContext},
    testutils::{Address as _, IssuerFlags, Ledger as _},
    token::{StellarAssetClient, TokenClient},
    Address, Bytes, BytesN, Env, IntoVal, Symbol, Vec as SVec,
};
use std::collections::BTreeMap;
use std::string::{String, ToString};
use std::vec::Vec;
use std::format;

// ---------------------------------------------------------------------------
// seed parsing helpers (std only)
// ---------------------------------------------------------------------------

fn seed_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/seed")
}

fn read(name: &str) -> String {
    std::fs::read_to_string(seed_dir().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// Minimal CSV: quoted fields, doubled quotes, '#' comment lines.
fn csv(name: &str) -> Vec<BTreeMap<String, String>> {
    let text = read(name);
    let mut rows: Vec<Vec<String>> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let mut fields = Vec::new();
        let mut cur = String::new();
        let mut q = false;
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if q {
                if c == '"' {
                    if i + 1 < chars.len() && chars[i + 1] == '"' {
                        cur.push('"');
                        i += 1;
                    } else {
                        q = false;
                    }
                } else {
                    cur.push(c);
                }
            } else if c == '"' {
                q = true;
            } else if c == ',' {
                fields.push(core::mem::take(&mut cur));
            } else {
                cur.push(c);
            }
            i += 1;
        }
        fields.push(cur);
        rows.push(fields);
    }
    let header = rows[0].clone();
    rows[1..]
        .iter()
        .map(|r| {
            header
                .iter()
                .enumerate()
                .map(|(i, h)| (h.clone(), r.get(i).cloned().unwrap_or_default()))
                .collect()
        })
        .collect()
}

/// "1,000,000.0000001" -> integer with `dec` decimals.
fn fixed(raw: &str, dec: usize) -> i128 {
    let s: String = raw.trim().chars().filter(|c| *c != ',' && *c != ' ').collect();
    let (w, f) = match s.split_once('.') {
        Some((w, f)) => (w.to_string(), f.to_string()),
        None => (s.clone(), String::new()),
    };
    assert!(f.len() <= dec, "too many decimals in {raw}");
    let f = format!("{:0<width$}", f, width = dec);
    w.parse::<i128>().unwrap() * 10i128.pow(dec as u32) + if dec == 0 { 0 } else { f.parse::<i128>().unwrap() }
}

fn cash_amt(raw: &str) -> i128 {
    fixed(raw, 7)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// "2026-10-05T13:00:00Z" -> unix seconds.
fn ts(iso: &str) -> u64 {
    let b = iso.as_bytes();
    let n = |a: usize, l: usize| iso[a..a + l].parse::<i64>().unwrap();
    assert!(b[10] == b'T' && iso.ends_with('Z'), "bad timestamp {iso}");
    let days = days_from_civil(n(0, 4), n(5, 2), n(8, 2));
    (days * 86_400 + n(11, 2) * 3_600 + n(14, 2) * 60 + if iso.len() >= 20 { n(17, 2) } else { 0 }) as u64
}

fn s(v: i128) -> Value {
    Value::String(v.to_string())
}

// ---------------------------------------------------------------------------
// deployment
// ---------------------------------------------------------------------------

struct Class {
    id: String,
    cash: Address,
    share: Address,
    reg: Address,
    dist: Address,
    vault: Address,
    treasury: Address,
    asset: Symbol,
    batch: u32,
    epochs: Vec<Value>,
    holders: Vec<String>, // investor ids of this class, in register order
    declared: i128,
    per_share: i128,
    settle_calls: BTreeMap<u32, u32>,
    liquidity_needed: BTreeMap<u32, bool>,
}

struct World<'a> {
    env: &'a Env,
    ops: Address,
    oracle: Address,
    keys: BTreeMap<String, SigningKey>,
    classes: Vec<Class>,
    inv: BTreeMap<String, Address>,     // investor id -> wallet (share holder)
    addr: BTreeMap<String, Address>,    // G strkey from the seed -> test address
    class_of: BTreeMap<String, usize>,  // investor id -> class index
    req_ids: BTreeMap<String, u64>,     // "inv_01:<at>" -> request id
    outcomes: BTreeMap<String, Vec<Value>>,
}

fn signer_keys() -> BTreeMap<String, SigningKey> {
    let mut k = BTreeMap::new();
    k.insert("ta_ops_1".to_string(), SigningKey::from_bytes(&[0x11; 32]));
    k.insert("ta_ops_2".to_string(), SigningKey::from_bytes(&[0x12; 32]));
    k.insert("fund_admin_1".to_string(), SigningKey::from_bytes(&[0x21; 32]));
    k
}

fn deploy<'a>(env: &'a Env, fund: &Value, register: &Value) -> World<'a> {
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();
    env.ledger().set_timestamp(ts("2026-10-05T06:00:00Z"));
    let keys = signer_keys();
    let mut signers = SVec::new(env);
    for sgn in fund["signers"].as_array().unwrap() {
        let k = &keys[sgn["name"].as_str().unwrap()];
        let role = if sgn["role"] == "Ta" { Role::Ta } else { Role::Admin };
        signers.push_back((BytesN::from_array(env, &k.verifying_key().to_bytes()), role));
    }
    let ops = env.register(OpsAccount, (signers,));
    let usdc_like = Address::generate(env);
    let oracle = env.register(
        NavOracle,
        (ops.clone(), OAsset::Stellar(usdc_like), fund["nav_decimals"].as_u64().unwrap() as u32, 86_400u32),
    );
    let mut w = World {
        env,
        ops: ops.clone(),
        oracle: oracle.clone(),
        keys,
        classes: Vec::new(),
        inv: BTreeMap::new(),
        addr: BTreeMap::new(),
        class_of: BTreeMap::new(),
        req_ids: BTreeMap::new(),
        outcomes: BTreeMap::new(),
    };
    let opsc = OpsAccountClient::new(env, &ops);
    for (ci, cls) in fund["classes"].as_array().unwrap().iter().enumerate() {
        let cash = env.register_stellar_asset_contract_v2(Address::generate(env)).address();
        // Share asset: flags set before any balance exists, then SAC admin -> registrar.
        let sac = env.register_stellar_asset_contract_v2(Address::generate(env));
        sac.issuer().set_flag(IssuerFlags::RequiredFlag);
        sac.issuer().set_flag(IssuerFlags::RevocableFlag);
        sac.issuer().set_flag(IssuerFlags::ClawbackEnabledFlag);
        let share = sac.address();
        let treasury = Address::generate(env);
        let reg = env.register(Compliance, (ops.clone(), share.clone()));
        StellarAssetClient::new(env, &share).set_admin(&reg);
        let dist = env.register(Distribution, (ops.clone(), reg.clone(), cash.clone(), treasury.clone()));
        let asset = Symbol::new(env, cls["oracle_asset"].as_str().unwrap());
        let cfg = Config {
            ops: ops.clone(),
            compliance: reg.clone(),
            share: share.clone(),
            cash: cash.clone(),
            treasury: treasury.clone(),
            oracle: oracle.clone(),
            oracle_asset: Asset::Other(asset.clone()),
            nav_decimals: fund["nav_decimals"].as_u64().unwrap() as u32,
            initial_nav: fixed(cls["initial_nav"].as_str().unwrap(), 14),
            min_subscription: cash_amt(cls["min_subscription"].as_str().unwrap()),
            max_strike_delay: fund["max_strike_delay_s"].as_u64().unwrap(),
            max_nav_move_bps: fund["max_nav_move_bps"].as_u64().unwrap() as u32,
            max_requests_per_epoch: fund["max_requests_per_epoch"].as_u64().unwrap() as u32,
        };
        let vault = env.register(AsyncVault, (cfg,));
        let rc = ComplianceClient::new(env, &reg);
        rc.bind(&vault, &dist);
        // Policy rows for this class's contracts (the oracle's once).
        for p in fund["policies"].as_array().unwrap() {
            let target = match p["contract"].as_str().unwrap() {
                "compliance" => reg.clone(),
                "async_vault" => vault.clone(),
                "distribution" => dist.clone(),
                "nav_oracle" if ci == 0 => oracle.clone(),
                _ => continue,
            };
            let pol = Policy {
                ta: p["ta"].as_u64().unwrap() as u32,
                admin: p["admin"].as_u64().unwrap() as u32,
                total: p["total"].as_u64().unwrap() as u32,
            };
            opsc.set_policy(&target, &Symbol::new(env, p["fn"].as_str().unwrap()), &Some(pol));
        }
        for j in fund["allowed_jurisdictions"].as_array().unwrap() {
            rc.set_jurisdiction(&Symbol::new(env, j.as_str().unwrap()), &true);
        }
        let id = cls["id"].as_str().unwrap().to_string();
        let mut holders = Vec::new();
        for r in register.as_array().unwrap() {
            if r["class"].as_str().unwrap() != id {
                continue;
            }
            let iid = r["investor_id"].as_str().unwrap().to_string();
            let wallet = Address::generate(env);
            let mut cashv = SVec::new(env);
            for c in r["cash_addresses"].as_array().unwrap() {
                let a = Address::generate(env);
                w.addr.insert(c.as_str().unwrap().to_string(), a.clone());
                cashv.push_back(a);
            }
            w.addr.insert(r["wallet"].as_str().unwrap().to_string(), wallet.clone());
            rc.set_investor(
                &wallet,
                &Investor {
                    kyc_expiry: r["kyc_expiry_unix"].as_u64().unwrap(),
                    jurisdiction: Symbol::new(env, r["jurisdiction"].as_str().unwrap()),
                    investor_type: r["investor_type"].as_u64().unwrap() as u32,
                    cash_addresses: cashv,
                    frozen: false,
                },
            );
            // Each simulated investor holds 2,000,000 of the class's cash asset.
            StellarAssetClient::new(env, &cash).mint(&wallet, &cash_amt("2,000,000"));
            w.inv.insert(iid.clone(), wallet);
            w.class_of.insert(iid.clone(), ci);
            holders.push(iid);
        }
        w.classes.push(Class {
            id,
            cash,
            share,
            reg,
            dist,
            vault,
            treasury,
            asset,
            batch: fund["settle_batch"].as_u64().unwrap() as u32,
            epochs: cls["epochs"].as_array().unwrap().clone(),
            holders,
            declared: 0,
            per_share: 0,
            settle_calls: BTreeMap::new(),
            liquidity_needed: BTreeMap::new(),
        });
    }
    // An address nobody registered (typos in the seed point here).
    w.addr.entry("__ext__".to_string()).or_insert_with(|| Address::generate(env));
    w
}

// ---------------------------------------------------------------------------
// timeline
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Step {
    Open(usize, u32, u64),
    Publish(usize, BTreeMap<String, String>),
    Strike(usize, BTreeMap<String, String>),
    Request(BTreeMap<String, String>),
    Settle(usize, u32, i128),
    ClaimAll(usize, u32),
    Dist(BTreeMap<String, String>),
    Forced(BTreeMap<String, String>),
}

fn class_idx(w: &World, id: &str) -> usize {
    w.classes.iter().position(|c| c.id == id).unwrap()
}

fn timeline(w: &World) -> Vec<(u64, usize, Step)> {
    let mut t: Vec<(u64, usize, Step)> = Vec::new();
    let mut n = 0usize;
    let mut push = |at: u64, st: Step, t: &mut Vec<(u64, usize, Step)>| {
        n += 1;
        t.push((at, n, st));
    };
    for (ci, c) in w.classes.iter().enumerate() {
        for e in &c.epochs {
            let ep = e["epoch"].as_u64().unwrap() as u32;
            push(ts(e["open_at"].as_str().unwrap()), Step::Open(ci, ep, ts(e["cutoff"].as_str().unwrap())), &mut t);
            let settle_at = ts(e["settle_at"].as_str().unwrap());
            push(settle_at, Step::Settle(ci, ep, cash_amt(e["liquidity_topup"].as_str().unwrap())), &mut t);
            push(settle_at + 300, Step::ClaimAll(ci, ep), &mut t);
        }
    }
    for r in csv("nav.csv") {
        let ci = class_idx(w, &r["class"]);
        push(ts(&r["published_at"]), Step::Publish(ci, r.clone()), &mut t);
        push(ts(&r["strike_at"]), Step::Strike(ci, r), &mut t);
    }
    for r in csv("requests.csv") {
        push(ts(&r["at"]), Step::Request(r), &mut t);
    }
    for r in csv("distribution.csv") {
        push(ts(&r["at"]), Step::Dist(r), &mut t);
    }
    for r in csv("forced.csv") {
        push(ts(&r["at"]), Step::Forced(r), &mut t);
    }
    t.sort_by_key(|(at, n, _)| (*at, *n));
    t
}

fn err_name<E: core::fmt::Debug, I>(r: &Result<I, Result<E, soroban_sdk::InvokeError>>) -> String {
    match r {
        Ok(_) => "ok".to_string(),
        Err(Ok(e)) => format!("{e:?}"),
        Err(Err(e)) => format!("host:{e:?}"),
    }
}

impl<'a> World<'a> {
    fn v(&self, ci: usize) -> AsyncVaultClient<'a> {
        AsyncVaultClient::new(self.env, &self.classes[ci].vault)
    }
    fn r(&self, ci: usize) -> ComplianceClient<'a> {
        ComplianceClient::new(self.env, &self.classes[ci].reg)
    }
    fn d(&self, ci: usize) -> DistributionClient<'a> {
        DistributionClient::new(self.env, &self.classes[ci].dist)
    }
    fn cash_of(&self, ci: usize, a: &Address) -> i128 {
        TokenClient::new(self.env, &self.classes[ci].cash).balance(a)
    }
    fn target_addr(&self, g: &str) -> Address {
        self.addr.get(g).cloned().unwrap_or_else(|| self.addr["__ext__"].clone())
    }
    fn record(&mut self, ci: usize, v: Value) {
        self.outcomes.entry(self.classes[ci].id.clone()).or_default().push(v);
    }

    fn run(&mut self, at: u64, step: Step) {
        if self.env.ledger().timestamp() < at {
            self.env.ledger().set_timestamp(at);
        }
        match step {
            Step::Open(ci, ep, cutoff) => {
                assert_eq!(self.v(ci).open_epoch(&cutoff), ep);
            }
            Step::Publish(ci, r) => {
                let asset = OAsset::Other(self.classes[ci].asset.clone());
                let nav = fixed(&r["nav"], 14);
                let res = NavOracleClient::new(self.env, &self.oracle).try_publish(&asset, &nav, &ts(&r["as_of"]));
                assert_eq!(err_name::<OracleError, _>(&res), "ok", "publish {r:?}");
            }
            Step::Strike(ci, r) => {
                let ep: u32 = r["epoch"].parse().unwrap();
                let res = if r["mode"] == "override" {
                    self.v(ci).try_strike_nav_override(&ep)
                } else {
                    self.v(ci).try_strike_nav(&ep)
                };
                let got = err_name::<VaultError, _>(&res);
                assert_eq!(got, r["expect"], "strike {r:?}");
                self.record(ci, json!({"at": r["strike_at"], "action": "strike", "epoch": ep, "nav": r["nav"], "expect": r["expect"], "result": got}));
            }
            Step::Request(r) => self.request(r),
            Step::Settle(ci, ep, topup) => {
                let batch = self.classes[ci].batch;
                let mut calls = 0u32;
                let mut needed = false;
                loop {
                    let res = self.v(ci).try_settle(&ep, &batch);
                    calls += 1;
                    match res {
                        Ok(Ok((_, 0))) => break,
                        Ok(Ok(_)) => continue,
                        Err(Ok(VaultError::InsufficientLiquidity)) if topup > 0 && !needed => {
                            needed = true;
                            self.v(ci).deposit_liquidity(&ep, &topup);
                        }
                        other => panic!("settle {} epoch {ep}: {other:?}", self.classes[ci].id),
                    }
                }
                assert_eq!(needed, topup > 0, "liquidity top-up expectation for epoch {ep}");
                self.classes[ci].settle_calls.insert(ep, calls);
                self.classes[ci].liquidity_needed.insert(ep, needed);
                assert_eq!(self.v(ci).epoch(&ep).status, EpochStatus::Settled);
            }
            Step::ClaimAll(ci, ep) => {
                let n = self.v(ci).queue_len(&ep);
                for req in self.v(ci).queue(&ep, &0, &n).iter() {
                    if req.status == ReqStatus::Claimable {
                        // TA push through the ops account.
                        self.v(ci).claim(&self.ops, &req.id);
                    }
                }
            }
            Step::Dist(r) => self.dist(r),
            Step::Forced(r) => self.forced(r),
        }
        self.check_invariants();
    }

    fn request(&mut self, r: BTreeMap<String, String>) {
        let ci = class_idx(self, &r["class"]);
        let who = self.inv[&r["investor"]].clone();
        let action = r["action"].as_str();
        let got = match action {
            "subscribe" => {
                let res = self.v(ci).try_request_subscribe(&who, &cash_amt(&r["amount"]));
                if let Ok(Ok(id)) = &res {
                    self.req_ids.insert(format!("{}:{}", r["investor"], r["at"]), *id);
                }
                err_name::<VaultError, _>(&res)
            }
            "redeem" => {
                let to = self.target_addr(&r["target"]);
                let res = self.v(ci).try_request_redeem(&who, &cash_amt(&r["amount"]), &to);
                if let Ok(Ok(id)) = &res {
                    self.req_ids.insert(format!("{}:{}", r["investor"], r["at"]), *id);
                }
                err_name::<VaultError, _>(&res)
            }
            "cancel" => {
                // target = "req:<investor>:<at of the request>"
                let key = r["target"].trim_start_matches("req:").to_string();
                let id = self.req_ids[&key];
                err_name::<VaultError, _>(&self.v(ci).try_cancel(&who, &id))
            }
            "transfer" => {
                let to = self.inv[&r["target"]].clone();
                err_name::<ComplianceError, _>(&self.r(ci).try_transfer(&who, &to, &cash_amt(&r["amount"])))
            }
            other => panic!("unknown action {other}"),
        };
        assert_eq!(got, r["expect"], "request row {r:?}");
        self.record(
            ci,
            json!({"seq": r["seq"], "at": r["at"], "action": action, "investor": r["investor"], "amount": r["amount"].trim(), "expect": r["expect"], "result": got}),
        );
    }

    fn dist(&mut self, r: BTreeMap<String, String>) {
        let ci = class_idx(self, &r["class"]);
        let got = match r["action"].as_str() {
            "declare" => {
                let memo = self.env.crypto().sha256(&Bytes::from_slice(self.env, r["memo"].as_bytes()));
                let memo = BytesN::from_array(self.env, &memo.to_array());
                let res = self.d(ci).try_declare(&cash_amt(&r["amount"]), &memo);
                if let Ok(Ok(ps)) = &res {
                    self.classes[ci].declared += cash_amt(&r["amount"]);
                    self.classes[ci].per_share = *ps;
                }
                err_name::<DistError, _>(&res)
            }
            "claim" => {
                let who = self.inv[&r["holder"]].clone();
                let to = self.target_addr(&r["to"]);
                err_name::<DistError, _>(&self.d(ci).try_claim(&who, &to))
            }
            "push" => {
                let mut ids: Vec<String> = self.classes[ci].holders.clone();
                if self.inv.contains_key("inv_05b") && self.class_of.get("inv_05b") == Some(&ci) {
                    ids.push("inv_05b".to_string());
                }
                let mut last = "ok".to_string();
                for chunk in ids.chunks(25) {
                    let mut v = SVec::new(self.env);
                    for id in chunk {
                        v.push_back(self.inv[id].clone());
                    }
                    last = err_name::<DistError, _>(&self.d(ci).try_claim_for(&v));
                }
                last
            }
            other => panic!("unknown distribution action {other}"),
        };
        assert_eq!(got, r["expect"], "distribution row {r:?}");
        self.record(ci, json!({"at": r["at"], "action": format!("distribution.{}", r["action"]), "investor": r["holder"], "amount": r["amount"], "expect": r["expect"], "result": got}));
    }

    fn forced(&mut self, r: BTreeMap<String, String>) {
        let from_id = r["from"].clone();
        let ci = self.class_of[&from_id];
        let to_id = r["to"].clone();
        // Register the replacement wallet once (TA policy, KYC re-verified).
        if !self.inv.contains_key(&to_id) {
            let wallet = Address::generate(self.env);
            let cash = self.target_addr(&r["new_cash"]);
            let expiry = ts(&format!("{}T00:00:00Z", r["new_kyc_expiry"]));
            self.r(ci).set_investor(
                &wallet,
                &Investor {
                    kyc_expiry: expiry,
                    jurisdiction: Symbol::new(self.env, &r["new_jurisdiction"]),
                    investor_type: 0,
                    cash_addresses: soroban_sdk::vec![self.env, cash],
                    frozen: false,
                },
            );
            self.addr.insert(r["new_wallet"].clone(), wallet.clone());
            self.inv.insert(to_id.clone(), wallet);
            self.class_of.insert(to_id.clone(), ci);
        }
        let from = self.inv[&from_id].clone();
        let to = self.inv[&to_id].clone();
        let amount = if r["shares"] == "all" {
            self.r(ci).balance(&from) - self.r(ci).locked(&from)
        } else {
            cash_amt(&r["shares"])
        };
        let reason_h = self.env.crypto().sha256(&Bytes::from_slice(self.env, r["reason"].as_bytes()));
        let reason = BytesN::from_array(self.env, &reason_h.to_array());
        // The ops account's policy decides, with real Ed25519 signatures of
        // the listed signers over an auth payload for this exact call.
        let ctx = soroban_sdk::vec![
            self.env,
            Context::Contract(ContractContext {
                contract: self.classes[ci].reg.clone(),
                fn_name: Symbol::new(self.env, "forced_transfer"),
                args: (from.clone(), to.clone(), amount, reason.clone()).into_val(self.env),
            }),
        ];
        let payload = BytesN::from_array(self.env, &reason_h.to_array());
        let mut sigs: Vec<(Vec<u8>, Sig)> = r["signers"]
            .split('+')
            .map(|n| {
                let k = &self.keys[n.trim()];
                let key = k.verifying_key().to_bytes();
                (
                    key.to_vec(),
                    Sig {
                        key: BytesN::from_array(self.env, &key),
                        sig: BytesN::from_array(self.env, &k.sign(&payload.to_array()).to_bytes()),
                    },
                )
            })
            .collect();
        sigs.sort_by(|a, b| a.0.cmp(&b.0));
        let mut sv: SVec<Sig> = SVec::new(self.env);
        for (_, sg) in sigs {
            sv.push_back(sg);
        }
        let auth = self.env.try_invoke_contract_check_auth::<OpsError>(&self.ops, &payload, sv.into_val(self.env), &ctx);
        let got = match auth {
            Ok(()) => {
                let res = self.r(ci).try_forced_transfer(&from, &to, &amount, &reason);
                err_name::<ComplianceError, _>(&res)
            }
            Err(Ok(e)) => format!("{e:?}"),
            Err(Err(e)) => format!("host:{e:?}"),
        };
        assert_eq!(got, r["expect"], "forced row {r:?}");
        self.record(ci, json!({"at": r["at"], "action": "forced_transfer", "investor": from_id, "to": to_id, "signers": r["signers"], "amount": s(amount), "expect": r["expect"], "result": got}));
    }

    fn all_requests(&self, ci: usize) -> Vec<Request> {
        let mut out = Vec::new();
        for ep in 1..=self.v(ci).current_epoch() {
            let n = self.v(ci).queue_len(&ep);
            for q in self.v(ci).queue(&ep, &0, &n).iter() {
                out.push(q);
            }
        }
        out
    }

    fn check_invariants(&self) {
        for (ci, c) in self.classes.iter().enumerate() {
            let reqs = self.all_requests(ci);
            // 1. vault cash == pending subscriptions + unclaimed claimable cash
            let mut expect_vault = 0i128;
            for q in &reqs {
                match (q.kind, q.status) {
                    (Kind::Subscribe, ReqStatus::Pending) => expect_vault += q.amount,
                    (_, ReqStatus::Claimable) => expect_vault += q.cash_out,
                    _ => {}
                }
            }
            assert_eq!(self.cash_of(ci, &c.vault), expect_vault, "{} vault cash", c.id);
            // 2. registrar supply == sum of SAC balances
            let share = TokenClient::new(self.env, &c.share);
            let sum: i128 = self
                .inv
                .iter()
                .filter(|(id, _)| self.class_of[*id] == ci)
                .map(|(_, a)| share.balance(a))
                .sum();
            assert_eq!(self.r(ci).total_shares(), sum, "{} supply", c.id);
            // 3. treasury == accepted subscription cash - redemption cash - distributions
            let mut net = 0i128;
            for q in &reqs {
                let settled = matches!(q.status, ReqStatus::Claimable | ReqStatus::Claimed);
                if !settled {
                    continue;
                }
                match q.kind {
                    Kind::Subscribe => net += q.amount - q.cash_out,
                    Kind::Redeem => net -= q.cash_out,
                }
            }
            assert_eq!(self.cash_of(ci, &c.treasury), net - c.declared, "{} treasury", c.id);
        }
    }

    fn golden(&self) -> Value {
        let mut classes = Vec::new();
        for (ci, c) in self.classes.iter().enumerate() {
            let v = self.v(ci);
            let ids_by_addr: BTreeMap<String, String> =
                self.inv.iter().map(|(id, a)| (format!("{a:?}"), id.clone())).collect();
            let mut epochs = Vec::new();
            for ep in 1..=v.current_epoch() {
                let e = v.epoch(&ep);
                epochs.push(json!({
                    "epoch": ep,
                    "status": format!("{:?}", e.status),
                    "nav": s(e.nav),
                    "nav_ts": e.nav_ts,
                    "sub_total": s(e.sub_total),
                    "redeem_shares_total": s(e.redeem_shares_total),
                    "liquidity": s(e.liquidity),
                    "claimable_cash": s(e.claimable_cash),
                    "surplus": s(e.sub_total + e.liquidity - e.claimable_cash),
                    "settle_calls": self.classes[ci].settle_calls.get(&ep).copied().unwrap_or(0),
                    "liquidity_needed": self.classes[ci].liquidity_needed.get(&ep).copied().unwrap_or(false),
                }));
            }
            let reqs: Vec<Value> = self
                .all_requests(ci)
                .iter()
                .map(|q| {
                    json!({
                        "id": q.id,
                        "epoch": q.epoch,
                        "investor": ids_by_addr[&format!("{:?}", q.investor)],
                        "kind": format!("{:?}", q.kind),
                        "amount": s(q.amount),
                        "status": format!("{:?}", q.status),
                        "reject": q.reject,
                        "shares_out": s(q.shares_out),
                        "cash_out": s(q.cash_out),
                    })
                })
                .collect();
            let mut holders = JMap::new();
            let d = self.d(ci);
            let mut ids: Vec<&String> = self.inv.keys().filter(|id| self.class_of[*id] == ci).collect();
            ids.sort();
            for id in ids {
                let a = &self.inv[id];
                holders.insert(
                    id.clone(),
                    json!({
                        "shares": s(self.r(ci).balance(a)),
                        "locked": s(self.r(ci).locked(a)),
                        "dist_accrued": s(d.accrued(a)),
                    }),
                );
            }
            classes.push(json!({
                "class": c.id,
                "epochs": epochs,
                "requests": reqs,
                "holders": holders,
                "total_shares": s(self.r(ci).total_shares()),
                "treasury": s(self.cash_of(ci, &c.treasury)),
                "vault_cash": s(self.cash_of(ci, &c.vault)),
                "last_nav": s(v.last_nav()),
                "distribution": {
                    "declared": s(d.declared()),
                    "claimed": s(d.claimed()),
                    "per_share_scaled": s(c.per_share),
                    "acc_scaled": s(d.acc()),
                    "carry_scaled": s(d.carry_scaled()),
                    "held": s(self.cash_of(ci, &c.dist)),
                },
                "outcomes": self.outcomes.get(&c.id).cloned().unwrap_or_default(),
            }));
        }
        json!({
            "generated_by": "contracts/async_vault/src/test_scenario.rs (FD_WRITE_GOLDEN=1 cargo test scenario_three_epochs_seed)",
            "data": "Simulated seed data (data/seed); fictional fund",
            "classes": classes,
        })
    }
}

#[test]
fn scenario_three_epochs_seed() {
    let env = Env::default();
    let fund: Value = serde_json::from_str(&read("fund.json")).unwrap();
    let register: Value = serde_json::from_str(&read("register.json")).unwrap();
    let mut w = deploy(&env, &fund, &register);
    for (at, _, step) in timeline(&w) {
        w.run(at, step);
    }

    let usd = class_idx(&w, "USD-D");
    let v = w.v(usd);
    // Epoch outcomes the seed was designed to produce.
    assert_eq!(v.current_epoch(), 3);
    for ep in 1..=3 {
        assert_eq!(v.epoch(&ep).status, EpochStatus::Settled);
    }
    assert_eq!(w.classes[usd].liquidity_needed[&3], true);
    assert_eq!(v.last_nav(), 99_999_870_000_000);
    // inv_07's epoch-2 subscription was refunded with reject = KycExpired.
    let reqs = w.all_requests(usd);
    let inv07 = w.inv["inv_07"].clone();
    let rej: Vec<&Request> = reqs.iter().filter(|q| q.investor == inv07 && q.epoch == 2).collect();
    assert_eq!(rej.len(), 1);
    assert_eq!((rej[0].reject, rej[0].shares_out, rej[0].status), (REJECT_KYC_EXPIRED, 0, ReqStatus::Claimed));
    // inv_12 (expired) fully redeemed to its registered cash address.
    assert_eq!(w.r(usd).balance(&w.inv["inv_12"]), 0);
    // inv_05's shares moved to inv_05b; supply unchanged by the forced transfer.
    assert_eq!(w.r(usd).balance(&w.inv["inv_05"]), 0);
    assert_eq!(w.r(usd).balance(&w.inv["inv_05b"]), cash_amt("12,000"));
    // Every share balance is deauthorised at rest.
    let sac = StellarAssetClient::new(&env, &w.classes[usd].share);
    for a in w.inv.values() {
        if w.class_of[&w.inv.iter().find(|(_, x)| *x == a).unwrap().0.clone()] == usd {
            assert!(!sac.authorized(a));
        }
    }
    // Distribution: everything declared is claimed or still owed, loss <= holders + 1 stroops.
    let d = w.d(usd);
    let mut owed = 0i128;
    for (id, a) in &w.inv {
        if w.class_of[id] == usd {
            owed += d.accrued(a);
        }
    }
    let loss = d.declared() - d.claimed() - owed;
    assert!(loss >= 0 && loss <= w.classes[usd].holders.len() as i128 + 2, "rounding loss {loss}");

    let golden = serde_json::to_string_pretty(&w.golden()).unwrap() + "\n";
    let path = seed_dir().join("expected-scenario.json");
    if std::env::var("FD_WRITE_GOLDEN").map(|v| v == "1").unwrap_or(false) {
        std::fs::write(&path, &golden).unwrap();
        std::println!("wrote {}", path.display());
    } else if let Ok(committed) = std::fs::read_to_string(&path) {
        assert_eq!(committed, golden, "expected-scenario.json is stale; rerun with FD_WRITE_GOLDEN=1");
    }
}
