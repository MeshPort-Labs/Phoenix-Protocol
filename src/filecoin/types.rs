use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageDeal {
    pub deal_id: String,
    pub content_cid: String,
    pub size: u64,
    pub emergency: bool,
    pub provider: String,
    pub created_at: DateTime<Utc>,
    pub retrieval_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageManifest {
    pub message_id: String,
    pub shard_deals: Vec<StorageDeal>,
    pub threshold_needed: usize,
    pub emergency: bool,
    pub created_at: DateTime<Utc>,
    pub manifest_cid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThresholdMessage {
    pub message_id: String,
    pub encrypted_shards: Vec<Vec<u8>>,
    pub threshold: usize,
    pub total_shards: usize,
    pub emergency: bool,
    pub sender: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilecoinStatus {
    pub connected: bool,
    pub network: String,
    pub latency: Option<u64>,
    pub last_check: DateTime<Utc>,
}

#[derive(Debug)]
pub enum FilecoinError {
    ConnectionError(String),
    StorageError(String),
    RetrievalError(String),
    SerializationError(String),
    ThresholdError(String),
}

impl std::fmt::Display for FilecoinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FilecoinError::ConnectionError(msg) => write!(f, "Connection error: {}", msg),
            FilecoinError::StorageError(msg) => write!(f, "Storage error: {}", msg),
            FilecoinError::RetrievalError(msg) => write!(f, "Retrieval error: {}", msg),
            FilecoinError::SerializationError(msg) => write!(f, "Serialization error: {}", msg),
            FilecoinError::ThresholdError(msg) => write!(f, "Threshold error: {}", msg),
        }
    }
}

impl std::error::Error for FilecoinError {}