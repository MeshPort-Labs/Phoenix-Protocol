import express from 'express';
import {
  initStorageService,
  handleStoreRequest,
  handleRetrieveRequest,
  createStorageManifest,
  getStorageManifest
} from './storage-service';
import {
  storeAllShards,
  retrieveRequiredShards,
  storeManifestWithRedundancy
} from './storacha-client';

const SPACE_DID = 'did:key:z6MkjFNNGTrYCXJz9zNry7X6YfAsRE82i69hHMhQiKpSQN3G';
let storachaClient: any = null;

(async () => {
  await initStorageService(SPACE_DID);
  // Optionally, get the client instance if needed for batch ops
  const { initStorachaClient } = await import('./storacha-client');
  storachaClient = await initStorachaClient({
    spaceDid: SPACE_DID,
    serviceUrl: 'https://api.storacha.network',
    retries: 3
  });

  const app = express();
  app.use(express.json());

  // Store single shard
  app.post('/api/store', async (req, res) => {
    try {
      const { data, emergency } = req.body;
      const shardData = new Uint8Array(Buffer.from(data, 'base64'));
      const deal = await handleStoreRequest(shardData, emergency);
      res.json(deal);
    } catch (err: any) {
      res.status(500).json({ error: err.message });
    }
  });

  // Retrieve single shard
  app.get('/api/retrieve/:cid', async (req, res) => {
    try {
      const { cid } = req.params;
      const data = await handleRetrieveRequest(cid);
      res.json({
        data: Buffer.from(data).toString('base64'),
        cid,
        size: data.length
      });
    } catch (err: any) {
      res.status(500).json({ error: err.message });
    }
  });

  // Create manifest
  app.post('/api/manifest', async (req, res) => {
    try {
      const { messageId, deals, thresholdNeeded, emergency } = req.body;
      const manifest = await createStorageManifest(messageId, deals, thresholdNeeded, emergency);
      res.json(manifest);
    } catch (err: any) {
      res.status(500).json({ error: err.message });
    }
  });

  // Get manifest by CID
  app.get('/api/manifest/:cid', async (req, res) => {
    try {
      const { cid } = req.params;
      const manifest = await getStorageManifest(cid);
      res.json(manifest);
    } catch (err: any) {
      res.status(500).json({ error: err.message });
    }
  });

  // Health check
  app.get('/api/status', async (req, res) => {
    try {
      const { checkStorachaStatus } = await import('./storacha-client');
      const status = await checkStorachaStatus(storachaClient);
      res.json(status);
    } catch (err: any) {
      res.status(500).json({ error: err.message });
    }
  });

  // Store all 9 shards in parallel (batch)
  app.post('/api/store-batch', async (req, res) => {
    try {
      const { shards, messageId, emergency } = req.body;
      if (!Array.isArray(shards) || shards.length !== 9) throw new Error('Must provide 9 base64-encoded shards');
      const shardDataArr = shards.map((b64: string) => new Uint8Array(Buffer.from(b64, 'base64')));
      const deals = await storeAllShards(storachaClient, shardDataArr, messageId, emergency);
      res.json(deals);
    } catch (err: any) {
      res.status(500).json({ error: err.message });
    }
  });

  // Retrieve N shards from a manifest (batch)
  app.post('/api/retrieve-batch', async (req, res) => {
    try {
      const { manifest, requiredCount } = req.body;
      const shards = await retrieveRequiredShards(storachaClient, manifest, requiredCount || 5);
      res.json({
        shards: shards.map((data) => Buffer.from(data).toString('base64')),
        count: shards.length
      });
    } catch (err: any) {
      res.status(500).json({ error: err.message });
    }
  });

  // Store manifest with redundancy (batch)
  app.post('/api/manifest-batch', async (req, res) => {
    try {
      const { manifest, emergency } = req.body;
      const cids = await storeManifestWithRedundancy(storachaClient, manifest, emergency);
      res.json({ cids });
    } catch (err: any) {
      res.status(500).json({ error: err.message });
    }
  });

  app.listen(8080, () => {
    console.log('Storacha service running on http://localhost:8080');
  });
})();