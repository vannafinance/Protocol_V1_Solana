#!/usr/bin/env node
/**
 * Surfpool fork helpers for TSLAx/GOOGLx + Kamino lite strategy registration.
 *
 *   npx tsx src/lite-fork.ts <command> [--flag value ...]
 */
import * as anchor from "@coral-xyz/anchor";
import { Connection, PublicKey, SystemProgram } from "@solana/web3.js";
import {
  ASSOCIATED_TOKEN_PROGRAM_ID,
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  getAssociatedTokenAddressSync,
} from "@solana/spl-token";
import {
  ASSET_DECIMALS,
  ASSET_MINTS,
  AssetKey,
  DEVNET_RPC_URL,
  assetKeyFromString,
  devnetConnection,
  loadKeypair,
  log,
  programAs,
  tokenProgramFor,
} from "./devnet-env";
import { optionalArg, parseArgs, requireArg, toBaseUnits } from "./devnet-cli";
import {
  assetConfigPda,
  debtPositionPda,
  litePositionPda,
  liteStrategyPda,
  marginPda,
  protocolConfigPda,
  reservePda,
  shareMintPda,
} from "./pda";

const KLEND = new PublicKey("KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD");
const XSTOCKS_MARKET = new PublicKey("5wJeMrUYECGq41fxRESKALVcHnNX26TAWy4W98yULsua");
const MARKET_AUTHORITY = new PublicKey("2Z7zhqp1eddmHNmEqexftST6DFPWmoL4QqfgiG5uJMJx");
const MAIN_MARKET = new PublicKey("7u3HeHxYDLhnCoErrtycNokbQYbWGzLs6JSDqGAv5PfF");
const MAIN_MARKET_AUTHORITY = new PublicKey("9DrvZvyWh1HuAoZxvYWMvkf2XCzryCpGgHqrMjyDWpmo");

type KaminoLiteKey = "tslax" | "googlx" | "usdc" | "wsol";

const KAMINO_RESERVES: Record<KaminoLiteKey, {
  market: PublicKey;
  marketAuthority: PublicKey;
  reserve: PublicKey;
  liquidityVault: PublicKey;
  collateralMint: PublicKey;
}> = {
  tslax: {
    market: XSTOCKS_MARKET,
    marketAuthority: MARKET_AUTHORITY,
    reserve: new PublicKey("5iTiczqgUegqA3PpoNpotizMbY9n1sRWr3oL6igKvWuf"),
    liquidityVault: new PublicKey("AvhRUjab47DCo9efnzmDha8xUeQFEs36Yywv1x8t3T2W"),
    collateralMint: new PublicKey("6bZpUNY1qmbvQBgCmfQJUA377X63ATvnpCHYh8hQnfjC"),
  },
  googlx: {
    market: XSTOCKS_MARKET,
    marketAuthority: MARKET_AUTHORITY,
    reserve: new PublicKey("4wg6rEkGgHaEuxMduP46C1xFZ24Lnp5YgdNkZAHxFzsN"),
    liquidityVault: new PublicKey("5vjGDURj7kT6HZtoSmfG9NgTak7deQ9u3uWgdktXv32G"),
    collateralMint: new PublicKey("FL41HF8KMuMmYHxGgHezsa5MLUKmNSsu32cC8Qru7TnB"),
  },
  // Kamino MAIN market (not xStocks) — used by the One-Click cross-asset carry
  // trade's yield leg (swap stock -> USDC/SOL, then lite_supply into these).
  usdc: {
    market: MAIN_MARKET,
    marketAuthority: MAIN_MARKET_AUTHORITY,
    reserve: new PublicKey("D6q6wuQSrifJKZYpR1M8R4YawnLDtDsMmWM1NbBmgJ59"),
    liquidityVault: new PublicKey("Bgq7trRgVMeq33yt235zM2onQ4bRDBsY5EWiTetF4qw6"),
    collateralMint: new PublicKey("B8V6WVjPxW1UGwVDfxH2d2r8SyT4cqn7dQRK6XneVa7D"),
  },
  wsol: {
    market: MAIN_MARKET,
    marketAuthority: MAIN_MARKET_AUTHORITY,
    reserve: new PublicKey("d4A2prbA2whesmvHaL88BH6Ewn5N4bTSU2Ze8P6Bc4Q"),
    liquidityVault: new PublicKey("GafNuUXj9rxGLn4y79dPu6MHSuPWeJR6UtTWuexpGh3U"),
    collateralMint: new PublicKey("2UywZrUdyqs5vDchy7fKQJKau2RVyuzBev2XKGPDSiX1"),
  },
};

/** Same set `stockKey` accepts, plus USDC/SOL for the main-market Kamino strategies. */
function kaminoLiteKey(symbol: string): KaminoLiteKey {
  const v = symbol.toLowerCase();
  if (v === "tslax" || v === "tsla") return "tslax";
  if (v === "googlx" || v === "googl") return "googlx";
  if (v === "usdc") return "usdc";
  if (v === "wsol" || v === "sol") return "wsol";
  throw new Error(`unsupported lite key "${symbol}" — use TSLAX|GOOGLX|USDC|SOL`);
}

const INSTRUCTIONS_SYSVAR = new PublicKey("Sysvar1nstructions1111111111111111111111111");

type Ctx = {
  args: Record<string, string>;
  conn: Connection;
  wallet: anchor.web3.Keypair;
  program: anchor.Program;
};

function hasIx(program: anchor.Program, snake: string, camel: string): boolean {
  const methods = program.methods as Record<string, unknown>;
  return typeof methods[camel] === "function" || typeof methods[snake] === "function";
}

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

function stockKey(symbol: string): "tslax" | "googlx" {
  const v = symbol.toLowerCase().replace(/x$/, "x");
  if (v === "tslax" || v === "tsla") return "tslax";
  if (v === "googlx" || v === "googl") return "googlx";
  throw new Error(`unsupported stock "${symbol}" — use TSLAX or GOOGLX`);
}

const commands: Record<string, (ctx: Ctx) => Promise<void>> = {
  "register-asset": async ({ args }) => {
    const symbol = (args.symbol ?? "TSLAX").toUpperCase();
    const asset = assetKeyFromString(symbol === "TSLAX" ? "tslax" : symbol === "GOOGLX" ? "googlx" : symbol.toLowerCase());
    log("delegating", `devnet.ts register-asset --asset ${asset}`);
    const { spawnSync } = await import("node:child_process");
    const r = spawnSync("npx", ["tsx", "src/devnet.ts", "register-asset", "--asset", asset], {
      cwd: __dirname + "/..",
      stdio: "inherit",
      env: process.env,
    });
    if (r.status !== 0) process.exit(r.status ?? 1);
  },

  "init-reserve": async ({ args }) => {
    const symbol = (args.symbol ?? "TSLAX").toUpperCase();
    const asset = assetKeyFromString(symbol === "TSLAX" ? "tslax" : symbol === "GOOGLX" ? "googlx" : symbol.toLowerCase());
    const { spawnSync } = await import("node:child_process");
    const r = spawnSync("npx", ["tsx", "src/devnet.ts", "initialize-reserve", "--asset", asset], {
      cwd: __dirname + "/..",
      stdio: "inherit",
      env: process.env,
    });
    if (r.status !== 0) process.exit(r.status ?? 1);
  },

  "register-xstocks": async () => {
    const { spawnSync } = await import("node:child_process");
    for (const asset of ["tslax", "googlx"] as AssetKey[]) {
      spawnSync("npx", ["tsx", "src/devnet.ts", "register-asset", "--asset", asset], {
        cwd: __dirname + "/..",
        stdio: "inherit",
        env: process.env,
      });
      spawnSync("npx", ["tsx", "src/devnet.ts", "initialize-reserve", "--asset", asset], {
        cwd: __dirname + "/..",
        stdio: "inherit",
        env: process.env,
      });
    }
    log("register-xstocks", "TSLAx + GOOGLx registered (or already present)");
  },

  "register-lite-strategy": async ({ args, wallet, program }) => {
    if (!hasIx(program, "admin_register_lite_strategy", "adminRegisterLiteStrategy")) {
      log("skip", "adminRegisterLiteStrategy not in IDL");
      return;
    }
    const key = kaminoLiteKey(optionalArg(args, "symbol", "TSLAX"));
    const mint = ASSET_MINTS[key];
    const kamino = KAMINO_RESERVES[key];
    const [protocolConfig] = protocolConfigPda();
    const [assetConfig] = assetConfigPda(mint);
    const [strategyConfig] = liteStrategyPda(mint);
    const sig = await method(program, "admin_register_lite_strategy", "adminRegisterLiteStrategy")()
      .accounts({
        admin: wallet.publicKey,
        payer: wallet.publicKey,
        protocolConfig,
        assetConfig,
        underlyingMint: mint,
        kaminoProgram: KLEND,
        lendingMarket: kamino.market,
        lendingMarketAuthority: kamino.marketAuthority,
        kaminoReserve: kamino.reserve,
        reserveLiquiditySupply: kamino.liquidityVault,
        reserveCollateralMint: kamino.collateralMint,
        strategyConfig,
        tokenProgram: tokenProgramFor(key),
        collateralTokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .rpc();
    log("admin_register_lite_strategy", `${key} strategy=${strategyConfig.toBase58()} tx=${sig}`);
  },

  "fund-sol": async ({ args, conn }) => {
    const to = new PublicKey(requireArg(args, "to"));
    const amount = Number(optionalArg(args, "amount", "10"));
    const sig = await conn.requestAirdrop(to, amount * 1e9);
    await conn.confirmTransaction(sig, "confirmed");
    log("fund-sol", `${amount} SOL → ${to.toBase58()} tx=${sig}`);
  },

  "fund-usdc": async ({ args }) => {
    const to = new PublicKey(requireArg(args, "to"));
    const amount = Number(optionalArg(args, "amount", "1000"));
    const raw = amount * 10 ** ASSET_DECIMALS.usdc;
    await callCheatcode("surfnet_setTokenAccount", [
      to.toBase58(),
      ASSET_MINTS.usdc.toBase58(),
      { amount: raw },
      TOKEN_PROGRAM_ID.toBase58(),
    ]);
    log("fund-usdc", `+${amount} USDC → ${to.toBase58()}`);
  },

  "fund-tslax": async ({ args }) => {
    const to = new PublicKey(requireArg(args, "to"));
    const amount = Number(optionalArg(args, "amount", "100"));
    const raw = amount * 10 ** ASSET_DECIMALS.tslax;
    await callCheatcode("surfnet_setTokenAccount", [
      to.toBase58(),
      ASSET_MINTS.tslax.toBase58(),
      { amount: raw },
      TOKEN_2022_PROGRAM_ID.toBase58(),
    ]);
    log("fund-tslax", `+${amount} TSLAx → ${to.toBase58()}`);
  },

  "fund-xstock": async ({ args }) => {
    const key = stockKey(requireArg(args, "symbol"));
    const to = new PublicKey(requireArg(args, "to"));
    const amount = Number(optionalArg(args, "amount", "50"));
    const raw = amount * 10 ** ASSET_DECIMALS[key];
    await callCheatcode("surfnet_setTokenAccount", [
      to.toBase58(),
      ASSET_MINTS[key].toBase58(),
      { amount: raw },
      tokenProgramFor(key).toBase58(),
    ]);
    log("fund-xstock", `+${amount} ${key} → ${to.toBase58()}`);
  },

  "supply-tslax": async ({ args, wallet, program }) => {
    const amount = optionalArg(args, "amount", "10");
    const mint = ASSET_MINTS.tslax;
    const raw = toBaseUnits(amount, ASSET_DECIMALS.tslax);
    const [protocolConfig] = protocolConfigPda();
    const [assetConfig] = assetConfigPda(mint);
    const [reserve] = reservePda(mint);
    const [shareMint] = shareMintPda(mint);
    const lenderAta = getAssociatedTokenAddressSync(mint, wallet.publicKey, false, TOKEN_2022_PROGRAM_ID);
    const vault = getAssociatedTokenAddressSync(mint, reserve, true, TOKEN_2022_PROGRAM_ID);
    const shareAta = getAssociatedTokenAddressSync(shareMint, wallet.publicKey, false, TOKEN_PROGRAM_ID);
    const sig = await method(program, "lender_supply", "lenderSupply")(raw, new anchor.BN(1))
      .accounts({
        lender: wallet.publicKey,
        protocolConfig,
        assetConfig,
        reserve,
        underlyingMint: mint,
        lenderTokenAccount: lenderAta,
        liquidityVault: vault,
        shareMint,
        lenderShareAccount: shareAta,
        tokenProgram: TOKEN_2022_PROGRAM_ID,
        shareTokenProgram: TOKEN_PROGRAM_ID,
        associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .rpc();
    log("supply-tslax", `${amount} TSLAx tx=${sig}`);
  },

  "supply-xstock": async ({ args, wallet, program }) => {
    const key = stockKey(requireArg(args, "symbol"));
    const amount = optionalArg(args, "amount", "20");
    const mint = ASSET_MINTS[key];
    const tp = tokenProgramFor(key);
    const raw = toBaseUnits(amount, ASSET_DECIMALS[key]);
    const [protocolConfig] = protocolConfigPda();
    const [assetConfig] = assetConfigPda(mint);
    const [reserve] = reservePda(mint);
    const [shareMint] = shareMintPda(mint);
    const lenderAta = getAssociatedTokenAddressSync(mint, wallet.publicKey, false, tp);
    const vault = getAssociatedTokenAddressSync(mint, reserve, true, tp);
    const shareAta = getAssociatedTokenAddressSync(shareMint, wallet.publicKey, false, TOKEN_PROGRAM_ID);
    const sig = await method(program, "lender_supply", "lenderSupply")(raw, new anchor.BN(1))
      .accounts({
        lender: wallet.publicKey,
        protocolConfig,
        assetConfig,
        reserve,
        underlyingMint: mint,
        lenderTokenAccount: lenderAta,
        liquidityVault: vault,
        shareMint,
        lenderShareAccount: shareAta,
        tokenProgram: tp,
        shareTokenProgram: TOKEN_PROGRAM_ID,
        associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .rpc();
    log("supply-xstock", `${amount} ${key} tx=${sig}`);
  },

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

  "lite-open": async ({ args, wallet, program, conn }) => {
    if (!hasIx(program, "lite_open", "liteOpen")) {
      log("skip", "liteOpen not in IDL");
      return;
    }
    const key = stockKey(optionalArg(args, "symbol", "TSLAX"));
    const equity = toBaseUnits(requireArg(args, "equity"), ASSET_DECIMALS[key]);
    const leverageBps = new anchor.BN(optionalArg(args, "leverage-bps", "20000"));
    const mint = ASSET_MINTS[key];
    const kamino = KAMINO_RESERVES[key];
    const tp = tokenProgramFor(key);
    const { refreshPrice } = await import("./devnet-pyth");
    const anchorWallet = new anchor.Wallet(wallet);
    const priceUpdate = await refreshPrice(conn, anchorWallet, key);
    const [protocolConfig] = protocolConfigPda();
    const [assetConfig] = assetConfigPda(mint);
    const [reserve] = reservePda(mint);
    const [margin] = marginPda(wallet.publicKey);
    const [debt] = PublicKey.findProgramAddressSync(
      [Buffer.from("debt"), margin.toBuffer(), reserve.toBuffer()],
      program.programId,
    );
    const [liteStrategy] = liteStrategyPda(mint);
    const [litePosition] = litePositionPda(margin);
    const userAta = getAssociatedTokenAddressSync(mint, wallet.publicKey, false, tp);
    const vault = getAssociatedTokenAddressSync(mint, reserve, true, tp);
    const ctokenAta = getAssociatedTokenAddressSync(kamino.collateralMint, margin, true, TOKEN_PROGRAM_ID);

    const sig = await method(program, "lite_open", "liteOpen")(equity, leverageBps)
      .accounts({
        owner: wallet.publicKey,
        protocolConfig,
        assetConfig,
        reserve,
        underlyingMint: mint,
        userTokenAccount: userAta,
        liquidityVault: vault,
        marginAccount: margin,
        debtPosition: debt,
        liteStrategy,
        litePosition,
        priceUpdate,
        kaminoProgram: KLEND,
        lendingMarket: XSTOCKS_MARKET,
        lendingMarketAuthority: MARKET_AUTHORITY,
        kaminoReserve: kamino.reserve,
        reserveLiquiditySupply: kamino.liquidityVault,
        reserveCollateralMint: kamino.collateralMint,
        userDestinationCollateral: ctokenAta,
        instructionSysvarAccount: INSTRUCTIONS_SYSVAR,
        tokenProgram: tp,
        collateralTokenProgram: TOKEN_PROGRAM_ID,
        associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .rpc();
    log("lite-open", `${key} equity=${args.equity} levBps=${leverageBps.toString()} tx=${sig}`);
  },

  "lite-supply": async ({ args, wallet, program }) => {
    if (!hasIx(program, "lite_supply", "liteSupply")) {
      log("skip", "liteSupply not in IDL — run `anchor build` and refresh the IDL");
      return;
    }
    const key = kaminoLiteKey(optionalArg(args, "symbol", "USDC"));
    const amount = toBaseUnits(requireArg(args, "amount"), ASSET_DECIMALS[key]);
    // Attributes a same-transaction borrow of this mint to the Kamino position, so a
    // later lite-reduce/lite-close repays it instead of forwarding everything to the
    // wallet — see `lite_supply`'s `attribute_shares_delta` arg. 0 (default) for a
    // plain unleveraged supply.
    const attributeSharesDelta = new anchor.BN(optionalArg(args, "attribute", "0"));
    const mint = ASSET_MINTS[key];
    const kamino = KAMINO_RESERVES[key];
    const tp = tokenProgramFor(key);
    const [protocolConfig] = protocolConfigPda();
    const [assetConfig] = assetConfigPda(mint);
    const [margin] = marginPda(wallet.publicKey);
    const [reserve] = reservePda(mint);
    const [debtPosition] = debtPositionPda(margin, reserve);
    const [liteStrategy] = liteStrategyPda(mint);
    const [litePosition] = litePositionPda(margin);
    const marginSourceAccount = getAssociatedTokenAddressSync(mint, margin, true, tp);
    const marginDestinationCollateral = getAssociatedTokenAddressSync(kamino.collateralMint, margin, true, TOKEN_PROGRAM_ID);

    const sig = await method(program, "lite_supply", "liteSupply")(amount, attributeSharesDelta)
      .accounts({
        owner: wallet.publicKey,
        protocolConfig,
        assetConfig,
        underlyingMint: mint,
        marginAccount: margin,
        marginSourceAccount,
        reserve,
        debtPosition,
        liteStrategy,
        litePosition,
        kaminoProgram: KLEND,
        lendingMarket: kamino.market,
        lendingMarketAuthority: kamino.marketAuthority,
        kaminoReserve: kamino.reserve,
        reserveLiquiditySupply: kamino.liquidityVault,
        reserveCollateralMint: kamino.collateralMint,
        marginDestinationCollateral,
        instructionSysvarAccount: INSTRUCTIONS_SYSVAR,
        tokenProgram: tp,
        collateralTokenProgram: TOKEN_PROGRAM_ID,
        associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .rpc();
    log("lite-supply", `${key} amount=${args.amount} attribute=${attributeSharesDelta.toString()} tx=${sig}`);
  },

  status: async ({ args, program }) => {
    const owner = new PublicKey(requireArg(args, "owner"));
    const [margin] = marginPda(owner);
    const [litePosition] = litePositionPda(margin);
    try {
      const pos = await (program.account as any).litePosition.fetch(litePosition);
      console.log(JSON.stringify({
        margin: margin.toBase58(),
        litePosition: litePosition.toBase58(),
        underlyingMint: pos.underlyingMint?.toBase58?.() ?? pos.underlying_mint?.toBase58?.(),
        equity: pos.equityUnderlying?.toString?.() ?? pos.equity_underlying?.toString?.(),
        deposited: pos.depositedUnderlying?.toString?.() ?? pos.deposited_underlying?.toString?.(),
        kaminoCollateral: pos.kaminoCollateralAmount?.toString?.() ?? pos.kamino_collateral_amount?.toString?.(),
      }, null, 2));
    } catch {
      console.log(JSON.stringify({ margin: margin.toBase58(), litePosition: null }, null, 2));
    }
  },
};

async function main() {
  const argv = process.argv.slice(2);
  const command = argv[0];
  if (!command || !commands[command]) {
    console.log("Usage: npx tsx src/lite-fork.ts <command> [--flag value]");
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
