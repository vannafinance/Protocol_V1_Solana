#!/usr/bin/env node
import * as anchor from "@coral-xyz/anchor";
import { ASSOCIATED_TOKEN_PROGRAM_ID, createAssociatedTokenAccountIdempotentInstruction, TOKEN_PROGRAM_ID } from "@solana/spl-token";
import {
  AccountMeta,
  AddressLookupTableAccount,
  ComputeBudgetProgram,
  Connection,
  PublicKey,
  SystemProgram,
  TransactionInstruction,
} from "@solana/web3.js";
import { ata, optionalArg, parseArgs, requireArg, toBaseUnits } from "./devnet-cli";
import {
  alignGmtradeStoreOnFork,
  BookEntry,
  closeOrderAccounts,
  closeOrderData,
  COLLATERAL,
  createOrderAccounts,
  createOrderData,
  venueAccountPriceAccounts,
  venueAccountEquityUsd,
  venueAccountOf,
  encodePosition,
  entryOf,
  validatorSegment,
  GM_MARKETS,
  gmEscrow,
  gmOrderPda,
  gmPositionPda,
  GMTRADE,
  GMTRADE_STORE,
  indexOracleAccounts,
  indexOracleConfigArg,
  isOpen,
  legBit,
  MARKET_BOOK,
  MARKET_DECREASE,
  MARKET_INCREASE,
  MarketKey,
  marketKeyFromString,
  OrderParams,
  positionEquityUsd,
  preparePositionAccounts,
  preparePositionData,
  prepareUserAccounts,
  prepareUserData,
  readVenueAccount,
  readIndexPriceUsd,
  readMarketBook,
  refreshMarketsOnFork,
  usdToGm,
} from "./gmtrade";
import { oracleMetas, readPrice, refreshOraclesOnFork } from "./oracle";
import {
  ASSET_DECIMALS,
  ASSET_MINTS,
  AssetKey,
  assetKeyFromString,
  DEVNET_RPC_URL,
  devnetConnection,
  loadKeypair,
  log,
  oracleAccountsFor,
  oracleConfigArg,
  agentAs,
  programAs,
  tokenProgramFor,
} from "./devnet-env";
import { buildRemainingAccounts, fetchMargin, getAssetIndexMap, inactiveAssets, PositionKey, trackedLegs } from "./devnet-positions";
import { JUPITER, jupiterRouteForMargin } from "./jupiter";
import { CTOKEN_DECIMALS, KAMINO_RECEIPTS, KaminoCall, kaminoCallAccounts, kaminoCallData, KLEND, receiptKeyFromString } from "./kamino";
import { assetConfigPda, integrationPda, marginPda, ORACLE, priceBookPda, protocolConfigPda, VALIDATOR } from "./pda";
import { ensurePriceBook, priceAccounts, setPriceSourceIx } from "./agents";
import { sendV0 } from "./send";

type Ctx = {
  args: Record<string, string>;
  conn: Connection;
  wallet: anchor.web3.Keypair;
  program: anchor.Program;
};

function method(program: anchor.Program, snake: string, camel: string) {
  const methods = program.methods as Record<string, (...args: unknown[]) => any>;
  if (typeof methods[camel] === "function") return methods[camel].bind(methods);
  if (typeof methods[snake] === "function") return methods[snake].bind(methods);
  throw new Error(`IDL is missing ${camel} / ${snake} — rebuild the program and refresh the IDL`);
}

async function callCheatcode(methodName: string, params: unknown[]): Promise<void> {
  const response = await fetch(DEVNET_RPC_URL, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: methodName, params }),
  });
  const body = (await response.json()) as { error?: { message: string } };
  if (body.error) throw new Error(`${methodName}: ${body.error.message}`);
}

const commands: Record<string, (ctx: Ctx) => Promise<void>> = {
  "fund-sol": async ({ args, conn }) => {
    const to = new PublicKey(requireArg(args, "to"));
    const amount = Number(optionalArg(args, "amount", "10"));
    const sig = await conn.requestAirdrop(to, amount * 1e9);
    await conn.confirmTransaction(sig, "confirmed");
    log("fund-sol", `${amount} SOL → ${to.toBase58()} tx=${sig}`);
  },

  fund: async ({ args }) => {
    const asset = assetKeyFromString(requireArg(args, "asset"));
    const to = new PublicKey(requireArg(args, "to"));
    const amount = requireArg(args, "amount");
    const raw = toBaseUnits(amount, ASSET_DECIMALS[asset]);
    await callCheatcode("surfnet_setTokenAccount", [
      to.toBase58(),
      ASSET_MINTS[asset].toBase58(),
      { amount: Number(raw.toString()) },
      tokenProgramFor(asset).toBase58(),
    ]);
    log("fund", `${amount} ${asset} → ${to.toBase58()}`);
  },

  "fund-usdc": async (ctx) => commands.fund({ ...ctx, args: { asset: "usdc", amount: "1000", ...ctx.args } }),

  "create-margin": async ({ wallet, program }) => {
    const [protocolConfig] = protocolConfigPda();
    const [marginAccount] = marginPda(wallet.publicKey);
    const sig = await method(program, "user_create_margin", "userCreateMargin")()
      .accounts({
        payer: wallet.publicKey,
        authority: wallet.publicKey,
        protocolConfig,
        marginAccount,
        systemProgram: SystemProgram.programId,
      })
      .rpc();
    log("create-margin", `${marginAccount.toBase58()} tx=${sig}`);
  },

  "register-kamino": async (ctx) => registerIntegration(ctx, KLEND, "klend"),

  "set-kamino-enabled": async ({ args, wallet, program }) => {
    const enabled = optionalArg(args, "enabled", "true") === "true";
    const [protocolConfig] = protocolConfigPda();
    const [integration] = integrationPda(KLEND);
    const sig = await method(program, "admin_set_integration_enabled", "adminSetIntegrationEnabled")(enabled)
      .accounts({ admin: wallet.publicKey, protocolConfig, integration })
      .rpc();
    log("admin_set_integration_enabled", `klend enabled=${enabled} tx=${sig}`);
  },

  "register-receipt": async ({ args, wallet, program, conn }) => {
    const key = receiptKeyFromString(requireArg(args, "symbol"));
    const receipt = KAMINO_RECEIPTS[key];
    const [protocolConfig] = protocolConfigPda();
    const [assetConfig] = assetConfigPda(receipt.collateralMint);
    const risk = [
      Number(optionalArg(args, "ltv-bps", "5000")),
      Number(optionalArg(args, "liq-threshold-bps", "6000")),
      Number(optionalArg(args, "liq-bonus-bps", "500")),
    ];
    const maxCollateral = toBaseUnits(optionalArg(args, "max-collateral", "0"), CTOKEN_DECIMALS);
    await refreshOraclesOnFork(conn, new anchor.Wallet(wallet), [receipt.underlying]);
    await ensurePriceBook(conn, wallet);
    const klend = { reserve: receipt.reserve, program: KLEND };
    const setSource = await setPriceSourceIx(conn, wallet, receipt.collateralMint, receipt.underlying, klend);
    const sig = await method(program, "admin_register_asset", "adminRegisterAsset")(maxCollateral, ...risk, true, false)
      .accounts({
        admin: wallet.publicKey,
        payer: wallet.publicKey,
        protocolConfig,
        underlyingMint: receipt.collateralMint,
        assetConfig,
        oracle: ORACLE,
        tokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .remainingAccounts(oracleMetas([priceBookPda(), ...priceAccounts(receipt.underlying, klend)]))
      .preInstructions([setSource])
      .rpc();
    log("register-receipt", `${key} assetConfig=${assetConfig.toBase58()} tx=${sig}`);
  },

  "kamino-deposit": async (ctx) => kaminoCall(ctx, "deposit"),
  "kamino-redeem": async (ctx) => kaminoCall(ctx, "redeem"),

  "register-jupiter": async (ctx) => registerIntegration(ctx, JUPITER, "jupiter"),

  "jupiter-swap": async ({ args, wallet, program, conn }) => {
    const from = assetKeyFromString(requireArg(args, "from"));
    const to = assetKeyFromString(requireArg(args, "to"));
    const amount = BigInt(toBaseUnits(requireArg(args, "amount"), ASSET_DECIMALS[from]).toString());
    const [margin] = marginPda(wallet.publicKey);
    const swap = await jupiterRouteForMargin({
      inputMint: ASSET_MINTS[from],
      outputMint: ASSET_MINTS[to],
      amount,
      margin,
      slippageBps: Number(optionalArg(args, "slippage-bps", "50")),
      dexes: args.dexes,
    });
    await refreshAllOracles(conn, wallet);
    const newAssets = await inactiveAssets(program, margin, [to]);
    const rest = await buildRemainingAccounts(program, margin, { priced: [from, to], newAssets });
    const ix = await method(program, "margin_execute", "marginExecute")(swap.data, swap.accounts.length, newAssets.length)
      .accounts(executeAccounts(wallet.publicKey, margin, JUPITER, null))
      .remainingAccounts([...swap.accounts, ...rest])
      .instruction();
    const vault = createVault(wallet.publicKey, margin, ASSET_MINTS[to], tokenProgramFor(to));

    const tables = await Promise.all(swap.lookupTables.map((t) => conn.getAddressLookupTable(t)));
    const jupiterTables = tables.map((t) => t.value).filter((t): t is AddressLookupTableAccount => t !== null);
    const sig = await sendV0(conn, wallet, [ComputeBudgetProgram.setComputeUnitLimit({ units: 1_000_000 }), vault, ix], jupiterTables);
    log("jupiter-swap", `${args.amount} ${from} -> ${to} (quoted ${swap.quotedOut}) tx=${sig}`);
  },
};

async function registerIntegration({ wallet, program, conn }: Ctx, target: PublicKey, label: string): Promise<void> {
  const [integration] = integrationPda(target);
  if (await conn.getAccountInfo(integration)) return log("integration exists", `${label} ${integration.toBase58()}`);
  const sig = await method(program, "admin_register_integration", "adminRegisterIntegration")()
    .accounts({
      admin: wallet.publicKey,
      payer: wallet.publicKey,
      protocolConfig: protocolConfigPda()[0],
      targetProgram: target,
      validator: VALIDATOR,
      integration,
      systemProgram: SystemProgram.programId,
    })
    .rpc();
  log("admin_register_integration", `${label} integration=${integration.toBase58()} tx=${sig}`);
}

const VENUE_ACCOUNT_LAMPORTS = 50_000_000;

function sideOf(args: Record<string, string>): boolean {
  const side = requireArg(args, "side").toLowerCase();
  if (side !== "long" && side !== "short") throw new Error(`--side must be long or short, got "${side}"`);
  return side === "long";
}

function marketOf(args: Record<string, string>): MarketKey {
  return marketKeyFromString(optionalArg(args, "market", "eth"));
}

async function refreshForGmtrade(conn: Connection, wallet: anchor.web3.Keypair): Promise<void> {
  await alignGmtradeStoreOnFork(conn);
  await refreshAllOracles(conn, wallet);
}

async function acceptablePrice(conn: Connection, key: MarketKey, args: Record<string, string>, buying: boolean): Promise<bigint | null> {
  const decimals = GM_MARKETS[key].indexDecimals;
  if (args["acceptable-price"]) return usdToGm(args["acceptable-price"]) / 10n ** BigInt(decimals);
  const bps = Number(optionalArg(args, "slippage-bps", "100"));
  if (bps === 0) return null;
  const index = await readIndexPriceUsd(conn, key);
  const limit = index * (buying ? 1 + bps / 10_000 : 1 - bps / 10_000);
  return usdToGm(limit.toFixed(8)) / 10n ** BigInt(decimals);
}

async function venueCall(ctx: Ctx, entry: BookEntry | null, data: Buffer, cpi: AccountMeta[], label: string, pre: TransactionInstruction[] = []): Promise<string> {
  const { wallet, program, conn } = ctx;
  const [margin] = marginPda(wallet.publicKey);
  const venueAccount = venueAccountOf(margin);
  const rest = await buildRemainingAccounts(program, margin, {
    priced: ["usdc", "gmtrade"],
    writable: ["usdc"],
    venueLegs: entry ? legBit(entry.leg) : 0n,
    validator: validatorSegment(),
  });
  const ix = await method(program, "margin_execute", "marginExecute")(data, cpi.length, 0)
    .accounts(executeAccounts(wallet.publicKey, margin, GMTRADE, GMTRADE_STORE))
    .remainingAccounts([...cpi, ...rest])
    .instruction();
  const balance = (await conn.getAccountInfo(venueAccount))?.lamports ?? 0;
  const topUp = balance < VENUE_ACCOUNT_LAMPORTS ? [SystemProgram.transfer({ fromPubkey: wallet.publicKey, toPubkey: venueAccount, lamports: VENUE_ACCOUNT_LAMPORTS - balance })] : [];
  const usdcAccount = createVault(wallet.publicKey, venueAccount, COLLATERAL, TOKEN_PROGRAM_ID);
  const sig = await sendV0(conn, wallet, [ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }), ...topUp, usdcAccount, ...pre, ix]);
  log(label, `${entry ? entry.key : "venue account"} tx=${sig}`);
  return sig;
}

function orderParams(kind: number, isLong: boolean, collateral: bigint, size: bigint, acceptable: bigint | null, args: Record<string, string>): OrderParams {
  return {
    kind,
    executionLamports: BigInt(optionalArg(args, "execution-lamports", "300000")),
    initialCollateralDeltaAmount: collateral,
    sizeDeltaValue: size,
    isLong,
    acceptablePrice: acceptable,
  };
}

function escrowIx(payer: PublicKey, venueAccount: PublicKey, entry: BookEntry, isLong: boolean): TransactionInstruction {
  return createAssociatedTokenAccountIdempotentInstruction(payer, gmEscrow(venueAccount, entry.leg, isLong), gmOrderPda(venueAccount, entry.leg, isLong), COLLATERAL);
}

const gmtradeCommands: Record<string, (ctx: Ctx) => Promise<void>> = {
  "register-gmtrade": async (ctx) => {
    const { args, wallet, program, conn } = ctx;
    const key = marketOf(args);
    const m = GM_MARKETS[key];
    const [protocolConfig] = protocolConfigPda();
    await registerIntegration(ctx, GMTRADE, "gmtrade");
    await refreshForGmtrade(conn, wallet);
    const oracle = agentAs(conn, wallet, "vanna_oracle");

    if (!(await conn.getAccountInfo(MARKET_BOOK))) {
      const sig = await oracle.methods
        .openMarketBook(GMTRADE_STORE, GMTRADE, oracleConfigArg("usdc"))
        .accounts({ admin: wallet.publicKey, payer: wallet.publicKey, protocolConfig, marketBook: MARKET_BOOK, collateralMint: COLLATERAL, systemProgram: SystemProgram.programId })
        .remainingAccounts(oracleMetas(oracleAccountsFor("usdc")))
        .rpc();
      log("open_market_book", `${MARKET_BOOK.toBase58()} tx=${sig}`);
    }
    const maxLeverageBps = Math.round(Number(optionalArg(args, "max-leverage", "5")) * 10_000);
    const trading = optionalArg(args, "trading", "on") === "on";
    let sig = await oracle.methods
      .listMarket(indexOracleConfigArg(key), maxLeverageBps, trading)
      .accounts({ admin: wallet.publicKey, protocolConfig, marketBook: MARKET_BOOK, market: m.market, indexMint: m.indexMint })
      .remainingAccounts(oracleMetas(indexOracleAccounts(key)))
      .rpc();
    const entry = entryOf(await readMarketBook(conn), key);
    log("list_market", `${m.name} leg=${entry.leg} max leverage ${maxLeverageBps / 10_000}x trading=${trading} tx=${sig}`);

    const [venueAsset] = assetConfigPda(GMTRADE_STORE);
    if (!(await conn.getAccountInfo(venueAsset))) {
      sig = await method(program, "admin_register_venue", "adminRegisterVenue")(GMTRADE_STORE)
        .accounts({
          admin: wallet.publicKey,
          payer: wallet.publicKey,
          protocolConfig,
          assetConfig: venueAsset,
          settleAsset: assetConfigPda(COLLATERAL)[0],
          oracle: ORACLE,
          systemProgram: SystemProgram.programId,
        })
        .rpc();
      log("admin_register_venue", `gmtrade asset=${venueAsset.toBase58()} tx=${sig}`);
      sig = await method(program, "admin_update_asset_config", "adminUpdateAssetConfig")(new anchor.BN(0), 0, 10_000, 0, true, false)
        .accounts({ admin: wallet.publicKey, protocolConfig, assetConfig: venueAsset })
        .rpc();
      log("venue enabled", `gmtrade tx=${sig}`);
    }
  },

  "gmtrade-open": async (ctx) => {
    const { args, wallet, conn } = ctx;
    const key = marketOf(args);
    const isLong = sideOf(args);
    const [margin] = marginPda(wallet.publicKey);
    const venueAccount = venueAccountOf(margin);
    await refreshForGmtrade(conn, wallet);
    const book = await readMarketBook(conn);
    const entry = entryOf(book, key);
    const collateral = BigInt(toBaseUnits(requireArg(args, "collateral"), 6).toString());
    const params = orderParams(MARKET_INCREASE, isLong, collateral, usdToGm(requireArg(args, "size")), await acceptablePrice(conn, key, args, isLong), args);
    let state = await readVenueAccount(conn, margin, book);
    if (!state.userExists) await venueCall(ctx, null, prepareUserData(), prepareUserAccounts(venueAccount), "prepare_user");
    state = await readVenueAccount(conn, margin, book);
    const market = state.markets.find((m) => m.entry.key === key)!;
    if (!market.positions[isLong ? 0 : 1]) {
      await venueCall(ctx, entry, preparePositionData(params), preparePositionAccounts(venueAccount, key, isLong), "prepare_position");
    }
    await venueCall(ctx, entry, createOrderData(params, entry.leg), createOrderAccounts(venueAccount, entry, isLong, true), "create_order_v2 (market increase)", [
      escrowIx(wallet.publicKey, venueAccount, entry, isLong),
    ]);
    console.log(`  order ${gmOrderPda(venueAccount, entry.leg, isLong).toBase58()} (leg ${entry.leg}): GMTrade's keepers execute it within seconds (on a fork: gmtrade-simulate-fill)`);
  },

  "gmtrade-close": async (ctx) => {
    const { args, wallet, conn } = ctx;
    const key = marketOf(args);
    const isLong = sideOf(args);
    const [margin] = marginPda(wallet.publicKey);
    const venueAccount = venueAccountOf(margin);
    await refreshForGmtrade(conn, wallet);
    const book = await readMarketBook(conn);
    const entry = entryOf(book, key);
    const position = (await readVenueAccount(conn, margin, book)).markets.find((m) => m.entry.key === key)!.positions[isLong ? 0 : 1];
    if (!position || position.sizeInUsd === 0n) throw new Error(`no open ${key} ${isLong ? "long" : "short"} position`);
    const size = args.size ? usdToGm(args.size) : position.sizeInUsd;
    const withdraw = BigInt(toBaseUnits(optionalArg(args, "withdraw", "0"), 6).toString());
    const params = orderParams(MARKET_DECREASE, isLong, withdraw, size, await acceptablePrice(conn, key, args, !isLong), args);
    await venueCall(ctx, entry, createOrderData(params, entry.leg), createOrderAccounts(venueAccount, entry, isLong, false), "create_order_v2 (market decrease)", [
      escrowIx(wallet.publicKey, venueAccount, entry, isLong),
    ]);
  },

  "gmtrade-cancel": async (ctx) => {
    const { args, wallet, conn } = ctx;
    const key = marketOf(args);
    const isLong = sideOf(args);
    const [margin] = marginPda(wallet.publicKey);
    const book = await readMarketBook(conn);
    const entry = entryOf(book, key);
    const order = (await readVenueAccount(conn, margin, book)).markets.find((m) => m.entry.key === key)!.orders[isLong ? 0 : 1];
    if (!order) throw new Error(`no pending ${key} ${isLong ? "long" : "short"} order`);
    await refreshForGmtrade(conn, wallet);
    await venueCall(ctx, entry, closeOrderData("cancelled by owner"), closeOrderAccounts(venueAccountOf(margin), entry, isLong, order.kind === MARKET_INCREASE), "close_order_v2");
  },

  "gmtrade-settle": async ({ args, wallet, program, conn }) => {
    const owner = new PublicKey(optionalArg(args, "owner", wallet.publicKey.toBase58()));
    const [margin] = marginPda(owner);
    const venueAccount = venueAccountOf(margin);
    const venue = (await getAssetIndexMap(program)).gmtrade;
    if (!venue) throw new Error("GMTrade is not registered as a venue");
    const legs = trackedLegs(await fetchMargin(program, margin), venue.index);
    const book = await readMarketBook(conn);
    const ix = await method(program, "public_venue_settle", "publicVenueSettle")()
      .accounts({
        caller: wallet.publicKey,
        protocolConfig: protocolConfigPda()[0],
        marginAccount: margin,
        venueAsset: venue.assetConfig,
        settleAsset: assetConfigPda(COLLATERAL)[0],
        settleMint: COLLATERAL,
        venueAccount,
        idle: ata(venueAccount, COLLATERAL, TOKEN_PROGRAM_ID),
        marginVault: ata(margin, COLLATERAL, TOKEN_PROGRAM_ID),
        oracle: ORACLE,
        tokenProgram: TOKEN_PROGRAM_ID,
        associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .remainingAccounts(oracleMetas(venueAccountPriceAccounts(venueAccount, book, legs)))
      .instruction();
    const sig = await sendV0(conn, wallet, [ComputeBudgetProgram.setComputeUnitLimit({ units: 600_000 }), ix]);
    log("public_venue_settle", `margin=${margin.toBase58()} tx=${sig}`);
  },

  "gmtrade-status": async ({ args, wallet, program, conn }) => {
    const owner = new PublicKey(optionalArg(args, "owner", wallet.publicKey.toBase58()));
    const [margin] = marginPda(owner);
    const book = await readMarketBook(conn);
    const venueAccount = await readVenueAccount(conn, margin, book);
    const venue = (await getAssetIndexMap(program)).gmtrade;
    const legs = venue ? trackedLegs(await fetchMargin(program, margin), venue.index) : 0n;
    const usdcPrice = await readPrice(conn, "usdc");
    const usdc = Number(usdcPrice.value) * 10 ** usdcPrice.exponent;
    const prices: Partial<Record<MarketKey, number>> = {};
    for (const entry of book) prices[entry.key] = await readIndexPriceUsd(conn, entry.key);
    const sides = ["long", "short"];
    const markets = venueAccount.markets
      .filter((m) => isOpen(m) || (legs & legBit(m.entry.leg)) !== 0n)
      .map((m) => {
        const index = prices[m.entry.key]!;
        const decimals = GM_MARKETS[m.entry.key].indexDecimals;
        return {
          market: m.entry.key,
          leg: m.entry.leg,
          tracked: (legs & legBit(m.entry.leg)) !== 0n,
          indexPriceUsd: index,
          orders: m.orders.map(
            (o, i) =>
              o && { side: sides[i], kind: o.kind === MARKET_INCREASE ? "increase" : "decrease", sizeUsd: Number(o.sizeDeltaValue) / 1e20, collateralUsdc: Number(o.initialCollateralDeltaAmount) / 1e6, escrowedUsdc: Number(o.escrowed) / 1e6 },
          ),
          positions: m.positions.map((p, i) => {
            if (!p || p.sizeInUsd === 0n) return null;
            const size = Number(p.sizeInUsd) / 1e20;
            const tokens = Number(p.sizeInTokens) / 10 ** decimals;
            return {
              side: sides[i],
              sizeUsd: size,
              sizeTokens: tokens,
              entryPriceUsd: size / tokens,
              collateralUsdc: Number(p.collateralAmount) / 1e6,
              leverage: size / (Number(p.collateralAmount) / 1e6),
              pnlUsd: i === 0 ? tokens * index - size : size - tokens * index,
              equityUsd: positionEquityUsd(p, i === 0, m.market, index, decimals, usdc),
            };
          }),
        };
      });
    console.log(
      JSON.stringify(
        {
          venueAccount: venueAccount.venueAccount.toBase58(),
          lamports: venueAccount.lamports,
          idleUsdc: Number(venueAccount.idle) / 1e6,
          trackedLegs: `0b${legs.toString(2)}`,
          markets,
          venueAccountEquityUsd: venueAccountEquityUsd(venueAccount, legs, prices, usdc),
        },
        null,
        2,
      ),
    );
  },

  "gmtrade-simulate-fill": async ({ args, wallet, conn }) => {
    const key = marketOf(args);
    const isLong = sideOf(args);
    const [margin] = marginPda(new PublicKey(optionalArg(args, "owner", wallet.publicKey.toBase58())));
    const venueAccount = venueAccountOf(margin);
    const book = await readMarketBook(conn);
    const entry = entryOf(book, key);
    const state = await readVenueAccount(conn, margin, book);
    const market = state.markets.find((m) => m.entry.key === key)!;
    const side = isLong ? 0 : 1;
    const order = market.orders[side];
    if (!order) throw new Error("no pending order to fill");
    const price = Number(optionalArg(args, "price", String(await readIndexPriceUsd(conn, key))));
    const positionKey = gmPositionPda(venueAccount, key, isLong);
    const positionInfo = await conn.getAccountInfo(positionKey);
    if (!positionInfo) throw new Error("position account missing");
    const p = market.positions[side]!;
    const unitPrice = (price * 1e20) / 10 ** GM_MARKETS[key].indexDecimals;
    let next = { ...p };
    let payout = 0n;
    if (order.kind === MARKET_INCREASE) {
      next = {
        sizeInTokens: p.sizeInTokens + BigInt(Math.floor(Number(order.sizeDeltaValue) / unitPrice)),
        collateralAmount: p.collateralAmount + order.escrowed,
        sizeInUsd: p.sizeInUsd + order.sizeDeltaValue,
        borrowingFactor: market.market.borrowingFactor[side],
        fundingFeeAmountPerSize: market.market.fundingPerSize[side],
      };
    } else {
      const closed = order.sizeDeltaValue >= p.sizeInUsd ? p.sizeInUsd : order.sizeDeltaValue;
      const share = Number(closed) / Number(p.sizeInUsd);
      const tokens = BigInt(Math.floor(Number(p.sizeInTokens) * share));
      const pnl = ((Number(tokens) * unitPrice - Number(closed)) / 1e20) * (isLong ? 1 : -1);
      const released = closed === p.sizeInUsd ? p.collateralAmount : order.initialCollateralDeltaAmount;
      payout = BigInt(Math.max(0, Math.floor(Number(released) + pnl * 1e6)));
      next = {
        sizeInTokens: p.sizeInTokens - tokens,
        collateralAmount: closed === p.sizeInUsd ? 0n : p.collateralAmount - released,
        sizeInUsd: p.sizeInUsd - closed,
        borrowingFactor: p.borrowingFactor,
        fundingFeeAmountPerSize: p.fundingFeeAmountPerSize,
      };
    }
    const data = encodePosition(positionInfo.data, next);
    await callCheatcode("surfnet_setAccount", [
      positionKey.toBase58(),
      { lamports: positionInfo.lamports, data: data.toString("hex"), owner: GMTRADE.toBase58(), executable: false, rentEpoch: 0 },
    ]);
    for (const account of [gmOrderPda(venueAccount, entry.leg, isLong), gmEscrow(venueAccount, entry.leg, isLong)]) {
      await callCheatcode("surfnet_setAccount", [account.toBase58(), { lamports: 0, data: "", owner: SystemProgram.programId.toBase58(), executable: false, rentEpoch: 0 }]);
    }
    if (payout > 0n) {
      await callCheatcode("surfnet_setTokenAccount", [venueAccount.toBase58(), COLLATERAL.toBase58(), { amount: Number(state.idle + payout) }, TOKEN_PROGRAM_ID.toBase58()]);
    }
    log("gmtrade-simulate-fill", `${key} ${isLong ? "long" : "short"} at $${price} — size now $${Number(next.sizeInUsd) / 1e20}, paid out ${Number(payout) / 1e6} USDC`);
  },

  "gmtrade-unwind": async ({ args, wallet, program, conn }) => {
    const key = marketOf(args);
    const isLong = sideOf(args);
    const owner = new PublicKey(requireArg(args, "owner"));
    const [margin] = marginPda(owner);
    const venueAccount = venueAccountOf(margin);
    await refreshForGmtrade(conn, wallet);
    const book = await readMarketBook(conn);
    const entry = entryOf(book, key);
    const position = (await readVenueAccount(conn, margin, book)).markets.find((m) => m.entry.key === key)!.positions[isLong ? 0 : 1];
    if (!position || position.sizeInUsd === 0n) throw new Error(`no open ${key} ${isLong ? "long" : "short"} position`);
    const params = orderParams(MARKET_DECREASE, isLong, 0n, position.sizeInUsd, null, args);
    const cpi = createOrderAccounts(venueAccount, entry, isLong, false);
    const rest = await buildRemainingAccounts(program, margin, { validator: validatorSegment() });
    const ix = await method(program, "public_venue_unwind", "publicVenueUnwind")(createOrderData(params, entry.leg), cpi.length)
      .accounts({
        caller: wallet.publicKey,
        marginAccount: margin,
        venueAsset: assetConfigPda(GMTRADE_STORE)[0],
        integration: integrationPda(GMTRADE)[0],
        targetProgram: GMTRADE,
        validator: VALIDATOR,
      })
      .remainingAccounts([...cpi, ...rest])
      .instruction();
    const balance = (await conn.getAccountInfo(venueAccount))?.lamports ?? 0;
    const topUp = balance < VENUE_ACCOUNT_LAMPORTS ? [SystemProgram.transfer({ fromPubkey: wallet.publicKey, toPubkey: venueAccount, lamports: VENUE_ACCOUNT_LAMPORTS - balance })] : [];
    const sig = await sendV0(conn, wallet, [ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }), ...topUp, escrowIx(wallet.publicKey, venueAccount, entry, isLong), ix]);
    log("public_venue_unwind", `${key} ${isLong ? "long" : "short"} of ${owner.toBase58()} tx=${sig}`);
  },
};

Object.assign(commands, gmtradeCommands);

async function refreshAllOracles(conn: Connection, wallet: anchor.web3.Keypair): Promise<void> {
  await refreshOraclesOnFork(conn, new anchor.Wallet(wallet), Object.keys(ASSET_MINTS) as AssetKey[]);
  if (await conn.getAccountInfo(MARKET_BOOK)) await refreshMarketsOnFork(conn, new anchor.Wallet(wallet));
}

function executeAccounts(authority: PublicKey, margin: PublicKey, target: PublicKey, venue: PublicKey | null) {
  return {
    authority,
    protocolConfig: protocolConfigPda()[0],
    marginAccount: margin,
    integration: integrationPda(target)[0],
    targetProgram: target,
    validator: VALIDATOR,
    venueAsset: venue ? assetConfigPda(venue)[0] : null,
    venueAccount: venue ? venueAccountOf(margin) : null,
  } as any;
}

function createVault(payer: PublicKey, owner: PublicKey, mint: PublicKey, tokenProgram: PublicKey): TransactionInstruction {
  return createAssociatedTokenAccountIdempotentInstruction(payer, ata(owner, mint, tokenProgram), owner, mint, tokenProgram);
}

async function kaminoCall({ args, wallet, program, conn }: Ctx, call: KaminoCall): Promise<void> {
  const key = receiptKeyFromString(requireArg(args, "symbol"));
  const receipt = KAMINO_RECEIPTS[key];
  const underlying = receipt.underlying;
  const spentDecimals = call === "deposit" ? ASSET_DECIMALS[underlying] : CTOKEN_DECIMALS;
  const amount = BigInt(toBaseUnits(requireArg(args, "amount"), spentDecimals).toString());
  const [margin] = marginPda(wallet.publicKey);
  const received = call === "deposit"
    ? { key: key as PositionKey, mint: receipt.collateralMint, tokenProgram: TOKEN_PROGRAM_ID }
    : { key: underlying as PositionKey, mint: ASSET_MINTS[underlying], tokenProgram: tokenProgramFor(underlying) };

  await refreshAllOracles(conn, wallet);
  const newAssets = await inactiveAssets(program, margin, [received.key]);
  const rest = await buildRemainingAccounts(program, margin, { priced: [underlying, key], newAssets });
  const cpiAccounts = kaminoCallAccounts(call, receipt, margin);
  const ix = await method(program, "margin_execute", "marginExecute")(kaminoCallData(call, amount), cpiAccounts.length, newAssets.length)
    .accounts(executeAccounts(wallet.publicKey, margin, KLEND, null))
    .remainingAccounts([...cpiAccounts, ...rest])
    .instruction();
  const vault = createVault(wallet.publicKey, margin, received.mint, received.tokenProgram);
  const sig = await sendV0(conn, wallet, [ComputeBudgetProgram.setComputeUnitLimit({ units: 1_000_000 }), vault, ix]);
  log(`kamino-${call}`, `${key} amount=${args.amount} tx=${sig}`);
}

async function main() {
  const argv = process.argv.slice(2);
  const command = argv[0];
  if (!command || !commands[command]) {
    console.log("Usage: npx tsx src/integrations-fork.ts <command> [--flag value]");
    console.log("Commands:", Object.keys(commands).join(", "));
    process.exit(command ? 1 : 0);
  }
  const args = parseArgs(argv.slice(1));
  const wallet = loadKeypair(args.wallet);
  const conn = devnetConnection();
  const program = programAs(conn, wallet);
  await commands[command]({ args, conn, wallet, program });
}

main().catch((err) => {
  console.error(err instanceof Error ? err.message : err);
  process.exit(1);
});
