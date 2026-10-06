//! NAV oracle: a minimal SEP-40 price feed for the fund's net asset value.
//!
//! The fund administrator (through the `ops_account` under its ADMIN policy)
//! publishes the NAV it struck for a share class, with the timestamp at which
//! it was struck. `async_vault` reads it through the SEP-40 subset
//! `decimals()` + `lastprice(asset)`, so a RedStone or Reflector SEP-40
//! contract can replace this one by changing an address.
#![no_std]

use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, Address, Env, Symbol, Vec,
};

const LEDGERS_PER_DAY: u32 = 17_280;
const INSTANCE_TTL_THRESHOLD: u32 = LEDGERS_PER_DAY * 30;
const INSTANCE_TTL_EXTEND_TO: u32 = LEDGERS_PER_DAY * 120;
/// NAV history is kept for about a year after its last write.
const PRICE_TTL_THRESHOLD: u32 = LEDGERS_PER_DAY * 30;
const PRICE_TTL_EXTEND_TO: u32 = LEDGERS_PER_DAY * 365;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum OracleError {
    NonPositivePrice = 1,
    TimestampInFuture = 2,
    TimestampNotIncreasing = 3,
}

/// SEP-40 asset identifier.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Asset {
    Stellar(Address),
    Other(Symbol),
}

/// SEP-40 price record.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceData {
    pub price: i128,
    pub timestamp: u64,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Publisher,
    Base,
    Decimals,
    Resolution,
    Assets,
    Last(Asset),
    Hist(Asset, u64),
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Published {
    #[topic]
    pub asset: Asset,
    pub price: i128,
    pub timestamp: u64,
}

#[contract]
pub struct NavOracle;

fn bump(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_EXTEND_TO);
}

#[contractimpl]
impl NavOracle {
    pub fn __constructor(env: Env, publisher: Address, base: Asset, decimals: u32, resolution: u32) {
        let s = env.storage().instance();
        s.set(&DataKey::Publisher, &publisher);
        s.set(&DataKey::Base, &base);
        s.set(&DataKey::Decimals, &decimals);
        s.set(&DataKey::Resolution, &resolution);
        s.set(&DataKey::Assets, &Vec::<Asset>::new(&env));
        bump(&env);
    }

    /// Publish a NAV. Publisher only (the ops account, ADMIN policy).
    pub fn publish(env: Env, asset: Asset, price: i128, timestamp: u64) -> Result<(), OracleError> {
        let publisher: Address = env.storage().instance().get(&DataKey::Publisher).unwrap();
        publisher.require_auth();
        if price <= 0 {
            return Err(OracleError::NonPositivePrice);
        }
        if timestamp > env.ledger().timestamp() {
            return Err(OracleError::TimestampInFuture);
        }
        let last_key = DataKey::Last(asset.clone());
        let p = env.storage().persistent();
        match p.get::<_, PriceData>(&last_key) {
            Some(last) => {
                if timestamp <= last.timestamp {
                    return Err(OracleError::TimestampNotIncreasing);
                }
            }
            None => {
                let mut assets: Vec<Asset> = env.storage().instance().get(&DataKey::Assets).unwrap();
                assets.push_back(asset.clone());
                env.storage().instance().set(&DataKey::Assets, &assets);
            }
        }
        let rec = PriceData { price, timestamp };
        p.set(&last_key, &rec);
        p.extend_ttl(&last_key, PRICE_TTL_THRESHOLD, PRICE_TTL_EXTEND_TO);
        let hist_key = DataKey::Hist(asset.clone(), timestamp);
        p.set(&hist_key, &price);
        p.extend_ttl(&hist_key, PRICE_TTL_THRESHOLD, PRICE_TTL_EXTEND_TO);
        bump(&env);
        Published {
            asset,
            price,
            timestamp,
        }
        .publish(&env);
        Ok(())
    }

    // ----- SEP-40 reads -----

    pub fn base(env: Env) -> Asset {
        env.storage().instance().get(&DataKey::Base).unwrap()
    }

    pub fn assets(env: Env) -> Vec<Asset> {
        env.storage().instance().get(&DataKey::Assets).unwrap()
    }

    pub fn decimals(env: Env) -> u32 {
        env.storage().instance().get(&DataKey::Decimals).unwrap()
    }

    pub fn resolution(env: Env) -> u32 {
        env.storage().instance().get(&DataKey::Resolution).unwrap()
    }

    pub fn lastprice(env: Env, asset: Asset) -> Option<PriceData> {
        env.storage().persistent().get(&DataKey::Last(asset))
    }

    /// The price published with exactly this timestamp, if any.
    pub fn price(env: Env, asset: Asset, timestamp: u64) -> Option<PriceData> {
        env.storage()
            .persistent()
            .get::<_, i128>(&DataKey::Hist(asset, timestamp))
            .map(|price| PriceData { price, timestamp })
    }

    pub fn publisher(env: Env) -> Address {
        env.storage().instance().get(&DataKey::Publisher).unwrap()
    }
}

#[cfg(test)]
mod test;
