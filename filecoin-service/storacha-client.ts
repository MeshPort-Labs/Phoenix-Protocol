import { create } from '@web3-storage/w3up-client';

interface StorachaConfig {
  spaceDid: string;
  serviceUrl: string;
  retries: number;
}

export interface StorageDeal {
  dealId: string;
  contentCid: string;
  size: number;
  emergency: boolean;
  provider: string;
  createdAt: string;
  retrievalUrl?: string;
}

export interface StorageManifest {
  messageId: string;
  shardDeals: StorageDeal[];
  thresholdNeeded: number;
  emergency: boolean;
  createdAt: string;
  manifestCid: string;
}

export async function initStorachaClient(config: StorachaConfig): Promise<any> {
  try {
    const client = await create();
    await client.login('mintugogoi567@gmail.com');

    await client.setCurrentSpace(config.spaceDid as `did:${string}:${string}`);

    try {
      await client.capability.upload.list({ size: 1 });
    } catch (err) {
      throw new Error('Storacha client health check failed: ' + (err as Error).message);
    }

    console.log('✅ Storacha client initialized and healthy (space: ' + config.spaceDid + ')');
    return client;
  } catch (error) {
    console.error('❌ Failed to initialize Storacha client:', error);
    throw error;
  }
}

export async function storeShard(
  client: any,
  shardData: Uint8Array,
  emergency: boolean
): Promise<StorageDeal> {
  // Ensure shardData is a regular Uint8Array for Blob compatibility
  const file = new File([new Uint8Array(shardData)], `shard-${Date.now()}.bin`);

  // Upload the file to Storacha
  const cid = await client.uploadFile(file);

  // Build the StorageDeal object
  const deal: StorageDeal = {
    dealId: `deal-${Date.now()}-${Math.random()}`,
    contentCid: cid.toString(),
    size: shardData.length,
    emergency,
    provider: 'storacha-network',
    createdAt: new Date().toISOString(),
    retrievalUrl: `https://${cid.toString()}.ipfs.w3s.link`,
  };

  return deal;
}

export async function retrieveShard(
  client: any,
  contentCid: string
): Promise<Uint8Array> {
  // Fetch the file from the gateway as an ArrayBuffer
  const url = `https://${contentCid}.ipfs.w3s.link`;
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`Failed to fetch shard: ${response.status} ${response.statusText}`);
  }
  const arrayBuffer = await response.arrayBuffer();
  return new Uint8Array(arrayBuffer);
}

export async function checkStorachaStatus(client: any): Promise<{
  connected: boolean;
  network: string;
  latency?: number;
}> {
  const start = Date.now();
  try {
    await client.capability.upload.list({ size: 1 });
    const latency = Date.now() - start;
    return {
      connected: true,
      network: 'storacha',
      latency
    };
  } catch {
    return {
      connected: false,
      network: 'storacha'
    };
  }
}

// Store all 9 shards for a message in parallel, with retry and redundancy
export async function storeAllShards(
  client: any,
  shards: Uint8Array[],
  messageId: string,
  emergency: boolean
): Promise<StorageDeal[]> {
  if (shards.length !== 9) throw new Error('Must provide exactly 9 shards');
  const maxRetries = 2;
  const redundancy = emergency ? 2 : 1;
  const deals: StorageDeal[] = [];

  for (let r = 0; r < redundancy; r++) {
    const results = await Promise.allSettled(
      shards.map(async (shard, i) => {
        let lastErr;
        for (let attempt = 0; attempt <= maxRetries; attempt++) {
          try {
            return await storeShard(client, shard, emergency);
          } catch (err) {
            lastErr = err;
          }
        }
        throw lastErr;
      })
    );
    for (const res of results) {
      if (res.status === 'fulfilled') deals.push(res.value);
      // Optionally, log or handle rejected uploads
      console.log("deal upload rejected", res)
    }
  }
  return deals;
}

// Retrieve required number of shards for decryption
export async function retrieveRequiredShards(
  client: any,
  manifest: StorageManifest,
  requiredCount: number = 5
): Promise<Uint8Array[]> {
  const shards: Uint8Array[] = [];
  const errors: any[] = [];
  for (const deal of manifest.shardDeals) {
    try {
      const data = await retrieveShard(client, deal.contentCid);
      shards.push(data);
      if (shards.length >= requiredCount) break;
    } catch (err) {
      errors.push({ cid: deal.contentCid, error: err });
    }
  }
  if (shards.length < requiredCount) {
    throw new Error(`Could only retrieve ${shards.length} shards, needed ${requiredCount}`);
  }
  return shards;
}

// Store manifest with redundancy if emergency
export async function storeManifestWithRedundancy(
  client: any,
  manifest: StorageManifest,
  emergency: boolean
): Promise<string[]> {
  const manifestData = new TextEncoder().encode(JSON.stringify(manifest));
  const redundancy = emergency ? 3 : 1;
  const cids: string[] = [];
  for (let i = 0; i < redundancy; i++) {
    const file = new File([manifestData], `manifest-${manifest.messageId}-${i}.json`, { type: 'application/json' });
    const cid = await client.uploadFile(file);
    cids.push(cid.toString());
  }
  return cids;
}

(async () => {
  await initStorachaClient({
    spaceDid: 'did:key:z6MkjFNNGTrYCXJz9zNry7X6YfAsRE82i69hHMhQiKpSQN3G',
    serviceUrl: 'https://api.storacha.network',
    retries: 3
  });
})(); 