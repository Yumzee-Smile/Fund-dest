/** One interface, two implementations (heuristic.ts, llm.ts). Neither calls the chain. */
import type { FormExtractInput, FormExtractOutput, KycTriageInput, KycTriageOutput } from "./schema.js";

export interface Provider {
  readonly name: "heuristic" | "llm";
  triage(input: KycTriageInput, now: Date): Promise<KycTriageOutput>;
  extract(input: FormExtractInput): Promise<FormExtractOutput>;
}

export interface FundRules {
  proofOfAddressDays: number;
  registryMonths: number;
  minSubscription: bigint;
  allowedJurisdictions: string[];
  classCurrency: Record<string, string>;
}
