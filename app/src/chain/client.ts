/**
 * Transaction composition and submission for the five contracts.
 *
 * Offline (the default): arguments are converted with the embedded contract
 * spec (`contract.Spec.funcArgsToScVals`) into an `invokeContractFunction`
 * operation inside an unsigned transaction envelope, printed as XDR next to
 * the equivalent `stellar contract invoke` command. Online (`--submit` and a
 * reachable `SOROBAN_RPC_URL`): the same call goes through `contract.Client`,
 * is simulated, signed (ops-account entries through ops-auth.ts) and sent.
 */
import { Account, Address, BASE_FEE, Keypair, Operation, StrKey, TransactionBuilder, contract, rpc, xdr } from "@stellar/stellar-sdk";
import { ASYNC_VAULT_SPEC, COMPLIANCE_SPEC, DISTRIBUTION_SPEC, NAV_ORACLE_SPEC, OPS_ACCOUNT_SPEC } from "./specs.js";
import type { Config } from "../config.js";
import { authorizeOpsEntry, type RoleKey } from "./ops-auth.js";
import type { PolicyRow } from "../seed.js";

export type ContractName = "ops_account" | "nav_oracle" | "compliance" | "distribution" | "async_vault";

const SPECS: Record<ContractName, string> = {
  ops_account: OPS_ACCOUNT_SPEC,
  nav_oracle: NAV_ORACLE_SPEC,
  compliance: COMPLIANCE_SPEC,
  distribution: DISTRIBUTION_SPEC,
  async_vault: ASYNC_VAULT_SPEC,
};

const specCache = new Map<ContractName, contract.Spec>();

export function specFor(name: ContractName): contract.Spec {
  let s = specCache.get(name);
  if (!s) {
    s = new contract.Spec(SPECS[name]);
    specCache.set(name, s);
  }
  return s;
}

/** Placeholder ids used when a contract id is not configured (offline plans). */
export function placeholderId(name: ContractName): string {
  const b = Buffer.alloc(32);
  b.write(name);
  return StrKey.encodeContract(b);
}
export const PLACEHOLDER_SOURCE = StrKey.encodeEd25519PublicKey(Buffer.alloc(32));

export interface Invocation {
  contract: ContractName;
  contractId: string;
  fn: string;
  args: Record<string, unknown>;
  scArgs: xdr.ScVal[];
  /** Base64 XDR of the unsigned transaction envelope (placeholder source account). */
  transactionXdr: string;
  /** Equivalent stellar-cli command. */
  cli: string;
  /** Who must authorise: "ops:<policy>", "investor", "treasury", ... */
  signer: string;
}

function cliArg(v: unknown): string {
  if (typeof v === "bigint" || typeof v === "number") return String(v);
  if (typeof v === "string") return v;
  if (v instanceof Uint8Array || Buffer.isBuffer(v)) return Buffer.from(v).toString("hex");
  return `'${JSON.stringify(v, (_k, x) => (typeof x === "bigint" ? String(x) : x instanceof Uint8Array ? Buffer.from(x).toString("hex") : x))}'`;
}

/** Build an invocation without touching the network. Throws if args do not match the spec. */
export function compose(
  name: ContractName,
  contractId: string,
  fn: string,
  args: Record<string, unknown>,
  networkPassphrase: string,
  signer: string,
): Invocation {
  const scArgs = specFor(name).funcArgsToScVals(fn, args);
  const op = Operation.invokeContractFunction({ contract: contractId, function: fn, args: scArgs });
  const tx = new TransactionBuilder(new Account(PLACEHOLDER_SOURCE, "0"), { fee: BASE_FEE, networkPassphrase })
    .addOperation(op)
    .setTimeout(300)
    .build();
  const flags = Object.entries(args)
    .map(([k, v]) => `--${k} ${cliArg(v)}`)
    .join(" ");
  return {
    contract: name,
    contractId,
    fn,
    args,
    scArgs,
    transactionXdr: tx.toXDR(),
    cli: `stellar contract invoke --id ${contractId} --network testnet --source-account <key> -- ${fn} ${flags}`.trimEnd(),
    signer,
  };
}

/** Decode a composed transaction back into (contract, fn, native args). */
export function decode(name: ContractName, txXdr: string, networkPassphrase: string): { contractId: string; fn: string; args: unknown[] } {
  const tx = TransactionBuilder.fromXDR(txXdr, networkPassphrase);
  const op = (tx as unknown as { operations: { func: xdr.HostFunction }[] }).operations[0];
  if (!(op.func instanceof xdr.HostFunctionInvokeContract)) throw new Error("not a contract invocation");
  const inv = op.func.invokeContract;
  const contractId = Address.fromScAddress(inv.contractAddress).toString();
  const fn = inv.functionName.toString();
  const spec = specFor(name);
  const fnSpec = spec.getFunc(fn);
  const args = inv.args.map((v: xdr.ScVal, i: number) => spec.scValToNative(v, fnSpec.inputs[i].type));
  return { contractId, fn, args };
}

/** True when an RPC endpoint is configured and answers getHealth. */
export async function rpcReachable(config: Config): Promise<boolean> {
  if (!config.rpcUrl) return false;
  try {
    const server = new rpc.Server(config.rpcUrl, { allowHttp: config.rpcUrl.startsWith("http:") });
    return (await server.getHealth()).status === "healthy";
  } catch {
    return false;
  }
}

export interface SubmitOptions {
  /** Keypair paying the fee and signing as a classic account (investor or treasury). */
  source: Keypair;
  /** Role keys that sign ops_account auth entries. */
  opsKeys?: RoleKey[];
  opsAddress?: string;
  policies?: PolicyRow[];
  contractName?: (contractId: string) => string;
}

/** Simulate, sign and send through contract.Client. Only used with a reachable RPC. */
export async function submit(config: Config, inv: Invocation, opts: SubmitOptions): Promise<{ hash: string; result: unknown }> {
  if (!config.rpcUrl) throw new Error("SOROBAN_RPC_URL is not set");
  const signer = contract.basicNodeSigner(opts.source, config.networkPassphrase);
  const client = new contract.Client(specFor(inv.contract), {
    contractId: inv.contractId,
    networkPassphrase: config.networkPassphrase,
    rpcUrl: config.rpcUrl,
    allowHttp: config.rpcUrl.startsWith("http:"),
    publicKey: opts.source.publicKey(),
    signTransaction: signer.signTransaction,
    signAuthEntry: signer.signAuthEntry,
  });
  const method = (client as unknown as Record<string, (a: Record<string, unknown>) => Promise<contract.AssembledTransaction<unknown>>>)[inv.fn];
  if (typeof method !== "function") throw new Error(`function ${inv.fn} not in the ${inv.contract} spec`);
  const assembled = await method.call(client, inv.args);
  if (opts.opsKeys && opts.opsAddress && opts.policies && opts.contractName) {
    const { opsKeys, opsAddress, policies, contractName } = opts;
    await assembled.signAuthEntries({
      address: opsAddress,
      authorizeEntry: (entry, _signer, validUntil, passphrase) =>
        authorizeOpsEntry(entry, opsKeys, policies, contractName, validUntil, passphrase, opsAddress),
    });
  }
  const sent = await assembled.signAndSend();
  return { hash: sent.sendTransactionResponse?.hash ?? "", result: sent.result };
}

/** Unwrap a contract.Client result (a Result wrapper for functions with errors, or the plain value). */
export function unwrapResult(v: unknown): unknown {
  if (v && typeof (v as { unwrap?: unknown }).unwrap === "function") return (v as { unwrap: () => unknown }).unwrap();
  return v;
}

/** Simulate a read-only call and return its decoded value. Only used with a reachable RPC. */
export async function view(config: Config, name: ContractName, contractId: string, fn: string, args: Record<string, unknown>, publicKey: string): Promise<unknown> {
  if (!config.rpcUrl) throw new Error("SOROBAN_RPC_URL is not set");
  const client = new contract.Client(specFor(name), {
    contractId,
    networkPassphrase: config.networkPassphrase,
    rpcUrl: config.rpcUrl,
    allowHttp: config.rpcUrl.startsWith("http:"),
    publicKey,
  });
  const method = (client as unknown as Record<string, (a: Record<string, unknown>) => Promise<contract.AssembledTransaction<unknown>>>)[fn];
  if (typeof method !== "function") throw new Error(`function ${fn} not in the ${name} spec`);
  const assembled = await method.call(client, args);
  return unwrapResult(assembled.result);
}

/** Raw contract events from RPC (RPC keeps about 7 days; see register/journal.ts). */
export async function fetchEvents(config: Config, contractIds: string[], startLedger?: number): Promise<rpc.Api.EventResponse[]> {
  if (!config.rpcUrl) throw new Error("SOROBAN_RPC_URL is not set");
  const server = new rpc.Server(config.rpcUrl, { allowHttp: config.rpcUrl.startsWith("http:") });
  const latest = await server.getLatestLedger();
  const from = startLedger ?? Math.max(1, latest.sequence - 17_280 * 7 + 100);
  const res = await server.getEvents({ startLedger: from, filters: [{ type: "contract", contractIds: contractIds.slice(0, 5) }], limit: 1000 });
  return res.events;
}
