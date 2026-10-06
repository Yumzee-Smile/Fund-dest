import { Networks } from "@stellar/stellar-sdk";

export interface Config {
  rpcUrl: string | null;
  networkPassphrase: string;
  ids: {
    ops: string | null;
    oracle: string | null;
    compliance: string | null;
    distribution: string | null;
    vault: string | null;
    share: string | null;
    cash: string | null;
  };
  secrets: {
    ta1: string | null;
    ta2: string | null;
    admin: string | null;
    treasury: string | null;
  };
  journal: string;
  llmApiKey: string | null;
  llmModel: string;
}

/** Every variable the app reads; see .env.example. */
export function loadConfig(env: NodeJS.ProcessEnv = process.env): Config {
  const s = (k: string): string | null => {
    const v = env[k]?.trim();
    return v ? v : null;
  };
  return {
    rpcUrl: s("SOROBAN_RPC_URL"),
    networkPassphrase: s("NETWORK_PASSPHRASE") ?? Networks.TESTNET,
    ids: {
      ops: s("FD_OPS_ID"),
      oracle: s("FD_ORACLE_ID"),
      compliance: s("FD_COMPLIANCE_ID"),
      distribution: s("FD_DISTRIBUTION_ID"),
      vault: s("FD_VAULT_ID"),
      share: s("FD_SHARE_ID"),
      cash: s("FD_CASH_ID"),
    },
    secrets: {
      ta1: s("FD_TA_SECRET_1"),
      ta2: s("FD_TA_SECRET_2"),
      admin: s("FD_ADMIN_SECRET"),
      treasury: s("FD_TREASURY_SECRET"),
    },
    journal: s("FD_JOURNAL") ?? "funddesk-journal.json",
    llmApiKey: s("LLM_API_KEY"),
    llmModel: s("LLM_MODEL") ?? "claude-sonnet-5",
  };
}
