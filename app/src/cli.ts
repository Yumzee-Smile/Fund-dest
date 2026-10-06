#!/usr/bin/env node
/**
 * funddesk: transfer-agent console for a tokenised fund on Stellar.
 *
 * Every chain command builds the transaction offline and prints it (contract,
 * function, arguments, required signer, the equivalent stellar-cli command and
 * the unsigned envelope XDR). Nothing is sent without --submit and a reachable
 * SOROBAN_RPC_URL.
 */
import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { Keypair } from "@stellar/stellar-sdk";
import { parseArgs, validateCommand } from "./args.js";
import { loadConfig, type Config } from "./config.js";
import { loadFund, loadSeed, defaultSeedDir, type Seed } from "./seed.js";
import { importInvestors, renderImportReport, type RegisterEntry } from "./register/csv.js";
import * as tx from "./chain/tx.js";
import { rpcReachable, submit, fetchEvents, unwrapResult, view, type Invocation } from "./chain/client.js";
import { PolicyError, type RoleKey } from "./chain/ops-auth.js";
import { Replay, compareWithGolden } from "./model/replay.js";
import { holderStatement, registerReport, renderRegisterMd, renderStatementMd } from "./register/statement.js";
import { loadJournal, saveJournal, ingest, fromRpcEvent } from "./register/journal.js";
import { rulesFromFund, selectProvider } from "./ai/index.js";
import { loadDocs, renderTriage, runTriage } from "./ai/kyc-triage.js";
import { renderExtract, runExtract } from "./ai/form-extract.js";
import { toJson } from "./register/json.js";
import { isoToUnix, unixToIso } from "./amount.js";

export interface Io {
  out: (s: string) => void;
  err: (s: string) => void;
}

const consoleIo: Io = { out: (s) => process.stdout.write(s + "\n"), err: (s) => process.stderr.write(s + "\n") };

function str(v: string | boolean | undefined): string | undefined {
  return typeof v === "string" ? v : undefined;
}

function printInvocation(io: Io, inv: Invocation, json: boolean): void {
  if (json) {
    io.out(toJson({ contract: inv.contract, contractId: inv.contractId, fn: inv.fn, args: inv.args, signer: inv.signer, cli: inv.cli, transactionXdr: inv.transactionXdr }));
    return;
  }
  io.out(`${inv.contract}.${inv.fn}  (auth: ${inv.signer})`);
  io.out(`  args: ${toJson(inv.args, 0)}`);
  io.out(`  cli:  ${inv.cli}`);
  io.out(`  xdr:  ${inv.transactionXdr}`);
}

function roleKeys(config: Config, fundSigners: { name: string; role: "Ta" | "Admin" }[], names: string[]): RoleKey[] {
  const secret: Record<string, string | null> = { ta_ops_1: config.secrets.ta1, ta_ops_2: config.secrets.ta2, fund_admin_1: config.secrets.admin };
  return names.map((n) => {
    const s = secret[n];
    const role = fundSigners.find((x) => x.name === n)?.role;
    if (!s || !role) throw new Error(`no secret configured for signer ${n}`);
    return { name: n, role, keypair: Keypair.fromSecret(s) };
  });
}

async function maybeSubmit(io: Io, config: Config, ctx: tx.Ctx, invs: Invocation[], flags: Record<string, string | boolean>, sourceSecret: string | null, signers: string[]): Promise<number> {
  for (const i of invs) printInvocation(io, i, flags.json === true);
  if (flags.submit !== true) {
    io.out("(not sent: add --submit with SOROBAN_RPC_URL set to simulate, sign and send)");
    return 0;
  }
  if (!(await rpcReachable(config))) {
    io.err(`--submit: RPC ${config.rpcUrl ?? "(SOROBAN_RPC_URL not set)"} is not reachable; nothing was sent`);
    return 2;
  }
  if (!sourceSecret) {
    io.err("--submit: no source secret configured for this action");
    return 2;
  }
  const keys = signers.length ? roleKeys(config, ctx.fund.signers, signers) : undefined;
  const names = new Map<string, string>();
  for (const n of ["compliance", "async_vault", "nav_oracle", "distribution", "ops_account"] as const) names.set(tx.contractId(ctx, n), n);
  for (const i of invs) {
    const r = await submit(config, i, {
      source: Keypair.fromSecret(sourceSecret),
      opsKeys: keys,
      opsAddress: tx.opsAddress(ctx),
      policies: ctx.fund.policies,
      contractName: (id) => names.get(id) ?? id,
    });
    io.out(`sent ${i.contract}.${i.fn}: ${r.hash}`);
  }
  return 0;
}

const statusTag = (v: unknown): string => (typeof v === "string" ? v : String((v as { tag?: string })?.tag ?? ""));

/**
 * settle --submit: optional treasury top-up (signed by the treasury), then settle
 * pages until remaining = 0, then a TA push-claim for every Claimable request,
 * read from the vault's own queue.
 */
async function settleAndClaim(io: Io, config: Config, ctx: tx.Ctx, epoch: number, batch: number, liquidity: string | undefined, settleFirst = true): Promise<number> {
  if (!(await rpcReachable(config))) {
    io.err(`--submit: RPC ${config.rpcUrl ?? "(SOROBAN_RPC_URL not set)"} is not reachable; nothing was sent`);
    return 2;
  }
  const taSecret = config.secrets.ta1;
  if (!taSecret) {
    io.err("--submit: FD_TA_SECRET_1 is not set");
    return 2;
  }
  const names = new Map<string, string>();
  for (const n of ["compliance", "async_vault", "nav_oracle", "distribution", "ops_account"] as const) names.set(tx.contractId(ctx, n), n);
  const source = Keypair.fromSecret(taSecret);
  const opsOpts = {
    source,
    opsKeys: roleKeys(config, ctx.fund.signers, ["ta_ops_1"]),
    opsAddress: tx.opsAddress(ctx),
    policies: ctx.fund.policies,
    contractName: (id: string) => names.get(id) ?? id,
  };
  if (liquidity) {
    if (!config.secrets.treasury) {
      io.err("--liquidity needs FD_TREASURY_SECRET (deposit_liquidity is authorised by the treasury)");
      return 2;
    }
    const r = await submit(config, tx.depositLiquidity(ctx, epoch, liquidity), { source: Keypair.fromSecret(config.secrets.treasury) });
    io.out(`deposit_liquidity ${liquidity}: ${r.hash}`);
  }
  for (let page = 0; settleFirst; page++) {
    let r: { hash: string; result: unknown };
    try {
      r = await submit(config, tx.settle(ctx, epoch, batch), opsOpts);
    } catch (e) {
      const m = e instanceof Error ? e.message : String(e);
      io.err(`settle page ${page + 1} failed: ${m}`);
      if (/InsufficientLiquidity|#19\b/.test(m)) io.err("the last page needs a treasury top-up: rerun with --liquidity <amount>");
      return 1;
    }
    const [processed, remaining] = (unwrapResult(r.result) as unknown[]).map((x) => Number(x));
    io.out(`settle page ${page + 1}: processed ${processed}, remaining ${remaining} (${r.hash})`);
    if (remaining === 0) break;
    if (processed === 0) {
      io.err("settle made no progress; stopping");
      return 1;
    }
  }
  const vault = tx.contractId(ctx, "async_vault");
  const len = Number(await view(config, "async_vault", vault, "queue_len", { epoch }, source.publicKey()));
  let claimed = 0;
  for (let start = 0; start < len; start += 50) {
    const reqs = (await view(config, "async_vault", vault, "queue", { epoch, start, limit: 50 }, source.publicKey())) as { id: bigint; status: unknown }[];
    for (const q of reqs) {
      if (statusTag(q.status) !== "Claimable") continue;
      const r = await submit(config, tx.claim(ctx, q.id), opsOpts);
      io.out(`claim #${q.id}: ${r.hash}`);
      claimed++;
    }
  }
  io.out(`epoch ${epoch}: ${settleFirst ? "settled, " : ""}${claimed} claim(s) pushed`);
  return 0;
}

function registerFor(seedDir: string): RegisterEntry[] {
  try {
    return JSON.parse(readFileSync(resolve(seedDir, "register.json"), "utf8")) as RegisterEntry[];
  } catch {
    return [];
  }
}

function classOf(seed: { fund: { classes: { id: string }[] } }, flag: string | undefined): string {
  return flag ?? seed.fund.classes[0].id;
}

function replaySeed(seedDir: string): { seed: Seed; replay: Replay; now: number } {
  const seed = loadSeed(seedDir);
  const replay = new Replay(seed);
  const res = replay.run();
  return { seed, replay, now: res.now };
}

export async function main(argv: string[], io: Io = consoleIo, env: NodeJS.ProcessEnv = process.env): Promise<number> {
  const v = validateCommand(parseArgs(argv));
  if (!v.ok) {
    io.err(v.error);
    return argv.length === 0 || argv.includes("--help") || argv.includes("-h") ? 0 : 1;
  }
  const { command, flags, positionals } = v;
  const config = loadConfig(env);
  const seedDir = str(flags["seed-dir"]) ?? defaultSeedDir();
  const fund = loadFund(seedDir);
  const ctx: tx.Ctx = { config, fund, register: registerFor(seedDir) };
  const json = flags.json === true;
  const ta = config.secrets.ta1;
  const admin = config.secrets.admin;
  try {
    switch (command) {
      case "init": {
        const f = loadFund(resolve(str(flags.fund)!, ".."));
        const steps = tx.initPlan({ ...ctx, fund: f }, f.classes[0].id);
        io.out(`Deployment plan for "${f.fund}" (${steps.length} steps; run by scripts/deploy-testnet.sh, not executed here)`);
        if (flags.submit !== true) {
          for (const s of steps) {
            io.out(`${String(s.n).padStart(2)}. ${s.what}`);
            if (s.command) io.out(`    $ ${s.command}`);
            if (s.invocation) io.out(`    $ ${s.invocation.cli}   [auth: ${s.invocation.signer}]`);
          }
        }
        if (flags.submit !== true) return 0;
        // Steps 1-8 (issuer flags, SAC, deployments, set_admin) are done by scripts/deploy-testnet.sh;
        // the ops-authorised steps (bind, policies, jurisdictions) are signed here by TA + ADMIN.
        const invs = steps.filter((s) => s.invocation).map((s) => s.invocation!);
        return maybeSubmit(io, config, ctx, invs, { ...flags, json: false }, ta, ["ta_ops_1", "fund_admin_1"]);
      }
      case "kyc import": {
        const text = readFileSync(positionals[0], "utf8");
        const fix = str(flags.fix) ? readFileSync(str(flags.fix)!, "utf8") : undefined;
        const r = importInvestors(text, fix);
        const out = str(flags.out) ?? "register.json";
        writeFileSync(out, JSON.stringify(r.register, null, 2) + "\n");
        io.out(renderImportReport(r));
        io.out(`wrote ${out}`);
        return 0;
      }
      case "kyc triage": {
        const now = str(flags.now) ? new Date(str(flags.now)!) : new Date();
        const reg = str(flags.register) ? (JSON.parse(readFileSync(str(flags.register)!, "utf8")) as RegisterEntry[]) : ctx.register;
        const { provider, note } = selectProvider(str(flags.provider), config, rulesFromFund(fund));
        const inputs = loadDocs(str(flags.docs)!, reg);
        const results = await runTriage(inputs, provider, now);
        io.err(note);
        const md = renderTriage(results, now, provider.name, inputs);
        if (str(flags.out)) {
          writeFileSync(`${str(flags.out)}.md`, md);
          writeFileSync(`${str(flags.out)}.json`, JSON.stringify(results, null, 2) + "\n");
          io.out(`wrote ${str(flags.out)}.md and ${str(flags.out)}.json`);
        }
        io.out(json ? JSON.stringify(results, null, 2) : md);
        return 0;
      }
      case "kyc extract": {
        const { provider, note } = selectProvider(str(flags.provider), config, rulesFromFund(fund));
        const rows = await runExtract(str(flags.forms)!, provider);
        io.err(note);
        io.out(json ? JSON.stringify(rows, null, 2) : renderExtract(rows));
        return 0;
      }
      case "kyc approve": {
        const cash = String(flags.cash).split(",").map((s) => s.trim()).filter(Boolean);
        const inv = tx.kycApprove(ctx, positionals[0], String(flags.expiry), String(flags.jurisdiction), cash, flags.type === "entity" ? 1 : 0);
        return maybeSubmit(io, config, ctx, [inv], flags, ta, ["ta_ops_1"]);
      }
      case "kyc expiring": {
        const now = str(flags.now) ? isoToUnix(str(flags.now)!) : Math.floor(Date.now() / 1000);
        const days = Number(flags.days);
        const rows = ctx.register
          .map((r) => ({ investor: r.investor_id, class: r.class, kyc_expiry: r.kyc_expiry, days: Math.floor((r.kyc_expiry_unix - now) / 86_400) }))
          .filter((r) => r.days < days)
          .sort((a, b) => a.days - b.days);
        if (json) io.out(JSON.stringify(rows, null, 2));
        else {
          io.out(`KYC expiring within ${days} day(s) of ${unixToIso(now)}: ${rows.length}`);
          for (const r of rows) io.out(`  ${r.investor} ${r.class} ${r.kyc_expiry} (${r.days < 0 ? "expired" : `${r.days} day(s)`})`);
        }
        return 0;
      }
      case "kyc freeze": {
        const inv = tx.kycFreeze(ctx, positionals[0], String(flags.reason), flags.unfreeze !== true);
        return maybeSubmit(io, config, ctx, [inv], flags, ta, ["ta_ops_1"]);
      }
      case "epoch open":
        return maybeSubmit(io, config, ctx, [tx.epochOpen(ctx, String(flags.cutoff))], flags, ta, ["ta_ops_1"]);
      case "subscribe": {
        const inv = flags.cancel !== undefined ? tx.cancel(ctx, String(flags.investor), String(flags.cancel)) : tx.subscribe(ctx, String(flags.investor), String(flags.amount));
        return maybeSubmit(io, config, ctx, [inv], flags, null, []);
      }
      case "redeem": {
        const inv = flags.cancel !== undefined ? tx.cancel(ctx, String(flags.investor), String(flags.cancel)) : tx.redeem(ctx, String(flags.investor), String(flags.shares), String(flags["cash-to"]));
        return maybeSubmit(io, config, ctx, [inv], flags, null, []);
      }
      case "transfer":
        return maybeSubmit(io, config, ctx, [tx.transfer(ctx, String(flags.from), String(flags.to), String(flags.shares))], flags, null, []);
      case "force": {
        const signers = (str(flags.signers) ?? "ta_ops_1+fund_admin_1").split("+");
        const inv = tx.force(ctx, String(flags.from), String(flags.to), String(flags.shares), String(flags.reason), signers);
        return maybeSubmit(io, config, ctx, [inv], flags, ta, signers);
      }
      case "strike": {
        const epoch = Number(flags.epoch);
        const cls = fund.classes.find((c) => c.id === classOf({ fund }, str(flags.class)))!;
        const invs: Invocation[] = [];
        if (str(flags.publish)) invs.push(tx.publish(ctx, cls.oracle_asset, str(flags.publish)!, str(flags["as-of"]) ?? unixToIso(Math.floor(Date.now() / 1000))));
        invs.push(tx.strike(ctx, epoch, flags.override === true));
        return maybeSubmit(io, config, ctx, invs, flags, admin, flags.override === true ? ["ta_ops_1", "fund_admin_1"] : ["fund_admin_1"]);
      }
      case "settle": {
        const epoch = Number(flags.epoch);
        const batch = Number(str(flags.batch) ?? fund.settle_batch);
        const invs: Invocation[] = [];
        if (str(flags.liquidity)) invs.push(tx.depositLiquidity(ctx, epoch, str(flags.liquidity)!));
        invs.push(tx.settle(ctx, epoch, batch));
        io.out(`settle loops until remaining = 0 (batch ${batch}), then claims every claimable request of epoch ${epoch} through ops`);
        if (flags.submit !== true) return maybeSubmit(io, config, ctx, invs, flags, ta, ["ta_ops_1"]);
        return settleAndClaim(io, config, ctx, epoch, batch, str(flags.liquidity));
      }
      case "claim": {
        if (str(flags.request)) return maybeSubmit(io, config, ctx, [tx.claim(ctx, BigInt(str(flags.request)!))], flags, ta, ["ta_ops_1"]);
        if (flags.submit === true) return settleAndClaim(io, config, ctx, Number(flags.epoch), 0, undefined, false);
        const { replay } = replaySeed(seedDir);
        const c = replay.classes[replay.classIndex(classOf({ fund }, str(flags.class)))];
        const ids = c.epochs[Number(flags.epoch) - 1]?.queue ?? [];
        io.out(`epoch ${flags.epoch}: ${ids.length} request(s) in the queue (ids from the offline replay; with --submit the ids are read from the vault)`);
        return maybeSubmit(io, config, ctx, ids.map((id) => tx.claim(ctx, id)), flags, ta, ["ta_ops_1"]);
      }
      case "distribute declare":
        return maybeSubmit(io, config, ctx, [tx.declare(ctx, String(flags.amount), String(flags.memo))], flags, admin, ["fund_admin_1"]);
      case "distribute claim":
        return maybeSubmit(io, config, ctx, [tx.distClaim(ctx, String(flags.holder), String(flags.to))], flags, null, []);
      case "distribute push": {
        const holders = str(flags.holders) ? str(flags.holders)!.split(",") : ctx.register.filter((r) => r.class === fund.classes[0].id).map((r) => r.investor_id);
        return maybeSubmit(io, config, ctx, tx.distPush(ctx, holders), flags, ta, ["ta_ops_1"]);
      }
      case "statement": {
        const { replay, now, seed } = replaySeed(seedDir);
        const fmt = str(flags.format) ?? (json ? "json" : "md");
        if (str(flags.investor)) {
          const id = str(flags.investor)!;
          const ci = replay.classOf.get(id);
          if (ci === undefined) throw new Error(`unknown investor ${id}`);
          const s = holderStatement(replay.classes[ci], id, now, seed.fund.nav_decimals);
          io.out(fmt === "json" ? JSON.stringify(s, null, 2) : renderStatementMd(s));
        } else {
          for (const c of replay.classes) {
            const r = registerReport(c, now);
            io.out(fmt === "json" ? JSON.stringify(r, null, 2) : renderRegisterMd(r));
          }
        }
        return 0;
      }
      case "journal": {
        const path = str(flags.journal) ?? config.journal;
        if (!(await rpcReachable(config))) {
          io.err(`journal: RPC ${config.rpcUrl ?? "(SOROBAN_RPC_URL not set)"} not reachable; ${path} unchanged`);
          return 2;
        }
        const names: Record<string, string> = {};
        for (const n of ["compliance", "async_vault", "nav_oracle", "distribution", "ops_account"] as const) names[tx.contractId(ctx, n)] = n;
        const j = loadJournal(path);
        const events = await fetchEvents(config, Object.keys(names), str(flags["from-ledger"]) ? Number(flags["from-ledger"]) : j.lastLedger);
        const added = ingest(j, events.map((e) => fromRpcEvent(e, names)));
        saveJournal(path, j);
        io.out(`journal ${path}: +${added} event(s), ${j.events.length} total`);
        return 0;
      }
      case "demo":
        return demo(io, seedDir);
    }
    io.err(`unhandled command ${command}`);
    return 1;
  } catch (e) {
    if (e instanceof PolicyError) {
      io.err(`refused locally by the ops policy mirror: ${e.code} (${e.message}); nothing was signed`);
      return 3;
    }
    io.err(`error: ${e instanceof Error ? e.message : String(e)}`);
    return 1;
  }
}

/** Offline replay of the seed; exit 1 if any figure differs from the Rust golden file. */
export function demo(io: Io, seedDir: string = defaultSeedDir()): number {
  const { seed, replay, now } = replaySeed(seedDir);
  const expected = JSON.parse(readFileSync(resolve(seedDir, "expected-scenario.json"), "utf8"));
  const diffs = compareWithGolden({ classes: replay.classes, now, fund: seed.fund }, expected);
  io.out(`Fund Desk demo - "${seed.fund.fund}" (fictional, simulated data)`);
  io.out(`register: ${seed.register.length} investors; ${seed.requests.length} order rows; ${seed.nav.length} NAV rows; ${seed.distribution.length} distribution rows; ${seed.forced.length} forced-transfer rows`);
  for (const c of replay.classes) {
    io.out("");
    io.out(`== ${c.id}`);
    for (const e of c.epochs) {
      io.out(
        `  epoch ${e.epoch}: ${e.status} NAV ${(Number(e.nav) / 1e14).toFixed(8)} | subs ${(Number(e.sub_total) / 1e7).toFixed(2)} | redeemed shares ${(Number(e.redeem_shares_total) / 1e7).toFixed(2)} | claimable cash ${(Number(e.claimable_cash) / 1e7).toFixed(2)} | liquidity ${(Number(e.liquidity) / 1e7).toFixed(2)} | settle calls ${e.settle_calls}`,
      );
    }
    const rejected = c.outcomes.filter((o) => o.result !== "ok");
    io.out(`  rejections as designed: ${rejected.length} (${rejected.map((o) => `${o.action}:${o.result}`).join(", ")})`);
    const r = registerReport(c, now);
    io.out(`  supply ${r.reconciliation.registrar_total_shares} = balances ${r.reconciliation.sum_of_balances} = journal ${r.reconciliation.journal_total}: ${r.reconciliation.shares_reconciled ? "reconciled" : "BREAK"}`);
    io.out(`  vault cash ${r.reconciliation.vault_cash} = pending + claimable ${r.reconciliation.pending_plus_claimable}: ${r.reconciliation.cash_reconciled ? "reconciled" : "BREAK"}`);
    if (c.dist.declared > 0n) io.out(`  distribution declared ${(Number(c.dist.declared) / 1e7).toFixed(7)}, claimed ${(Number(c.dist.claimed) / 1e7).toFixed(7)}, rounding left in contract ${c.dist.declared - c.dist.claimed} stroop(s)`);
  }
  io.out("");
  const s = holderStatement(replay.classes[0], "inv_12", now, seed.fund.nav_decimals);
  io.out(`statement inv_12: shares ${s.shares}, KYC ${s.kyc.status}, requests ${s.history.map((h) => `#${h.id} ${h.kind} ${h.status}`).join(", ")}`);
  if (diffs.length) {
    io.err(`MISMATCH with expected-scenario.json (${diffs.length} difference(s)):`);
    for (const d of diffs.slice(0, 20)) io.err(`  ${d}`);
    return 1;
  }
  io.out("TS model == Rust scenario golden file (data/seed/expected-scenario.json): every figure matches");
  return 0;
}

const isMain = process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  main(process.argv.slice(2)).then((code) => process.exit(code));
}
