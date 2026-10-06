/**
 * Local event journal. Soroban RPC keeps events for about 7 days
 * (research/01 §14.7), so the TA's console ingests contract events into a
 * JSON file it owns and reconciles against it. Events are stored with the
 * registrar's names; holders are addresses on-chain and investor ids in the
 * offline replay.
 */
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { scValToNative, type rpc } from "@stellar/stellar-sdk";

export interface JournalEvent {
  at?: number;
  ledger?: number;
  tx?: string;
  contract: string;
  type: string;
  [k: string]: unknown;
}

export interface Journal {
  version: 1;
  events: JournalEvent[];
  lastLedger?: number;
}

export function loadJournal(path: string): Journal {
  if (!existsSync(path)) return { version: 1, events: [] };
  return JSON.parse(readFileSync(path, "utf8")) as Journal;
}

export function saveJournal(path: string, j: Journal): void {
  writeFileSync(path, JSON.stringify(j, null, 2) + "\n");
}

/** Names of the #[topic] fields after the event name, per contract event. */
export const TOPIC_FIELDS: Record<string, string[]> = {
  transferred: ["from", "to"],
  forced: ["from", "to"],
  issued: ["to"],
  burned: ["holder"],
  investor_set: ["investor"],
  frozen: ["investor"],
  jurisdiction_set: ["code"],
  requested: ["id", "epoch"],
  cancelled: ["id"],
  struck: ["epoch"],
  nav_override: ["epoch"],
  settled: ["epoch"],
  claimed: ["id"],
  aborted: ["epoch"],
  dist_claimed: ["holder"],
  published: ["asset"],
};

/** Convert an RPC event: topic[0] is the event name (snake case), topic[1..] and value per #[contractevent]. */
export function fromRpcEvent(e: rpc.Api.EventResponse, names: Record<string, string>): JournalEvent {
  const topics = e.topic.map((t) => scValToNative(t));
  const value = scValToNative(e.value) as Record<string, unknown>;
  const contractId = e.contractId?.toString() ?? "";
  const norm = (v: unknown): unknown => (typeof v === "bigint" ? v.toString() : v);
  const data: Record<string, unknown> = {};
  if (value && typeof value === "object") for (const [k, v] of Object.entries(value)) data[k] = norm(v);
  const type = String(topics[0]);
  const fields = TOPIC_FIELDS[type] ?? [];
  topics.slice(1).forEach((t, i) => (data[fields[i] ?? `topic${i + 1}`] = norm(t)));
  return { ledger: e.ledger, tx: e.txHash, contract: names[contractId] ?? contractId, type, ...data };
}

/** Merge new events, skipping duplicates (same tx + type + payload). */
export function ingest(j: Journal, events: JournalEvent[]): number {
  const key = (e: JournalEvent) => JSON.stringify(e);
  const seen = new Set(j.events.map(key));
  let added = 0;
  for (const e of events) {
    if (!seen.has(key(e))) {
      j.events.push(e);
      seen.add(key(e));
      added++;
      if (e.ledger && (!j.lastLedger || e.ledger > j.lastLedger)) j.lastLedger = e.ledger;
    }
  }
  return added;
}

/** Share balances implied by registrar events alone. */
export function balancesFromJournal(events: JournalEvent[]): Map<string, bigint> {
  const b = new Map<string, bigint>();
  const add = (h: unknown, v: bigint) => b.set(String(h), (b.get(String(h)) ?? 0n) + v);
  for (const e of events) {
    if (e.contract !== "compliance") continue;
    const amt = BigInt(String(e.amount ?? "0"));
    if (e.type === "issued") add(e.to, amt);
    else if (e.type === "burned") add(e.holder, -amt);
    else if (e.type === "transferred" || e.type === "forced") {
      add(e.from, -amt);
      add(e.to, amt);
    }
  }
  return b;
}
