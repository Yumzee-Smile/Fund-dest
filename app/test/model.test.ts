import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { cashForShares, moveExceedsBand, sharesForCash } from "../src/model/nav.js";
import { Accumulator, SCALE } from "../src/model/distribution.js";
import { Replay, compareWithGolden, golden } from "../src/model/replay.js";
import { loadSeed } from "../src/seed.js";
import { parseAmount, parseNav, formatAmount } from "../src/amount.js";
import { SEED } from "./paths.js";

const D = 14;

test("nav mirror: same figures as math.rs", () => {
  assert.deepEqual(sharesForCash(1_000_000_000_001n, 100_000_000_000_000n, D), { shares: 1_000_000_000_001n, dust: 0n });
  const nav = 100_000_412_000_000n;
  const { shares, dust } = sharesForCash(100_000_000_000n, nav, D);
  assert.equal(shares, 99_999_588_001n);
  assert.ok(dust >= 0n && dust <= 2n);
  assert.equal(cashForShares(1_000_000_000_000n, nav, D), 1_000_004_120_000n);
  assert.equal(moveExceedsBand(nav, parseNav("1.0270"), 25), true);
  assert.equal(moveExceedsBand(nav, parseNav("0.9999987"), 25), false);
});

test("nav mirror: rounding never creates value (sampled)", () => {
  let seed = 12345n;
  const rnd = (mod: bigint) => {
    seed = (seed * 6364136223846793005n + 1442695040888963407n) % 2n ** 64n;
    return seed % mod;
  };
  for (let i = 0; i < 2000; i++) {
    const cash = rnd(10n ** 17n);
    const nav = 50_000_000_000_000n + rnd(150_000_000_000_000n);
    const { shares, dust } = sharesForCash(cash, nav, D);
    assert.ok((shares * nav) / 10n ** 14n <= cash);
    assert.ok(dust >= 0n && dust < (nav + 10n ** 14n - 1n) / 10n ** 14n + 1n);
    assert.ok(cashForShares(shares, nav, D) <= cash);
  }
});

test("accumulator mirror: proportional, carried remainder, transfer semantics", () => {
  const a = new Accumulator();
  a.onChange("A", 0n);
  a.onChange("B", 0n);
  const bal = new Map([["A", 30n], ["B", 10n]]);
  a.declare(100n, 40n);
  assert.equal(a.accrued("A", bal.get("A")!), 75n);
  assert.equal(a.accrued("B", bal.get("B")!), 25n);
  // A transfers everything to B after the declaration: A keeps its entitlement.
  a.onChange("A", 30n);
  a.onChange("B", 10n);
  assert.equal(a.accrued("A", 0n), 75n);
  a.declare(40n, 40n);
  assert.equal(a.accrued("B", 40n), 65n);
  const c = new Accumulator();
  c.declare(1n, 3n * 10_000_000n);
  assert.ok(c.carryScaled > 0n);
  assert.equal(c.acc * 30_000_000n + c.carryScaled, SCALE);
  assert.throws(() => c.declare(1n, 0n), /NoSupply/);
});

test("amount parsing is lenient on format, strict on meaning", () => {
  assert.equal(parseAmount(" 4,000.00 "), 40_000_000_000n);
  assert.equal(parseAmount("1,000,000.0000001"), 10_000_000_000_001n);
  assert.throws(() => parseAmount("1.00000001"), /decimals/);
  assert.throws(() => parseAmount("-5"), /negative/);
  assert.equal(formatAmount(124_185_531_907n, 7), "12,418.5531907");
});

test("seed replay reproduces expected-scenario.json written by the Rust scenario", () => {
  const seed = loadSeed(SEED);
  const res = new Replay(seed).run();
  const expected = JSON.parse(readFileSync(join(SEED, "expected-scenario.json"), "utf8"));
  const diffs = compareWithGolden(res, expected);
  assert.deepEqual(diffs, []);
  // Spot checks against the scenario's design.
  const g = golden(res) as { classes: { class: string; epochs: { liquidity_needed: boolean; settle_calls: number }[]; requests: { investor: string; epoch: number; reject: number }[] }[] };
  const usd = g.classes.find((c) => c.class === "USD-D")!;
  assert.equal(usd.epochs[2].liquidity_needed, true);
  assert.equal(usd.epochs[2].settle_calls, 2);
  assert.equal(usd.requests.find((r) => r.investor === "inv_07" && r.epoch === 2)!.reject, 4);
});

test("seed replay invariants: supply == balances, vault cash == pending + claimable", () => {
  const res = new Replay(loadSeed(SEED)).run();
  for (const c of res.classes) {
    const sum = [...c.investors.keys()].reduce((a, id) => a + c.bal(id), 0n);
    assert.equal(c.totalShares, sum);
    assert.equal(c.vaultCash, 0n);
    assert.ok(c.dist.declared - c.dist.claimed <= BigInt(c.investors.size + 1));
  }
});
