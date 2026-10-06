/**
 * Model-backed provider (Anthropic Messages API), used only when LLM_API_KEY
 * is set. It never replaces the heuristic: both run, the model's answer must
 * pass the strict schema (one retry with the validation errors), and any
 * field where the two disagree sends the investor to `needs_human`. Invalid
 * output twice falls back to the heuristic result, also as `needs_human`.
 * Nothing it returns is written on-chain; a TA approves every date.
 */
import { HeuristicProvider } from "./heuristic.js";
import { parseStrictJson, validateForm, validateTriage, type FormExtractInput, type FormExtractOutput, type KycTriageInput, type KycTriageOutput } from "./schema.js";
import type { Provider } from "./provider.js";

export const LLM_ENDPOINT = "https://api.anthropic.com/v1/messages";

export interface LlmOptions {
  apiKey: string;
  model: string;
  heuristic: HeuristicProvider;
  fetchImpl?: typeof fetch;
  maxTokens?: number;
}

const TRIAGE_SYSTEM = `You read KYC document text (OCR output) for a fund transfer agent.
For each document return its type (passport | id_card | proof_of_address | registry_extract | other),
its effective expiry as YYYY-MM-DD or null, the basis (stated | mrz | issue+90d | issue+3m | none),
whether a date was ambiguous, and a short verbatim evidence quote. Proof of address expires 90 days
after issue and a registry extract 3 months after issue (fund assumptions). Then give the investor's
effective_expiry (minimum over required document types), on_chain_mismatch_days and a bucket
(expired | lt30 | lt90 | ok | needs_human). Answer with one JSON object only, no prose.`;

const FORM_SYSTEM = `You extract a fund subscription form into JSON with exactly these keys:
legal_name, investor_type (individual|entity), jurisdiction (ISO alpha-2), share_class, currency,
amount (decimal string with 7 decimals), amount_in_words_matches (boolean|null), wallet, cash_addresses (array),
signed (boolean), date (YYYY-MM-DD|null), flags (array of strings). Answer with one JSON object only.`;

export class LlmProvider implements Provider {
  readonly name = "llm" as const;
  private fetchImpl: typeof fetch;
  constructor(private opts: LlmOptions) {
    this.fetchImpl = opts.fetchImpl ?? fetch;
  }

  private async call(system: string, user: string): Promise<string> {
    const res = await this.fetchImpl(LLM_ENDPOINT, {
      method: "POST",
      headers: { "content-type": "application/json", "x-api-key": this.opts.apiKey, "anthropic-version": "2023-06-01" },
      body: JSON.stringify({ model: this.opts.model, max_tokens: this.opts.maxTokens ?? 2000, system, messages: [{ role: "user", content: user }] }),
    });
    if (!res.ok) throw new Error(`LLM HTTP ${res.status}`);
    const body = (await res.json()) as { content?: { type: string; text?: string }[] };
    return (body.content ?? []).filter((c) => c.type === "text").map((c) => c.text ?? "").join("");
  }

  /** Ask, validate, retry once with the errors. Returns null after two invalid answers. */
  async ask<T>(system: string, user: string, validate: (v: unknown) => string[]): Promise<T | null> {
    let prompt = user;
    for (let attempt = 0; attempt < 2; attempt++) {
      let problems: string[];
      try {
        const v = parseStrictJson(await this.call(system, prompt));
        problems = validate(v);
        if (problems.length === 0) return v as T;
      } catch (e) {
        problems = [e instanceof Error ? e.message : String(e)];
      }
      prompt = `${user}\n\nYour previous answer was rejected: ${problems.join("; ")}. Return only the JSON object.`;
    }
    return null;
  }

  async triage(input: KycTriageInput, now: Date): Promise<KycTriageOutput> {
    const h = await this.opts.heuristic.triage(input, now);
    const m = await this.ask<KycTriageOutput>(TRIAGE_SYSTEM, JSON.stringify({ now: now.toISOString(), ...input }), validateTriage);
    if (!m) return { ...h, bucket: "needs_human", reasons: [...(h.reasons ?? []), "model output invalid twice; heuristic result shown"] };
    const diffs: string[] = [];
    if (m.effective_expiry !== h.effective_expiry) diffs.push(`effective_expiry model=${m.effective_expiry} heuristic=${h.effective_expiry}`);
    if (m.bucket !== h.bucket) diffs.push(`bucket model=${m.bucket} heuristic=${h.bucket}`);
    for (const hd of h.documents) {
      const md = m.documents.find((d) => d.doc_id === hd.doc_id);
      if (!md) diffs.push(`${hd.doc_id} missing in model output`);
      else {
        if (md.doc_type !== hd.doc_type) diffs.push(`${hd.doc_id}.doc_type`);
        if (md.expiry !== hd.expiry) diffs.push(`${hd.doc_id}.expiry`);
      }
    }
    return diffs.length ? { ...m, investor_id: input.investor_id, bucket: "needs_human", reasons: diffs } : { ...m, investor_id: input.investor_id, reasons: [] };
  }

  async extract(input: FormExtractInput): Promise<FormExtractOutput> {
    const h = await this.opts.heuristic.extract(input);
    const m = await this.ask<FormExtractOutput>(FORM_SYSTEM, input.text, validateForm);
    if (!m) return { ...h, flags: [...h.flags, "llm_invalid_output", "needs_human"] };
    const keys: (keyof FormExtractOutput)[] = ["legal_name", "investor_type", "jurisdiction", "share_class", "currency", "amount", "wallet", "signed", "date"];
    const dis = keys.filter((k) => JSON.stringify(m[k]) !== JSON.stringify(h[k]));
    // The deterministic validators always run on the heuristic reading; their flags are kept.
    const flags = [...new Set([...h.flags, ...dis.map((k) => `provider_disagreement:${k}`), ...(dis.length ? ["needs_human"] : [])])];
    return { ...m, flags };
  }
}
