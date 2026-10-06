import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { importInvestors, normaliseJurisdiction, parseExpiry } from "../src/register/csv.js";
import { SEED } from "./paths.js";

const csv = readFileSync(join(SEED, "investors.csv"), "utf8");
const fix = readFileSync(join(SEED, "investors.fix.csv"), "utf8");

test("investors.csv: 25 rows -> 24 valid + 1 checksum error", () => {
  const r = importInvestors(csv);
  assert.equal(r.register.length, 24);
  assert.equal(r.errors.length, 1);
  assert.equal(r.errors[0].investor_id, "inv_20");
  assert.equal(r.errors[0].field, "wallet");
  assert.match(r.errors[0].message, /checksum/);
});

test("codes are normalised: padded, lower-case and alpha-3", () => {
  const r = importInvestors(csv, fix);
  const code = (id: string) => r.register.find((x) => x.investor_id === id)!.jurisdiction;
  assert.equal(code("inv_02"), "DE"); // "De "
  assert.equal(code("inv_04"), "ES"); // "ESP"
  assert.equal(code("inv_08"), "FR"); // "fr"
  assert.equal(code("inv_15"), "BE"); // "be"
  assert.equal(code("inv_25"), "AT"); // "at "
  assert.equal(code("inv_23"), "US"); // valid code, blocked later by the registrar
  assert.ok(r.warnings.some((w) => w.investor_id === "inv_13" && /same legal name/.test(w.message)));
});

test("the corrected row makes 25 valid entries equal to the committed register.json", () => {
  const r = importInvestors(csv, fix);
  assert.equal(r.errors.length, 0);
  assert.equal(r.register.length, 25);
  const committed = JSON.parse(readFileSync(join(SEED, "register.json"), "utf8"));
  assert.deepEqual(r.register, committed);
  const e10 = r.register.find((x) => x.investor_id === "inv_10")!;
  assert.equal(e10.investor_type, 1);
  assert.equal(e10.cash_addresses.length, 2);
  assert.equal(e10.kyc_expiry, "2028-03-31T00:00:00Z"); // "31.03.2028"
});

test("date formats and jurisdiction edge cases", () => {
  assert.equal(parseExpiry("2026-10-06T12:00:00Z").iso, "2026-10-06T12:00:00Z");
  assert.equal(parseExpiry("15.03.2027").iso, "2027-03-15T00:00:00Z");
  const amb = parseExpiry("03/04/2027");
  assert.equal(amb.ambiguous, true);
  assert.equal(amb.iso, "2027-04-03T00:00:00Z"); // register export is day-first
  assert.equal(parseExpiry("31/02/2027").iso, null);
  assert.equal(parseExpiry("next year").iso, null);
  assert.equal(normaliseJurisdiction("XX").code, null);
  assert.equal(normaliseJurisdiction("LUX").code, "LU");
});
