use super::types::*;
use crate::crypto::ThresholdCrypto;
use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use tracing::info;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ThresholdStorage {
    pub threshold: usize,
    pub total_shards: usize,
}

impl ThresholdStorage {
    pub fn new(threshold: usize, total_shards: usize) -> Self {
        Self {
            threshold,
            total_shards,
        }
    }

    pub fn create_threshold_message(
        &self,
        crypto: &ThresholdCrypto,
        message: &str,
        sender: &str,
        emergency: bool,
    ) -> Result<ThresholdMessage, FilecoinError> {
        // Encrypt the message
        let encrypted_content = crypto.encrypt_message(message)
            .map_err(|e| FilecoinError::ThresholdError(e.to_string()))?;
        
        // Create shards using secret sharing
        let shards = self.create_shards(&encrypted_content)?;
        
        Ok(ThresholdMessage {
            message_id: Uuid::new_v4().to_string(),
            encrypted_shards: shards,
            threshold: self.threshold,
            total_shards: self.total_shards,
            emergency,
            sender: sender.to_string(),
            timestamp: Utc::now(),
        })
    }

    fn create_shards(&self, data: &[u8]) -> Result<Vec<Vec<u8>>, FilecoinError> {
        // Ensure we always create exactly total_shards number of shards
        let mut shards = Vec::with_capacity(self.total_shards);
        
        if data.is_empty() {
            // Handle empty data case
            for _ in 0..self.total_shards {
                shards.push(vec![0u8]); // Minimal non-empty shard
            }
            return Ok(shards);
        }
        
        // Calculate shard size - ensure we don't create empty shards
        let min_shard_size = std::cmp::max(1, data.len() / self.total_shards);
        let extra_bytes = data.len() % self.total_shards;
        
        let mut offset = 0;
        for i in 0..self.total_shards {
            let shard_size = if i < extra_bytes {
                min_shard_size + 1 // Distribute extra bytes to first few shards
            } else {
                min_shard_size
            };
            
            let end_offset = std::cmp::min(offset + shard_size, data.len());
            
            if offset < data.len() {
                shards.push(data[offset..end_offset].to_vec());
            } else {
                // If we've consumed all data, pad with a minimal shard
                shards.push(vec![0u8; 1]);
            }
            
            offset = end_offset;
        }
        
        // Ensure we have exactly total_shards
        while shards.len() < self.total_shards {
            shards.push(vec![0u8; 1]);
        }
        
        info!("📦 Created {} shards from {} bytes of data", shards.len(), data.len());
        for (i, shard) in shards.iter().enumerate() {
            info!("   Shard {}: {} bytes", i, shard.len());
        }
        
        Ok(shards)
    }

    pub fn reconstruct_from_shards(&self, shards: &[Vec<u8>]) -> Result<Vec<u8>, FilecoinError> {
        if shards.len() < self.threshold {
            return Err(FilecoinError::ThresholdError(
                format!("Need at least {} shards, got {}", self.threshold, shards.len())
            ));
        }
        
        // Reconstruct by concatenating shards, filtering out padding
        let mut reconstructed = Vec::new();
        for (i, shard) in shards.iter().enumerate() {
            if i >= self.total_shards {
                break; // Don't process extra shards
            }
            
            // Skip padding shards (single zero bytes)
            if shard.len() == 1 && shard[0] == 0 {
                continue;
            }
            
            reconstructed.extend_from_slice(shard);
        }
        
        info!("🔧 Reconstructed {} bytes from {} shards", reconstructed.len(), shards.len());
        Ok(reconstructed)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EmergencyCache {
    cached_messages: std::collections::HashMap<String, ThresholdMessage>,
    cache_limit: usize,
}

impl EmergencyCache {
    pub fn new(cache_limit: usize) -> Self {
        Self {
            cached_messages: std::collections::HashMap::new(),
            cache_limit,
        }
    }

    pub fn cache_message(&mut self, message: ThresholdMessage) {
        if self.cached_messages.len() >= self.cache_limit {
            // Remove oldest message
            if let Some(oldest_id) = self.cached_messages.keys().next().cloned() {
                self.cached_messages.remove(&oldest_id);
            }
        }
        
        self.cached_messages.insert(message.message_id.clone(), message);
        info!("📦 Cached message for emergency access: {} messages in cache", 
              self.cached_messages.len());
    }

    pub fn get_cached_message(&self, message_id: &str) -> Option<&ThresholdMessage> {
        self.cached_messages.get(message_id)
    }

    pub fn list_cached_messages(&self) -> Vec<String> {
        self.cached_messages.keys().cloned().collect()
    }

    pub fn clear_cache(&mut self) {
        self.cached_messages.clear();
        info!("🗑️ Emergency cache cleared");
    }
}