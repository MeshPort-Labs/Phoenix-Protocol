use serde::{Serialize, Deserialize};
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::crypto::DecryptionShare;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EncryptedMessage {
    pub id: String,
    pub sender: String,
    pub encrypted_content: Vec<u8>,
    pub timestamp: DateTime<Utc>,
    pub ttl: u32,
    pub hop_count: u32,
    pub threshold_needed: usize,
}

impl EncryptedMessage {
    pub fn new(sender: String, encrypted_content: Vec<u8>, threshold_needed: usize, ttl: u32) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            sender,
            encrypted_content,
            timestamp: Utc::now(),
            ttl,
            hop_count: 0,
            threshold_needed,
        }
    }
    
    pub fn decrement_ttl(&mut self) -> bool {
        if self.ttl > 0 {
            self.ttl -= 1;
            self.hop_count += 1;
            true
        } else {
            false
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ShareRequest {
    pub message_id: String,
    pub requester: String,
    pub requester_name: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ShareResponse {
    pub message_id: String,
    pub provider: String,
    pub provider_name: String,
    pub share: DecryptionShare,
    pub timestamp: DateTime<Utc>,
}

impl ShareRequest {
    pub fn new(message_id: String, requester: String, requester_name: String) -> Self {
        Self {
            message_id,
            requester,
            requester_name,
            timestamp: Utc::now(),
        }
    }
}

impl ShareResponse {
    pub fn new(message_id: String, provider: String, provider_name: String, share: DecryptionShare) -> Self {
        Self {
            message_id,
            provider,
            provider_name,
            share,
            timestamp: Utc::now(),
        }
    }
}