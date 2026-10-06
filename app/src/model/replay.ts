/**
 * Offline replay of the seed journey through the TypeScript model.
 *
 * It mirrors, rule for rule, what `scenario_three_epochs_seed` executes in the
 * Soroban host: the same merged timeline, the same checks in the same order,
 * the same rounding (model/nav.ts) and the same accumulator
 * (model/distribution.ts). `npm run demo` compares the result with
 * data/seed/expected-scenario.json, which the Rust scenario writes, and fails
 * on any difference. The app uses the same engine to preview what a batch of
 * orders will do before anything is signed.
 */
import { Accumulator } from "./distribution.js";
import { cashForShares, moveExceedsBand, sharesForCash } from "./nav.js";
import { isoToUnix, parseFixed } from "../amount.js";
import type { Fund, Row, Seed } from "../seed.js";
import type { RegisterEntry } from "../register/csv.js";
import type { JournalEvent } from "../register/journal.js";

export type Kind = "Subscribe" | "Redeem";
export type ReqStatus = "Pending" | "Cancelled" | "Claimable" | "Claimed";
export type EpochStatus = "Open" | "Struck" | "Settling" | "Settled" | "Aborted";

export interface Request {
  id: number;
  epoch: number;
  investor: string;
  kind: Kind;
  amount: bigint;
  cash_to: string | null;
  status: ReqStatus;
  shares_out: bigint;
  cash_out: bigint;
  reject: number;
}

export interface Epoch {
  epoch: number;
  cutoff: number;
  status: EpochStatus;
  nav: bigint;
  nav_ts: number;
  sub_total: bigint;
  redeem_shares_total: bigint;
  liquidity: bigint;
  cursor: number;
  claimable_cash: bigint;
  queue: number[];
  settle_calls: number;
  liquidity_needed: boolean;
}

export interface InvestorRec {
  id: string;
  kyc_expiry: number;
  jurisdiction: string;
  cash: string[]; // seed strkeys
  frozen: boolean;
  legal_name: string;
}

export interface Outcome {
  [k: string]: string | number;
}

export class ClassState {
  investors = new Map<string, InvestorRec>();
  holderOrder: string[] = [];
  shares = new Map<string, bigint>();
  locked = new Map<string, bigint>();
  totalShares = 0n;
  epochs: Epoch[] = [];
  requests: Request[] = [];
  lastNav: bigint;
  treasury = 0n;
  vaultCash = 0n;
  dist = new Accumulator();
  outcomes: Outcome[] = [];
  allowed: Set<string>;
  paidCash = new Map<string, bigint>(); // cash address -> received from vault/distribution
  /** Registrar events in the order the contracts would emit them (Issued, Burned, Transferred, Forced). */
  journal: JournalEvent[] = [];
  constructor(
    public id: string,
    public minSubscription: bigint,
    initialNav: bigint,
    allowed: string[],
    public topups: Map<number, bigint>,
  ) {
    this.lastNav = initialNav;
    this.allowed = new Set(allowed);
  }
  bal(h: string): bigint {
    return this.shares.get(h) ?? 0n;
  }
  lck(h: string): bigint {
    return this.locked.get(h) ?? 0n;
  }
  current(): Epoch | undefined {
    return this.epochs[this.epochs.length - 1];
  }
}

export class FundError extends Error {}
const fail = (name: string): never => {
  throw new FundError(name);
};

interface Eligibility {
  registered: boolean;
  kyc_valid: boolean;
  jurisdiction_ok: boolean;
  frozen: boolean;
  can_receive: boolean;
  can_send: boolean;
  can_redeem: boolean;
}

export function eligibility(c: ClassState, id: string, now: number): Eligibility {
  const inv = c.investors.get(id);
  if (!inv) {
    return { registered: false, kyc_valid: false, jurisdiction_ok: false, frozen: false, can_receive: false, can_send: false, can_redeem: false };
  }
  const kyc = inv.kyc_expiry > now;
  const j = c.allowed.has(inv.jurisdiction);
  return {
    registered: true,
    kyc_valid: kyc,
    jurisdiction_ok: j,
    frozen: inv.frozen,
    can_receive: !inv.frozen && kyc && j,
    can_send: !inv.frozen && kyc,
    can_redeem: !inv.frozen,
  };
}

function requireReceive(e: Eligibility): void {
  if (!e.registered) fail("NotRegistered");
  if (e.frozen) fail("Frozen");
  if (!e.kyc_valid) fail("KycExpired");
  if (!e.jurisdiction_ok) fail("JurisdictionBlocked");
}

function requireSend(e: Eligibility): void {
  if (!e.registered) fail("NotRegistered");
  if (e.frozen) fail("Frozen");
  if (!e.kyc_valid) fail("KycExpired");
}

type Step =
  | { kind: "open"; ci: number; epoch: number; cutoff: number }
  | { kind: "publish"; ci: number; row: Row }
  | { kind: "strike"; ci: number; row: Row }
  | { kind: "request"; row: Row }
  | { kind: "settle"; ci: number; epoch: number; topup: bigint }
  | { kind: "claimAll"; ci: number; epoch: number }
  | { kind: "dist"; row: Row }
  | { kind: "forced"; row: Row };

export interface ReplayResult {
  classes: ClassState[];
  now: number;
  fund: Fund;
}

/** One replay engine for both classes, sharing one oracle and one ops account. */
export class Replay {
  classes: ClassState[] = [];
  classOf = new Map<string, number>();
  oracle = new Map<string, { price: bigint; ts: number }>();
  reqKeys = new Map<string, number>();
  now = 0;
  private strkeyOwner = new Map<string, string>(); // cash strkey -> investor id

  constructor(public seed: Seed) {
    const f = seed.fund;
    for (const [ci, cls] of f.classes.entries()) {
      const topups = new Map<number, bigint>();
      for (const e of cls.epochs) topups.set(e.epoch, parseFixed(e.liquidity_topup, 7));
      const c = new ClassState(cls.id, parseFixed(cls.min_subscription, 7), parseFixed(cls.initial_nav, f.nav_decimals), f.allowed_jurisdictions, topups);
      for (const r of seed.register.filter((x) => x.class === cls.id)) {
        this.register(c, r.investor_id, r);
        this.classOf.set(r.investor_id, ci);
        c.holderOrder.push(r.investor_id);
      }
      this.classes.push(c);
    }
  }

  private register(c: ClassState, id: string, r: Pick<RegisterEntry, "kyc_expiry_unix" | "jurisdiction" | "cash_addresses" | "legal_name">): void {
    c.investors.set(id, { id, kyc_expiry: r.kyc_expiry_unix, jurisdiction: r.jurisdiction, cash: r.cash_addresses, frozen: false, legal_name: r.legal_name });
    for (const a of r.cash_addresses) this.strkeyOwner.set(a, id);
  }

  classIndex(id: string): number {
    const i = this.classes.findIndex((c) => c.id === id);
    if (i < 0) throw new Error(`unknown class ${id}`);
    return i;
  }

  timeline(): { at: number; n: number; step: Step }[] {
    const t: { at: number; n: number; step: Step }[] = [];
    let n = 0;
    const push = (at: number, step: Step) => t.push({ at, n: ++n, step });
    for (const [ci, cls] of this.seed.fund.classes.entries()) {
      for (const e of cls.epochs) {
        push(isoToUnix(e.open_at), { kind: "open", ci, epoch: e.epoch, cutoff: isoToUnix(e.cutoff) });
        const s = isoToUnix(e.settle_at);
        push(s, { kind: "settle", ci, epoch: e.epoch, topup: parseFixed(e.liquidity_topup, 7) });
        push(s + 300, { kind: "claimAll", ci, epoch: e.epoch });
      }
    }
    for (const row of this.seed.nav) {
      const ci = this.classIndex(row.class);
      push(isoToUnix(row.published_at), { kind: "publish", ci, row });
      push(isoToUnix(row.strike_at), { kind: "strike", ci, row });
    }
    for (const row of this.seed.requests) push(isoToUnix(row.at), { kind: "request", row });
    for (const row of this.seed.distribution) push(isoToUnix(row.at), { kind: "dist", row });
    for (const row of this.seed.forced) push(isoToUnix(row.at), { kind: "forced", row });
    t.sort((a, b) => a.at - b.at || a.n - b.n);
    return t;
  }

  run(): ReplayResult {
    for (const { at, step } of this.timeline()) {
      if (this.now < at) this.now = at;
      this.step(step);
    }
    return { classes: this.classes, now: this.now, fund: this.seed.fund };
  }

  private attempt(fn: () => void): string {
    try {
      fn();
      return "ok";
    } catch (e) {
      if (e instanceof FundError) return e.message;
      throw e;
    }
  }

  private step(s: Step): void {
    const f = this.seed.fund;
    switch (s.kind) {
      case "open": {
        const c = this.classes[s.ci];
        const prev = c.current();
        if (s.cutoff <= this.now) throw new Error("CutoffInPast");
        if (prev && this.now < prev.cutoff) throw new Error("PreviousEpochOpen");
        c.epochs.push({
          epoch: c.epochs.length + 1, cutoff: s.cutoff, status: "Open", nav: 0n, nav_ts: 0, sub_total: 0n, redeem_shares_total: 0n,
          liquidity: 0n, cursor: 0, claimable_cash: 0n, queue: [], settle_calls: 0, liquidity_needed: false,
        });
        if (c.epochs.length !== s.epoch) throw new Error("epoch numbering mismatch");
        break;
      }
      case "publish": {
        const c = this.classes[s.ci];
        const key = f.classes[s.ci].oracle_asset;
        const tsAsOf = isoToUnix(s.row.as_of);
        const last = this.oracle.get(key);
        const price = parseFixed(s.row.nav, f.nav_decimals);
        if (price <= 0n || tsAsOf > this.now || (last && tsAsOf <= last.ts)) throw new Error(`publish rejected for ${c.id}`);
        this.oracle.set(key, { price, ts: tsAsOf });
        break;
      }
      case "strike": {
        const c = this.classes[s.ci];
        const ep = Number(s.row.epoch);
        const override = s.row.mode === "override";
        const result = this.attempt(() => this.strike(c, s.ci, ep, !override));
        if (result !== s.row.expect) throw new Error(`strike ${c.id} e${ep}: expected ${s.row.expect}, got ${result}`);
        c.outcomes.push({ at: s.row.strike_at, action: "strike", epoch: ep, nav: s.row.nav, expect: s.row.expect, result });
        break;
      }
      case "request":
        this.request(s.row);
        break;
      case "settle":
        this.settleAll(this.classes[s.ci], s.epoch, s.topup);
        break;
      case "claimAll": {
        const c = this.classes[s.ci];
        for (const id of c.epochs[s.epoch - 1].queue) {
          const r = c.requests[id - 1];
          if (r.status === "Claimable") this.claim(c, r);
        }
        break;
      }
      case "dist":
        this.dist(s.row);
        break;
      case "forced":
        this.forced(s.row);
        break;
    }
  }

  private strike(c: ClassState, ci: number, ep: number, band: boolean): void {
    const f = this.seed.fund;
    const e = c.epochs[ep - 1] ?? fail("WrongEpochState");
    if (e.status !== "Open") fail("WrongEpochState");
    if (this.now < e.cutoff) fail("EpochNotClosed");
    const p = this.oracle.get(f.classes[ci].oracle_asset) ?? fail("PriceMissing");
    if (p.ts < e.cutoff) fail("StalePrice");
    if (p.ts > e.cutoff + f.max_strike_delay_s) fail("PriceTooLate");
    if (p.price <= 0n) fail("NavNonPositive");
    if (band && moveExceedsBand(c.lastNav, p.price, f.max_nav_move_bps)) fail("NavMoveTooLarge");
    e.nav = p.price;
    e.nav_ts = p.ts;
    e.status = "Struck";
  }

  private openForRequests(c: ClassState): Epoch {
    const e = c.current() ?? fail("NoOpenEpoch");
    if (e.status !== "Open") fail("NoOpenEpoch");
    if (this.now >= e.cutoff) fail("CutoffPassed");
    return e;
  }

  private cashOwner(strkey: string): string | undefined {
    return this.strkeyOwner.get(strkey);
  }

  private request(row: Row): void {
    const ci = this.classIndex(row.class);
    const c = this.classes[ci];
    const who = row.investor;
    const f = this.seed.fund;
    const action = row.action;
    const result = this.attempt(() => {
      if (action === "subscribe") {
        const amount = parseFixed(row.amount, 7);
        const e = this.openForRequests(c);
        if (amount < c.minSubscription) fail("BelowMinimum");
        requireReceive(eligibility(c, who, this.now));
        if (e.queue.length >= f.max_requests_per_epoch) fail("EpochFull");
        const id = c.requests.length + 1;
        c.requests.push({ id, epoch: e.epoch, investor: who, kind: "Subscribe", amount, cash_to: null, status: "Pending", shares_out: 0n, cash_out: 0n, reject: 0 });
        e.queue.push(id);
        e.sub_total += amount;
        c.vaultCash += amount;
        this.reqKeys.set(`${who}:${row.at}`, id);
      } else if (action === "redeem") {
        const shares = parseFixed(row.amount, 7);
        const e = this.openForRequests(c);
        if (shares <= 0n) fail("InvalidAmount");
        const st = eligibility(c, who, this.now);
        if (!st.registered) fail("NotRegistered");
        if (st.frozen) fail("Frozen");
        if (!c.investors.get(who)!.cash.includes(row.target)) fail("CashAddressNotAllowed");
        if (shares > c.bal(who) - c.lck(who)) fail("InsufficientShares");
        if (e.queue.length >= f.max_requests_per_epoch) fail("EpochFull");
        const id = c.requests.length + 1;
        c.requests.push({ id, epoch: e.epoch, investor: who, kind: "Redeem", amount: shares, cash_to: row.target, status: "Pending", shares_out: 0n, cash_out: 0n, reject: 0 });
        e.queue.push(id);
        c.locked.set(who, c.lck(who) + shares);
        e.redeem_shares_total += shares;
        this.reqKeys.set(`${who}:${row.at}`, id);
      } else if (action === "cancel") {
        const id = this.reqKeys.get(row.target.replace(/^req:/, "")) ?? fail("RequestNotFound");
        const r = c.requests[id - 1];
        if (r.investor !== who) fail("NotRequestOwner");
        if (r.status !== "Pending") fail("NotPending");
        const e = c.epochs[r.epoch - 1];
        if (this.now >= e.cutoff) fail("CutoffPassed");
        if (r.kind === "Subscribe") {
          c.vaultCash -= r.amount;
          e.sub_total -= r.amount;
        } else {
          c.locked.set(who, c.lck(who) - r.amount);
          e.redeem_shares_total -= r.amount;
        }
        r.status = "Cancelled";
      } else if (action === "transfer") {
        this.transfer(c, who, row.target, parseFixed(row.amount, 7));
      } else {
        throw new Error(`unknown action ${action}`);
      }
    });
    if (result !== row.expect) throw new Error(`row ${row.seq} ${action} ${who}: expected ${row.expect}, got ${result}`);
    c.outcomes.push({ seq: row.seq, at: row.at, action, investor: who, amount: row.amount.trim(), expect: row.expect, result });
  }

  private transfer(c: ClassState, from: string, to: string, amount: bigint): void {
    if (amount <= 0n) fail("InvalidAmount");
    if (from === to) fail("SameAddress");
    requireSend(eligibility(c, from, this.now));
    requireReceive(eligibility(c, to, this.now));
    if (amount > c.bal(from) - c.lck(from)) fail("InsufficientUnlocked");
    c.dist.onChange(from, c.bal(from));
    c.dist.onChange(to, c.bal(to));
    c.shares.set(from, c.bal(from) - amount);
    c.shares.set(to, c.bal(to) + amount);
    c.journal.push({ at: this.now, contract: "compliance", type: "transferred", from, to, amount: amount.toString() });
  }

  /** One `settle` call; throws InsufficientLiquidity without changing state. */
  private settleCall(c: ClassState, e: Epoch, batch: number): number {
    const d = this.seed.fund.nav_decimals;
    if (e.status !== "Struck" && e.status !== "Settling") fail("WrongEpochState");
    const end = Math.min(e.queue.length, e.cursor + batch);
    const updates: { r: Request; shares_out: bigint; cash_out: bigint; reject: number; burn: boolean }[] = [];
    let claimable = e.claimable_cash;
    for (let i = e.cursor; i < end; i++) {
      const r = c.requests[e.queue[i] - 1];
      if (r.status !== "Pending") continue;
      if (r.kind === "Subscribe") {
        const st = eligibility(c, r.investor, this.now);
        if (!st.can_receive) {
          const reject = st.frozen ? 6 : !st.registered || !st.kyc_valid ? 4 : !st.jurisdiction_ok ? 5 : 0;
          updates.push({ r, shares_out: 0n, cash_out: r.amount, reject, burn: false });
          claimable += r.amount;
        } else {
          const { shares, dust } = sharesForCash(r.amount, e.nav, d);
          updates.push({ r, shares_out: shares, cash_out: dust, reject: 0, burn: false });
          claimable += dust;
        }
      } else {
        const cash = cashForShares(r.amount, e.nav, d);
        updates.push({ r, shares_out: 0n, cash_out: cash, reject: 0, burn: true });
        claimable += cash;
      }
    }
    const remaining = e.queue.length - end;
    let surplus = 0n;
    if (remaining === 0) {
      surplus = e.sub_total + e.liquidity - claimable;
      if (surplus < 0n) fail("InsufficientLiquidity");
    }
    // commit
    for (const u of updates) {
      if (u.burn) {
        c.dist.onChange(u.r.investor, c.bal(u.r.investor));
        c.shares.set(u.r.investor, c.bal(u.r.investor) - u.r.amount);
        c.locked.set(u.r.investor, c.lck(u.r.investor) - u.r.amount);
        c.totalShares -= u.r.amount;
        c.journal.push({ at: this.now, contract: "compliance", type: "burned", holder: u.r.investor, amount: u.r.amount.toString() });
      }
      u.r.shares_out = u.shares_out;
      u.r.cash_out = u.cash_out;
      u.r.reject = u.reject;
      u.r.status = "Claimable";
    }
    e.claimable_cash = claimable;
    e.cursor = end;
    if (remaining === 0) {
      c.vaultCash -= surplus;
      c.treasury += surplus;
      c.lastNav = e.nav;
      e.status = "Settled";
    } else {
      e.status = "Settling";
    }
    return remaining;
  }

  settleAll(c: ClassState, ep: number, topup: bigint): void {
    const e = c.epochs[ep - 1];
    let calls = 0;
    let needed = false;
    for (;;) {
      calls++;
      try {
        if (this.settleCall(c, e, this.seed.fund.settle_batch) === 0) break;
      } catch (err) {
        if (err instanceof FundError && err.message === "InsufficientLiquidity" && topup > 0n && !needed) {
          needed = true;
          e.liquidity += topup;
          c.treasury -= topup;
          c.vaultCash += topup;
          continue;
        }
        throw err;
      }
    }
    if (needed !== topup > 0n) throw new Error(`liquidity expectation mismatch for ${c.id} epoch ${ep}`);
    e.settle_calls = calls;
    e.liquidity_needed = needed;
  }

  private issue(c: ClassState, to: string, amount: bigint): void {
    const st = eligibility(c, to, this.now);
    if (!st.registered) fail("NotRegistered");
    if (st.frozen) fail("Frozen");
    c.dist.onChange(to, c.bal(to));
    c.shares.set(to, c.bal(to) + amount);
    c.totalShares += amount;
    c.journal.push({ at: this.now, contract: "compliance", type: "issued", to, amount: amount.toString() });
  }

  private pay(c: ClassState, to: string, amount: bigint): void {
    c.paidCash.set(to, (c.paidCash.get(to) ?? 0n) + amount);
  }

  private claim(c: ClassState, r: Request): void {
    r.status = "Claimed";
    if (r.shares_out > 0n) this.issue(c, r.investor, r.shares_out);
    if (r.cash_out > 0n) {
      c.vaultCash -= r.cash_out;
      this.pay(c, r.kind === "Redeem" && r.cash_to ? r.cash_to : `wallet:${r.investor}`, r.cash_out);
    }
  }

  private dist(row: Row): void {
    const ci = this.classIndex(row.class);
    const c = this.classes[ci];
    const result = this.attempt(() => {
      if (row.action === "declare") {
        const amount = parseFixed(row.amount, 7);
        try {
          c.dist.declare(amount, c.totalShares);
        } catch (e) {
          fail((e as Error).message);
        }
        c.treasury -= amount;
      } else if (row.action === "claim") {
        const inv = c.investors.get(row.holder);
        if (!inv || !inv.cash.includes(row.to)) fail("CashAddressNotAllowed");
        if (inv!.frozen) fail("Frozen");
        const paid = c.dist.payOut(row.holder, c.bal(row.holder));
        if (paid === 0n) fail("NothingToClaim");
        this.pay(c, row.to, paid);
      } else if (row.action === "push") {
        const ids = [...c.holderOrder];
        if (this.classOf.get("inv_05b") === ci && c.investors.has("inv_05b")) ids.push("inv_05b");
        for (let i = 0; i < ids.length; i += 25) {
          for (const h of ids.slice(i, i + 25)) {
            const inv = c.investors.get(h);
            if (!inv || inv.frozen) continue;
            const paid = c.dist.payOut(h, c.bal(h));
            if (paid > 0n) this.pay(c, inv.cash[0], paid);
          }
        }
      } else {
        throw new Error(`unknown distribution action ${row.action}`);
      }
    });
    if (result !== row.expect) throw new Error(`distribution ${row.action}: expected ${row.expect}, got ${result}`);
    c.outcomes.push({ at: row.at, action: `distribution.${row.action}`, investor: row.holder, amount: row.amount, expect: row.expect, result });
  }

  private forced(row: Row): void {
    const f = this.seed.fund;
    const ci = this.classOf.get(row.from)!;
    const c = this.classes[ci];
    if (!c.investors.has(row.to)) {
      this.register(c, row.to, {
        kyc_expiry_unix: isoToUnix(`${row.new_kyc_expiry}T00:00:00Z`),
        jurisdiction: row.new_jurisdiction,
        cash_addresses: [row.new_cash],
        legal_name: `${c.investors.get(row.from)?.legal_name ?? row.from} (replacement wallet)`,
      });
      this.classOf.set(row.to, ci);
    }
    const amount = row.shares === "all" ? c.bal(row.from) - c.lck(row.from) : parseFixed(row.shares, 7);
    const policy = f.policies.find((p) => p.contract === "compliance" && p.fn === "forced_transfer")!;
    const roles = row.signers.split("+").map((n) => f.signers.find((s) => s.name === n.trim())?.role);
    const ta = roles.filter((r) => r === "Ta").length;
    const admin = roles.filter((r) => r === "Admin").length;
    let result: string;
    if (ta < policy.ta) result = "InsufficientTa";
    else if (admin < policy.admin) result = "InsufficientAdmin";
    else if (ta + admin < policy.total) result = "InsufficientTotal";
    else
      result = this.attempt(() => {
        if (amount <= 0n) fail("InvalidAmount");
        if (row.from === row.to) fail("SameAddress");
        requireReceive(eligibility(c, row.to, this.now));
        if (amount > c.bal(row.from) - c.lck(row.from)) fail("InsufficientUnlocked");
        c.dist.onChange(row.from, c.bal(row.from));
        c.dist.onChange(row.to, c.bal(row.to));
        c.shares.set(row.from, c.bal(row.from) - amount);
        c.shares.set(row.to, c.bal(row.to) + amount);
        c.journal.push({ at: this.now, contract: "compliance", type: "forced", from: row.from, to: row.to, amount: amount.toString() });
      });
    if (result !== row.expect) throw new Error(`forced ${row.from}->${row.to}: expected ${row.expect}, got ${result}`);
    c.outcomes.push({ at: row.at, action: "forced_transfer", investor: row.from, to: row.to, signers: row.signers, amount: amount.toString(), expect: row.expect, result });
  }
}

/** The same shape as data/seed/expected-scenario.json (minus provenance fields). */
export function golden(res: ReplayResult): unknown {
  return {
    classes: res.classes.map((c) => {
      const ids = [...c.investors.keys()].sort();
      return {
        class: c.id,
        epochs: c.epochs.map((e) => ({
          epoch: e.epoch,
          status: e.status,
          nav: e.nav.toString(),
          nav_ts: e.nav_ts,
          sub_total: e.sub_total.toString(),
          redeem_shares_total: e.redeem_shares_total.toString(),
          liquidity: e.liquidity.toString(),
          claimable_cash: e.claimable_cash.toString(),
          surplus: (e.sub_total + e.liquidity - e.claimable_cash).toString(),
          settle_calls: e.settle_calls,
          liquidity_needed: e.liquidity_needed,
        })),
        requests: c.requests.map((r) => ({
          id: r.id,
          epoch: r.epoch,
          investor: r.investor,
          kind: r.kind,
          amount: r.amount.toString(),
          status: r.status,
          reject: r.reject,
          shares_out: r.shares_out.toString(),
          cash_out: r.cash_out.toString(),
        })),
        holders: Object.fromEntries(
          ids.map((id) => [id, { shares: c.bal(id).toString(), locked: c.lck(id).toString(), dist_accrued: c.dist.accrued(id, c.bal(id)).toString() }]),
        ),
        total_shares: c.totalShares.toString(),
        treasury: c.treasury.toString(),
        vault_cash: c.vaultCash.toString(),
        last_nav: c.lastNav.toString(),
        distribution: {
          declared: c.dist.declared.toString(),
          claimed: c.dist.claimed.toString(),
          per_share_scaled: c.dist.lastPerShare.toString(),
          acc_scaled: c.dist.acc.toString(),
          carry_scaled: c.dist.carryScaled.toString(),
          held: (c.dist.declared - c.dist.claimed).toString(),
        },
        outcomes: c.outcomes,
      };
    }),
  };
}

/** Deep comparison that returns the paths that differ (empty = identical). */
export function diff(a: unknown, b: unknown, path = "$"): string[] {
  if (typeof a !== typeof b) return [`${path}: ${JSON.stringify(a)} != ${JSON.stringify(b)}`];
  if (a === null || b === null || typeof a !== "object") return a === b ? [] : [`${path}: ${JSON.stringify(a)} != ${JSON.stringify(b)}`];
  if (Array.isArray(a) !== Array.isArray(b)) return [`${path}: array vs object`];
  const out: string[] = [];
  const ka = Object.keys(a as object);
  const kb = Object.keys(b as object);
  for (const k of new Set([...ka, ...kb])) {
    out.push(...diff((a as Record<string, unknown>)[k], (b as Record<string, unknown>)[k], `${path}.${k}`));
  }
  return out;
}

/** Compare a replay with the committed golden file (provenance keys ignored). */
export function compareWithGolden(res: ReplayResult, expected: { classes: unknown }): string[] {
  return diff(golden(res), { classes: expected.classes });
}
