/**
 * I/O types of the AI module and strict validators for them. Every provider
 * output (heuristic or model) is validated before anyone sees it; a model
 * answer that fails validation twice is discarded.
 */
export type DocType = "passport" | "id_card" | "proof_of_address" | "registry_extract" | "other";
export type Basis = "stated" | "mrz" | "issue+90d" | "issue+3m" | "none";
export type Bucket = "expired" | "lt30" | "lt90" | "ok" | "needs_human";

export interface KycTriageInput {
  investor_id: string;
  investor_type?: "individual" | "entity";
  on_chain: { kyc_expiry: string; jurisdiction: string };
  documents: { doc_id: string; text: string }[];
}

export interface TriageDoc {
  doc_id: string;
  doc_type: DocType;
  expiry: string | null; // YYYY-MM-DD
  basis: Basis;
  ambiguous: boolean;
  evidence: string;
}

export interface KycTriageOutput {
  investor_id: string;
  documents: TriageDoc[];
  effective_expiry: string | null;
  on_chain_mismatch_days: number | null;
  bucket: Bucket;
  /** Why the bucket is needs_human (empty otherwise). Not part of the model's answer. */
  reasons?: string[];
}

export interface FormExtractInput {
  form_id: string;
  text: string;
}

export interface FormExtractOutput {
  legal_name: string;
  investor_type: "individual" | "entity";
  jurisdiction: string;
  share_class: string;
  currency: string;
  amount: string;
  amount_in_words_matches: boolean | null;
  wallet: string;
  cash_addresses: string[];
  signed: boolean;
  date: string | null;
  flags: string[];
}

const DOC_TYPES = ["passport", "id_card", "proof_of_address", "registry_extract", "other"];
const BASES = ["stated", "mrz", "issue+90d", "issue+3m", "none"];
const BUCKETS = ["expired", "lt30", "lt90", "ok", "needs_human"];
const DATE = /^\d{4}-\d{2}-\d{2}$/;

const isObj = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null && !Array.isArray(v);

/** Returns a list of problems; empty means valid. */
export function validateTriage(v: unknown): string[] {
  const e: string[] = [];
  if (!isObj(v)) return ["not an object"];
  if (typeof v.investor_id !== "string") e.push("investor_id");
  if (!Array.isArray(v.documents)) e.push("documents");
  else
    v.documents.forEach((d, i) => {
      if (!isObj(d)) return e.push(`documents[${i}]`);
      if (typeof d.doc_id !== "string") e.push(`documents[${i}].doc_id`);
      if (!DOC_TYPES.includes(d.doc_type as string)) e.push(`documents[${i}].doc_type`);
      if (!(d.expiry === null || (typeof d.expiry === "string" && DATE.test(d.expiry)))) e.push(`documents[${i}].expiry`);
      if (!BASES.includes(d.basis as string)) e.push(`documents[${i}].basis`);
      if (typeof d.ambiguous !== "boolean") e.push(`documents[${i}].ambiguous`);
      if (typeof d.evidence !== "string") e.push(`documents[${i}].evidence`);
    });
  if (!(v.effective_expiry === null || (typeof v.effective_expiry === "string" && DATE.test(v.effective_expiry)))) e.push("effective_expiry");
  if (!(v.on_chain_mismatch_days === null || Number.isInteger(v.on_chain_mismatch_days))) e.push("on_chain_mismatch_days");
  if (!BUCKETS.includes(v.bucket as string)) e.push("bucket");
  return e;
}

export function validateForm(v: unknown): string[] {
  const e: string[] = [];
  if (!isObj(v)) return ["not an object"];
  for (const k of ["legal_name", "jurisdiction", "share_class", "currency", "amount", "wallet"]) if (typeof v[k] !== "string") e.push(k);
  if (!["individual", "entity"].includes(v.investor_type as string)) e.push("investor_type");
  if (typeof v.amount === "string" && !/^\d+\.\d{7}$/.test(v.amount)) e.push("amount (7 dp decimal string)");
  if (!(v.amount_in_words_matches === null || typeof v.amount_in_words_matches === "boolean")) e.push("amount_in_words_matches");
  if (!Array.isArray(v.cash_addresses) || v.cash_addresses.some((x) => typeof x !== "string")) e.push("cash_addresses");
  if (typeof v.signed !== "boolean") e.push("signed");
  if (!(v.date === null || (typeof v.date === "string" && DATE.test(v.date)))) e.push("date");
  if (!Array.isArray(v.flags) || v.flags.some((x) => typeof x !== "string")) e.push("flags");
  return e;
}

/** Strict JSON: the whole answer must be one JSON value (a ```json fence is tolerated). */
export function parseStrictJson(text: string): unknown {
  const t = text.trim().replace(/^```(?:json)?\s*/i, "").replace(/\s*```$/, "");
  return JSON.parse(t);
}
