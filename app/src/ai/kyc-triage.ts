/** KYC expiry triage over a directory of document texts; Markdown + JSON report. */
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import type { Provider } from "./provider.js";
import type { KycTriageInput, KycTriageOutput } from "./schema.js";
import type { RegisterEntry } from "../register/csv.js";

/** Files are named `<investor_id>__<doc>.txt`. */
export function loadDocs(dir: string, register: RegisterEntry[]): KycTriageInput[] {
  const byInv = new Map<string, { doc_id: string; text: string }[]>();
  for (const f of readdirSync(dir).filter((x) => x.endsWith(".txt")).sort()) {
    const inv = f.split("__")[0];
    const list = byInv.get(inv) ?? [];
    list.push({ doc_id: f.replace(/\.txt$/, ""), text: readFileSync(join(dir, f), "utf8") });
    byInv.set(inv, list);
  }
  return [...byInv.entries()].map(([investor_id, documents]) => {
    const r = register.find((x) => x.investor_id === investor_id);
    return {
      investor_id,
      investor_type: r?.investor_type === 1 ? "entity" : "individual",
      on_chain: { kyc_expiry: r?.kyc_expiry ?? "1970-01-01T00:00:00Z", jurisdiction: r?.jurisdiction ?? "??" },
      documents,
    };
  });
}

export async function runTriage(inputs: KycTriageInput[], provider: Provider, now: Date): Promise<KycTriageOutput[]> {
  const out: KycTriageOutput[] = [];
  for (const i of inputs) out.push(await provider.triage(i, now));
  return out;
}

const ORDER = ["expired", "needs_human", "lt30", "lt90", "ok"];

export function renderTriage(results: KycTriageOutput[], now: Date, provider: string, inputs: KycTriageInput[]): string {
  const lines = [
    `# KYC expiry triage`,
    "",
    `As of ${now.toISOString().slice(0, 10)} · provider: ${provider} · ${results.length} investor(s)`,
    "",
    "Suggestions only. Nothing here is written on-chain; the TA approves each date with `funddesk kyc approve`.",
    "Proof of address = issue + 90 days and registry extract = issue + 3 months are **Assumptions** (fund.json).",
    "",
    "| Investor | Bucket | Effective expiry | On-chain expiry | Mismatch (days) | Why |",
    "|---|---|---|---|---|---|",
  ];
  const sorted = [...results].sort((a, b) => ORDER.indexOf(a.bucket) - ORDER.indexOf(b.bucket) || a.investor_id.localeCompare(b.investor_id));
  for (const r of sorted) {
    const onChain = inputs.find((i) => i.investor_id === r.investor_id)?.on_chain.kyc_expiry.slice(0, 10) ?? "";
    lines.push(`| ${r.investor_id} | ${r.bucket} | ${r.effective_expiry ?? "-"} | ${onChain} | ${r.on_chain_mismatch_days ?? "-"} | ${(r.reasons ?? []).join("; ") || "-"} |`);
  }
  lines.push("", "## Documents", "");
  for (const r of sorted) {
    for (const d of r.documents) {
      lines.push(`- **${d.doc_id}**: ${d.doc_type}, expiry ${d.expiry ?? "none"} (${d.basis}${d.ambiguous ? ", ambiguous" : ""}) - ${d.evidence}`);
    }
  }
  return lines.join("\n") + "\n";
}
