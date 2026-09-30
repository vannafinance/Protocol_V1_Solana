import * as anchor from "@coral-xyz/anchor";
import { Connection, PublicKey } from "@solana/web3.js";
import { HermesClient } from "@pythnetwork/hermes-client";
import { PythSolanaReceiver } from "@pythnetwork/pyth-solana-receiver";
import { sendTransactions } from "@pythnetwork/solana-utils";
import { PYTH_FEED_IDS, PYTH_RECEIVER_PROGRAM_ID, PYTH_SHARD_ID, PythFeedKey, pythFeedAccount } from "./devnet-env";

const HERMES_URL = process.env.HERMES_URL ?? "https://hermes.pyth.network";

function hermesClient(): HermesClient {
  const key = process.env.PYTH_API_KEY;
  return new HermesClient(HERMES_URL, key ? { headers: { Authorization: `Bearer ${key}` } } : undefined);
}

const REFERENCE_PRICE: Record<PythFeedKey, number> = {
  usdc: 1,
  usdt: 1,
  wsol: 118,
  jupsolRate: 1.21,
  jupusd: 1,
  eth: 2700,
  btc: 100000,
};

const PRICE_UPDATE_V2_DISCRIMINATOR = Buffer.from([0x22, 0xf1, 0x23, 0x63, 0x9d, 0x7e, 0xf4, 0xcd]);
const PRICE_FEED_ID_OFFSET = 41;
export const PRICE_OFFSET = 73;
export const CONF_OFFSET = 81;
export const EXPONENT_OFFSET = 89;
export const PUBLISH_TIME_OFFSET = 93;
const PREV_PUBLISH_TIME_OFFSET = 101;
export const EMA_PRICE_OFFSET = 109;
const EMA_CONF_OFFSET = 117;
const MIN_PRICE_ACCOUNT_SIZE = 134;

function hexToBytes(hex: string): Buffer {
  const clean = hex.startsWith("0x") ? hex.slice(2) : hex;
  return Buffer.from(clean, "hex");
}

const priceFeedAccountAddress = pythFeedAccount;

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

export async function callForkCheatcode(rpcUrl: string, method: string, params: unknown[]): Promise<void> {
  const response = await fetch(rpcUrl, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
  });
  const body = (await response.json()) as { error?: { message: string } };
  if (body.error) throw new Error(`${method} failed: ${body.error.message}`);
}

async function fabricatePrice(connection: Connection, feed: PythFeedKey): Promise<PublicKey> {
  const feedId = PYTH_FEED_IDS[feed];
  const address = priceFeedAccountAddress(feedId);

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

  const override = process.env[`FORK_PRICE_${feed.toUpperCase()}`];
  const price = override ? Number(override) : REFERENCE_PRICE[feed];
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

export async function refreshPrice(
  connection: Connection,
  wallet: anchor.Wallet,
  feed: PythFeedKey,
): Promise<PublicKey> {
  try {
    const feedId = PYTH_FEED_IDS[feed];
    const hermes = hermesClient();
    const priceUpdate = await hermes.getLatestPriceUpdates([feedId], { encoding: "base64" });

    const receiver = new PythSolanaReceiver({ connection, wallet });
    const builder = receiver.newTransactionBuilder({});
    await builder.addUpdatePriceFeed(priceUpdate.binary.data, PYTH_SHARD_ID);
    const txs = await builder.buildVersionedTransactions({ computeUnitPriceMicroLamports: 50_000 });
    await sendTransactions(txs, connection, wallet as never);

    return receiver.getPriceFeedAccountAddress(PYTH_SHARD_ID, feedId);
  } catch (err) {
    console.warn(`[pyth] real Hermes update for ${feed} failed (${(err as Error).message}); fabricating on fork`);
    return fabricatePrice(connection, feed);
  }
}
