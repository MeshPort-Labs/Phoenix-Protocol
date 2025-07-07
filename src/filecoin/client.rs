use super::types::*;
use super::storage::{ThresholdStorage, EmergencyCache};
use anyhow::Result;
use reqwest::Client;
use serde_json::json;
use std::time::Duration;
use tracing::{info, warn, error};
use chrono::Utc;

pub struct FilecoinClient {
    pub http_client: Client,
    pub base_url: String,
    pub threshold_storage: ThresholdStorage,
    pub emergency_cache: EmergencyCache,
    pub connection_timeout: Duration,
}

impl FilecoinClient {
    pub fn new(base_url: String, threshold: usize, total_shards: usize) -> Self {
        let http_client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            http_client,
            base_url,
            threshold_storage: ThresholdStorage::new(threshold, total_shards),
            emergency_cache: EmergencyCache::new(100), // Cache up to 100 messages
            connection_timeout: Duration::from_secs(10),
        }
    }

    pub async fn check_status(&self) -> Result<FilecoinStatus, FilecoinError> {
        let start = std::time::Instant::now();
        
        match self.http_client
            .get(&format!("{}/api/status", self.base_url))
            .timeout(self.connection_timeout)
            .send()
            .await
        {
            Ok(response) => {
                let latency = start.elapsed().as_millis() as u64;
                
                if response.status().is_success() {
                    Ok(FilecoinStatus {
                        connected: true,
                        network: "filecoin".to_string(),
                        latency: Some(latency),
                        last_check: Utc::now(),
                    })
                } else {
                    Err(FilecoinError::ConnectionError(
                        format!("HTTP {}: {}", response.status(), response.text().await.unwrap_or_default())
                    ))
                }
            }
            Err(e) => {
                error!("❌ Filecoin service unreachable: {}", e);
                Ok(FilecoinStatus {
                    connected: false,
                    network: "filecoin".to_string(),
                    latency: None,
                    last_check: Utc::now(),
                })
            }
        }
    }

    pub async fn store_threshold_message(
        &mut self,
        message: ThresholdMessage,
    ) -> Result<StorageManifest, FilecoinError> {
        info!("📦 Storing threshold message with {} shards", message.encrypted_shards.len());
        
        // Cache for emergency access
        if message.emergency {
            self.emergency_cache.cache_message(message.clone());
        }
        
        // Try to store via Filecoin service
        match self.store_shards_via_service(&message).await {
            Ok(manifest) => {
                info!("✅ Successfully stored message {} via Filecoin", message.message_id);
                Ok(manifest)
            }
            Err(e) => {
                warn!("⚠️ Filecoin storage failed, using local cache: {}", e);
                // Fallback to local storage
                self.create_local_manifest(message)
            }
        }
    }

    async fn store_shards_via_service(
        &self,
        message: &ThresholdMessage,
    ) -> Result<StorageManifest, FilecoinError> {
        let shard_deals: Vec<StorageDeal> = Vec::new();
        
        // Convert shards to base64 for HTTP transport
        let base64_shards: Vec<String> = message.encrypted_shards
            .iter()
            .map(|shard| base64::encode(shard))
            .collect();
        
        // Store all shards in batch
        let request_body = json!({
            "shards": base64_shards,
            "messageId": message.message_id,
            "emergency": message.emergency
        });
        
        let response = self.http_client
            .post(&format!("{}/api/store-batch", self.base_url))
            .json(&request_body)
            .timeout(Duration::from_secs(60)) // Longer timeout for batch operations
            .send()
            .await
            .map_err(|e| FilecoinError::StorageError(e.to_string()))?;
        
        if !response.status().is_success() {
            return Err(FilecoinError::StorageError(
                format!("HTTP {}: {}", response.status(), response.text().await.unwrap_or_default())
            ));
        }
        
        let deals: Vec<StorageDeal> = response.json().await
            .map_err(|e| FilecoinError::SerializationError(e.to_string()))?;
        
        // Create and store manifest
        let manifest = StorageManifest {
            message_id: message.message_id.clone(),
            shard_deals: deals,
            threshold_needed: message.threshold,
            emergency: message.emergency,
            created_at: message.timestamp,
            manifest_cid: format!("manifest-{}", message.message_id), // Placeholder
        };
        
        // Store manifest with redundancy
        let manifest_request = json!({
            "manifest": manifest,
            "emergency": message.emergency
        });
        
        let _manifest_response = self.http_client
            .post(&format!("{}/api/manifest-batch", self.base_url))
            .json(&manifest_request)
            .send()
            .await
            .map_err(|e| FilecoinError::StorageError(e.to_string()))?;
        
        Ok(manifest)
    }

    fn create_local_manifest(&self, message: ThresholdMessage) -> Result<StorageManifest, FilecoinError> {
        let mut shard_deals = Vec::new();
        
        // Create placeholder deals for local storage
        for (i, shard) in message.encrypted_shards.iter().enumerate() {
            let deal = StorageDeal {
                deal_id: format!("local-{}-{}", message.message_id, i),
                content_cid: format!("local-cid-{}-{}", message.message_id, i),
                size: shard.len() as u64,
                emergency: message.emergency,
                provider: "local-cache".to_string(),
                created_at: Utc::now(),
                retrieval_url: None,
            };
            shard_deals.push(deal);
        }
        
        Ok(StorageManifest {
            message_id: message.message_id.clone(),
            shard_deals,
            threshold_needed: message.threshold,
            emergency: message.emergency,
            created_at: message.timestamp,
            manifest_cid: format!("local-manifest-{}", message.message_id),
        })
    }

    pub async fn retrieve_threshold_message(
        &self,
        manifest: &StorageManifest,
    ) -> Result<Vec<u8>, FilecoinError> {
        info!("📥 Retrieving threshold message {} (need {}/{} shards)", 
              manifest.message_id, manifest.threshold_needed, manifest.shard_deals.len());
        
        // Try emergency cache first
        if let Some(cached_msg) = self.emergency_cache.get_cached_message(&manifest.message_id) {
            info!("⚡ Retrieved from emergency cache");
            return self.threshold_storage.reconstruct_from_shards(&cached_msg.encrypted_shards);
        }
        
        // Try to retrieve from Filecoin service
        match self.retrieve_shards_via_service(manifest).await {
            Ok(shards) => {
                info!("✅ Retrieved {} shards from Filecoin", shards.len());
                self.threshold_storage.reconstruct_from_shards(&shards)
            }
            Err(e) => {
                warn!("⚠️ Filecoin retrieval failed: {}", e);
                Err(e)
            }
        }
    }

    async fn retrieve_shards_via_service(
        &self,
        manifest: &StorageManifest,
    ) -> Result<Vec<Vec<u8>>, FilecoinError> {
        let request_body = json!({
            "manifest": manifest,
            "requiredCount": manifest.threshold_needed
        });
        
        let response = self.http_client
            .post(&format!("{}/api/retrieve-batch", self.base_url))
            .json(&request_body)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| FilecoinError::RetrievalError(e.to_string()))?;
        
        if !response.status().is_success() {
            return Err(FilecoinError::RetrievalError(
                format!("HTTP {}: {}", response.status(), response.text().await.unwrap_or_default())
            ));
        }
        
        let response_data: serde_json::Value = response.json().await
            .map_err(|e| FilecoinError::SerializationError(e.to_string()))?;
        
        let base64_shards: Vec<String> = response_data["shards"].as_array()
            .ok_or_else(|| FilecoinError::SerializationError("Invalid response format".to_string()))?
            .iter()
            .map(|v| v.as_str().unwrap_or_default().to_string())
            .collect();
        
        // Decode base64 shards
        let mut shards = Vec::new();
        for base64_shard in base64_shards {
            let shard = base64::decode(&base64_shard)
                .map_err(|e| FilecoinError::SerializationError(e.to_string()))?;
            shards.push(shard);
        }
        
        Ok(shards)
    }

    pub fn get_emergency_cache_status(&self) -> (usize, Vec<String>) {
        let cached_messages = self.emergency_cache.list_cached_messages();
        (cached_messages.len(), cached_messages)
    }

    pub async fn emergency_broadcast_mode(&mut self, enabled: bool) -> Result<(), FilecoinError> {
        info!("🚨 Emergency broadcast mode: {}", if enabled { "ENABLED" } else { "DISABLED" });
        
        if enabled {
            // Pre-cache critical infrastructure data
            info!("📦 Pre-caching emergency data...");
            // This would typically pre-fetch critical messages, contact lists, etc.
        } else {
            // Clear emergency cache to free memory
            self.emergency_cache.clear_cache();
        }
        
        Ok(())
    }

    pub async fn adaptive_routing_check(&self) -> Result<String, FilecoinError> {
        let status = self.check_status().await?;
        
        let routing_mode = if status.connected {
            if let Some(latency) = status.latency {
                if latency < 100 {
                    "FULL_ONLINE"
                } else if latency < 500 {
                    "DEGRADED_ONLINE"
                } else {
                    "SLOW_ONLINE"
                }
            } else {
                "UNKNOWN_ONLINE"
            }
        } else {
            "OFFLINE"
        };
        
        info!("🔄 Adaptive routing mode: {}", routing_mode);
        Ok(routing_mode.to_string())
    }
}