/**
 * Bigint mirror of contracts/async_vault/src/math.rs. Every result rounds in
 * the fund's favour; `d` is the oracle's decimals (14 in the seed).
 */
export const BPS = 10_000n;

export function pow10(d: number): bigint {
  return 10n ** BigInt(d);
}

/** shares = floor(cash*10^d/nav); cost = ceil(shares*nav/10^d); dust = cash - cost. */
export function sharesForCash(cash: bigint, nav: bigint, d: number): { shares: bigint; dust: bigint } {
  if (cash < 0n || nav <= 0n) throw new RangeError("invalid cash or nav");
  const scale = pow10(d);
  const shares = (cash * scale) / nav;
  const prod = shares * nav;
  const cost = prod / scale + (prod % scale === 0n ? 0n : 1n);
  return { shares, dust: cash - cost };
}

/** floor(shares*nav/10^d) */
export function cashForShares(shares: bigint, nav: bigint, d: number): bigint {
  if (shares < 0n || nav <= 0n) throw new RangeError("invalid shares or nav");
  return (shares * nav) / pow10(d);
}

/** |new - old| * 10_000 > old * maxBps */
export function moveExceedsBand(oldNav: bigint, newNav: bigint, maxBps: number): boolean {
  const diff = newNav > oldNav ? newNav - oldNav : oldNav - newNav;
  return diff * BPS > oldNav * BigInt(maxBps);
}

/** Position value in cash stroops at a NAV (floor). */
export function valueAt(shares: bigint, nav: bigint, d: number): bigint {
  return cashForShares(shares, nav, d);
}
