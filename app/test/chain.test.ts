import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { StrKey } from "@stellar/stellar-sdk";
import * as tx from "../src/chain/tx.js";
import { decode, placeholderId } from "../src/chain/client.js";
import { loadConfig } from "../src/config.js";
import { loadFund } from "../src/seed.js";
import { main } from "../src/cli.js";
import type { RegisterEntry } from "../src/register/csv.js";
import { SEED } from "./paths.js";

const fund = loadFund(SEED);
const register = JSON.parse(readFileSync(join(SEED, "register.json"), "utf8")) as RegisterEntry[];
const config = loadConfig({});
const ctx: tx.Ctx = { config, fund, register };
const inv = (id: string) => register.find((r) => r.investor_id === id)!;

test("subscription and redemption transactions round-trip through the contract spec", () => {
  const s = tx.subscribe(ctx, "inv_10", "1,000,000.0000001");
  const d = decode("async_vault", s.transactionXdr, config.networkPassphrase);
  assert.equal(d.fn, "request_subscribe");
  assert.equal(d.contractId, placeholderId("async_vault"));
  assert.deepEqual(d.args, [inv("inv_10").wallet, 10_000_000_000_001n]);
  const r = tx.redeem(ctx, "inv_12", "8,000", inv("inv_12").cash_addresses[0]);
  assert.deepEqual(decode("async_vault", r.transactionXdr, config.networkPassphrase).args, [inv("inv_12").wallet, 80_000_000_000n, inv("inv_12").cash_addresses[0]]);
  assert.match(r.cli, /request_redeem --investor G/);
});

test("set_investor encodes the Investor struct", () => {
  const a = tx.kycApprove(ctx, "inv_07", "2027-10-06", "ie", inv("inv_07").cash_addresses);
  const d = decode("compliance", a.transactionXdr, config.networkPassphrase);
  const rec = d.args[1] as Record<string, unknown>;
  assert.equal(rec.jurisdiction, "IE");
  assert.equal(rec.kyc_expiry, 1822780800n);
  assert.deepEqual(rec.cash_addresses, inv("inv_07").cash_addresses);
  assert.throws(() => tx.kycApprove(ctx, "inv_07", "2027-10-06", "IRL", inv("inv_07").cash_addresses), /alpha-2/);
  assert.throws(() => tx.kycApprove(ctx, "inv_07", "2027-10-06", "IE", []), /1 to 3/);
  assert.throws(() => tx.subscribe(ctx, "inv_99", "1000"), /unknown investor/);
});

test("publish + strike, declare with hashed memo, push in batches of 25", () => {
  const p = tx.publish(ctx, "USD_D", "0.99999870000000", "2026-10-07T16:00:00Z");
  const d = decode("nav_oracle", p.transactionXdr, config.networkPassphrase);
  assert.deepEqual(d.args[0], { tag: "Other", values: ["USD_D"] });
  assert.equal(d.args[1], 99_999_870_000_000n);
  assert.equal(tx.strike(ctx, 3, true).fn, "strike_nav_override");
  const decl = tx.declare(ctx, "12,418.5531907", "SEEDFUND-USD-D-2026-10 monthly income");
  const dd = decode("distribution", decl.transactionXdr, config.networkPassphrase);
  assert.equal(dd.args[0], 124_185_531_907n);
  assert.equal((dd.args[1] as Buffer).length, 32);
  const holders = register.map((r) => r.investor_id);
  assert.equal(tx.distPush(ctx, holders).length, 1);
  assert.equal(tx.distPush(ctx, [...holders, ...holders]).length, 2);
});

test("deployment plan follows the flag -> SAC -> contracts -> set_admin -> bind order", () => {
  const steps = tx.initPlan(ctx, "USD-D");
  const idx = (re: RegExp) => steps.findIndex((s) => re.test(s.what));
  assert.ok(idx(/AUTH_REQUIRED/) < idx(/SAC/));
  assert.ok(idx(/deploy compliance/) < idx(/SAC admin to compliance/));
  assert.ok(idx(/SAC admin to compliance/) < idx(/^bind/));
  assert.equal(steps.filter((s) => s.what.startsWith("policy ")).length, fund.policies.length);
  assert.equal(steps.filter((s) => s.what.startsWith("allow jurisdiction")).length, 13);
  assert.ok(StrKey.isValidContract(placeholderId("compliance")));
});

test("CLI composes offline and never sends without a reachable RPC", async () => {
  const out: string[] = [];
  const err: string[] = [];
  const io = { out: (s: string) => out.push(s), err: (s: string) => err.push(s) };
  assert.equal(await main(["subscribe", "--investor", "inv_01", "--amount", "25,000", "--seed-dir", SEED], io, {}), 0);
  assert.match(out.join("\n"), /async_vault.request_subscribe/);
  assert.match(out.join("\n"), /not sent/);
  const code = await main(["epoch", "open", "--cutoff", "2026-10-08T13:00:00Z", "--submit", "--seed-dir", SEED], io, { SOROBAN_RPC_URL: "http://127.0.0.1:9/" });
  assert.equal(code, 2);
  assert.match(err.join("\n"), /not reachable; nothing was sent/);
  assert.equal(await main(["bogus"], io, {}), 1);
});

test("CLI kyc expiring and statement run offline", async () => {
  const out: string[] = [];
  const io = { out: (s: string) => out.push(s), err: () => {} };
  assert.equal(await main(["kyc", "expiring", "--days", "30", "--now", "2026-10-05T00:00:00Z", "--seed-dir", SEED], io, {}), 0);
  const text = out.join("\n");
  assert.match(text, /inv_07/);
  assert.match(text, /inv_12/);
  assert.match(text, /inv_19/);
  out.length = 0;
  assert.equal(await main(["statement", "--investor", "inv_12", "--format", "json", "--seed-dir", SEED], io, {}), 0);
  assert.equal(JSON.parse(out.join("\n")).kyc.status, "EXPIRED");
});
