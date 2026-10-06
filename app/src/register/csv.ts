/**
 * Investor register import: normalise and validate the TA's CSV export.
 *
 * - StrKey checksum on every wallet and cash address (a typo fails the row);
 * - jurisdiction: trims, upper-cases, maps ISO alpha-3 to alpha-2 and checks
 *   the ISO-3166 table;
 * - KYC expiry: ISO date, ISO datetime, DD.MM.YYYY and DD/MM/YYYY (day first;
 *   flagged when both parts are <= 12);
 * - investor type, 1 to 3 cash addresses, duplicate wallets, duplicate names.
 */
import { StrKey } from "@stellar/stellar-sdk";
import { parseCsv } from "./csv-core.js";
import { isoToUnix, unixToIso } from "../amount.js";

export { parseCsv, csvEscape } from "./csv-core.js";

export interface RegisterEntry {
  investor_id: string;
  legal_name: string;
  investor_type: 0 | 1;
  class: string;
  jurisdiction: string;
  kyc_expiry: string;
  kyc_expiry_unix: number;
  wallet: string;
  cash_addresses: string[];
}

export interface ImportIssue {
  line: number;
  investor_id: string;
  field: string;
  message: string;
}

export interface ImportResult {
  register: RegisterEntry[];
  errors: ImportIssue[];
  warnings: ImportIssue[];
}

// ISO-3166-1 alpha-2 codes (officially assigned).
const ALPHA2 = new Set(
  (
    "AD AE AF AG AI AL AM AO AQ AR AS AT AU AW AX AZ BA BB BD BE BF BG BH BI BJ BL BM BN BO BQ BR BS BT BV BW BY BZ " +
    "CA CC CD CF CG CH CI CK CL CM CN CO CR CU CV CW CX CY CZ DE DJ DK DM DO DZ EC EE EG EH ER ES ET FI FJ FK FM FO FR " +
    "GA GB GD GE GF GG GH GI GL GM GN GP GQ GR GS GT GU GW GY HK HM HN HR HT HU ID IE IL IM IN IO IQ IR IS IT JE JM JO " +
    "JP KE KG KH KI KM KN KP KR KW KY KZ LA LB LC LI LK LR LS LT LU LV LY MA MC MD ME MF MG MH MK ML MM MN MO MP MQ MR " +
    "MS MT MU MV MW MX MY MZ NA NC NE NF NG NI NL NO NP NR NU NZ OM PA PE PF PG PH PK PL PM PN PR PS PT PW PY QA RE RO " +
    "RS RU RW SA SB SC SD SE SG SH SI SJ SK SL SM SN SO SR SS ST SV SX SY SZ TC TD TF TG TH TJ TK TL TM TN TO TR TT TV " +
    "TW TZ UA UG UM US UY UZ VA VC VE VG VI VN VU WF YE YT ZA ZM ZW"
  ).split(" "),
);

// Alpha-3 -> alpha-2 for the jurisdictions a European/Gulf/Asian fund meets.
const ALPHA3: Record<string, string> = {
  FRA: "FR", DEU: "DE", NLD: "NL", ESP: "ES", ITA: "IT", LUX: "LU", BEL: "BE", IRL: "IE", PRT: "PT", AUT: "AT",
  CHE: "CH", SGP: "SG", ARE: "AE", USA: "US", GBR: "GB", DNK: "DK", SWE: "SE", NOR: "NO", FIN: "FI", POL: "PL",
  CZE: "CZ", GRC: "GR", HKG: "HK", JPN: "JP", MEX: "MX", CAN: "CA", MCO: "MC", LIE: "LI", MLT: "MT", CYP: "CY",
};

export function normaliseJurisdiction(raw: string): { code: string | null; note?: string } {
  const t = raw.trim().toUpperCase();
  let code = t;
  let note: string | undefined;
  if (t.length === 3 && ALPHA3[t]) {
    code = ALPHA3[t];
    note = `alpha-3 "${raw}" mapped to ${code}`;
  } else if (t !== raw) {
    note = `"${raw}" normalised to ${t}`;
  }
  if (!/^[A-Z]{2}$/.test(code) || !ALPHA2.has(code)) return { code: null };
  return { code, note };
}

/** Parse a KYC expiry into an ISO UTC instant. Date-only values expire at 00:00Z of that day. */
export function parseExpiry(raw: string): { iso: string | null; note?: string; ambiguous?: boolean } {
  const s = raw.trim();
  let y: number, m: number, d: number;
  let note: string | undefined;
  let ambiguous = false;
  let mt: RegExpMatchArray | null;
  if (/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(:\d{2})?Z$/.test(s)) {
    const t = Date.parse(s);
    return Number.isNaN(t) ? { iso: null } : { iso: unixToIso(Math.floor(t / 1000)) };
  } else if ((mt = s.match(/^(\d{4})-(\d{2})-(\d{2})$/))) {
    [y, m, d] = [Number(mt[1]), Number(mt[2]), Number(mt[3])];
  } else if ((mt = s.match(/^(\d{2})\.(\d{2})\.(\d{4})$/))) {
    [d, m, y] = [Number(mt[1]), Number(mt[2]), Number(mt[3])];
    note = `DD.MM.YYYY "${s}"`;
  } else if ((mt = s.match(/^(\d{2})\/(\d{2})\/(\d{4})$/))) {
    [d, m, y] = [Number(mt[1]), Number(mt[2]), Number(mt[3])];
    ambiguous = d <= 12 && m <= 12 && d !== m;
    note = ambiguous ? `ambiguous "${s}" read day-first` : `DD/MM/YYYY "${s}"`;
  } else {
    return { iso: null };
  }
  const t = Date.UTC(y, m - 1, d);
  const back = new Date(t);
  if (back.getUTCFullYear() !== y || back.getUTCMonth() !== m - 1 || back.getUTCDate() !== d) return { iso: null };
  return { iso: unixToIso(t / 1000), note, ambiguous };
}

/** Rows keyed by header; lines starting with '#' are comments. */
export function readRows(text: string): { line: number; row: Record<string, string> }[] {
  const lines = text.split(/\r?\n/);
  const kept: string[] = [];
  const lineNo: number[] = [];
  lines.forEach((l, i) => {
    if (!l.trimStart().startsWith("#") && l.trim() !== "") {
      kept.push(l);
      lineNo.push(i + 1);
    }
  });
  const rows = parseCsv(kept.join("\n"));
  const header = rows[0].map((h) => h.trim());
  return rows.slice(1).map((r, i) => ({
    line: lineNo[i + 1],
    row: Object.fromEntries(header.map((h, j) => [h, r[j] ?? ""])),
  }));
}

/**
 * Import the register export. `fixText` rows (same columns) replace rows of
 * the same investor_id, which is how a corrected export is applied.
 */
export function importInvestors(text: string, fixText?: string): ImportResult {
  const rows = readRows(text);
  if (fixText) {
    for (const f of readRows(fixText)) {
      const i = rows.findIndex((r) => r.row.investor_id === f.row.investor_id);
      if (i >= 0) rows[i] = { line: rows[i].line, row: f.row };
      else rows.push(f);
    }
  }
  const register: RegisterEntry[] = [];
  const errors: ImportIssue[] = [];
  const warnings: ImportIssue[] = [];
  const seenWallets = new Map<string, string>();
  const seenNames = new Map<string, string>();
  for (const { line, row } of rows) {
    const id = row.investor_id.trim();
    const err = (field: string, message: string) => errors.push({ line, investor_id: id, field, message });
    const warn = (field: string, message: string) => warnings.push({ line, investor_id: id, field, message });
    const before = errors.length;
    const juris = normaliseJurisdiction(row.jurisdiction ?? "");
    if (!juris.code) err("jurisdiction", `not an ISO-3166 code: "${row.jurisdiction}"`);
    else if (juris.note) warn("jurisdiction", juris.note);
    const exp = parseExpiry(row.kyc_expiry ?? "");
    if (!exp.iso) err("kyc_expiry", `unparseable date: "${row.kyc_expiry}"`);
    else if (exp.note) warn("kyc_expiry", exp.note);
    const wallet = (row.wallet ?? "").trim();
    if (!StrKey.isValidEd25519PublicKey(wallet)) err("wallet", `invalid StrKey (checksum or format): ${wallet}`);
    const cash = (row.cash_addresses ?? "").split(/[;|]/).map((s) => s.trim()).filter(Boolean);
    if (cash.length < 1 || cash.length > 3) err("cash_addresses", `expected 1 to 3 cash addresses, got ${cash.length}`);
    for (const c of cash) if (!StrKey.isValidEd25519PublicKey(c)) err("cash_addresses", `invalid StrKey: ${c}`);
    const typeRaw = (row.investor_type ?? "").trim().toLowerCase();
    const type = typeRaw === "entity" || typeRaw === "1" ? 1 : typeRaw === "individual" || typeRaw === "0" ? 0 : null;
    if (type === null) err("investor_type", `unknown investor type "${row.investor_type}"`);
    if (errors.length > before) continue;
    if (seenWallets.has(wallet)) {
      err("wallet", `wallet already registered to ${seenWallets.get(wallet)}`);
      continue;
    }
    seenWallets.set(wallet, id);
    const name = row.legal_name.trim();
    if (seenNames.has(name.toLowerCase())) {
      warn("legal_name", `same legal name as ${seenNames.get(name.toLowerCase())} with a different wallet; confirm they are different people`);
    } else {
      seenNames.set(name.toLowerCase(), id);
    }
    register.push({
      investor_id: id,
      legal_name: name,
      investor_type: type as 0 | 1,
      class: row.class.trim(),
      jurisdiction: juris.code!,
      kyc_expiry: exp.iso!,
      kyc_expiry_unix: isoToUnix(exp.iso!),
      wallet,
      cash_addresses: cash,
    });
  }
  return { register, errors, warnings };
}

/** Plain-text error report for the TA. */
export function renderImportReport(r: ImportResult): string {
  const out = [`register: ${r.register.length} valid row(s), ${r.errors.length} error(s), ${r.warnings.length} normalisation note(s)`];
  for (const e of r.errors) out.push(`ERROR line ${e.line} ${e.investor_id} ${e.field}: ${e.message}`);
  for (const w of r.warnings) out.push(`note  line ${w.line} ${w.investor_id} ${w.field}: ${w.message}`);
  return out.join("\n");
}
