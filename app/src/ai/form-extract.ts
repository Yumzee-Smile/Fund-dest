/** Subscription-form extraction over a directory of form texts. */
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import type { Provider } from "./provider.js";
import type { FormExtractOutput } from "./schema.js";

export async function runExtract(dir: string, provider: Provider): Promise<{ form_id: string; result: FormExtractOutput }[]> {
  const out: { form_id: string; result: FormExtractOutput }[] = [];
  for (const f of readdirSync(dir).filter((x) => x.endsWith(".txt")).sort()) {
    const form_id = f.replace(/\.txt$/, "");
    out.push({ form_id, result: await provider.extract({ form_id, text: readFileSync(join(dir, f), "utf8") }) });
  }
  return out;
}

export function renderExtract(rows: { form_id: string; result: FormExtractOutput }[]): string {
  const lines = ["| Form | Name | Class | Amount | Jurisdiction | Signed | Flags |", "|---|---|---|---|---|---|---|"];
  for (const { form_id, result: r } of rows) {
    lines.push(`| ${form_id} | ${r.legal_name} | ${r.share_class}/${r.currency} | ${r.amount} | ${r.jurisdiction} | ${r.signed ? "yes" : "NO"} | ${r.flags.join(", ") || "-"} |`);
  }
  return lines.join("\n") + "\n";
}
