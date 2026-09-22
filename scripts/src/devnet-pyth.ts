import * as anchor from "@coral-xyz/anchor";
import { Connection, PublicKey } from "@solana/web3.js";
import { HermesClient } from "@pythnetwork/hermes-client";
import { PythSolanaReceiver } from "@pythnetwork/pyth-solana-receiver";
import { sendTransactions } from "@pythnetwork/solana-utils";
import { AssetKey, DEVNET_RPC_URL, PYTH_FEED_IDS, PYTH_SHARD_ID } from "./devnet-env";

const HERMES_URL = "https://hermes.pyth.network";

/** Sentinel USD prices used only when fabricating a PriceUpdateV2 on the local Surfpool fork
 * (real Hermes has no feed for the PreStocks tokens at all, and as of 2026-08-26 requires an API
 * key for every feed — see PYTH_API_KEY in the frontend's .env.local). Not used against real
 * Devnet/mainnet. */
const REFERENCE_PRICE_USD: Record<AssetKey, number> = {
  usdc: 1,
  wsol: 190,
  tslax: 250,
  googlx: 175,
  aaplx: 340,
  anthropic: 1020,
  openai: 1030,
};

const PYTH_RECEIVER_PROGRAM_ID = new PublicKey("rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ");
const PYTH_PUSH_ORACLE_PROGRAM_ID = new PublicKey("pythWSnswVUd12oZpeFP8e9CVaEqJg25g1Vtc2biRsT");

const PRICE_UPDATE_V2_DISCRIMINATOR = Buffer.from([0x22, 0xf1, 0x23, 0x63, 0x9d, 0x7e, 0xf4, 0xcd]);
const PRICE_FEED_ID_OFFSET = 41;
const PRICE_OFFSET = 73;
const CONF_OFFSET = 81;
const EXPONENT_OFFSET = 89;
const PUBLISH_TIME_OFFSET = 93;
const PREV_PUBLISH_TIME_OFFSET = 101;
const EMA_PRICE_OFFSET = 109;
const EMA_CONF_OFFSET = 117;
const MIN_PRICE_ACCOUNT_SIZE = 134;

function hexToBytes(hex: string): Buffer {
  const clean = hex.startsWith("0x") ? hex.slice(2) : hex;
  return Buffer.from(clean, "hex");
}

function priceFeedAccountAddress(feedId: string): PublicKey {
  return PublicKey.findProgramAddressSync(
    [Buffer.from([0, 0]), hexToBytes(feedId)],
    PYTH_PUSH_ORACLE_PROGRAM_ID,
  )[0];
}

/** Builds a fresh PriceUpdateV2 account buffer from a template (any existing PriceUpdateV2 —
 * layout is identical across feeds), rewriting the feed ID/price/timestamps for `asset`. */
function buildForkPriceAccountData(template: Buffer, feedId: string, usdPrice: number): Buffer {
  const data = Buffer.from(template);
  if (data.length < MIN_PRICE_ACCOUNT_SIZE) {
    throw new Error(`template PriceUpdateV2 account too small (${data.length} bytes)`);
  }
  PRICE_UPDATE_V2_DISCRIMINATOR.copy(data, 0);
  hexToBytes(feedId).copy(data, PRICE_FEED_ID_OFFSET);
  const exponent = -8;
  const scale = 10 ** -exponent;
  const rawPrice = BigInt(Math.round(usdPrice * scale));
  const conf = BigInt(Math.max(1, Math.round(usdPrice * 0.002 * scale)));
  const now = BigInt(Math.floor(Date.now() / 1000));
  data.writeBigInt64LE(rawPrice, PRICE_OFFSET);
  data.writeBigUInt64LE(conf, CONF_OFFSET);
  data.writeInt32LE(exponent, EXPONENT_OFFSET);
  data.writeBigInt64LE(now, PUBLISH_TIME_OFFSET);
  data.writeBigInt64LE(now - 1n, PREV_PUBLISH_TIME_OFFSET);
  data.writeBigInt64LE(rawPrice, EMA_PRICE_OFFSET);
  data.writeBigUInt64LE(conf, EMA_CONF_OFFSET);
  return data;
}

async function callForkCheatcode(rpcUrl: string, method: string, params: unknown[]): Promise<void> {
  const response = await fetch(rpcUrl, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
  });
  const body = (await response.json()) as { error?: { message: string } };
  if (body.error) throw new Error(`${method} failed: ${body.error.message}`);
}

/** Fabricates (or refreshes) `asset`'s PriceUpdateV2 account directly on the Surfpool fork via
 * the `surfnet_setAccount` cheatcode — no Hermes call, no wallet signature. Used as the fallback
 * whenever real Hermes is unreachable (401 without an API key) and for the PreStocks tokens,
 * which have no real Pyth feed to begin with. */
async function fabricatePrice(connection: Connection, asset: AssetKey): Promise<PublicKey> {
  const feedId = PYTH_FEED_IDS[asset];
  const address = priceFeedAccountAddress(feedId);

  // Any existing PriceUpdateV2 account (this asset's own, if already fabricated once, else any
  // other feed's) works as the byte-layout template — the layout is identical for every feed.
  let template = await connection.getAccountInfo(address);
  if (!template?.owner.equals(PYTH_RECEIVER_PROGRAM_ID)) {
    const solAddress = priceFeedAccountAddress(PYTH_FEED_IDS.wsol);
    template = await connection.getAccountInfo(solAddress);
  }
  if (!template?.owner.equals(PYTH_RECEIVER_PROGRAM_ID)) {
    throw new Error(
      "No existing PriceUpdateV2 template found on the fork to clone the byte layout from " +
        "— refresh SOL's real price at least once first, or run against a fork that has cloned " +
        "mainnet Pyth accounts.",
    );
  }

  const price = REFERENCE_PRICE_USD[asset];
  const data = buildForkPriceAccountData(template.data, feedId, price);
  await callForkCheatcode(connection.rpcEndpoint, "surfnet_setAccount", [
    address.toBase58(),
    {
      lamports: template.lamports,
      data: data.toString("hex"),
      owner: PYTH_RECEIVER_PROGRAM_ID.toBase58(),
      executable: false,
      rentEpoch: 0,
    },
  ]);
  return address;
}

/**
 * Posts a fresh Pyth price update for `asset` to its long-lived Pyth "price feed account" and
 * returns that account's address to pass as the `price_update` / `debt_price_update` /
 * `collateral_price_update` account in a vanna_lending instruction.
 *
 * Tries a real, signed Hermes update first (what a production integration does); falls back to
 * fabricating the PriceUpdateV2 directly on the local Surfpool fork via `surfnet_setAccount` when
 * Hermes is unreachable (e.g. the 401-without-an-API-key case) or when `asset` has no real Pyth
 * feed at all (the PreStocks tokens — Hermes returns an empty result for their synthetic sentinel
 * feed IDs).
 */
export async function refreshPrice(
  connection: Connection,
  wallet: anchor.Wallet,
  asset: AssetKey,
): Promise<PublicKey> {
  try {
    const feedId = PYTH_FEED_IDS[asset];
    const hermes = new HermesClient(HERMES_URL);
    const priceUpdate = await hermes.getLatestPriceUpdates([feedId], { encoding: "base64" });

    const receiver = new PythSolanaReceiver({ connection, wallet });
    const builder = receiver.newTransactionBuilder({});
    await builder.addUpdatePriceFeed(priceUpdate.binary.data, PYTH_SHARD_ID);
    const txs = await builder.buildVersionedTransactions({ computeUnitPriceMicroLamports: 50_000 });
    await sendTransactions(txs, connection, wallet as never);

    return receiver.getPriceFeedAccountAddress(PYTH_SHARD_ID, feedId);
  } catch (err) {
    console.warn(`[pyth] real Hermes update for ${asset} failed (${(err as Error).message}); fabricating on fork`);
    return fabricatePrice(connection, asset);
  }
}

export interface LivePrice {
  price: bigint;
  exponent: number;
  confidence: bigint;
  publishTime: bigint;
}

/**
 * Reads the current real Pyth price for `asset` directly from Hermes — no transaction, no wallet,
 * no on-chain post. Falls back to the fork's already-fabricated on-chain PriceUpdateV2 (if any)
 * when Hermes is unreachable or has no feed for `asset`.
 */
export async function fetchLivePrice(asset: AssetKey): Promise<LivePrice> {
  try {
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
  } catch (err) {
    console.warn(`[pyth] real Hermes read for ${asset} failed (${(err as Error).message}); reading fork account`);
    const connection = new Connection(DEVNET_RPC_URL, "confirmed");
    const address = priceFeedAccountAddress(PYTH_FEED_IDS[asset]);
    const info = await connection.getAccountInfo(address);
    if (!info?.owner.equals(PYTH_RECEIVER_PROGRAM_ID)) {
      throw new Error(`No fabricated PriceUpdateV2 for ${asset} on the fork yet — call refreshPrice first`);
    }
    const price = info.data.readBigInt64LE(PRICE_OFFSET);
    const exponent = info.data.readInt32LE(EXPONENT_OFFSET);
    const confidence = info.data.readBigUInt64LE(CONF_OFFSET);
    const publishTime = info.data.readBigInt64LE(PUBLISH_TIME_OFFSET);
    return { price, exponent, confidence, publishTime };
  }
}
