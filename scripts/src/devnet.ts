#!/usr/bin/env node
/**
 * One CLI for every vanna_lending instruction against a local Surfpool mainnet fork.
 *
 * Usage:
 *   npx tsx src/devnet.ts <command> [--flag value ...]
 *   npm run devnet -- <command> [--flag value ...]
 *
 * Run with no command (or an unrecognized one) to print the full command list.
 */
import * as anchor from "@coral-xyz/anchor";
import { ASSOCIATED_TOKEN_PROGRAM_ID, TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { PublicKey, SystemProgram } from "@solana/web3.js";
import { ata, optionalArg, parseArgs, requireArg, toBaseUnits, toBigInt, tokenBalance } from "./devnet-cli";
import {
  AssetKey,
  ASSET_DECIMALS,
  ASSET_MINTS,
  assetKeyFromString,
  devnetConnection,
  feedIdToBytes,
  loadKeypair,
  log,
  programAs,
  PYTH_FEED_IDS,
  tokenProgramFor,
} from "./devnet-env";
import {
  accrue,
  BALANCE_TO_BORROW_THRESHOLD_WAD,
  calculateHealth,
  debtSharesToAssetsUp,
  formatHealthFactorWad,
  formatTokenAmount,
  formatUsd,
  borrowRatePerSecondWad,
  normalizeTokenValue,
  RateCurve,
  ReserveLike,
  SECONDS_PER_YEAR,
  utilizationWad,
  WAD,
} from "./devnet-math";
import { AssetIndexInfo, buildRemainingAccounts, EMPTY_ASSET_INDEX, getAssetIndexMap } from "./devnet-positions";
import { fetchLivePrice, refreshPrice } from "./devnet-pyth";
import { assetConfigPda, debtPositionPda, marginPda, protocolConfigPda, reservePda, shareMintPda } from "./pda";

const U128_MAX = new anchor.BN("340282366920938463463374607431768211455");
const MODE_NAMES = ["Normal", "BorrowPaused", "WithdrawOnly", "Halted"];
const RESERVE_STATUS_NAMES = ["Active", "SupplyOnly", "RepayOnly", "Frozen"];
const RISK_DEFAULTS: Record<AssetKey, { ltv: number; liqThreshold: number; liqBonus: number }> = {
  usdc: { ltv: 8000, liqThreshold: 8500, liqBonus: 500 },
  wsol: { ltv: 7000, liqThreshold: 8000, liqBonus: 500 },
  tslax: { ltv: 5500, liqThreshold: 6500, liqBonus: 700 },
  googlx: { ltv: 6000, liqThreshold: 7000, liqBonus: 700 },
  aaplx: { ltv: 5500, liqThreshold: 6500, liqBonus: 700 },
  anthropic: { ltv: 5500, liqThreshold: 6500, liqBonus: 700 },
  openai: { ltv: 5500, liqThreshold: 6500, liqBonus: 700 },
};

interface Ctx {
  args: Record<string, string>;
  conn: anchor.web3.Connection;
  wallet: anchor.web3.Keypair;
  anchorWallet: anchor.Wallet;
  program: anchor.Program;
}

/** Refreshes real Pyth prices for every registered asset — needed by any instruction that scans
 * every active position on a margin account (borrow, withdraw-collateral, liquidate). */
async function refreshAllPrices(ctx: Ctx): Promise<Record<AssetKey, PublicKey>> {
  log("refreshing Pyth prices", "usdc + wsol + tslax + googlx + aaplx + anthropic + openai (including Kamino collateral)");
  return {
    usdc: await refreshPrice(ctx.conn, ctx.anchorWallet, "usdc"),
    wsol: await refreshPrice(ctx.conn, ctx.anchorWallet, "wsol"),
    tslax: await refreshPrice(ctx.conn, ctx.anchorWallet, "tslax"),
    googlx: await refreshPrice(ctx.conn, ctx.anchorWallet, "googlx"),
    aaplx: await refreshPrice(ctx.conn, ctx.anchorWallet, "aaplx"),
    anthropic: await refreshPrice(ctx.conn, ctx.anchorWallet, "anthropic"),
    openai: await refreshPrice(ctx.conn, ctx.anchorWallet, "openai"),
  };
}

/** Generic Anchor-account fetch by camelCase namespace (e.g. "protocolConfig", "reserve"). */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
async function fetchAccount(program: anchor.Program, name: string, address: PublicKey): Promise<any> {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (program.account as Record<string, { fetch(a: PublicKey): Promise<any> }>)[name].fetch(address);
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
function rateCurveFromAccount(curve: any): RateCurve {
  return {
    linearCoeffWad: toBigInt(curve.linearCoeffWad),
    jumpCoeffWad: toBigInt(curve.jumpCoeffWad),
    rateMultiplierWad: toBigInt(curve.rateMultiplierWad),
  };
}

/** Rate-curve CLI flags as decimal coefficients (e.g. `--rate-multiplier 3.5`), converted to WAD.
 * Defaults match the EVM deployment (`new DefaultRateModel(1e16, 3e17, 35e17, 31556952e18)`):
 * ~1.75% APR at 50% utilization, ~2.8% at 80%, ~7.9% at 95%, 112% at 100%. */
function rateCurveFromArgs(args: Record<string, string>) {
  return {
    linearCoeffWad: toBaseUnits(optionalArg(args, "linear-coeff", "0.01"), 18),
    jumpCoeffWad: toBaseUnits(optionalArg(args, "jump-coeff", "0.3"), 18),
    rateMultiplierWad: toBaseUnits(optionalArg(args, "rate-multiplier", "3.5"), 18),
  };
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
function reserveFromAccount(reserveAcc: any): ReserveLike {
  return {
    accountedLiquidityAssets: toBigInt(reserveAcc.accountedLiquidityAssets),
    totalBorrowAssets: toBigInt(reserveAcc.totalBorrowAssets),
    accruedProtocolFees: toBigInt(reserveAcc.accruedProtocolFees),
    borrowIndexWad: toBigInt(reserveAcc.borrowIndexWad),
    lastUpdateTimestamp: toBigInt(reserveAcc.lastUpdateTimestamp),
    rateCurve: rateCurveFromAccount(reserveAcc.rateCurve),
    reserveFactorBps: reserveAcc.reserveFactorBps,
  };
}

/** Projects a fetched `Reserve` account's interest accrual to now; on-chain fields are only
 * current as of `last_update_timestamp`. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
function liveAccrual(reserveAcc: any) {
  return accrue(reserveFromAccount(reserveAcc), BigInt(Math.floor(Date.now() / 1000)));
}

/** Live view of one wallet's positions (balances, debt after interest, USD values) and the
 * health snapshot the program would compute, for display only. */
async function computePositionHealth(program: anchor.Program, conn: anchor.web3.Connection, owner: PublicKey) {
  const [margin] = marginPda(owner);
  const marginAcc = await fetchAccount(program, "marginAccount", margin);
  const indexMap = await getAssetIndexMap(program);
  const byIndex = new Map<number, AssetIndexInfo>();
  for (const info of Object.values(indexMap)) byIndex.set(info.index, info);

  const collateralBreakdown: Array<{ asset: AssetKey; amountRaw: bigint; valueUsd: bigint; ltvBps: number; liquidationThresholdBps: number }> = [];
  for (const idx of marginAcc.collateralAssetIndexes as number[]) {
    if (idx === EMPTY_ASSET_INDEX) continue;
    const info = byIndex.get(idx);
    if (!info) continue;
    const vaultBalance: bigint = await tokenBalance(conn, ata(margin, info.mint)).catch(() => 0n);
    const livePrice = await fetchLivePrice(info.key);
    const assetConfigAcc = await fetchAccount(program, "assetConfig", info.assetConfig);
    const valueUsd = normalizeTokenValue(vaultBalance, livePrice.price, livePrice.exponent, ASSET_DECIMALS[info.key], false);
    collateralBreakdown.push({
      asset: info.key,
      amountRaw: vaultBalance,
      valueUsd,
      ltvBps: assetConfigAcc.ltvBps,
      liquidationThresholdBps: assetConfigAcc.liquidationThresholdBps,
    });
  }

  const debtBreakdown: Array<{ asset: AssetKey; amountRaw: bigint; valueUsd: bigint }> = [];
  for (const idx of marginAcc.debtAssetIndexes as number[]) {
    if (idx === EMPTY_ASSET_INDEX) continue;
    const info = byIndex.get(idx);
    if (!info) continue;
    const [reserve] = reservePda(info.mint);
    const [debtPosition] = debtPositionPda(margin, reserve);
    const reserveAcc = await fetchAccount(program, "reserve", reserve);
    const debtAcc = await fetchAccount(program, "debtPosition", debtPosition);
    const live = liveAccrual(reserveAcc);
    const currentDebtRaw = debtSharesToAssetsUp(toBigInt(debtAcc.borrowShares), toBigInt(reserveAcc.totalBorrowShares), live.newTotalBorrowAssets);
    const livePrice = await fetchLivePrice(info.key);
    const valueUsd = normalizeTokenValue(currentDebtRaw, livePrice.price, livePrice.exponent, ASSET_DECIMALS[info.key], true);
    debtBreakdown.push({ asset: info.key, amountRaw: currentDebtRaw, valueUsd });
  }

  const collaterals = collateralBreakdown.map((c) => ({ collateralValue: c.valueUsd }));
  const debts = debtBreakdown.map((d) => ({ debtValue: d.valueUsd }));
  const health = calculateHealth(collaterals, debts);

  return { margin, collateralBreakdown, debtBreakdown, health };
}

const COMMANDS: Record<string, (ctx: Ctx) => Promise<void>> = {
  // -- one-time protocol setup --------------------------------------------------------------------
  "initialize-protocol": async ({ args, wallet, program }) => {
    // `admin` must sign, so the loaded wallet becomes admin — there is no `--admin` override.
    const treasury = new PublicKey(optionalArg(args, "treasury", wallet.publicKey.toBase58()));
    const maxAssets = Number(optionalArg(args, "max-assets", "24")); // matches MAX_ASSETS in constants.rs
    const [protocolConfig] = protocolConfigPda();
    const sig = await program.methods
      .initializeProtocol(treasury, maxAssets)
      .accounts({ payer: wallet.publicKey, admin: wallet.publicKey, protocolConfig, systemProgram: SystemProgram.programId })
      .rpc();
    log("initialize_protocol", `protocolConfig=${protocolConfig.toBase58()} admin=${wallet.publicKey.toBase58()} tx=${sig}`);
  },

  "register-asset": async ({ args, wallet, program }) => {
    const asset = assetKeyFromString(requireArg(args, "asset"));
    const mint = ASSET_MINTS[asset];
    const decimals = ASSET_DECIMALS[asset];
    const d = RISK_DEFAULTS[asset];
    const ltvBps = Number(optionalArg(args, "ltv-bps", String(d.ltv)));
    const liqThresholdBps = Number(optionalArg(args, "liq-threshold-bps", String(d.liqThreshold)));
    const liqBonusBps = Number(optionalArg(args, "liq-bonus-bps", String(d.liqBonus)));
    const maxConfidenceBps = Number(optionalArg(args, "max-confidence-bps", "1000"));
    const maxPriceAgeSecs = Number(optionalArg(args, "max-price-age-secs", "3600"));
    const maxCollateral = toBaseUnits(optionalArg(args, "max-collateral", "0"), decimals);
    const collateralEnabled = optionalArg(args, "collateral-enabled", "true") === "true";
    const borrowEnabled = optionalArg(args, "borrow-enabled", "true") === "true";
    const [protocolConfig] = protocolConfigPda();
    const [assetConfig] = assetConfigPda(mint);
    const sig = await program.methods
      .adminRegisterAsset(
        feedIdToBytes(PYTH_FEED_IDS[asset]),
        maxCollateral,
        ltvBps,
        liqThresholdBps,
        liqBonusBps,
        maxConfidenceBps,
        maxPriceAgeSecs,
        collateralEnabled,
        borrowEnabled,
      )
      .accounts({
        admin: wallet.publicKey,
        payer: wallet.publicKey,
        protocolConfig,
        underlyingMint: mint,
        assetConfig,
        tokenProgram: tokenProgramFor(asset),
        systemProgram: SystemProgram.programId,
      })
      .rpc();
    log("admin_register_asset", `${asset} assetConfig=${assetConfig.toBase58()} ltv=${ltvBps}bps tx=${sig}`);
  },

  "initialize-reserve": async ({ args, wallet, program }) => {
    const asset = assetKeyFromString(requireArg(args, "asset"));
    const mint = ASSET_MINTS[asset];
    const decimals = ASSET_DECIMALS[asset];
    const rateCurve = rateCurveFromArgs(args);
    const reserveFactorBps = Number(optionalArg(args, "reserve-factor-bps", "1000"));
    const supplyCap = toBaseUnits(optionalArg(args, "supply-cap", "0"), decimals);
    const borrowCap = toBaseUnits(optionalArg(args, "borrow-cap", "0"), decimals);
    const status = Number(optionalArg(args, "status", "0"));
    const [protocolConfig] = protocolConfigPda();
    const [assetConfig] = assetConfigPda(mint);
    const [reserve] = reservePda(mint);
    const [shareMint] = shareMintPda(mint);
    const sig = await program.methods
      .adminInitializeReserve(rateCurve, reserveFactorBps, supplyCap, borrowCap, status)
      .accounts({
        admin: wallet.publicKey,
        payer: wallet.publicKey,
        protocolConfig,
        assetConfig,
        underlyingMint: mint,
        reserve,
        liquidityVault: ata(reserve, mint, tokenProgramFor(asset)),
        shareMint,
        tokenProgram: tokenProgramFor(asset),
        shareTokenProgram: TOKEN_PROGRAM_ID,
        associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .rpc();
    log("admin_initialize_reserve", `${asset} reserve=${reserve.toBase58()} tx=${sig}`);
  },

  // -- governance ---------------------------------------------------------------------------------
  "propose-authority": async ({ args, wallet, program }) => {
    const newAdmin = new PublicKey(requireArg(args, "new-admin"));
    const [protocolConfig] = protocolConfigPda();
    const sig = await program.methods.adminProposeAuthority(newAdmin).accounts({ admin: wallet.publicKey, protocolConfig }).rpc();
    log("admin_propose_authority", `pending_admin=${newAdmin.toBase58()} tx=${sig}`);
  },

  "accept-admin": async ({ wallet, program }) => {
    const [protocolConfig] = protocolConfigPda();
    const sig = await program.methods.authorityAcceptAdmin().accounts({ pendingAdmin: wallet.publicKey, protocolConfig }).rpc();
    log("authority_accept_admin", `new_admin=${wallet.publicKey.toBase58()} tx=${sig}`);
  },

  "set-operating-mode": async ({ args, wallet, program }) => {
    const mode = Number(requireArg(args, "mode"));
    if (!(mode in MODE_NAMES)) throw new Error(`--mode must be 0-3, got ${mode}`);
    const [protocolConfig] = protocolConfigPda();
    const sig = await program.methods.adminSetOperatingMode(mode).accounts({ admin: wallet.publicKey, protocolConfig }).rpc();
    log("admin_set_operating_mode", `mode=${mode} (${MODE_NAMES[mode]}) tx=${sig}`);
  },

  "update-asset-config": async ({ args, wallet, program }) => {
    const asset = assetKeyFromString(requireArg(args, "asset"));
    const mint = ASSET_MINTS[asset];
    const decimals = ASSET_DECIMALS[asset];
    const d = RISK_DEFAULTS[asset];
    const ltvBps = Number(optionalArg(args, "ltv-bps", String(d.ltv)));
    const liqThresholdBps = Number(optionalArg(args, "liq-threshold-bps", String(d.liqThreshold)));
    const liqBonusBps = Number(optionalArg(args, "liq-bonus-bps", String(d.liqBonus)));
    const maxConfidenceBps = Number(optionalArg(args, "max-confidence-bps", "1000"));
    const maxPriceAgeSecs = Number(optionalArg(args, "max-price-age-secs", "3600"));
    const maxCollateral = toBaseUnits(optionalArg(args, "max-collateral", "0"), decimals);
    const collateralEnabled = optionalArg(args, "collateral-enabled", "true") === "true";
    const borrowEnabled = optionalArg(args, "borrow-enabled", "true") === "true";
    const [protocolConfig] = protocolConfigPda();
    const [assetConfig] = assetConfigPda(mint);
    const sig = await program.methods
      .adminUpdateAssetConfig(maxCollateral, ltvBps, liqThresholdBps, liqBonusBps, maxConfidenceBps, maxPriceAgeSecs, collateralEnabled, borrowEnabled)
      .accounts({ admin: wallet.publicKey, protocolConfig, assetConfig })
      .rpc();
    log("admin_update_asset_config", `${asset} ltv=${ltvBps}bps liq_threshold=${liqThresholdBps}bps tx=${sig}`);
  },

  "update-reserve-config": async ({ args, wallet, program }) => {
    const asset = assetKeyFromString(requireArg(args, "asset"));
    const mint = ASSET_MINTS[asset];
    const decimals = ASSET_DECIMALS[asset];
    const rateCurve = rateCurveFromArgs(args);
    const reserveFactorBps = Number(optionalArg(args, "reserve-factor-bps", "1000"));
    const supplyCap = toBaseUnits(optionalArg(args, "supply-cap", "0"), decimals);
    const borrowCap = toBaseUnits(optionalArg(args, "borrow-cap", "0"), decimals);
    const status = Number(optionalArg(args, "status", "0"));
    const [protocolConfig] = protocolConfigPda();
    const [reserve] = reservePda(mint);
    const sig = await program.methods
      .adminUpdateReserveConfig(rateCurve, reserveFactorBps, supplyCap, borrowCap, status)
      .accounts({ admin: wallet.publicKey, protocolConfig, underlyingMint: mint, reserve })
      .rpc();
    log("admin_update_reserve_config", `${asset} status=${status} tx=${sig}`);
  },

  "collect-protocol-fees": async ({ args, wallet, program }) => {
    const asset = assetKeyFromString(requireArg(args, "asset"));
    const mint = ASSET_MINTS[asset];
    const decimals = ASSET_DECIMALS[asset];
    const amount = toBaseUnits(requireArg(args, "amount"), decimals);
    const [protocolConfig] = protocolConfigPda();
    const [reserve] = reservePda(mint);
    const protocolConfigAccount = await fetchAccount(program, "protocolConfig", protocolConfig);
    const treasuryAta = ata(protocolConfigAccount.treasury, mint);
    const sig = await program.methods
      .adminCollectProtocolFees(amount)
      .accounts({
        admin: wallet.publicKey,
        protocolConfig,
        underlyingMint: mint,
        reserve,
        liquidityVault: ata(reserve, mint),
        treasuryAta,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .rpc();
    log("admin_collect_protocol_fees", `${asset} amount=${amount.toString()} treasuryAta=${treasuryAta.toBase58()} tx=${sig}`);
  },

  // -- lender -------------------------------------------------------------------------------------
  "supply-liquidity": async ({ args, wallet, program }) => {
    const asset = assetKeyFromString(requireArg(args, "asset"));
    const mint = ASSET_MINTS[asset];
    const decimals = ASSET_DECIMALS[asset];
    const amount = toBaseUnits(requireArg(args, "amount"), decimals);
    const minShares = new anchor.BN(optionalArg(args, "min-shares", "1"));
    const [protocolConfig] = protocolConfigPda();
    const [assetConfig] = assetConfigPda(mint);
    const [reserve] = reservePda(mint);
    const [shareMint] = shareMintPda(mint);
    const sig = await program.methods
      .lenderSupply(amount, minShares)
      .accounts({
        lender: wallet.publicKey,
        protocolConfig,
        assetConfig,
        reserve,
        underlyingMint: mint,
        lenderTokenAccount: ata(wallet.publicKey, mint, tokenProgramFor(asset)),
        liquidityVault: ata(reserve, mint, tokenProgramFor(asset)),
        shareMint,
        lenderShareAccount: ata(wallet.publicKey, shareMint),
        tokenProgram: tokenProgramFor(asset),
        associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .rpc();
    log("lender_supply", `${asset} amount=${amount.toString()} tx=${sig}`);
  },

  "redeem-liquidity": async ({ args, wallet, program }) => {
    const asset = assetKeyFromString(requireArg(args, "asset"));
    const mint = ASSET_MINTS[asset];
    const shares = new anchor.BN(requireArg(args, "shares"));
    const minAssets = new anchor.BN(optionalArg(args, "min-assets", "1"));
    const [protocolConfig] = protocolConfigPda();
    const [assetConfig] = assetConfigPda(mint);
    const [reserve] = reservePda(mint);
    const [shareMint] = shareMintPda(mint);
    const sig = await program.methods
      .lenderRedeem(shares, minAssets)
      .accounts({
        lender: wallet.publicKey,
        protocolConfig,
        assetConfig,
        reserve,
        underlyingMint: mint,
        lenderTokenAccount: ata(wallet.publicKey, mint, tokenProgramFor(asset)),
        liquidityVault: ata(reserve, mint, tokenProgramFor(asset)),
        shareMint,
        lenderShareAccount: ata(wallet.publicKey, shareMint),
        tokenProgram: tokenProgramFor(asset),
      })
      .rpc();
    log("lender_redeem", `${asset} shares=${shares.toString()} tx=${sig}`);
  },

  "refresh-reserve": async ({ args, program }) => {
    const asset = assetKeyFromString(requireArg(args, "asset"));
    const mint = ASSET_MINTS[asset];
    const [reserve] = reservePda(mint);
    const sig = await program.methods.publicRefreshReserve().accounts({ reserve }).rpc();
    const reserveAccount = await fetchAccount(program, "reserve", reserve);
    log("public_refresh_reserve", `${asset} total_borrow_assets=${reserveAccount.totalBorrowAssets.toString()} tx=${sig}`);
  },

  // -- margin lifecycle ---------------------------------------------------------------------------
  "create-margin": async ({ wallet, program }) => {
    const [margin] = marginPda(wallet.publicKey);
    const sig = await program.methods
      .userCreateMargin()
      .accounts({ authority: wallet.publicKey, payer: wallet.publicKey, marginAccount: margin, systemProgram: SystemProgram.programId })
      .rpc();
    log("user_create_margin", `margin=${margin.toBase58()} tx=${sig}`);
  },

  "close-margin": async ({ wallet, program }) => {
    const [margin] = marginPda(wallet.publicKey);
    const sig = await program.methods.userCloseMargin().accounts({ authority: wallet.publicKey, marginAccount: margin }).rpc();
    log("user_close_margin", `margin=${margin.toBase58()} rent reclaimed tx=${sig}`);
  },

  "deposit-collateral": async ({ args, wallet, program }) => {
    const asset = assetKeyFromString(requireArg(args, "asset"));
    const mint = ASSET_MINTS[asset];
    const decimals = ASSET_DECIMALS[asset];
    const amount = toBaseUnits(requireArg(args, "amount"), decimals);
    const [protocolConfig] = protocolConfigPda();
    const [margin] = marginPda(wallet.publicKey);
    const [assetConfig] = assetConfigPda(mint);
    const tp = tokenProgramFor(asset);
    const marginVault = ata(margin, mint, tp);
    const sig = await program.methods
      .userDepositCollateral(amount)
      .accounts({
        authority: wallet.publicKey,
        protocolConfig,
        marginAccount: margin,
        assetConfig,
        mint,
        sourceTokenAccount: ata(wallet.publicKey, mint, tp),
        marginVault,
        tokenProgram: tp,
        associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .rpc();
    log("user_deposit_collateral", `${asset} amount=${amount.toString()} marginVault=${marginVault.toBase58()} tx=${sig}`);
  },

  "withdraw-collateral": async (ctx) => {
    const { args, wallet, program } = ctx;
    const asset = assetKeyFromString(requireArg(args, "asset"));
    const mint = ASSET_MINTS[asset];
    const decimals = ASSET_DECIMALS[asset];
    const amount = toBaseUnits(requireArg(args, "amount"), decimals);
    const minHealthFactor = new anchor.BN(optionalArg(args, "min-health-factor", "0"));
    const [protocolConfig] = protocolConfigPda();
    const [margin] = marginPda(wallet.publicKey);
    const [assetConfig] = assetConfigPda(mint);
    const tp = tokenProgramFor(asset);
    const marginVault = ata(margin, mint, tp);

    const priceAccounts = await refreshAllPrices(ctx);
    const remainingAccounts = await buildRemainingAccounts(program, margin, priceAccounts, { excludeCollateral: asset });

    const sig = await program.methods
      .userWithdrawCollateral(amount, minHealthFactor)
      .accounts({
        authority: wallet.publicKey,
        protocolConfig,
        marginAccount: margin,
        assetConfig,
        mint,
        priceUpdate: priceAccounts[asset],
        destinationTokenAccount: ata(wallet.publicKey, mint, tp),
        marginVault,
        tokenProgram: tp,
      })
      .remainingAccounts(remainingAccounts)
      .rpc();
    log("user_withdraw_collateral", `${asset} amount=${amount.toString()} tx=${sig}`);
  },

  "close-collateral-position": async ({ args, wallet, program }) => {
    const asset = assetKeyFromString(requireArg(args, "asset"));
    const mint = ASSET_MINTS[asset];
    const [margin] = marginPda(wallet.publicKey);
    const [assetConfig] = assetConfigPda(mint);
    const marginVault = ata(margin, mint);
    const sig = await program.methods
      .userCloseCollateralPosition()
      .accounts({ authority: wallet.publicKey, marginAccount: margin, assetConfig, mint, marginVault, tokenProgram: TOKEN_PROGRAM_ID })
      .rpc();
    log("user_close_collateral_position", `${asset} vault=${marginVault.toBase58()} rent reclaimed tx=${sig}`);
  },

  // -- borrow/repay -------------------------------------------------------------------------------
  "open-debt-position": async ({ args, wallet, program }) => {
    const asset = assetKeyFromString(requireArg(args, "asset"));
    const mint = ASSET_MINTS[asset];
    const [margin] = marginPda(wallet.publicKey);
    const [assetConfig] = assetConfigPda(mint);
    const [reserve] = reservePda(mint);
    const [debtPosition] = debtPositionPda(margin, reserve);
    const sig = await program.methods
      .userOpenDebtPosition()
      .accounts({
        authority: wallet.publicKey,
        payer: wallet.publicKey,
        marginAccount: margin,
        assetConfig,
        reserve,
        debtPosition,
        systemProgram: SystemProgram.programId,
      })
      .rpc();
    log("user_open_debt_position", `${asset} debtPosition=${debtPosition.toBase58()} tx=${sig}`);
  },

  "close-debt-position": async ({ args, wallet, program }) => {
    const asset = assetKeyFromString(requireArg(args, "asset"));
    const mint = ASSET_MINTS[asset];
    const [margin] = marginPda(wallet.publicKey);
    const [assetConfig] = assetConfigPda(mint);
    const [reserve] = reservePda(mint);
    const [debtPosition] = debtPositionPda(margin, reserve);
    const sig = await program.methods
      .userCloseDebtPosition()
      .accounts({ authority: wallet.publicKey, marginAccount: margin, assetConfig, reserve, debtPosition })
      .rpc();
    log("user_close_debt_position", `${asset} debtPosition=${debtPosition.toBase58()} rent reclaimed tx=${sig}`);
  },

  borrow: async (ctx) => {
    const { args, wallet, program } = ctx;
    const asset = assetKeyFromString(requireArg(args, "asset"));
    const mint = ASSET_MINTS[asset];
    const decimals = ASSET_DECIMALS[asset];
    const amount = toBaseUnits(requireArg(args, "amount"), decimals);
    const maxDebtShares = args["max-debt-shares"] ? new anchor.BN(args["max-debt-shares"]) : U128_MAX;
    const [protocolConfig] = protocolConfigPda();
    const [margin] = marginPda(wallet.publicKey);
    const [assetConfig] = assetConfigPda(mint);
    const [reserve] = reservePda(mint);
    const [debtPosition] = debtPositionPda(margin, reserve);
    const marginVault = ata(margin, mint);

    const priceAccounts = await refreshAllPrices(ctx);
    const remainingAccounts = await buildRemainingAccounts(program, margin, priceAccounts, {
      excludeCollateral: asset,
      excludeDebt: asset,
    });

    const sig = await program.methods
      .userBorrow(amount, maxDebtShares)
      .accounts({
        authority: wallet.publicKey,
        protocolConfig,
        marginAccount: margin,
        assetConfig,
        reserve,
        debtPosition,
        priceUpdate: priceAccounts[asset],
        mint,
        reserveVault: ata(reserve, mint),
        marginVault,
        tokenProgram: TOKEN_PROGRAM_ID,
        associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .remainingAccounts(remainingAccounts)
      .rpc();
    log("user_borrow", `${asset} amount=${amount.toString()} tx=${sig}`);
  },

  "repay-from-margin": async ({ args, wallet, program }) => {
    const asset = assetKeyFromString(requireArg(args, "asset"));
    const mint = ASSET_MINTS[asset];
    const decimals = ASSET_DECIMALS[asset];
    const repayAll = args["repay-all"] === "true";
    const maxAssets = repayAll ? new anchor.BN(0) : toBaseUnits(requireArg(args, "amount"), decimals);
    const [margin] = marginPda(wallet.publicKey);
    const [assetConfig] = assetConfigPda(mint);
    const [reserve] = reservePda(mint);
    const [debtPosition] = debtPositionPda(margin, reserve);
    const sig = await program.methods
      .userRepayFromMargin(maxAssets, repayAll)
      .accounts({
        authority: wallet.publicKey,
        marginAccount: margin,
        assetConfig,
        reserve,
        debtPosition,
        mint,
        marginVault: ata(margin, mint),
        reserveVault: ata(reserve, mint),
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .rpc();
    log("user_repay_from_margin", `${asset} ${repayAll ? "repay_all" : `amount=${maxAssets.toString()}`} tx=${sig}`);
  },

  "repay-from-wallet": async ({ args, wallet, program }) => {
    const asset = assetKeyFromString(requireArg(args, "asset"));
    const mint = ASSET_MINTS[asset];
    const decimals = ASSET_DECIMALS[asset];
    const repayAll = args["repay-all"] === "true";
    const maxAssets = repayAll ? new anchor.BN(0) : toBaseUnits(requireArg(args, "amount"), decimals);
    const marginOwner = new PublicKey(optionalArg(args, "margin-owner", wallet.publicKey.toBase58()));
    const [margin] = marginPda(marginOwner);
    const [assetConfig] = assetConfigPda(mint);
    const [reserve] = reservePda(mint);
    const [debtPosition] = debtPositionPda(margin, reserve);
    const sig = await program.methods
      .publicRepayFromWallet(maxAssets, repayAll)
      .accounts({
        payer: wallet.publicKey,
        marginAccount: margin,
        assetConfig,
        reserve,
        debtPosition,
        mint,
        payerTokenAccount: ata(wallet.publicKey, mint),
        reserveVault: ata(reserve, mint),
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .rpc();
    log(
      "public_repay_from_wallet",
      `${asset} margin_owner=${marginOwner.toBase58()} ${repayAll ? "repay_all" : `amount=${maxAssets.toString()}`} tx=${sig}`,
    );
  },

  // -- liquidation --------------------------------------------------------------------------------
  liquidate: async (ctx) => {
    const { args, wallet, program } = ctx;
    const marginOwner = new PublicKey(requireArg(args, "margin-owner"));
    const debtAsset = assetKeyFromString(requireArg(args, "debt-asset"));
    const collateralAsset = assetKeyFromString(requireArg(args, "collateral-asset"));
    const debtMint = ASSET_MINTS[debtAsset];
    const collateralMint = ASSET_MINTS[collateralAsset];
    const repayAmount = toBaseUnits(requireArg(args, "repay-amount"), ASSET_DECIMALS[debtAsset]);
    const minCollateralOut = new anchor.BN(optionalArg(args, "min-collateral-out", "0"));

    const [margin] = marginPda(marginOwner);
    const [debtAssetConfig] = assetConfigPda(debtMint);
    const [debtReserve] = reservePda(debtMint);
    const [debtPosition] = debtPositionPda(margin, debtReserve);
    const [collateralAssetConfig] = assetConfigPda(collateralMint);

    const priceAccounts = await refreshAllPrices(ctx);
    const remainingAccounts = await buildRemainingAccounts(program, margin, priceAccounts, {
      excludeCollateral: collateralAsset,
      excludeDebt: debtAsset,
    });

    const sig = await program.methods
      .publicLiquidate(repayAmount, minCollateralOut)
      .accounts({
        liquidator: wallet.publicKey,
        marginAccount: margin,
        debtAssetConfig,
        debtReserve,
        debtPosition,
        debtPriceUpdate: priceAccounts[debtAsset],
        debtMint,
        liquidatorDebtSource: ata(wallet.publicKey, debtMint),
        debtReserveVault: ata(debtReserve, debtMint),
        collateralAssetConfig,
        collateralPriceUpdate: priceAccounts[collateralAsset],
        collateralMint,
        liquidatorCollateralDestination: ata(wallet.publicKey, collateralMint),
        collateralMarginVault: ata(margin, collateralMint),
        tokenProgram: TOKEN_PROGRAM_ID,
        associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .remainingAccounts(remainingAccounts)
      .rpc();
    log("public_liquidate", `margin_owner=${marginOwner.toBase58()} repaid ${debtAsset} seized ${collateralAsset} tx=${sig}`);
  },

  // -- read-only views (no transaction, nothing signed) -------------------------------------------
  "get-protocol-config": async ({ program }) => {
    const [protocolConfig] = protocolConfigPda();
    const acc = await fetchAccount(program, "protocolConfig", protocolConfig);
    console.log(
      JSON.stringify(
        {
          address: protocolConfig.toBase58(),
          admin: acc.admin.toBase58(),
          pendingAdmin: acc.pendingAdmin.toBase58(),
          treasury: acc.treasury.toBase58(),
          operatingMode: `${acc.operatingMode} (${MODE_NAMES[acc.operatingMode]})`,
          maxAssetsPerMargin: acc.maxAssetsPerMargin,
          nextAssetIndex: acc.nextAssetIndex,
        },
        null,
        2,
      ),
    );
  },

  "get-asset-config": async ({ args, program }) => {
    const asset = assetKeyFromString(requireArg(args, "asset"));
    const [assetConfig] = assetConfigPda(ASSET_MINTS[asset]);
    const acc = await fetchAccount(program, "assetConfig", assetConfig);
    console.log(
      JSON.stringify(
        {
          address: assetConfig.toBase58(),
          asset,
          mint: acc.mint.toBase58(),
          reserve: acc.reserve.toBase58(),
          assetIndex: acc.assetIndex,
          decimals: acc.decimals,
          ltvBps: acc.ltvBps,
          liquidationThresholdBps: acc.liquidationThresholdBps,
          liquidationBonusBps: acc.liquidationBonusBps,
          maxConfidenceBps: acc.maxConfidenceBps,
          maxPriceAgeSecs: acc.maxPriceAgeSecs,
          maxCollateralPerMargin: acc.maxCollateralPerMargin.toString() === "0" ? "uncapped" : acc.maxCollateralPerMargin.toString(),
          collateralEnabled: acc.collateralEnabled,
          borrowEnabled: acc.borrowEnabled,
        },
        null,
        2,
      ),
    );
  },

  "get-reserve": async ({ args, program }) => {
    const asset = assetKeyFromString(requireArg(args, "asset"));
    const decimals = ASSET_DECIMALS[asset];
    const [reserve] = reservePda(ASSET_MINTS[asset]);
    const acc = await fetchAccount(program, "reserve", reserve);
    const live = liveAccrual(acc);
    const util = utilizationWad(toBigInt(acc.accountedLiquidityAssets), live.newTotalBorrowAssets);
    const borrowAprWad = borrowRatePerSecondWad(rateCurveFromAccount(acc.rateCurve), util) * SECONDS_PER_YEAR;
    const wadToPercent = (v: bigint) => `${(Number((v * 10_000n) / WAD) / 100).toFixed(2)}%`;

    console.log(
      JSON.stringify(
        {
          address: reserve.toBase58(),
          asset,
          status: RESERVE_STATUS_NAMES[acc.status] ?? acc.status,
          accountedLiquidityAssets: formatTokenAmount(toBigInt(acc.accountedLiquidityAssets), decimals),
          totalBorrowAssets_onChain: formatTokenAmount(toBigInt(acc.totalBorrowAssets), decimals),
          totalBorrowAssets_liveNow: formatTokenAmount(live.newTotalBorrowAssets, decimals),
          accruedProtocolFees_liveNow: formatTokenAmount(live.newAccruedProtocolFees, decimals),
          utilization: wadToPercent(util),
          borrowApr: wadToPercent(borrowAprWad),
          supplyCap: acc.supplyCap.toString() === "0" ? "uncapped" : formatTokenAmount(toBigInt(acc.supplyCap), decimals),
          borrowCap: acc.borrowCap.toString() === "0" ? "uncapped" : formatTokenAmount(toBigInt(acc.borrowCap), decimals),
          liquidityVault: acc.liquidityVault.toBase58(),
          shareMint: acc.shareMint.toBase58(),
        },
        null,
        2,
      ),
    );
  },

  "get-margin-account": async ({ args, wallet, program }) => {
    const owner = new PublicKey(optionalArg(args, "owner", wallet.publicKey.toBase58()));
    const [margin] = marginPda(owner);
    const acc = await fetchAccount(program, "marginAccount", margin);
    const indexMap = await getAssetIndexMap(program);
    const byIndex = new Map<number, AssetIndexInfo>();
    for (const info of Object.values(indexMap)) byIndex.set(info.index, info);

    const describe = (indexes: number[]) =>
      indexes.filter((i) => i !== EMPTY_ASSET_INDEX).map((i) => byIndex.get(i)?.key ?? `unknown index ${i}`);

    console.log(
      JSON.stringify(
        {
          address: margin.toBase58(),
          owner: owner.toBase58(),
          status: acc.status,
          activeCollateral: describe(acc.collateralAssetIndexes),
          activeDebt: describe(acc.debtAssetIndexes),
          eventSequence: acc.eventSequence.toString(),
        },
        null,
        2,
      ),
    );
  },

  "get-debt-position": async ({ args, wallet, program }) => {
    const asset = assetKeyFromString(requireArg(args, "asset"));
    const decimals = ASSET_DECIMALS[asset];
    const owner = new PublicKey(optionalArg(args, "owner", wallet.publicKey.toBase58()));
    const [margin] = marginPda(owner);
    const [reserve] = reservePda(ASSET_MINTS[asset]);
    const [debtPosition] = debtPositionPda(margin, reserve);
    const debtAcc = await fetchAccount(program, "debtPosition", debtPosition);
    const reserveAcc = await fetchAccount(program, "reserve", reserve);
    const live = liveAccrual(reserveAcc);
    const currentDebtRaw = debtSharesToAssetsUp(toBigInt(debtAcc.borrowShares), toBigInt(reserveAcc.totalBorrowShares), live.newTotalBorrowAssets);

    console.log(
      JSON.stringify(
        {
          address: debtPosition.toBase58(),
          owner: owner.toBase58(),
          asset,
          borrowShares: debtAcc.borrowShares.toString(),
          currentDebt: formatTokenAmount(currentDebtRaw, decimals),
          currentDebtRaw: currentDebtRaw.toString(),
        },
        null,
        2,
      ),
    );
  },

  "get-balance": async ({ args, wallet, conn }) => {
    const asset = assetKeyFromString(requireArg(args, "asset"));
    const decimals = ASSET_DECIMALS[asset];
    const owner = new PublicKey(optionalArg(args, "owner", wallet.publicKey.toBase58()));
    const account = ata(owner, ASSET_MINTS[asset]);
    const raw = await tokenBalance(conn, account).catch(() => 0n);
    console.log(
      JSON.stringify({ owner: owner.toBase58(), asset, tokenAccount: account.toBase58(), balance: formatTokenAmount(raw, decimals), balanceRaw: raw.toString() }, null, 2),
    );
  },

  "get-margin-vault-balance": async ({ args, wallet, conn }) => {
    const asset = assetKeyFromString(requireArg(args, "asset"));
    const decimals = ASSET_DECIMALS[asset];
    const owner = new PublicKey(optionalArg(args, "owner", wallet.publicKey.toBase58()));
    const [margin] = marginPda(owner);
    const account = ata(margin, ASSET_MINTS[asset]);
    const raw = await tokenBalance(conn, account).catch(() => 0n);
    console.log(
      JSON.stringify(
        { owner: owner.toBase58(), margin: margin.toBase58(), asset, tokenAccount: account.toBase58(), balance: formatTokenAmount(raw, decimals), balanceRaw: raw.toString() },
        null,
        2,
      ),
    );
  },

  "get-share-balance": async ({ args, wallet, conn }) => {
    const asset = assetKeyFromString(requireArg(args, "asset"));
    const decimals = ASSET_DECIMALS[asset];
    const owner = new PublicKey(optionalArg(args, "owner", wallet.publicKey.toBase58()));
    const [shareMint] = shareMintPda(ASSET_MINTS[asset]);
    const account = ata(owner, shareMint);
    const raw = await tokenBalance(conn, account).catch(() => 0n);
    console.log(
      JSON.stringify(
        { owner: owner.toBase58(), asset, shareMint: shareMint.toBase58(), tokenAccount: account.toBase58(), shareBalance: formatTokenAmount(raw, decimals), shareBalanceRaw: raw.toString() },
        null,
        2,
      ),
    );
  },

  "get-health-factor": async ({ args, wallet, program, conn }) => {
    const owner = new PublicKey(optionalArg(args, "owner", wallet.publicKey.toBase58()));
    const { collateralBreakdown, health } = await computePositionHealth(program, conn, owner);
    const totalCollateralValue = collateralBreakdown.reduce((sum, c) => sum + c.valueUsd, 0n);
    console.log(
      JSON.stringify(
        {
          owner: owner.toBase58(),
          // Raw, un-weighted sum of every collateral asset's USD value — NOT LTV/threshold-discounted.
          totalCollateralValue: formatUsd(totalCollateralValue),
          borrowPower: formatUsd(health.borrowPower),
          liquidationCollateralValue: formatUsd(health.liquidationCollateralValue),
          totalDebtValue: formatUsd(health.totalDebtValue),
          borrowHealthFactor: formatHealthFactorWad(health.borrowHealthFactorWad),
          liquidationHealthFactor: formatHealthFactorWad(health.liquidationHealthFactorWad),
          isBorrowHealthy: health.totalDebtValue === 0n || health.borrowHealthFactorWad > BALANCE_TO_BORROW_THRESHOLD_WAD,
          isLiquidatable: health.totalDebtValue > 0n && health.liquidationHealthFactorWad <= BALANCE_TO_BORROW_THRESHOLD_WAD,
        },
        null,
        2,
      ),
    );
  },

  "get-position-summary": async ({ args, wallet, program, conn }) => {
    const owner = new PublicKey(optionalArg(args, "owner", wallet.publicKey.toBase58()));
    const { collateralBreakdown, debtBreakdown, health } = await computePositionHealth(program, conn, owner);
    console.log(
      JSON.stringify(
        {
          owner: owner.toBase58(),
          collateral: collateralBreakdown.map((c) => ({
            asset: c.asset,
            amount: formatTokenAmount(c.amountRaw, ASSET_DECIMALS[c.asset]),
            valueUsd: formatUsd(c.valueUsd),
          })),
          debt: debtBreakdown.map((d) => ({
            asset: d.asset,
            amount: formatTokenAmount(d.amountRaw, ASSET_DECIMALS[d.asset]),
            valueUsd: formatUsd(d.valueUsd),
          })),
          borrowPower: formatUsd(health.borrowPower),
          totalDebtValue: formatUsd(health.totalDebtValue),
          borrowHealthFactor: formatHealthFactorWad(health.borrowHealthFactorWad),
          liquidationHealthFactor: formatHealthFactorWad(health.liquidationHealthFactorWad),
          isBorrowHealthy: health.totalDebtValue === 0n || health.borrowHealthFactorWad > BALANCE_TO_BORROW_THRESHOLD_WAD,
          isLiquidatable: health.totalDebtValue > 0n && health.liquidationHealthFactorWad <= BALANCE_TO_BORROW_THRESHOLD_WAD,
        },
        null,
        2,
      ),
    );
  },
};

function printUsage(): void {
  console.error("Usage: npx tsx src/devnet.ts <command> [--flag value ...]\n");
  console.error("Commands:");
  for (const name of Object.keys(COMMANDS)) console.error(`  ${name}`);
}

async function main() {
  const [command, ...rest] = process.argv.slice(2);
  const handler = command ? COMMANDS[command] : undefined;
  if (!handler) {
    printUsage();
    process.exit(1);
  }

  const args = parseArgs(rest);
  const conn = devnetConnection();
  const wallet = loadKeypair(args.wallet);
  const anchorWallet = new anchor.Wallet(wallet);
  const program = programAs(conn, wallet);

  await handler({ args, conn, wallet, anchorWallet, program });
}

main().catch((err) => {
  console.error("❌ command failed:", err);
  process.exit(1);
});
