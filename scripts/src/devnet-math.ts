/**
 * BigInt replicas of the on-chain math in `programs/vanna_lending/src/math/*.rs`, for display only
 * (live health factor, current debt, APRs). The program re-validates everything it executes, so if
 * this ever disagrees with it, the program is right.
 */

export const WAD = 10n ** 18n;
export const BALANCE_TO_BORROW_THRESHOLD_WAD = 1_100_000_000_000_000_000n;
export const USD_VALUE_DECIMALS = 9;
export const BASIS_POINTS = 10_000n;
export const SECONDS_PER_YEAR = 31_556_952n;
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

/**
 * `math/health.rs::normalize_token_value`
 * value = token_amount * price * 10^(price_exponent - token_decimals + USD_VALUE_DECIMALS)
 */
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

/** `state/reserve.rs::RateCurve` — WAD-scaled borrow-rate curve coefficients. */
export interface RateCurve {
  linearCoeffWad: bigint;
  jumpCoeffWad: bigint;
  rateMultiplierWad: bigint;
}

/**
 * `math/interest.rs::utilization_wad`
 * utilization = total_borrows / (liquidity + total_borrows)
 */
export function utilizationWad(accountedLiquidityAssets: bigint, totalBorrowAssets: bigint): bigint {
  const gross = accountedLiquidityAssets + totalBorrowAssets;
  if (gross === 0n) return 0n;
  return mulDivFloor(totalBorrowAssets, WAD, gross);
}

/** `math/interest.rs::wad_pow` — square-and-multiply, each step rounded half-up. */
function wadPow(base: bigint, exp: number): bigint {
  if (base === 0n) return exp === 0 ? WAD : 0n;
  const half = WAD / 2n;
  let result = exp % 2 === 1 ? base : WAD;
  for (let e = Math.floor(exp / 2); e > 0; e = Math.floor(e / 2)) {
    base = (base * base + half) / WAD;
    if (e % 2 === 1) result = (result * base + half) / WAD;
  }
  return result;
}

/**
 * `math/interest.rs::borrow_rate_per_second_wad`
 * borrow_rate = rate_multiplier * (u * linear_coeff + u^32 * linear_coeff + u^64 * jump_coeff) / SECONDS_PER_YEAR
 */
export function borrowRatePerSecondWad(curve: RateCurve, utilWad: bigint): bigint {
  const polynomial =
    mulDivFloor(utilWad, curve.linearCoeffWad, WAD) +
    mulDivFloor(wadPow(utilWad, 32), curve.linearCoeffWad, WAD) +
    mulDivFloor(wadPow(utilWad, 64), curve.jumpCoeffWad, WAD);
  return mulDivFloor(curve.rateMultiplierWad, polynomial, SECONDS_PER_YEAR * WAD);
}

export interface ReserveLike {
  accountedLiquidityAssets: bigint;
  totalBorrowAssets: bigint;
  accruedProtocolFees: bigint;
  borrowIndexWad: bigint;
  lastUpdateTimestamp: bigint;
  rateCurve: RateCurve;
  reserveFactorBps: number;
}

export interface AccrualResult {
  newTotalBorrowAssets: bigint;
  newAccruedProtocolFees: bigint;
  newBorrowIndexWad: bigint;
  interestAccrued: bigint;
}

/**
 * `math/interest.rs::accrue`, projected to `now`. On-chain fields are only current as of
 * `last_update_timestamp`; this brings them up to date without a transaction.
 * interest = ceil(total_borrows * borrow_rate * elapsed), protocol_fee = interest * reserve_factor
 */
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

  const util = utilizationWad(reserve.accountedLiquidityAssets, reserve.totalBorrowAssets);
  const growthWad = borrowRatePerSecondWad(reserve.rateCurve, util) * elapsed;
  const interest = mulDivCeil(reserve.totalBorrowAssets, growthWad, WAD);

  const protocolFee = mulDivFloor(interest, BigInt(reserve.reserveFactorBps), BASIS_POINTS);
  const newTotalBorrowAssets = reserve.totalBorrowAssets + interest;
  const newAccruedProtocolFees = reserve.accruedProtocolFees + protocolFee;
  let newBorrowIndexWad = mulDivFloor(reserve.borrowIndexWad, newTotalBorrowAssets, reserve.totalBorrowAssets);
  if (newBorrowIndexWad < WAD) newBorrowIndexWad = WAD;

  return { newTotalBorrowAssets, newAccruedProtocolFees, newBorrowIndexWad, interestAccrued: interest };
}

/**
 * `math/shares.rs::debt_shares_to_assets_up`
 * debt = ceil(borrow_shares * total_borrows / total_borrow_shares)
 */
export function debtSharesToAssetsUp(borrowShares: bigint, totalBorrowShares: bigint, totalBorrowAssets: bigint): bigint {
  if (totalBorrowShares === 0n) return 0n;
  return mulDivCeil(borrowShares, totalBorrowAssets, totalBorrowShares);
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

function healthFactorWad(collateralValue: bigint, debtValue: bigint): bigint {
  if (debtValue === 0n) return U128_MAX;
  return mulDivFloor(collateralValue, WAD, debtValue);
}

/**
 * `math/health.rs::calculate_health`
 * health_factor = sum(collateral_usd) / sum(debt_usd); healthy when debt == 0 or health_factor > 1.10
 */
export function calculateHealth(collaterals: CollateralValuation[], debts: DebtValuation[]): HealthSnapshot {
  const totalCollateralValue = collaterals.reduce((sum, c) => sum + c.collateralValue, 0n);
  const totalDebtValue = debts.reduce((sum, d) => sum + d.debtValue, 0n);

  return {
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
