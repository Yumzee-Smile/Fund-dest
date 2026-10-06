/**
 * Fixed-point amounts. Cash (USDC/EURC) and shares are classic Stellar assets
 * with 7 decimals; the NAV uses the oracle's decimals (14 in the seed). Input
 * arrives as "1,000,000.0000001", " 4,000.00 ", "12000" or "USD 250", so
 * parsing is strict about meaning and lenient about formatting.
 */
export const CASH_DECIMALS = 7;
export const NAV_DECIMALS = 14;

export class AmountError extends Error {}

/** Parse a human decimal into an integer with `decimals` places. */
export function parseFixed(raw: string, decimals: number = CASH_DECIMALS): bigint {
  if (typeof raw !== "string") throw new AmountError("amount must be a string");
  let s = raw.trim().replace(/^(usdc|usd|eurc|eur|\$|€)\s*/i, "").replace(/\s*(usdc|usd|eurc|eur|shares?)$/i, "").trim();
  if (s === "") throw new AmountError("empty amount");
  if (s.startsWith("-") || /^\(.*\)$/.test(s)) throw new AmountError(`negative amount not allowed: "${raw}"`);
  s = s.replace(/,/g, "").replace(/(\d)[\s ]+(?=\d{3}(\D|$))/g, "$1");
  if (!/^\d+(\.\d+)?$/.test(s)) throw new AmountError(`unparseable amount: "${raw}"`);
  const [whole, frac = ""] = s.split(".");
  if (frac.length > decimals) throw new AmountError(`more than ${decimals} decimals: "${raw}"`);
  return BigInt(whole) * 10n ** BigInt(decimals) + BigInt(frac.padEnd(decimals, "0") || "0");
}

export const parseAmount = (raw: string): bigint => parseFixed(raw, CASH_DECIMALS);
export const parseNav = (raw: string): bigint => parseFixed(raw, NAV_DECIMALS);

/** Format an integer with `decimals` places, trimming trailing zeros to `minDecimals`. */
export function formatFixed(v: bigint, decimals: number = CASH_DECIMALS, minDecimals = 2): string {
  const neg = v < 0n;
  const abs = neg ? -v : v;
  const scale = 10n ** BigInt(decimals);
  const whole = (abs / scale).toString().replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  let frac = (abs % scale).toString().padStart(decimals, "0").replace(/0+$/, "");
  if (frac.length < minDecimals) frac = frac.padEnd(minDecimals, "0");
  return `${neg ? "-" : ""}${whole}${frac ? "." + frac : ""}`;
}

export const formatAmount = (v: bigint, minDecimals = 2): string => formatFixed(v, CASH_DECIMALS, minDecimals);
export const formatNav = (v: bigint): string => formatFixed(v, NAV_DECIMALS, NAV_DECIMALS);

/** ISO-8601 UTC instant -> unix seconds. Throws on anything else. */
export function isoToUnix(iso: string): number {
  if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(:\d{2})?Z$/.test(iso)) throw new Error(`not an ISO UTC timestamp: "${iso}"`);
  return Math.floor(Date.parse(iso) / 1000);
}

export function unixToIso(t: number): string {
  return new Date(t * 1000).toISOString().replace(".000Z", "Z");
}
