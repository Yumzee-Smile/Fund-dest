/**
 * Bigint mirror of contracts/distribution: a per-share accumulator scaled by
 * 10^18 with the division remainder carried across declarations.
 */
export const SCALE = 10n ** 18n;

export interface HolderState {
  snap: bigint;
  accruedScaled: bigint;
}

export class Accumulator {
  acc = 0n;
  carryScaled = 0n;
  declared = 0n;
  claimed = 0n;
  lastPerShare = 0n;
  private holders = new Map<string, HolderState>();

  state(h: string): HolderState {
    return this.holders.get(h) ?? { snap: 0n, accruedScaled: 0n };
  }

  private totalScaled(h: string, balance: bigint): bigint {
    const st = this.state(h);
    return st.accruedScaled + balance * (this.acc - st.snap);
  }

  /** Called before a holder's balance changes, with the old balance. */
  onChange(h: string, oldBalance: bigint): void {
    this.holders.set(h, { snap: this.acc, accruedScaled: this.totalScaled(h, oldBalance) });
  }

  /** Returns the scaled per-share increment. Throws "NoSupply" / "InvalidAmount". */
  declare(amount: bigint, totalShares: bigint): bigint {
    if (amount <= 0n) throw new Error("InvalidAmount");
    if (totalShares <= 0n) throw new Error("NoSupply");
    const num = amount * SCALE + this.carryScaled;
    const perShare = num / totalShares;
    this.carryScaled = num % totalShares;
    this.acc += perShare;
    this.declared += amount;
    this.lastPerShare = perShare;
    return perShare;
  }

  accrued(h: string, balance: bigint): bigint {
    return this.totalScaled(h, balance) / SCALE;
  }

  /** Pay the whole-stroop entitlement; keeps the fractional remainder. */
  payOut(h: string, balance: bigint): bigint {
    const tot = this.totalScaled(h, balance);
    const payout = tot / SCALE;
    if (payout === 0n) return 0n;
    this.holders.set(h, { snap: this.acc, accruedScaled: tot - payout * SCALE });
    this.claimed += payout;
    return payout;
  }
}
