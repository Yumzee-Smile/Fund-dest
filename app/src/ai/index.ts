/** Provider selection: heuristic unless asked for the model and a key is configured. */
import { DEFAULT_RULES, HeuristicProvider } from "./heuristic.js";
import { LlmProvider } from "./llm.js";
import type { FundRules, Provider } from "./provider.js";
import type { Config } from "../config.js";
import type { Fund } from "../seed.js";
import { parseAmount } from "../amount.js";

export function rulesFromFund(fund: Fund): FundRules {
  const usd = fund.classes[0];
  return {
    ...DEFAULT_RULES,
    proofOfAddressDays: fund.assumptions.proof_of_address_validity_days,
    registryMonths: fund.assumptions.registry_extract_validity_months,
    minSubscription: parseAmount(usd.min_subscription),
    allowedJurisdictions: fund.allowed_jurisdictions,
    classCurrency: Object.fromEntries(fund.classes.map((c) => [c.id, c.cash])),
  };
}

export function selectProvider(wanted: string | undefined, config: Config, rules: FundRules): { provider: Provider; note: string } {
  const heuristic = new HeuristicProvider(rules);
  if (wanted === "llm") {
    if (!config.llmApiKey) return { provider: heuristic, note: "LLM_API_KEY not set: using the heuristic provider" };
    return { provider: new LlmProvider({ apiKey: config.llmApiKey, model: config.llmModel, heuristic }), note: `model ${config.llmModel} + heuristic cross-check` };
  }
  return { provider: heuristic, note: "heuristic provider" };
}
