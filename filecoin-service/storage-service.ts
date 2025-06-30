import type { StorageDeal, StorageManifest } from './storacha-client';
import { initStorachaClient, storeShard, retrieveShard } from './storacha-client';

let storachaClient: any = null;

export async function initStorageService(spaceDid: string) {
  storachaClient = await initStorachaClient({
    spaceDid,
    serviceUrl: 'https://api.storacha.network',
    retries: 3
  });
}

export async function handleStoreRequest(shardData: Uint8Array, emergency: boolean) {
  return await storeShard(storachaClient, shardData, emergency);
}

export async function handleRetrieveRequest(contentCid: string) {
  return await retrieveShard(storachaClient, contentCid);
}

export async function createStorageManifest(
  messageId: string,
  deals: StorageDeal[],
  thresholdNeeded: number,
  emergency: boolean
): Promise<StorageManifest> {
  const manifest: Omit<StorageManifest, 'manifestCid'> = {
    messageId,
    shardDeals: deals,
    thresholdNeeded,
    emergency,
    createdAt: new Date().toISOString(),
  };
  // Store manifest as a file in Storacha
  const manifestFile = new File([
    JSON.stringify(manifest)
  ], `manifest-${messageId}.json`, { type: 'application/json' });
  const cid = await storachaClient.uploadFile(manifestFile);
  return { ...manifest, manifestCid: cid.toString() } as StorageManifest;
}

export async function getStorageManifest(manifestCid: string): Promise<StorageManifest> {
  const data = await retrieveShard(storachaClient, manifestCid);
  const json = new TextDecoder().decode(data);
  return JSON.parse(json);
} 