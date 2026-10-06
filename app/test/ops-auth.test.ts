import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { Address, Keypair, Networks, StrKey, nativeToScVal, xdr } from "@stellar/stellar-sdk";
import { authorizeOpsEntry, authPayload, checkPolicy, decodeSigVec, PolicyError, sigVecScVal, sortedUniqueKeys, type RoleKey } from "../src/chain/ops-auth.js";
import { loadFund } from "../src/seed.js";
import { force } from "../src/chain/tx.js";
import { loadConfig } from "../src/config.js";
import { main } from "../src/cli.js";
import { SEED } from "./paths.js";

const fund = loadFund(SEED);
const kp = (n: number) => Keypair.fromRawEd25519Seed(createHash("sha256").update(`test-signer-${n}`).digest());
const keys: RoleKey[] = [
  { name: "ta_ops_1", role: "Ta", keypair: kp(1) },
  { name: "ta_ops_2", role: "Ta", keypair: kp(2) },
  { name: "fund_admin_1", role: "Admin", keypair: kp(3) },
];

test("signatures are deduplicated and sorted by raw public key", () => {
  const messy = [keys[2], keys[0], keys[2], keys[1], keys[0]];
  const s = sortedUniqueKeys(messy);
  assert.equal(s.length, 3);
  for (let i = 1; i < s.length; i++) assert.ok(Buffer.compare(s[i - 1].keypair.rawPublicKey(), s[i].keypair.rawPublicKey()) < 0);
  const payload = createHash("sha256").update("payload").digest();
  const v = decodeSigVec(sigVecScVal(payload, messy));
  assert.equal(v.length, 3);
  for (const { key, sig } of v) {
    const k = Keypair.fromPublicKey(StrKey.encodeEd25519PublicKey(key));
    assert.ok(k.verify(payload, sig));
  }
});

test("local policy mirror refuses a forced transfer with one role", () => {
  assert.throws(() => checkPolicy(fund.policies, "compliance", "forced_transfer", ["Ta"]), (e: unknown) => e instanceof PolicyError && e.code === "InsufficientAdmin");
  assert.throws(() => checkPolicy(fund.policies, "compliance", "forced_transfer", ["Admin"]), (e: unknown) => e instanceof PolicyError && e.code === "InsufficientTa");
  assert.throws(() => checkPolicy(fund.policies, "compliance", "forced_transfer", ["Ta", "Ta"]), (e: unknown) => e instanceof PolicyError && e.code === "InsufficientAdmin");
  checkPolicy(fund.policies, "compliance", "forced_transfer", ["Ta", "Admin"]);
  checkPolicy(fund.policies, "async_vault", "pause", ["Admin"]);
  assert.throws(() => checkPolicy(fund.policies, "usdc", "transfer", ["Ta", "Admin"]), (e: unknown) => e instanceof PolicyError && e.code === "NoPolicy");
  const ctx = { config: loadConfig({}), fund, register: [] };
  const a = StrKey.encodeEd25519PublicKey(Buffer.alloc(32, 1));
  const b = StrKey.encodeEd25519PublicKey(Buffer.alloc(32, 2));
  assert.throws(() => force(ctx, a, b, "12000", "lost keys", ["ta_ops_1"]), PolicyError);
  const inv = force(ctx, a, b, "12000", "lost keys", ["ta_ops_1", "fund_admin_1"]);
  assert.equal(inv.fn, "forced_transfer");
});

function entryFor(opsId: string, contractId: string, fn: string): xdr.SorobanAuthorizationEntry {
  const invocation = new xdr.SorobanAuthorizedInvocation({
    function: xdr.SorobanAuthorizedFunction.sorobanAuthorizedFunctionTypeContractFn(
      new xdr.InvokeContractArgs({ contractAddress: Address.fromString(contractId).toScAddress(), functionName: fn, args: [nativeToScVal(1, { type: "u32" })] }),
    ),
    subInvocations: [],
  });
  return new xdr.SorobanAuthorizationEntry({
    credentials: xdr.SorobanCredentials.sorobanCredentialsAddress(
      new xdr.SorobanAddressCredentials({ address: Address.fromString(opsId).toScAddress(), nonce: 7n, signatureExpirationLedger: 0, signature: xdr.ScVal.scvVoid() }),
    ),
    rootInvocation: invocation,
  });
}

test("authorizeOpsEntry signs the HashIdPreimage payload with sorted role keys", async () => {
  const ops = StrKey.encodeContract(Buffer.alloc(32, 9));
  const compliance = StrKey.encodeContract(Buffer.alloc(32, 4));
  const entry = entryFor(ops, compliance, "forced_transfer");
  const name = (id: string) => (id === compliance ? "compliance" : id);
  await assert.rejects(authorizeOpsEntry(entry, [keys[0]], fund.policies, name, 1000, Networks.TESTNET, ops), PolicyError);
  const signed = await authorizeOpsEntry(entry, [keys[2], keys[0]], fund.policies, name, 1000, Networks.TESTNET, ops);
  const creds = signed.credentials as unknown as { address: xdr.SorobanAddressCredentials };
  const sigs = decodeSigVec(creds.address.signature);
  assert.equal(sigs.length, 2);
  assert.ok(Buffer.compare(sigs[0].key, sigs[1].key) < 0);
  const payload = authPayload(signed, 1000, Networks.TESTNET);
  for (const s of sigs) assert.ok(Keypair.fromPublicKey(StrKey.encodeEd25519PublicKey(s.key)).verify(payload, s.sig));
});

test("CLI refuses a single-role forced transfer before signing (exit 3)", async () => {
  const out: string[] = [];
  const err: string[] = [];
  const code = await main(
    ["force", "--from", "inv_05", "--to", "inv_01", "--shares", "12000", "--reason", "lost keys", "--signers", "ta_ops_1", "--seed-dir", SEED],
    { out: (s) => out.push(s), err: (s) => err.push(s) },
    {},
  );
  assert.equal(code, 3);
  assert.match(err.join("\n"), /InsufficientAdmin/);
});
