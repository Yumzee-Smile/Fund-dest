import { test } from "node:test";
import assert from "node:assert/strict";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { Replay } from "../src/model/replay.js";
import { loadSeed } from "../src/seed.js";
import { holderStatement, registerReport, renderRegisterMd, renderStatementMd } from "../src/register/statement.js";
import { SEED, SNAPSHOTS } from "./paths.js";

const seed = loadSeed(SEED);
const replay = new Replay(seed);
const { now } = replay.run();

/** Compare with test/snapshots/<name>; FD_UPDATE_SNAPSHOTS=1 rewrites them. */
function snapshot(name: string, text: string): void {
  const p = join(SNAPSHOTS, name);
  if (process.env.FD_UPDATE_SNAPSHOTS === "1" || !existsSync(p)) {
    mkdirSync(SNAPSHOTS, { recursive: true });
    writeFileSync(p, text);
  }
  assert.equal(text, readFileSync(p, "utf8"), `snapshot ${name} changed; rerun with FD_UPDATE_SNAPSHOTS=1 if intended`);
}

test("holder statement: inv_19 (expired after the distribution)", () => {
  const s = holderStatement(replay.classes[0], "inv_19", now, seed.fund.nav_decimals);
  assert.equal(s.kyc.can_send, false);
  assert.equal(s.kyc.can_redeem, true);
  snapshot("statement-inv_19.md", renderStatementMd(s));
});

test("holder statement: inv_05b after the forced transfer", () => {
  const s = holderStatement(replay.classes[0], "inv_05b", now, seed.fund.nav_decimals);
  assert.equal(s.shares, "12,000.0000000");
  snapshot("statement-inv_05b.md", renderStatementMd(s));
});

test("TA register reconciles and lists exceptions", () => {
  const r = registerReport(replay.classes[0], now);
  assert.equal(r.reconciliation.shares_reconciled, true);
  assert.equal(r.reconciliation.cash_reconciled, true);
  assert.ok(r.kyc_expiring.some((k) => k.investor === "inv_12"));
  assert.equal(r.exceptions.length, 10);
  snapshot("register-USD-D.md", renderRegisterMd(r));
  snapshot("register-EUR-A.json", JSON.stringify(registerReport(replay.classes[1], now), null, 2) + "\n");
});
