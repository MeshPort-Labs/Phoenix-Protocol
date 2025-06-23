use serde::{Serialize, Deserialize};
use chrono::{DateTime, Utc};
use uuid::Uuid;

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