/** Loading of the simulated seed data in data/seed/. */
import { readFileSync, existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { importInvestors, readRows, type RegisterEntry } from "./register/csv.js";

export function defaultSeedDir(): string {
  const here = dirname(fileURLToPath(import.meta.url));
  // dist/src/seed.js -> ../../../data/seed ; src/seed.ts (ts-node) -> ../../data/seed
  for (const up of ["../../../data/seed", "../../data/seed"]) {
    const p = resolve(here, up);
    if (existsSync(join(p, "fund.json"))) return p;
  }
  return resolve(process.cwd(), "../data/seed");
}

export interface EpochCal {
  epoch: number;
  open_at: string;
  cutoff: string;
  settle_at: string;
  liquidity_topup: string;
}

export interface ClassCfg {
  id: string;
  type: string;
  cash: string;
  oracle_asset: string;
  min_subscription: string;
  initial_nav: string;
  epochs: EpochCal[];
}

export interface PolicyRow {
  contract: string;
  fn: string;
  ta: number;
  admin: number;
  total: number;
}

export interface Fund {
  fund: string;
  nav_decimals: number;
  cash_decimals: number;
  max_strike_delay_s: number;
  max_nav_move_bps: number;
  max_requests_per_epoch: number;
  settle_batch: number;
  allowed_jurisdictions: string[];
  signers: { name: string; role: "Ta" | "Admin" }[];
  assumptions: { proof_of_address_validity_days: number; registry_extract_validity_months: number };
  policies: PolicyRow[];
  classes: ClassCfg[];
}

export type Row = Record<string, string>;

export interface Seed {
  dir: string;
  fund: Fund;
  register: RegisterEntry[];
  requests: Row[];
  nav: Row[];
  distribution: Row[];
  forced: Row[];
}

const rows = (dir: string, f: string): Row[] => readRows(readFileSync(join(dir, f), "utf8")).map((r) => r.row);

export function loadFund(dir: string = defaultSeedDir()): Fund {
  return JSON.parse(readFileSync(join(dir, "fund.json"), "utf8")) as Fund;
}

export function loadSeed(dir: string = defaultSeedDir()): Seed {
  const imported = importInvestors(
    readFileSync(join(dir, "investors.csv"), "utf8"),
    readFileSync(join(dir, "investors.fix.csv"), "utf8"),
  );
  if (imported.errors.length) throw new Error(`seed register has errors: ${JSON.stringify(imported.errors)}`);
  return {
    dir,
    fund: loadFund(dir),
    register: imported.register,
    requests: rows(dir, "requests.csv"),
    nav: rows(dir, "nav.csv"),
    distribution: rows(dir, "distribution.csv"),
    forced: rows(dir, "forced.csv"),
  };
}
