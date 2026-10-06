/** CLI argument parsing and validation, pure and testable. */

export interface ParsedArgs {
  command: string[]; // e.g. ["kyc", "import"]
  flags: Record<string, string | boolean>;
  positionals: string[];
}

/** Commands with sub-commands; everything else is a single word. */
const GROUPS = new Set(["kyc", "epoch", "distribute"]);

export function parseArgs(argv: string[]): ParsedArgs {
  const out: ParsedArgs = { command: [], flags: {}, positionals: [] };
  let i = 0;
  if (argv[0] && !argv[0].startsWith("-")) {
    out.command.push(argv[0]);
    i = 1;
    if (GROUPS.has(argv[0]) && argv[1] && !argv[1].startsWith("-")) {
      out.command.push(argv[1]);
      i = 2;
    }
  }
  for (; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--") {
      out.positionals.push(...argv.slice(i + 1));
      break;
    }
    if (a.startsWith("--")) {
      const eq = a.indexOf("=");
      if (eq > 0) {
        out.flags[a.slice(2, eq)] = a.slice(eq + 1);
      } else {
        const key = a.slice(2);
        const next = argv[i + 1];
        if (next !== undefined && !next.startsWith("--")) {
          out.flags[key] = next;
          i++;
        } else {
          out.flags[key] = true;
        }
      }
    } else if (a === "-h") {
      out.flags.help = true;
    } else {
      out.positionals.push(a);
    }
  }
  return out;
}

export interface CommandSpec {
  summary: string;
  required: string[];
  optional: string[];
  positionals?: string[];
  oneOf?: string[][];
}

const COMMON = ["submit", "json", "seed-dir", "out"];

export const COMMANDS: Record<string, CommandSpec> = {
  init: { summary: "Deployment plan: share asset flags, SAC, five contracts, set_admin, bind, policies, jurisdictions.", required: ["fund"], optional: [] },
  "kyc import": { summary: "Normalise and validate the register CSV into register.json + an error report.", required: [], optional: ["fix"], positionals: ["csv"] },
  "kyc triage": { summary: "KYC expiry triage report (Markdown + JSON) from document texts.", required: ["docs"], optional: ["provider", "register", "now"] },
  "kyc extract": { summary: "Subscription-form extraction with validation flags.", required: ["forms"], optional: ["provider"] },
  "kyc approve": { summary: "compliance.set_investor under the TA policy.", required: ["expiry", "jurisdiction", "cash"], optional: ["type"], positionals: ["investor"] },
  "kyc expiring": { summary: "Investors whose KYC expires within --days.", required: ["days"], optional: ["now"] },
  "kyc freeze": { summary: "compliance.set_frozen; the reason text is hashed.", required: ["reason"], optional: ["unfreeze"], positionals: ["investor"] },
  "epoch open": { summary: "async_vault.open_epoch.", required: ["cutoff"], optional: ["class"] },
  subscribe: { summary: "async_vault.request_subscribe (or --cancel <request id>).", required: ["investor"], optional: ["amount", "class", "cancel"], oneOf: [["amount", "cancel"]] },
  redeem: { summary: "async_vault.request_redeem (or --cancel <request id>).", required: ["investor"], optional: ["shares", "cash-to", "class", "cancel"], oneOf: [["shares", "cancel"]] },
  transfer: { summary: "compliance.transfer between registered holders.", required: ["from", "to", "shares"], optional: [] },
  force: { summary: "compliance.forced_transfer; assembles TA + ADMIN signatures.", required: ["from", "to", "shares", "reason"], optional: ["signers"] },
  strike: { summary: "nav_oracle.publish then async_vault.strike_nav (or strike_nav_override).", required: ["epoch"], optional: ["publish", "as-of", "override", "class"] },
  settle: { summary: "Loop async_vault.settle until remaining = 0, then claim every claimable request.", required: ["epoch"], optional: ["batch", "liquidity", "class"] },
  claim: { summary: "async_vault.claim for one request or every claimable request of an epoch.", required: [], optional: ["request", "epoch", "all", "class"], oneOf: [["request", "epoch"]] },
  "distribute declare": { summary: "distribution.declare.", required: ["amount", "memo"], optional: [] },
  "distribute claim": { summary: "distribution.claim to a registered cash address.", required: ["holder", "to"], optional: [] },
  "distribute push": { summary: "distribution.claim_for in batches of 25.", required: [], optional: ["holders"] },
  statement: { summary: "Holder statement or TA register (offline from the seed replay, or from the journal).", required: [], optional: ["investor", "register", "format", "journal"], oneOf: [["investor", "register"]] },
  journal: { summary: "Ingest contract events from RPC into the local journal (RPC keeps ~7 days).", required: [], optional: ["from-ledger", "journal"] },
  demo: { summary: "Replay the seed offline and compare with data/seed/expected-scenario.json.", required: [], optional: [] },
};

export type Validated =
  | { ok: true; command: string; flags: Record<string, string | boolean>; positionals: string[] }
  | { ok: false; error: string };

export function validateCommand(p: ParsedArgs): Validated {
  const name = p.command.join(" ");
  if (!name || p.flags.help) return { ok: false, error: usage() };
  const spec = COMMANDS[name];
  if (!spec) return { ok: false, error: `unknown command "${name}"\n\n${usage()}` };
  const missing = spec.required.filter((k) => p.flags[k] === undefined || p.flags[k] === true);
  if (missing.length) return { ok: false, error: `${name}: missing required option(s): ${missing.map((m) => "--" + m).join(", ")}` };
  const known = new Set([...spec.required, ...spec.optional, ...COMMON, "help"]);
  const unknown = Object.keys(p.flags).filter((k) => !known.has(k));
  if (unknown.length) return { ok: false, error: `${name}: unknown option(s): ${unknown.map((m) => "--" + m).join(", ")}` };
  for (const group of spec.oneOf ?? []) {
    const set = group.filter((k) => p.flags[k] !== undefined);
    if (set.length > 1) return { ok: false, error: `${name}: use only one of ${group.map((m) => "--" + m).join(", ")}` };
  }
  const needPos = spec.positionals ?? [];
  if (p.positionals.length < needPos.length) return { ok: false, error: `${name}: missing <${needPos[p.positionals.length]}>` };
  if (name === "subscribe" && p.flags.amount === undefined && p.flags.cancel === undefined) return { ok: false, error: "subscribe: give --amount <x> or --cancel <request id>" };
  if (name === "redeem" && p.flags.cancel === undefined && (p.flags.shares === undefined || p.flags["cash-to"] === undefined)) {
    return { ok: false, error: "redeem: give --shares <x> --cash-to <address>, or --cancel <request id>" };
  }
  if (name === "claim" && p.flags.request === undefined && !(p.flags.epoch !== undefined && p.flags.all === true)) {
    return { ok: false, error: "claim: give --request <id> or --epoch <n> --all" };
  }
  if (name === "statement" && p.flags.investor === undefined && p.flags.register === undefined) {
    return { ok: false, error: "statement: give --investor <id> or --register" };
  }
  if (name === "kyc triage" && p.flags.provider !== undefined && !["heuristic", "llm"].includes(String(p.flags.provider))) {
    return { ok: false, error: "kyc triage: --provider must be heuristic or llm" };
  }
  return { ok: true, command: name, flags: p.flags, positionals: p.positionals };
}

export function usage(): string {
  const lines = ["usage: funddesk <command> [options]", ""];
  for (const [name, spec] of Object.entries(COMMANDS)) {
    const pos = (spec.positionals ?? []).map((x) => `<${x}>`).join(" ");
    const req = spec.required.map((r) => `--${r} <v>`).join(" ");
    lines.push(`  ${[name, pos, req].filter(Boolean).join(" ")}`);
    lines.push(`      ${spec.summary}`);
  }
  lines.push("", "Every chain command builds the transaction offline and prints it; nothing is sent without --submit and a reachable SOROBAN_RPC_URL.");
  return lines.join("\n");
}
