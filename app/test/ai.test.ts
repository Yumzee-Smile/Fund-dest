import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { HeuristicProvider, mrzCheckDigit, parseMrz, wordsToNumber, findDate } from "../src/ai/heuristic.js";
import { LlmProvider } from "../src/ai/llm.js";
import { loadDocs, runTriage, renderTriage } from "../src/ai/kyc-triage.js";
import { runExtract } from "../src/ai/form-extract.js";
import { rulesFromFund } from "../src/ai/index.js";
import { validateTriage } from "../src/ai/schema.js";
import { loadFund } from "../src/seed.js";
import type { RegisterEntry } from "../src/register/csv.js";
import { FIXTURES, SEED } from "./paths.js";

const fund = loadFund(SEED);
const rules = rulesFromFund(fund);
const register = JSON.parse(readFileSync(join(SEED, "register.json"), "utf8")) as RegisterEntry[];
const kycLabels = JSON.parse(readFileSync(join(FIXTURES, "kyc/labels.json"), "utf8"));
const formLabels = JSON.parse(readFileSync(join(FIXTURES, "forms/labels.json"), "utf8"));
const now = new Date(kycLabels.now);

test("MRZ check digits (ICAO 9303 examples)", () => {
  assert.equal(mrzCheckDigit("L898902C3"), 6);
  assert.equal(mrzCheckDigit("740812"), 2);
  assert.equal(mrzCheckDigit("120415"), 9);
  const good = readFileSync(join(FIXTURES, "kyc/inv_01__passport.txt"), "utf8");
  assert.deepEqual(parseMrz(good)?.valid, true);
  const bad = readFileSync(join(FIXTURES, "kyc/inv_05__passport.txt"), "utf8");
  assert.deepEqual(parseMrz(bad)?.failed.includes("expiry date"), true);
  const td1 = readFileSync(join(FIXTURES, "kyc/inv_02__idcard.txt"), "utf8");
  assert.equal(parseMrz(td1)?.format, "TD1");
  assert.equal(parseMrz(td1)?.expiry, "2027-03-15");
});

test("date reading: EN/FR/DE/ES formats and dd/mm ambiguity", () => {
  assert.deepEqual(findDate("Fecha de caducidad: 03/04/2027"), { date: "2027-03-04", ambiguous: true, raw: "03/04/2027" });
  assert.equal(findDate("Gültig bis: 15.03.2027")?.date, "2027-03-15");
  assert.equal(findDate("Date of expiry: 06 OCT 2026")?.date, "2026-10-06");
  assert.equal(findDate("09 SET/SEP 2027")?.date, "2027-09-09");
  assert.equal(findDate("Date d'expiration / Date of expiry: 14 03 2031")?.date, "2031-03-14");
});

test("heuristic triage matches labels.json on the 12 fixtures (Simulated accuracy)", async () => {
  const inputs = loadDocs(join(FIXTURES, "kyc"), register);
  const docCount = inputs.reduce((a, i) => a + i.documents.length, 0);
  assert.equal(docCount, 12);
  const results = await runTriage(inputs, new HeuristicProvider(rules), now);
  let fields = 0;
  let right = 0;
  for (const r of results) {
    assert.deepEqual(validateTriage(r), []);
    const want = kycLabels.investors[r.investor_id];
    assert.equal(r.bucket, want.bucket, `${r.investor_id} bucket`);
    assert.equal(r.effective_expiry, want.effective_expiry, `${r.investor_id} effective`);
    for (const d of r.documents) {
      const l = kycLabels.documents[d.doc_id];
      for (const k of ["doc_type", "expiry", "basis", "ambiguous"] as const) {
        fields++;
        if (d[k] === l[k]) right++;
      }
    }
  }
  // bad MRZ, ambiguous date and unreadable scan all go to a person
  for (const id of ["inv_05", "inv_22", "inv_24"]) assert.equal(results.find((r) => r.investor_id === id)!.bucket, "needs_human");
  assert.equal(results.find((r) => r.investor_id === "inv_12")!.bucket, "expired");
  assert.equal(right, fields, `field accuracy ${right}/${fields}`);
  console.log(`heuristic KYC field accuracy on synthetic fixtures (Simulated, not a claim about real documents): ${right}/${fields}`);
  const md = renderTriage(results, now, "heuristic", inputs);
  assert.match(md, /Assumptions/);
});

test("form extraction flags the wallet typo, the words conflict and the missing signature", async () => {
  const rows = await runExtract(join(FIXTURES, "forms"), new HeuristicProvider(rules));
  assert.equal(rows.length, 8);
  let fields = 0;
  let right = 0;
  for (const { form_id, result } of rows) {
    const want = formLabels.forms[form_id];
    assert.deepEqual([...result.flags].sort(), [...want.flags].sort(), `${form_id} flags`);
    for (const k of ["legal_name", "investor_type", "jurisdiction", "share_class", "currency", "amount", "amount_in_words_matches", "signed", "date"]) {
      fields++;
      if (JSON.stringify((result as unknown as Record<string, unknown>)[k]) === JSON.stringify(want[k])) right++;
    }
  }
  assert.equal(right, fields, `form field accuracy ${right}/${fields}`);
  console.log(`heuristic form field accuracy on synthetic fixtures (Simulated): ${right}/${fields}`);
  assert.equal(wordsToNumber("one hundred fifty thousand"), 150000);
  assert.equal(wordsToNumber("twenty-five thousand"), 25000);
});

function mockFetch(bodies: string[]): { fetch: typeof fetch; calls: () => number } {
  let n = 0;
  const f = (async () => {
    const text = bodies[Math.min(n, bodies.length - 1)];
    n++;
    return new Response(JSON.stringify({ content: [{ type: "text", text }] }), { status: 200, headers: { "content-type": "application/json" } });
  }) as typeof fetch;
  return { fetch: f, calls: () => n };
}

const inv01 = () => loadDocs(join(FIXTURES, "kyc"), register).find((i) => i.investor_id === "inv_01")!;

test("LlmProvider: invalid JSON twice -> heuristic fallback marked needs_human", async () => {
  const m = mockFetch(["Sure! Here is the answer: {not json", "{\"investor_id\": 3}"]);
  const p = new LlmProvider({ apiKey: "test", model: "claude-sonnet-5", heuristic: new HeuristicProvider(rules), fetchImpl: m.fetch });
  const out = await p.triage(inv01(), now);
  assert.equal(m.calls(), 2);
  assert.equal(out.bucket, "needs_human");
  assert.equal(out.effective_expiry, "2026-11-18"); // the heuristic's reading is kept for the reviewer
  assert.ok(out.reasons!.some((r) => /invalid twice/.test(r)));
});

test("LlmProvider: valid answer that disagrees with the heuristic -> needs_human", async () => {
  const h = await new HeuristicProvider(rules).triage(inv01(), now);
  const wrong = { ...h, effective_expiry: "2031-03-14", bucket: "ok", reasons: undefined };
  const m = mockFetch([JSON.stringify(wrong)]);
  const p = new LlmProvider({ apiKey: "test", model: "claude-sonnet-5", heuristic: new HeuristicProvider(rules), fetchImpl: m.fetch });
  const out = await p.triage(inv01(), now);
  assert.equal(m.calls(), 1);
  assert.equal(out.bucket, "needs_human");
  assert.ok(out.reasons!.some((r) => /effective_expiry/.test(r)));
});

test("LlmProvider: agreeing answer passes; form fallback flags needs_human", async () => {
  const h = await new HeuristicProvider(rules).triage(inv01(), now);
  const m = mockFetch(["```json\n" + JSON.stringify({ ...h, reasons: undefined }) + "\n```"]);
  const p = new LlmProvider({ apiKey: "test", model: "claude-sonnet-5", heuristic: new HeuristicProvider(rules), fetchImpl: m.fetch });
  assert.equal((await p.triage(inv01(), now)).bucket, "lt90");
  const bad = mockFetch(["nope", "still nope"]);
  const p2 = new LlmProvider({ apiKey: "test", model: "claude-sonnet-5", heuristic: new HeuristicProvider(rules), fetchImpl: bad.fetch });
  const form = await p2.extract({ form_id: "f01", text: readFileSync(join(FIXTURES, "forms/f01_clean.txt"), "utf8") });
  assert.ok(form.flags.includes("needs_human") && form.flags.includes("llm_invalid_output"));
});
