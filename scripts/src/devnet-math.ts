/**
 * Pure BigInt replicas of the on-chain fixed-point math in `programs/vanna_lending/src/math/*.rs`
 * — used only for **client-side display** (computing a live health factor, current debt, APR,
 * etc. to show a user). These never influence what actually gets submitted on-chain: every
 * mutating instruction still has its true numbers validated by the program itself. If this ever
 * disagrees with the program, the program is right — this is a read-only convenience.
 */

export const WAD = 10n ** 18n;
export const BALANCE_TO_BORROW_THRESHOLD_WAD = 1_100_000_000_000_000_000n;
export const USD_VALUE_DECIMALS = 9;
export const BASIS_POINTS = 10_000n;
export const SECONDS_PER_YEAR = 31_536_000n;
/** Anchor decodes Rust's `u128::MAX` health-factor sentinel ("infinite health") as this value. */
export const U128_MAX = (1n << 128n) - 1n;

export function mulDivFloor(a: bigint, b: bigint, c: bigint): bigint {
  return (a * b) / c;
}

export function mulDivCeil(a: bigint, b: bigint, c: bigint): bigint {
  return (a * b + (c - 1n)) / c;
}

function pow10(n: number): bigint {
  return 10n ** BigInt(n);
}

/** `math/health.rs::normalize_token_value` — token amount + oracle price -> nano-USD value. */
export function normalizeTokenValue(
  tokenAmount: bigint,
  price: bigint,
  priceExponent: number,
  tokenDecimals: number,
  roundUp: boolean,
): bigint {
  const base = tokenAmount * price;
  const netExp = priceExponent - tokenDecimals + USD_VALUE_DECIMALS;
  if (netExp >= 0) {
    return base * pow10(netExp);
  }
  const factor = pow10(-netExp);
  return roundUp ? mulDivCeil(base, 1n, factor) : mulDivFloor(base, 1n, factor);
}

/** `math/interest.rs::utilization_bps`. */
export function utilizationBps(accountedLiquidityAssets: bigint, totalBorrowAssets: bigint): bigint {
  const gross = accountedLiquidityAssets + totalBorrowAssets;
  if (gross === 0n) return 0n;
  return mulDivFloor(totalBorrowAssets, BASIS_POINTS, gross);
}

/** `math/interest.rs::kink_rate_bps` — annualized borrow APR in basis points. */
export function kinkRateBps(
  utilBps: bigint,
  baseRateBps: number,
  slope1Bps: number,
  slope2Bps: number,
  optimalUtilizationBps: number,
): bigint {
  const optimal = BigInt(optimalUtilizationBps);
  const base = BigInt(baseRateBps);
  if (utilBps <= optimal) {
    if (optimal === 0n) return base;
    return base + mulDivFloor(BigInt(slope1Bps), utilBps, optimal);
  }
  const excessRoom = BASIS_POINTS - optimal;
  const excessUtilization = utilBps - optimal;
  const slope2Component = mulDivFloor(BigInt(slope2Bps), excessUtilization, excessRoom);
  return base + BigInt(slope1Bps) + slope2Component;
}

export interface ReserveLike {
  accountedLiquidityAssets: bigint;
  totalBorrowAssets: bigint;
  accruedProtocolFees: bigint;
  borrowIndexWad: bigint;
  lastUpdateTimestamp: bigint;
  baseRateBps: number;
  slope1Bps: number;
  slope2Bps: number;
  optimalUtilizationBps: number;
  reserveFactorBps: number;
}

export interface AccrualResult {
  newTotalBorrowAssets: bigint;
  newAccruedProtocolFees: bigint;
  newBorrowIndexWad: bigint;
  interestAccrued: bigint;
}

/** `math/interest.rs::accrue`, projected to `now` — the on-chain reserve fields are only true as
 * of `last_update_timestamp`; this brings them current for display without sending a transaction. */
export function accrue(reserve: ReserveLike, now: bigint): AccrualResult {
  const elapsed = now - reserve.lastUpdateTimestamp;
  if (elapsed <= 0n || reserve.totalBorrowAssets === 0n) {
    return {
      newTotalBorrowAssets: reserve.totalBorrowAssets,
      newAccruedProtocolFees: reserve.accruedProtocolFees,
      newBorrowIndexWad: reserve.borrowIndexWad,
      interestAccrued: 0n,
    };
  }

  const util = utilizationBps(reserve.accountedLiquidityAssets, reserve.totalBorrowAssets);
  const rateBps = kinkRateBps(util, reserve.baseRateBps, reserve.slope1Bps, reserve.slope2Bps, reserve.optimalUtilizationBps);

  const numerator = reserve.totalBorrowAssets * rateBps * elapsed;
  const denominator = BASIS_POINTS * SECONDS_PER_YEAR;
  const interest = mulDivCeil(numerator, 1n, denominator);

  const protocolFee = mulDivFloor(interest, BigInt(reserve.reserveFactorBps), BASIS_POINTS);
  const newTotalBorrowAssets = reserve.totalBorrowAssets + interest;
  const newAccruedProtocolFees = reserve.accruedProtocolFees + protocolFee;
  let newBorrowIndexWad = mulDivFloor(reserve.borrowIndexWad, newTotalBorrowAssets, reserve.totalBorrowAssets);
  if (newBorrowIndexWad < WAD) newBorrowIndexWad = WAD;

  return { newTotalBorrowAssets, newAccruedProtocolFees, newBorrowIndexWad, interestAccrued: interest };
}

/** `math/shares.rs::debt_shares_to_assets_up`. */
export function debtSharesToAssetsUp(borrowShares: bigint, totalBorrowShares: bigint, totalBorrowAssets: bigint): bigint {
  if (totalBorrowShares === 0n) return 0n;
  return mulDivCeil(borrowShares, totalBorrowAssets, totalBorrowShares);
}

/** `math/shares.rs::lender_total_assets`. */
export function lenderTotalAssets(accountedLiquidityAssets: bigint, totalBorrowAssets: bigint, accruedProtocolFees: bigint): bigint {
  return accountedLiquidityAssets + totalBorrowAssets - accruedProtocolFees;
}

/** `math/shares.rs::supply_shares_to_assets_down`. */
export function supplySharesToAssetsDown(shares: bigint, totalShareSupply: bigint, lenderTotalAssetsNow: bigint): bigint {
  if (totalShareSupply === 0n) return 0n;
  return mulDivFloor(shares, lenderTotalAssetsNow, totalShareSupply);
}

export interface CollateralValuation {
  collateralValue: bigint;
}

export interface DebtValuation {
  debtValue: bigint;
}

export interface HealthSnapshot {
  borrowPower: bigint;
  liquidationCollateralValue: bigint;
  totalDebtValue: bigint;
  borrowHealthFactorWad: bigint;
  liquidationHealthFactorWad: bigint;
}

function healthFactorWad(numerator: bigint, denominator: bigint): bigint {
  if (denominator === 0n) return U128_MAX;
  return mulDivFloor(numerator, WAD, denominator);
}

/** `math/health.rs::calculate_health`. */
export function calculateHealth(collaterals: CollateralValuation[], debts: DebtValuation[]): HealthSnapshot {
  let totalCollateralValue = 0n;
  for (const c of collaterals) {
    totalCollateralValue += c.collateralValue;
  }
  let totalDebtValue = 0n;
  for (const d of debts) totalDebtValue += d.debtValue;

  return {
    // Compatibility names retained for existing CLI consumers. Vanna's canonical risk model
    // uses the same raw collateral total for borrow and liquidation health.
    borrowPower: totalCollateralValue,
    liquidationCollateralValue: totalCollateralValue,
    totalDebtValue,
    borrowHealthFactorWad: healthFactorWad(totalCollateralValue, totalDebtValue),
    liquidationHealthFactorWad: healthFactorWad(totalCollateralValue, totalDebtValue),
  };
}

/** Formats a WAD-scaled (1e18) health factor as a short decimal string, or "∞" for no debt. */
export function formatHealthFactorWad(wad: bigint): string {
  if (wad === U128_MAX) return "∞ (no debt)";
  const whole = wad / WAD;
  const frac = (wad % WAD) * 10000n / WAD;
  return `${whole}.${frac.toString().padStart(4, "0")}`;
}

/** Formats a nano-USD value (1e9 per dollar, see `USD_VALUE_DECIMALS`) as a `$` string. */
export function formatUsd(nanoUsd: bigint): string {
  const scale = pow10(USD_VALUE_DECIMALS);
  const whole = nanoUsd / scale;
  const frac = (nanoUsd % scale) * 100n / scale;
  return `$${whole}.${frac.toString().padStart(2, "0")}`;
}

/** Formats a raw token amount as a human decimal string for the given mint decimals. */
export function formatTokenAmount(raw: bigint, decimals: number): string {
  const scale = pow10(decimals);
  const whole = raw / scale;
  const frac = raw % scale;
  return frac === 0n ? whole.toString() : `${whole}.${frac.toString().padStart(decimals, "0").replace(/0+$/, "")}`;
}
