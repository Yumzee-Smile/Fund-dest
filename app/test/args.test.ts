import { test } from "node:test";
import assert from "node:assert/strict";
import { parseArgs, validateCommand } from "../src/args.js";

test("two-word commands and flags parse", () => {
  const p = parseArgs(["kyc", "import", "investors.csv", "--fix", "fix.csv", "--json"]);
  assert.deepEqual(p.command, ["kyc", "import"]);
  assert.deepEqual(p.positionals, ["investors.csv"]);
  assert.equal(p.flags.fix, "fix.csv");
  assert.equal(p.flags.json, true);
  const q = parseArgs(["strike", "--epoch=3", "--publish", "0.9999987", "--override"]);
  assert.deepEqual(q.command, ["strike"]);
  assert.equal(q.flags.epoch, "3");
  assert.equal(q.flags.override, true);
});

test("required options, positionals and unknown options are enforced", () => {
  assert.equal(validateCommand(parseArgs(["kyc", "import"])).ok, false);
  assert.equal(validateCommand(parseArgs(["kyc", "import", "a.csv"])).ok, true);
  const r = validateCommand(parseArgs(["force", "--from", "inv_05", "--to", "inv_05b", "--shares", "all"]));
  assert.equal(r.ok, false);
  assert.match((r as { error: string }).error, /--reason/);
  const u = validateCommand(parseArgs(["transfer", "--from", "a", "--to", "b", "--shares", "1", "--bogus", "x"]));
  assert.match((u as { error: string }).error, /unknown option\(s\): --bogus/);
  assert.equal(validateCommand(parseArgs(["nope"])).ok, false);
});

test("subscribe needs --amount or --cancel, not both", () => {
  assert.equal(validateCommand(parseArgs(["subscribe", "--investor", "inv_01"])).ok, false);
  assert.equal(validateCommand(parseArgs(["subscribe", "--investor", "inv_01", "--amount", "1,000"])).ok, true);
  assert.equal(validateCommand(parseArgs(["subscribe", "--investor", "inv_01", "--cancel", "5"])).ok, true);
  assert.equal(validateCommand(parseArgs(["subscribe", "--investor", "inv_01", "--amount", "1", "--cancel", "5"])).ok, false);
});

test("redeem needs --shares with --cash-to", () => {
  assert.equal(validateCommand(parseArgs(["redeem", "--investor", "inv_12", "--shares", "8000"])).ok, false);
  assert.equal(validateCommand(parseArgs(["redeem", "--investor", "inv_12", "--shares", "8000", "--cash-to", "GABC"])).ok, true);
});

test("claim, statement and provider validation", () => {
  assert.equal(validateCommand(parseArgs(["claim"])).ok, false);
  assert.equal(validateCommand(parseArgs(["claim", "--epoch", "2", "--all"])).ok, true);
  assert.equal(validateCommand(parseArgs(["claim", "--request", "7"])).ok, true);
  assert.equal(validateCommand(parseArgs(["statement"])).ok, false);
  assert.equal(validateCommand(parseArgs(["statement", "--register", "--format", "json"])).ok, true);
  assert.equal(validateCommand(parseArgs(["kyc", "triage", "--docs", "d", "--provider", "gpt"])).ok, false);
  assert.equal(validateCommand(parseArgs(["kyc", "triage", "--docs", "d", "--provider", "llm"])).ok, true);
});
