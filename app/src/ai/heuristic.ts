/**
 * Deterministic provider: the default everywhere and the reference the model
 * provider is checked against.
 *
 * KYC triage
 * - passport / ID-card MRZ (TD3 and TD1) with ICAO 9303 check digits; a failed
 *   check digit sends the investor to `needs_human`;
 * - keyword + date regex in EN/FR/DE/ES/IT ("expiry", "valid until",
 *   "date d'expiration", "gültig bis", "fecha de caducidad", ...);
 * - an ambiguous dd/mm vs mm/dd date takes the earlier reading and is flagged;
 * - proof of address = issue + 90 days, registry extract = issue + 3 months
 *   (Assumptions, configurable in fund.json);
 * - effective expiry = minimum over the required document types.
 *
 * Subscription forms: labelled-field regex, then validators (StrKey checksum,
 * ISO code table, minimum amount, class/currency, amount in words).
 */
import { StrKey } from "@stellar/stellar-sdk";
import { normaliseJurisdiction } from "../register/csv.js";
import { formatFixed, parseAmount, AmountError } from "../amount.js";
import type { FundRules, Provider } from "./provider.js";
import type { Basis, Bucket, DocType, FormExtractInput, FormExtractOutput, KycTriageInput, KycTriageOutput, TriageDoc } from "./schema.js";

// ---------------------------------------------------------------------------
// MRZ
// ---------------------------------------------------------------------------

const mrzVal = (c: string): number => (c >= "0" && c <= "9" ? c.charCodeAt(0) - 48 : c === "<" ? 0 : c.charCodeAt(0) - 55);

export function mrzCheckDigit(s: string): number {
  const w = [7, 3, 1];
  let sum = 0;
  for (let i = 0; i < s.length; i++) sum += mrzVal(s[i]) * w[i % 3];
  return sum % 10;
}

export interface MrzResult {
  format: "TD3" | "TD1";
  docType: "passport" | "id_card";
  expiry: string; // YYYY-MM-DD
  valid: boolean;
  failed: string[];
}

const yymmdd = (s: string): string => `20${s.slice(0, 2)}-${s.slice(2, 4)}-${s.slice(4, 6)}`;

/** Find and check an MRZ in OCR text. */
export function parseMrz(text: string): MrzResult | null {
  const lines = text.split(/\r?\n/).map((l) => l.trim().replace(/\s/g, "")).filter((l) => /^[A-Z0-9<]+$/.test(l));
  for (let i = 0; i + 1 < lines.length; i++) {
    const [l1, l2] = [lines[i], lines[i + 1]];
    if (l1.length === 44 && l2.length === 44 && l1.startsWith("P")) {
      const failed: string[] = [];
      const chk = (field: string, digit: string, name: string) => {
        if (String(mrzCheckDigit(field)) !== digit) failed.push(name);
      };
      chk(l2.slice(0, 9), l2[9], "document number");
      chk(l2.slice(13, 19), l2[19], "birth date");
      chk(l2.slice(21, 27), l2[27], "expiry date");
      chk(l2.slice(28, 42), l2[42], "personal number");
      chk(l2.slice(0, 10) + l2.slice(13, 20) + l2.slice(21, 43), l2[43], "composite");
      return { format: "TD3", docType: "passport", expiry: yymmdd(l2.slice(21, 27)), valid: failed.length === 0, failed };
    }
    if (l1.length === 30 && l2.length === 30 && /^[IAC]/.test(l1)) {
      const failed: string[] = [];
      if (String(mrzCheckDigit(l1.slice(5, 14))) !== l1[14]) failed.push("document number");
      if (String(mrzCheckDigit(l2.slice(0, 6))) !== l2[6]) failed.push("birth date");
      if (String(mrzCheckDigit(l2.slice(8, 14))) !== l2[14]) failed.push("expiry date");
      const comp = l1.slice(5, 30) + l2.slice(0, 7) + l2.slice(8, 15) + l2.slice(18, 29);
      if (String(mrzCheckDigit(comp)) !== l2[29]) failed.push("composite");
      return { format: "TD1", docType: "id_card", expiry: yymmdd(l2.slice(8, 14)), valid: failed.length === 0, failed };
    }
  }
  return null;
}

// ---------------------------------------------------------------------------
// dates
// ---------------------------------------------------------------------------

const MONTHS: Record<string, number> = {
  JAN: 1, FEB: 2, MAR: 3, APR: 4, MAY: 5, JUN: 6, JUL: 7, AUG: 8, SEP: 9, OCT: 10, NOV: 11, DEC: 12,
  JANV: 1, FEV: 2, FÉV: 2, AVR: 4, MAI: 5, JUIN: 6, JUIL: 7, AOU: 8, AOÛ: 8, SEPT: 9, DÉC: 12,
  MÄR: 3, MRZ: 3, OKT: 10, DEZ: 12, ENE: 1, ABR: 4, AGO: 8, DIC: 12, GEN: 1, MAG: 5, GIU: 6, LUG: 7, SET: 9, OTT: 10,
};

const iso = (y: number, m: number, d: number): string | null => {
  const t = Date.UTC(y, m - 1, d);
  const b = new Date(t);
  if (b.getUTCFullYear() !== y || b.getUTCMonth() !== m - 1 || b.getUTCDate() !== d) return null;
  return b.toISOString().slice(0, 10);
};

export interface FoundDate {
  date: string;
  ambiguous: boolean;
  raw: string;
}

/** Parse the first date in `s`. dd/mm vs mm/dd ambiguity -> earlier reading, flagged. */
export function findDate(s: string): FoundDate | null {
  let m: RegExpMatchArray | null;
  if ((m = s.match(/(\d{4})-(\d{2})-(\d{2})/))) {
    const d = iso(+m[1], +m[2], +m[3]);
    return d ? { date: d, ambiguous: false, raw: m[0] } : null;
  }
  if ((m = s.match(/(\d{1,2})\s*[./]\s*(\d{1,2})\s*[./]\s*(\d{4})/))) {
    const a = +m[1];
    const b = +m[2];
    const y = +m[3];
    const sep = m[0].includes(".") ? "." : "/";
    if (sep === "/" && a <= 12 && b <= 12 && a !== b) {
      const dm = iso(y, b, a)!;
      const md = iso(y, a, b)!;
      return { date: dm < md ? dm : md, ambiguous: true, raw: m[0] };
    }
    const d = iso(y, b, a);
    return d ? { date: d, ambiguous: false, raw: m[0] } : null;
  }
  if ((m = s.match(/(\d{1,2})\s+([A-ZÄÉÛa-zäéû]{3,4})(?:\/([A-Za-z]{3}))?\.?\s+(\d{4})/))) {
    const mon = MONTHS[m[2].toUpperCase()] ?? (m[3] ? MONTHS[m[3].toUpperCase()] : undefined);
    if (!mon) return null;
    const d = iso(+m[4], mon, +m[1]);
    return d ? { date: d, ambiguous: false, raw: m[0] } : null;
  }
  if ((m = s.match(/(\d{2})\s+(\d{2})\s+(\d{4})/))) {
    const d = iso(+m[3], +m[2], +m[1]);
    return d ? { date: d, ambiguous: false, raw: m[0] } : null;
  }
  return null;
}

const EXPIRY_KEYS = /(date of expiry|expiry|expires|valid until|date d'expiration|expiration|gültig bis|gueltig bis|fecha de caducidad|caducidad|data di scadenza|scadenza)/i;
const ISSUE_KEYS = /(date de facture|bill date|invoice date|statement date|date of issue|issued on|délivré le|delivre le|ausgestellt am|datum|fecha de emisi[oó]n|data di emissione)/i;

/** A date following one of the keywords on the same line. */
function keyedDate(text: string, keys: RegExp): (FoundDate & { line: string }) | null {
  for (const line of text.split(/\r?\n/)) {
    const k = line.match(keys);
    if (!k) continue;
    const rest = line.slice((k.index ?? 0) + k[0].length);
    const d = findDate(rest);
    if (d) return { ...d, line: line.trim() };
  }
  return null;
}

export function addDays(isoDate: string, days: number): string {
  return new Date(Date.parse(`${isoDate}T00:00:00Z`) + days * 86_400_000).toISOString().slice(0, 10);
}

export function addMonths(isoDate: string, months: number): string {
  const [y, m, d] = isoDate.split("-").map(Number);
  const t = new Date(Date.UTC(y, m - 1 + months, 1));
  const last = new Date(Date.UTC(t.getUTCFullYear(), t.getUTCMonth() + 1, 0)).getUTCDate();
  return new Date(Date.UTC(t.getUTCFullYear(), t.getUTCMonth(), Math.min(d, last))).toISOString().slice(0, 10);
}

// ---------------------------------------------------------------------------
// document classification
// ---------------------------------------------------------------------------

export function classify(text: string): DocType {
  const t = text.toUpperCase();
  if (/REGISTRE DE COMMERCE|HANDELSREGISTER|COMPANIES HOUSE|REGISTRO MERCANTIL|COMMERCIAL REGISTER|REGISTRY EXTRACT/.test(t)) return "registry_extract";
  if (/IDENTITY CARD|PERSONALAUSWEIS|DOCUMENTO NACIONAL DE IDENTIDAD|CARTE NATIONALE D'IDENTIT|CARTA D'IDENTIT/.test(t)) return "id_card";
  if (/\bPASSPORT\b|PASSEPORT|PASSAPORTO|REISEPASS|PASAPORTE/.test(t)) return "passport";
  if (/FACTURE|\bBILL\b|INVOICE|KONTOAUSZUG|BANK STATEMENT|ELECTRICITY|ELECTRICITE|UTILITY|WATER AUTHORITY/.test(t)) return "proof_of_address";
  return "other";
}

// ---------------------------------------------------------------------------
// provider
// ---------------------------------------------------------------------------

export const DEFAULT_RULES: FundRules = {
  proofOfAddressDays: 90,
  registryMonths: 3,
  minSubscription: 1_000n * 10_000_000n,
  allowedJurisdictions: ["FR", "DE", "NL", "ES", "IT", "LU", "BE", "IE", "PT", "AT", "CH", "SG", "AE"],
  classCurrency: { "USD-D": "USDC", "EUR-A": "EURC" },
};

export function triageDocument(doc_id: string, text: string, rules: FundRules): TriageDoc & { mrzFailed: boolean } {
  let doc_type = classify(text);
  const mrz = parseMrz(text);
  if (mrz && doc_type === "other") doc_type = mrz.docType;
  let expiry: string | null = null;
  let basis: Basis = "none";
  let ambiguous = false;
  let evidence = "";
  let mrzFailed = false;
  if (doc_type === "passport" || doc_type === "id_card") {
    if (mrz && mrz.valid) {
      expiry = mrz.expiry;
      basis = "mrz";
      evidence = `MRZ ${mrz.format} expiry ${mrz.expiry}, all check digits valid`;
    } else {
      if (mrz) {
        mrzFailed = true;
        evidence = `MRZ ${mrz.format} check digit failed (${mrz.failed.join(", ")}); `;
      }
      const k = keyedDate(text, EXPIRY_KEYS);
      if (k) {
        expiry = k.date;
        basis = "stated";
        ambiguous = k.ambiguous;
        evidence += `"${k.line}"${k.ambiguous ? " (dd/mm vs mm/dd ambiguous; earlier reading taken)" : ""}`;
      } else {
        evidence += "no expiry date found";
      }
    }
  } else if (doc_type === "proof_of_address" || doc_type === "registry_extract") {
    const k = keyedDate(text, ISSUE_KEYS);
    if (k) {
      ambiguous = k.ambiguous;
      if (doc_type === "proof_of_address") {
        expiry = addDays(k.date, rules.proofOfAddressDays);
        basis = "issue+90d";
      } else {
        expiry = addMonths(k.date, rules.registryMonths);
        basis = "issue+3m";
      }
      evidence = `issued ${k.date} ("${k.line}") + ${doc_type === "proof_of_address" ? `${rules.proofOfAddressDays} days` : `${rules.registryMonths} months`} [Assumption]`;
    } else {
      evidence = "no issue date found";
    }
  } else {
    evidence = "document type not recognised (unreadable or unsupported)";
  }
  return { doc_id, doc_type, expiry, basis, ambiguous, evidence, mrzFailed };
}

export function bucketFor(effective: string | null, now: Date): Bucket {
  if (!effective) return "needs_human";
  const days = Math.floor((Date.parse(`${effective}T00:00:00Z`) - now.getTime()) / 86_400_000);
  if (days <= 0) return "expired";
  if (days < 30) return "lt30";
  if (days < 90) return "lt90";
  return "ok";
}

export function triage(input: KycTriageInput, now: Date, rules: FundRules = DEFAULT_RULES): KycTriageOutput {
  const docs = input.documents.map((d) => triageDocument(d.doc_id, d.text, rules));
  const reasons: string[] = [];
  const required: DocType[][] = input.investor_type === "entity" ? [["registry_extract"]] : [["passport", "id_card"], ["proof_of_address"]];
  for (const d of docs) {
    if (d.mrzFailed) reasons.push(`${d.doc_id}: MRZ check digit failed`);
    if (d.ambiguous) reasons.push(`${d.doc_id}: ambiguous date`);
    if (d.doc_type === "other") reasons.push(`${d.doc_id}: unreadable or unrecognised document`);
  }
  const perType: string[] = [];
  for (const group of required) {
    const dates = docs.filter((d) => group.includes(d.doc_type) && d.expiry).map((d) => d.expiry!) ;
    if (dates.length === 0) reasons.push(`missing a readable ${group.join(" or ")}`);
    else perType.push(dates.sort().at(-1)!); // best document of the type
  }
  const effective = reasons.length ? null : perType.sort()[0] ?? null;
  const bucket: Bucket = reasons.length ? "needs_human" : bucketFor(effective, now);
  const onChain = input.on_chain.kyc_expiry.slice(0, 10);
  const mismatch = effective ? Math.round((Date.parse(`${effective}T00:00:00Z`) - Date.parse(`${onChain}T00:00:00Z`)) / 86_400_000) : null;
  return {
    investor_id: input.investor_id,
    documents: docs.map(({ mrzFailed: _m, ...d }) => d),
    effective_expiry: effective,
    on_chain_mismatch_days: mismatch,
    bucket,
    reasons,
  };
}

// ---------------------------------------------------------------------------
// subscription forms
// ---------------------------------------------------------------------------

const SMALL: Record<string, number> = {
  zero: 0, one: 1, two: 2, three: 3, four: 4, five: 5, six: 6, seven: 7, eight: 8, nine: 9, ten: 10, eleven: 11, twelve: 12,
  thirteen: 13, fourteen: 14, fifteen: 15, sixteen: 16, seventeen: 17, eighteen: 18, nineteen: 19, twenty: 20, thirty: 30,
  forty: 40, fifty: 50, sixty: 60, seventy: 70, eighty: 80, ninety: 90,
};
const SCALES: Record<string, number> = { thousand: 1_000, million: 1_000_000, billion: 1_000_000_000 };

/** English number words -> integer ("one hundred fifty thousand" -> 150000). */
export function wordsToNumber(words: string): number | null {
  const toks = words.toLowerCase().replace(/[-,]/g, " ").replace(/\band\b/g, " ").split(/\s+/).filter(Boolean);
  if (!toks.length) return null;
  let total = 0;
  let cur = 0;
  for (const t of toks) {
    if (t in SMALL) cur += SMALL[t];
    else if (t === "hundred") cur = (cur || 1) * 100;
    else if (t in SCALES) {
      total += (cur || 1) * SCALES[t];
      cur = 0;
    } else return null;
  }
  return total + cur;
}

const field = (text: string, label: RegExp): string => {
  for (const line of text.split(/\r?\n/)) {
    const m = line.match(label);
    if (m) return line.slice((m.index ?? 0) + m[0].length).replace(/^\s*[:：]\s*/, "").trim();
  }
  return "";
};

export function extractForm(input: FormExtractInput, rules: FundRules = DEFAULT_RULES): FormExtractOutput {
  const t = input.text;
  const flags: string[] = [];
  const legal_name = field(t, /^legal name/i);
  const typeRaw = field(t, /^investor type/i).toLowerCase();
  const investor_type = typeRaw.startsWith("ent") || typeRaw.includes("company") ? "entity" : "individual";
  const countryRaw = field(t, /^country( of residence)?/i);
  const code = countryRaw.match(/\(([A-Za-z]{2,3})\)/)?.[1] ?? countryRaw;
  const j = normaliseJurisdiction(code);
  const jurisdiction = j.code ?? code.toUpperCase();
  if (!j.code) flags.push("bad_jurisdiction");
  else if (!rules.allowedJurisdictions.includes(j.code)) flags.push("jurisdiction_not_allowed");
  const share_class = field(t, /^share class/i).toUpperCase();
  const currency = field(t, /^currency/i).toUpperCase();
  if (rules.classCurrency[share_class] === undefined) flags.push("unknown_share_class");
  else if (rules.classCurrency[share_class] !== currency) flags.push("class_currency_mismatch");
  const figures = field(t, /^subscription amount \(figures\)/i);
  let amount = "";
  let value: bigint | null = null;
  try {
    value = parseAmount(figures);
    amount = formatFixed(value, 7, 7).replace(/,/g, "");
  } catch (e) {
    if (e instanceof AmountError) flags.push("amount_unparseable");
    else throw e;
  }
  if (value !== null && value < rules.minSubscription) flags.push("below_minimum");
  const words = field(t, /^subscription amount \(words\)/i);
  let amount_in_words_matches: boolean | null = null;
  if (words && value !== null) {
    const n = wordsToNumber(words);
    amount_in_words_matches = n === null ? null : BigInt(n) === value / 10_000_000n;
    if (amount_in_words_matches === false) flags.push("amount_words_mismatch");
  }
  const wallet = field(t, /^stellar wallet/i);
  if (!StrKey.isValidEd25519PublicKey(wallet)) flags.push("wallet_invalid_strkey");
  const cash_addresses = field(t, /^cash address(\(es\))?/i).split(/[;,\s]+/).filter(Boolean);
  if (cash_addresses.length < 1 || cash_addresses.length > 3) flags.push("cash_address_count");
  if (cash_addresses.some((c) => !StrKey.isValidEd25519PublicKey(c))) flags.push("cash_address_invalid_strkey");
  const sig = field(t, /^signature/i);
  const signed = sig.replace(/[_\s.-]/g, "").length > 2;
  if (!signed) flags.push("not_signed");
  const dateRaw = field(t, /^date/i);
  const d = findDate(dateRaw);
  const date = d ? d.date : null;
  if (!date) flags.push("date_missing");
  if (!legal_name) flags.push("missing_field:legal_name");
  return { legal_name, investor_type, jurisdiction, share_class, currency, amount, amount_in_words_matches, wallet, cash_addresses, signed, date, flags };
}

export class HeuristicProvider implements Provider {
  readonly name = "heuristic" as const;
  constructor(private rules: FundRules = DEFAULT_RULES) {}
  async triage(input: KycTriageInput, now: Date): Promise<KycTriageOutput> {
    return triage(input, now, this.rules);
  }
  async extract(input: FormExtractInput): Promise<FormExtractOutput> {
    return extractForm(input, this.rules);
  }
}
