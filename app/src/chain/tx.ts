/**
 * One builder per CLI action: resolves seed identifiers (inv_07, "USD-D",
 * human amounts, ISO dates) into contract arguments and composes the
 * invocation offline. Nothing here talks to the network.
 */
import { createHash } from "node:crypto";
import { StrKey } from "@stellar/stellar-sdk";
import { compose, placeholderId, type ContractName, type Invocation } from "./client.js";
import { checkPolicy, type Role } from "./ops-auth.js";
import { isoToUnix, parseAmount, parseNav } from "../amount.js";
import type { Config } from "../config.js";
import type { Fund } from "../seed.js";
import type { RegisterEntry } from "../register/csv.js";

export interface Ctx {
  config: Config;
  fund: Fund;
  register: RegisterEntry[];
}

const ID_ENV: Record<ContractName, keyof Config["ids"]> = {
  ops_account: "ops",
  nav_oracle: "oracle",
  compliance: "compliance",
  distribution: "distribution",
  async_vault: "vault",
};

export function contractId(ctx: Ctx, name: ContractName): string {
  return ctx.config.ids[ID_ENV[name]] ?? placeholderId(name);
}

export function opsAddress(ctx: Ctx): string {
  return contractId(ctx, "ops_account");
}

/** "inv_07" or a G.../C... address -> address. */
export function resolveAddress(ctx: Ctx, who: string): string {
  const t = who.trim();
  if (StrKey.isValidEd25519PublicKey(t) || StrKey.isValidContract(t)) return t;
  const r = ctx.register.find((x) => x.investor_id === t);
  if (!r) throw new Error(`unknown investor or invalid address: "${who}"`);
  return r.wallet;
}

export function sha256(text: string): Buffer {
  return createHash("sha256").update(text, "utf8").digest();
}

function inv(ctx: Ctx, name: ContractName, fn: string, args: Record<string, unknown>, signer: string): Invocation {
  return compose(name, contractId(ctx, name), fn, args, ctx.config.networkPassphrase, signer);
}

function policyLabel(ctx: Ctx, contract: string, fn: string): string {
  const p = ctx.fund.policies.find((x) => x.contract === contract && x.fn === fn);
  return p ? `ops (TA>=${p.ta}, ADMIN>=${p.admin}, total>=${p.total})` : "ops (no policy row!)";
}

// ----- compliance -----

export function kycApprove(ctx: Ctx, investor: string, expiryIso: string, jurisdiction: string, cash: string[], type: 0 | 1 = 0): Invocation {
  const code = jurisdiction.trim().toUpperCase();
  if (!/^[A-Z]{2}$/.test(code)) throw new Error(`jurisdiction must be ISO alpha-2, got "${jurisdiction}"`);
  if (cash.length < 1 || cash.length > 3) throw new Error("1 to 3 cash addresses");
  for (const c of cash) if (!StrKey.isValidEd25519PublicKey(c) && !StrKey.isValidContract(c)) throw new Error(`invalid cash address ${c}`);
  const iso = /^\d{4}-\d{2}-\d{2}$/.test(expiryIso) ? `${expiryIso}T00:00:00Z` : expiryIso;
  return inv(
    ctx,
    "compliance",
    "set_investor",
    {
      investor: resolveAddress(ctx, investor),
      rec: { kyc_expiry: BigInt(isoToUnix(iso)), jurisdiction: code, investor_type: type, cash_addresses: cash, frozen: false },
    },
    policyLabel(ctx, "compliance", "set_investor"),
  );
}

export function kycFreeze(ctx: Ctx, investor: string, reason: string, frozen = true): Invocation {
  return inv(ctx, "compliance", "set_frozen", { investor: resolveAddress(ctx, investor), frozen, reason: sha256(reason) }, policyLabel(ctx, "compliance", "set_frozen"));
}

export function transfer(ctx: Ctx, from: string, to: string, shares: string): Invocation {
  return inv(ctx, "compliance", "transfer", { from: resolveAddress(ctx, from), to: resolveAddress(ctx, to), amount: parseAmount(shares) }, "holder (from)");
}

/** Forced transfer; refuses locally unless the signer set meets the TA + ADMIN policy. */
export function force(ctx: Ctx, from: string, to: string, shares: string, reason: string, signers: string[]): Invocation {
  const roles: Role[] = signers.map((n) => {
    const s = ctx.fund.signers.find((x) => x.name === n.trim());
    if (!s) throw new Error(`unknown signer "${n}"`);
    return s.role;
  });
  checkPolicy(ctx.fund.policies, "compliance", "forced_transfer", roles);
  return inv(
    ctx,
    "compliance",
    "forced_transfer",
    { from: resolveAddress(ctx, from), to: resolveAddress(ctx, to), amount: parseAmount(shares), reason: sha256(reason) },
    `${policyLabel(ctx, "compliance", "forced_transfer")} signed by ${signers.join("+")}`,
  );
}

// ----- vault -----

export function epochOpen(ctx: Ctx, cutoffIso: string): Invocation {
  return inv(ctx, "async_vault", "open_epoch", { cutoff: BigInt(isoToUnix(cutoffIso)) }, policyLabel(ctx, "async_vault", "open_epoch"));
}

export function subscribe(ctx: Ctx, investor: string, amount: string): Invocation {
  return inv(ctx, "async_vault", "request_subscribe", { investor: resolveAddress(ctx, investor), amount: parseAmount(amount) }, "investor");
}

export function redeem(ctx: Ctx, investor: string, shares: string, cashTo: string): Invocation {
  return inv(ctx, "async_vault", "request_redeem", { investor: resolveAddress(ctx, investor), shares: parseAmount(shares), cash_to: cashTo }, "investor");
}

export function cancel(ctx: Ctx, investor: string, requestId: string): Invocation {
  return inv(ctx, "async_vault", "cancel", { investor: resolveAddress(ctx, investor), request_id: BigInt(requestId) }, "investor");
}

export function publish(ctx: Ctx, oracleAsset: string, nav: string, asOfIso: string): Invocation {
  return inv(
    ctx,
    "nav_oracle",
    "publish",
    { asset: { tag: "Other", values: [oracleAsset] }, price: parseNav(nav), timestamp: BigInt(isoToUnix(asOfIso)) },
    policyLabel(ctx, "nav_oracle", "publish"),
  );
}

export function strike(ctx: Ctx, epoch: number, override: boolean): Invocation {
  const fn = override ? "strike_nav_override" : "strike_nav";
  return inv(ctx, "async_vault", fn, { epoch }, policyLabel(ctx, "async_vault", fn));
}

export function depositLiquidity(ctx: Ctx, epoch: number, amount: string): Invocation {
  return inv(ctx, "async_vault", "deposit_liquidity", { epoch, amount: parseAmount(amount) }, "treasury");
}

export function settle(ctx: Ctx, epoch: number, batch: number): Invocation {
  return inv(ctx, "async_vault", "settle", { epoch, max_items: batch }, policyLabel(ctx, "async_vault", "settle"));
}

export function claim(ctx: Ctx, requestId: number | bigint, caller?: string): Invocation {
  const c = caller ?? opsAddress(ctx);
  return inv(ctx, "async_vault", "claim", { caller: c, request_id: BigInt(requestId) }, caller ? "investor" : policyLabel(ctx, "async_vault", "claim"));
}

// ----- distribution -----

export function declare(ctx: Ctx, amount: string, memo: string): Invocation {
  return inv(ctx, "distribution", "declare", { amount: parseAmount(amount), memo: sha256(memo) }, `${policyLabel(ctx, "distribution", "declare")} + treasury`);
}

export function distClaim(ctx: Ctx, holder: string, to: string): Invocation {
  return inv(ctx, "distribution", "claim", { holder: resolveAddress(ctx, holder), to }, "holder");
}

export function distPush(ctx: Ctx, holders: string[]): Invocation[] {
  const out: Invocation[] = [];
  for (let i = 0; i < holders.length; i += 25) {
    out.push(inv(ctx, "distribution", "claim_for", { holders: holders.slice(i, i + 25).map((h) => resolveAddress(ctx, h)) }, policyLabel(ctx, "distribution", "claim_for")));
  }
  return out;
}

// ----- deployment plan -----

export interface PlanStep {
  n: number;
  what: string;
  command?: string;
  invocation?: Invocation;
}

/** The deployment order from ARCHITECTURE.md, as a printable plan. */
export function initPlan(ctx: Ctx, classId: string): PlanStep[] {
  const cls = ctx.fund.classes.find((c) => c.id === classId) ?? ctx.fund.classes[0];
  const steps: PlanStep[] = [];
  let n = 0;
  const add = (what: string, command?: string, invocation?: Invocation) => steps.push({ n: ++n, what, command, invocation });
  const code = cls.id.replace("-", "").slice(0, 12);
  add(`issuer account sets AUTH_REQUIRED, AUTH_REVOCABLE, AUTH_CLAWBACK_ENABLED before any trustline exists (share asset ${code})`,
    "stellar tx new set-options --source-account issuer --set-required --set-revocable --set-clawback-enabled");
  add("deploy the share asset's SAC", `stellar contract asset deploy --asset ${code}:<issuer> --source-account issuer`);
  add("deploy ops_account with the TA and ADMIN signer keys", "stellar contract deploy --wasm ops_account.wasm -- --signers '[...]'");
  add(`deploy nav_oracle (publisher = ops_account, decimals ${ctx.fund.nav_decimals})`, "stellar contract deploy --wasm nav_oracle.wasm -- --publisher <ops> ...");
  add("deploy compliance (ops, share SAC)", "stellar contract deploy --wasm compliance.wasm -- --ops <ops> --share <sac>");
  add("deploy distribution (ops, compliance, cash SAC, treasury)", "stellar contract deploy --wasm distribution.wasm -- ...");
  add(`deploy async_vault (config: min ${cls.min_subscription}, band ${ctx.fund.max_nav_move_bps} bps, strike delay ${ctx.fund.max_strike_delay_s}s)`, "stellar contract deploy --wasm async_vault.wasm -- --cfg '{...}'");
  add("hand SAC admin to compliance (irreversible without the registrar)", "stellar contract invoke --id <sac> --source-account issuer -- set_admin --new_admin <compliance>");
  add("bind vault + distribution in compliance (TA + ADMIN)", undefined, inv(ctx, "compliance", "bind", { vault: contractId(ctx, "async_vault"), distribution: contractId(ctx, "distribution") }, policyLabel(ctx, "compliance", "bind")));
  const target: Record<string, ContractName> = { compliance: "compliance", async_vault: "async_vault", nav_oracle: "nav_oracle", distribution: "distribution" };
  for (const p of ctx.fund.policies) {
    add(
      `policy ${p.contract}.${p.fn} = TA ${p.ta} / ADMIN ${p.admin} / total ${p.total}`,
      undefined,
      inv(ctx, "ops_account", "set_policy", { contract: contractId(ctx, target[p.contract]), fn_name: p.fn, policy: { ta: p.ta, admin: p.admin, total: p.total } }, "ops (self: TA + ADMIN)"),
    );
  }
  for (const j of ctx.fund.allowed_jurisdictions) {
    add(`allow jurisdiction ${j}`, undefined, inv(ctx, "compliance", "set_jurisdiction", { code: j, allowed: true }, policyLabel(ctx, "compliance", "set_jurisdiction")));
  }
  return steps;
}
