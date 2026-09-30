export const WAD = 10n ** 18n;
export const BALANCE_TO_BORROW_THRESHOLD_WAD = 1_100_000_000_000_000_000n;
export const USD_VALUE_DECIMALS = 9;
export const BASIS_POINTS = 10_000n;
export const SECONDS_PER_YEAR = 31_556_952n;
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

export interface RateCurve {
  linearCoeffWad: bigint;
  jumpCoeffWad: bigint;
  rateMultiplierWad: bigint;
}

export function utilizationWad(accountedLiquidityAssets: bigint, totalBorrowAssets: bigint): bigint {
  const gross = accountedLiquidityAssets + totalBorrowAssets;
  if (gross === 0n) return 0n;
  return mulDivFloor(totalBorrowAssets, WAD, gross);
}

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

export function formatHealthFactorWad(wad: bigint): string {
  if (wad === U128_MAX) return "∞ (no debt)";
  const whole = wad / WAD;
  const frac = (wad % WAD) * 10000n / WAD;
  return `${whole}.${frac.toString().padStart(4, "0")}`;
}

export function formatUsd(nanoUsd: bigint): string {
  const scale = pow10(USD_VALUE_DECIMALS);
  const whole = nanoUsd / scale;
  const frac = (nanoUsd % scale) * 100n / scale;
  return `$${whole}.${frac.toString().padStart(2, "0")}`;
}

export function formatTokenAmount(raw: bigint, decimals: number): string {
  const scale = pow10(decimals);
  const whole = raw / scale;
  const frac = raw % scale;
  return frac === 0n ? whole.toString() : `${whole}.${frac.toString().padStart(decimals, "0").replace(/0+$/, "")}`;
}
