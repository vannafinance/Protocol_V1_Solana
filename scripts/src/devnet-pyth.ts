import * as anchor from "@coral-xyz/anchor";
import { Connection, PublicKey } from "@solana/web3.js";
import { HermesClient } from "@pythnetwork/hermes-client";
import { PythSolanaReceiver } from "@pythnetwork/pyth-solana-receiver";
import { sendTransactions } from "@pythnetwork/solana-utils";
import { AssetKey, PYTH_FEED_IDS, PYTH_SHARD_ID } from "./devnet-env";

const HERMES_URL = "https://hermes.pyth.network";

/**
 * Posts a fresh, real Pyth price update for `asset` to its long-lived Pyth "price feed account"
 * on Devnet, then returns that account's address to pass as the `price_update` /
 * `debt_price_update` / `collateral_price_update` account in a vanna_lending instruction.
 *
 * Unlike a local Surfpool surfnet, Devnet has no cheatcode to fabricate a `PriceUpdateV2` account
 * — this fetches a real signed price update from Pyth's Hermes service and posts it on-chain for
 * real, exactly what a production integration does. The price feed account address is a stable
 * PDA (derived from `PYTH_SHARD_ID` + the feed ID), so every script can compute it independently
 * without any shared state file — just refresh it immediately before using it, since
 * `admin_register_asset` sets `max_price_age_secs = 3600` for both assets in these scripts.
 */
export async function refreshPrice(
  connection: Connection,
  wallet: anchor.Wallet,
  asset: AssetKey,
): Promise<PublicKey> {
  const feedId = PYTH_FEED_IDS[asset];
  const hermes = new HermesClient(HERMES_URL);
  const priceUpdate = await hermes.getLatestPriceUpdates([feedId], { encoding: "base64" });

  const receiver = new PythSolanaReceiver({ connection, wallet });
  const builder = receiver.newTransactionBuilder({});
  await builder.addUpdatePriceFeed(priceUpdate.binary.data, PYTH_SHARD_ID);
  const txs = await builder.buildVersionedTransactions({ computeUnitPriceMicroLamports: 50_000 });
  await sendTransactions(txs, connection, wallet as never);

  return receiver.getPriceFeedAccountAddress(PYTH_SHARD_ID, feedId);
}

export interface LivePrice {
  price: bigint;
  exponent: number;
  confidence: bigint;
  publishTime: bigint;
}

/**
 * Reads the current real Pyth price for `asset` directly from Hermes — no transaction, no wallet,
 * no on-chain post. Use this for anything read-only (a displayed health factor, a preview value);
 * only instructions that actually need a validated on-chain `PriceUpdateV2` account should call
 * `refreshPrice` above.
 */
export async function fetchLivePrice(asset: AssetKey): Promise<LivePrice> {
  const hermes = new HermesClient(HERMES_URL);
  const result = await hermes.getLatestPriceUpdates([PYTH_FEED_IDS[asset]], { parsed: true });
  const parsed = result.parsed?.[0];
  if (!parsed) throw new Error(`Hermes returned no parsed price update for ${asset}`);
  return {
    price: BigInt(parsed.price.price),
    exponent: parsed.price.expo,
    confidence: BigInt(parsed.price.conf),
    publishTime: BigInt(parsed.price.publish_time),
  };
}
