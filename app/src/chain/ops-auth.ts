/**
 * Authorization entries for `ops_account`, the custom account the TA and the
 * fund administrator share.
 *
 * The contract's `__check_auth` expects its signature as `Vec<Sig>` where
 * `Sig = { key: BytesN<32>, sig: BytesN<64> }`, sorted strictly by public key
 * (duplicates are rejected on-chain), and applies a per-function role policy.
 * This module:
 *
 * - computes the Soroban auth payload (sha256 of the HashIdPreimage XDR);
 * - signs it with every role key, deduplicates and sorts by raw public key;
 * - encodes the signature vector as the ScVal the contract decodes;
 * - checks the local mirror of the policy table (data/seed/fund.json) before
 *   anything is submitted, so a forced transfer with only a TA key is refused
 *   locally with the same error name the contract would return.
 */
import { createHash } from "node:crypto";
import { Address, Keypair, authorizeEntry, buildAuthorizationEntryPreimage, xdr } from "@stellar/stellar-sdk";
import type { PolicyRow } from "../seed.js";

export type Role = "Ta" | "Admin";

export interface RoleKey {
  name: string;
  role: Role;
  keypair: Keypair;
}

export class PolicyError extends Error {
  constructor(public code: "NoPolicy" | "InsufficientTa" | "InsufficientAdmin" | "InsufficientTotal" | "EmptySignatures", msg: string) {
    super(msg);
  }
}

/** Same order of checks as `__check_auth`: TA, then ADMIN, then total. */
export function checkPolicy(policies: PolicyRow[], contract: string, fn: string, roles: Role[]): void {
  if (roles.length === 0) throw new PolicyError("EmptySignatures", "no signatures");
  // Administering the ops account itself is hard-coded on-chain: one TA and one ADMIN.
  const p = contract === "ops_account" ? { ta: 1, admin: 1, total: 2 } : policies.find((r) => r.contract === contract && r.fn === fn);
  if (!p) throw new PolicyError("NoPolicy", `no ops policy for ${contract}.${fn}`);
  const ta = roles.filter((r) => r === "Ta").length;
  const admin = roles.filter((r) => r === "Admin").length;
  if (ta < p.ta) throw new PolicyError("InsufficientTa", `${contract}.${fn} needs ${p.ta} TA signature(s), got ${ta}`);
  if (admin < p.admin) throw new PolicyError("InsufficientAdmin", `${contract}.${fn} needs ${p.admin} ADMIN signature(s), got ${admin}`);
  if (ta + admin < p.total) throw new PolicyError("InsufficientTotal", `${contract}.${fn} needs ${p.total} signature(s), got ${ta + admin}`);
}

/** Deduplicate by public key and sort by the raw 32-byte key. */
export function sortedUniqueKeys(keys: RoleKey[]): RoleKey[] {
  const seen = new Map<string, RoleKey>();
  for (const k of keys) seen.set(k.keypair.publicKey(), k);
  return [...seen.values()].sort((a, b) => Buffer.compare(a.keypair.rawPublicKey(), b.keypair.rawPublicKey()));
}

/** Encode `Vec<Sig>`: a vector of maps with symbol keys "key" and "sig" (sorted). */
export function sigVecScVal(payload: Uint8Array, keys: RoleKey[]): xdr.ScVal {
  const items = sortedUniqueKeys(keys).map((k) =>
    xdr.ScVal.scvMap([
      new xdr.ScMapEntry({ key: xdr.ScVal.scvSymbol("key"), val: xdr.ScVal.scvBytes(k.keypair.rawPublicKey()) }),
      new xdr.ScMapEntry({ key: xdr.ScVal.scvSymbol("sig"), val: xdr.ScVal.scvBytes(k.keypair.sign(Buffer.from(payload))) }),
    ]),
  );
  return xdr.ScVal.scvVec(items);
}

/** sha256(HashIdPreimage XDR): what every signer signs. */
export function authPayload(entry: xdr.SorobanAuthorizationEntry, validUntilLedgerSeq: number, networkPassphrase: string): Buffer {
  const preimage = buildAuthorizationEntryPreimage(entry, validUntilLedgerSeq, networkPassphrase);
  return createHash("sha256").update(preimage.toXDR()).digest();
}

/** Root function of an auth entry as `{ contract, fn }` (contract as a C... StrKey). */
export function rootCall(entry: xdr.SorobanAuthorizationEntry): { contract: string; fn: string } {
  const f = entry.rootInvocation.function;
  if (!(f instanceof xdr.SorobanAuthorizedFunctionContractFn)) throw new Error("auth entry is not a contract call");
  const args = f.contractFn;
  return { contract: Address.fromScAddress(args.contractAddress).toString(), fn: args.functionName.toString() };
}

/**
 * Sign an auth entry of `opsAddress`. `contractName` maps the root contract
 * id to the policy table's name (compliance, async_vault, ...). Refuses to
 * sign when the local policy mirror says the keys are not enough.
 */
export async function authorizeOpsEntry(
  entry: xdr.SorobanAuthorizationEntry,
  keys: RoleKey[],
  policies: PolicyRow[],
  contractName: (contractId: string) => string,
  validUntilLedgerSeq: number,
  networkPassphrase: string,
  opsAddress: string,
): Promise<xdr.SorobanAuthorizationEntry> {
  const { contract, fn } = rootCall(entry);
  const uniq = sortedUniqueKeys(keys);
  checkPolicy(policies, contractName(contract), fn, uniq.map((k) => k.role));
  return authorizeEntry(
    entry,
    async (_preimage, payload) => ({ signatureScVal: sigVecScVal(payload, uniq), address: opsAddress }),
    validUntilLedgerSeq,
    networkPassphrase,
    opsAddress,
  );
}

/** Decode a signature ScVal back into `[{ key, sig }]` (for tests and audits). */
export function decodeSigVec(v: xdr.ScVal): { key: Buffer; sig: Buffer }[] {
  if (!(v instanceof xdr.ScValVec)) throw new Error("signature is not a vector");
  return (v.vec ?? []).map((m) => {
    if (!(m instanceof xdr.ScValMap)) throw new Error("signature item is not a map");
    const entries = m.map ?? [];
    const get = (name: string): Buffer => {
      const e = entries.find((x) => x.key instanceof xdr.ScValSymbol && x.key.sym.toString() === name);
      if (!e || !(e.val instanceof xdr.ScValBytes)) throw new Error(`missing ${name}`);
      return Buffer.from(e.val.bytes.value);
    };
    return { key: get("key"), sig: get("sig") };
  });
}
