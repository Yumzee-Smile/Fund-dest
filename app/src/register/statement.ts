/**
 * Holder statement and TA register, rendered from a class state (the offline
 * replay, or the same structure rebuilt from the journal).
 *
 * The register carries the reconciliation the TA signs off:
 * registrar total_shares vs sum of share balances vs the journal; vault cash vs
 * pending + claimable; and the KYC-expiring list and exception list (every
 * automated rejection, for a person to act on).
 */
import { formatAmount, formatNav, unixToIso } from "../amount.js";
import { eligibility, type ClassState } from "../model/replay.js";
import { valueAt } from "../model/nav.js";
import { balancesFromJournal } from "./journal.js";

export interface HolderStatement {
  class: string;
  investor: string;
  legal_name: string;
  as_of: string;
  shares: string;
  locked_shares: string;
  nav: string;
  position_value: string;
  accrued_distribution: string;
  kyc: { expiry: string; status: string; jurisdiction: string; frozen: boolean; can_receive: boolean; can_send: boolean; can_redeem: boolean };
  pending: { id: number; epoch: number; kind: string; amount: string }[];
  claimable: { id: number; epoch: number; kind: string; shares_out: string; cash_out: string; reject: number }[];
  history: { id: number; epoch: number; kind: string; amount: string; status: string; shares_out: string; cash_out: string }[];
}

export function holderStatement(c: ClassState, id: string, now: number, navDecimals: number): HolderStatement {
  const inv = c.investors.get(id);
  if (!inv) throw new Error(`${id} is not registered in ${c.id}`);
  const st = eligibility(c, id, now);
  const days = Math.floor((inv.kyc_expiry - now) / 86_400);
  const status = !st.kyc_valid ? "EXPIRED" : days < 30 ? `expires in ${days} day(s)` : "valid";
  const mine = c.requests.filter((r) => r.investor === id);
  const bal = c.bal(id);
  return {
    class: c.id,
    investor: id,
    legal_name: inv.legal_name,
    as_of: unixToIso(now),
    shares: formatAmount(bal, 7),
    locked_shares: formatAmount(c.lck(id), 7),
    nav: formatNav(c.lastNav),
    position_value: formatAmount(valueAt(bal, c.lastNav, navDecimals)),
    accrued_distribution: formatAmount(c.dist.accrued(id, bal), 7),
    kyc: { expiry: unixToIso(inv.kyc_expiry), status, jurisdiction: inv.jurisdiction, frozen: inv.frozen, can_receive: st.can_receive, can_send: st.can_send, can_redeem: st.can_redeem },
    pending: mine.filter((r) => r.status === "Pending").map((r) => ({ id: r.id, epoch: r.epoch, kind: r.kind, amount: formatAmount(r.amount, 7) })),
    claimable: mine
      .filter((r) => r.status === "Claimable")
      .map((r) => ({ id: r.id, epoch: r.epoch, kind: r.kind, shares_out: formatAmount(r.shares_out, 7), cash_out: formatAmount(r.cash_out, 7), reject: r.reject })),
    history: mine.map((r) => ({ id: r.id, epoch: r.epoch, kind: r.kind, amount: formatAmount(r.amount, 7), status: r.status, shares_out: formatAmount(r.shares_out, 7), cash_out: formatAmount(r.cash_out, 7) })),
  };
}

export function renderStatementMd(s: HolderStatement): string {
  const l = [
    `# Holder statement - ${s.investor} (${s.legal_name}), class ${s.class}`,
    "",
    `As of ${s.as_of}. Simulated data.`,
    "",
    `| Item | Value |`,
    `|---|---|`,
    `| Shares | ${s.shares} |`,
    `| Locked for redemption | ${s.locked_shares} |`,
    `| Last settled NAV | ${s.nav} |`,
    `| Position value | ${s.position_value} |`,
    `| Accrued distribution | ${s.accrued_distribution} |`,
    `| KYC | ${s.kyc.status} (expiry ${s.kyc.expiry}, ${s.kyc.jurisdiction}${s.kyc.frozen ? ", FROZEN" : ""}) |`,
    `| May receive / send / redeem | ${s.kyc.can_receive ? "yes" : "no"} / ${s.kyc.can_send ? "yes" : "no"} / ${s.kyc.can_redeem ? "yes" : "no"} |`,
    "",
    `Pending requests: ${s.pending.length ? s.pending.map((p) => `#${p.id} ${p.kind} ${p.amount} (epoch ${p.epoch})`).join("; ") : "none"}`,
    `Claimable: ${s.claimable.length ? s.claimable.map((p) => `#${p.id} ${p.kind} shares ${p.shares_out} cash ${p.cash_out}${p.reject ? ` (refund, reject ${p.reject})` : ""}`).join("; ") : "none"}`,
    "",
    ...(s.history.length
      ? [
          "| Request | Epoch | Kind | Amount | Status | Shares out | Cash out |",
          "|---|---|---|---|---|---|---|",
          ...s.history.map((h) => `| #${h.id} | ${h.epoch} | ${h.kind} | ${h.amount} | ${h.status} | ${h.shares_out} | ${h.cash_out} |`),
        ]
      : ["Request history: none (shares received by forced transfer or holder transfer only)"]),
  ];
  return l.join("\n") + "\n";
}

export interface RegisterReport {
  class: string;
  as_of: string;
  holders: { investor: string; legal_name: string; shares: string; locked: string; jurisdiction: string; kyc_expiry: string; kyc_valid: boolean }[];
  reconciliation: {
    registrar_total_shares: string;
    sum_of_balances: string;
    journal_total: string;
    shares_reconciled: boolean;
    vault_cash: string;
    pending_plus_claimable: string;
    cash_reconciled: boolean;
  };
  kyc_expiring: { investor: string; kyc_expiry: string; days: number; expired: boolean }[];
  exceptions: { at: string; action: string; investor: string; result: string }[];
}

export function registerReport(c: ClassState, now: number, expiringDays = 30): RegisterReport {
  const ids = [...c.investors.keys()].sort();
  const sum = ids.reduce((a, id) => a + c.bal(id), 0n);
  const jb = balancesFromJournal(c.journal);
  const jt = [...jb.values()].reduce((a, v) => a + v, 0n);
  const jMatches = ids.every((id) => (jb.get(id) ?? 0n) === c.bal(id));
  let pc = 0n;
  for (const r of c.requests) {
    if (r.kind === "Subscribe" && r.status === "Pending") pc += r.amount;
    if (r.status === "Claimable") pc += r.cash_out;
  }
  const expiring = ids
    .map((id) => ({ investor: id, kyc_expiry: unixToIso(c.investors.get(id)!.kyc_expiry), days: Math.trunc((c.investors.get(id)!.kyc_expiry - now) / 86_400), expired: c.investors.get(id)!.kyc_expiry <= now }))
    .filter((x) => x.days < expiringDays)
    .sort((a, b) => a.days - b.days);
  return {
    class: c.id,
    as_of: unixToIso(now),
    holders: ids.map((id) => {
      const inv = c.investors.get(id)!;
      return {
        investor: id,
        legal_name: inv.legal_name,
        shares: formatAmount(c.bal(id), 7),
        locked: formatAmount(c.lck(id), 7),
        jurisdiction: inv.jurisdiction,
        kyc_expiry: unixToIso(inv.kyc_expiry),
        kyc_valid: inv.kyc_expiry > now,
      };
    }),
    reconciliation: {
      registrar_total_shares: formatAmount(c.totalShares, 7),
      sum_of_balances: formatAmount(sum, 7),
      journal_total: formatAmount(jt, 7),
      shares_reconciled: c.totalShares === sum && sum === jt && jMatches,
      vault_cash: formatAmount(c.vaultCash, 7),
      pending_plus_claimable: formatAmount(pc, 7),
      cash_reconciled: c.vaultCash === pc,
    },
    kyc_expiring: expiring,
    exceptions: c.outcomes
      .filter((o) => o.result !== "ok")
      .map((o) => ({ at: String(o.at), action: String(o.action), investor: String(o.investor ?? ""), result: String(o.result) })),
  };
}

export function renderRegisterMd(r: RegisterReport): string {
  const x = r.reconciliation;
  const l = [
    `# TA register - class ${r.class}`,
    "",
    `As of ${r.as_of}. Simulated data; the legal register of record stays with the TA.`,
    "",
    "## Reconciliation",
    "",
    `- registrar total_shares ${x.registrar_total_shares} | sum of SAC balances ${x.sum_of_balances} | journal ${x.journal_total} -> ${x.shares_reconciled ? "RECONCILED" : "BREAK"}`,
    `- vault cash ${x.vault_cash} | pending + claimable ${x.pending_plus_claimable} -> ${x.cash_reconciled ? "RECONCILED" : "BREAK"}`,
    "",
    "## Holders",
    "",
    "| Investor | Name | Shares | Locked | Jurisdiction | KYC expiry |",
    "|---|---|---|---|---|---|",
    ...r.holders.map((h) => `| ${h.investor} | ${h.legal_name} | ${h.shares} | ${h.locked} | ${h.jurisdiction} | ${h.kyc_expiry}${h.kyc_valid ? "" : " (EXPIRED)"} |`),
    "",
    "## KYC expiring or expired",
    "",
    ...(r.kyc_expiring.length ? r.kyc_expiring.map((k) => `- ${k.investor}: ${k.kyc_expiry} (${k.expired ? (k.days < 0 ? `expired ${-k.days} day(s) ago` : "expired <1 day ago") : `${k.days} day(s)`})`) : ["- none"]),
    "",
    "## Exceptions for a person to act on",
    "",
    ...(r.exceptions.length ? r.exceptions.map((e) => `- ${e.at} ${e.action} ${e.investor}: ${e.result}`) : ["- none"]),
  ];
  return l.join("\n") + "\n";
}
