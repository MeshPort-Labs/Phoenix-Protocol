use serde::{Deserialize, Serialize};

use threshold_crypto::{PublicKeySet, SecretKeySet, SecretKeyShare};
use anyhow::Result;
use std::fmt;
use rand::SeedableRng;
use std::collections::BTreeMap;

pub struct ThresholdCrypto {
    pub threshold: usize,
    pub total_shards: usize,
    pub my_shard_id: usize,
    _secret_key_set: SecretKeySet,
    public_key_set: PublicKeySet,
    my_secret_share: SecretKeyShare,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DecryptionShare {
    pub shard_id: usize,
    pub share_data: Vec<u8>,
}

#[derive(Debug)]
pub enum CryptoError {
    EncryptionFailed(String),
    DecryptionFailed(String),
    InvalidShard,
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CryptoError::EncryptionFailed(msg) => write!(f, "Encryption failed: {}", msg),
            CryptoError::DecryptionFailed(msg) => write!(f, "Decryption failed: {}", msg),
            CryptoError::InvalidShard => write!(f, "Invalid shard ID"),
        }
    }
}

impl std::error::Error for CryptoError {}

impl ThresholdCrypto {
    pub fn generate_keys(threshold: usize, total_shards: usize, my_shard_id: usize) -> Result<Self, CryptoError> {
        if my_shard_id >= total_shards {
            return Err(CryptoError::InvalidShard);
        }
        
        // TODO:
        // Use deterministic seed for consistent key generation across nodes
        // later, this would be coordinated differently, but for testing we'll use a fixed seed
        let mut rng = rand::rngs::StdRng::from_seed([42u8; 32]); // Fixed seed for all nodes
        
        let secret_key_set = SecretKeySet::random(threshold - 1, &mut rng);
        let public_key_set = secret_key_set.public_keys();
        let my_secret_share = secret_key_set.secret_key_share(my_shard_id);
        
        Ok(Self {
            threshold,
            total_shards,
            my_shard_id,
            _secret_key_set: secret_key_set,
            public_key_set,
            my_secret_share,
        })
    }

    pub fn test_encryption_round_trip(&self, message: &str) -> Result<String, CryptoError> {
        // Test encrypt/decrypt with this single node (for verification)
        let public_key = self.public_key_set.public_key();
        
        // Encrypt
        let ciphertext = public_key.encrypt(message.as_bytes());
        
        // Create decryption share - handle Option properly
        let dec_share = self.my_secret_share.decrypt_share(&ciphertext)
            .ok_or_else(|| CryptoError::DecryptionFailed("Failed to create decryption share".to_string()))?;
        
        // For single-node test, create a BTreeMap with just our share
        let mut shares = std::collections::BTreeMap::new();
        shares.insert(self.my_shard_id, dec_share);
        
        // Decrypt (this will only work if threshold = 1)
        let decrypted = self.public_key_set.decrypt(&shares, &ciphertext)
            .map_err(|e| CryptoError::DecryptionFailed(e.to_string()))?;
        
        String::from_utf8(decrypted)
            .map_err(|e| CryptoError::DecryptionFailed(e.to_string()))
    }

    pub fn get_info(&self) -> String {
        format!("ThresholdCrypto: {}/{} threshold, shard {}, public_key: {:?}", 
                self.threshold, self.total_shards, self.my_shard_id, 
                hex::encode(&self.public_key_set.public_key().to_bytes()))
    }

    pub fn encrypt_message(&self, plaintext: &str) -> Result<Vec<u8>, CryptoError> {
        let public_key = self.public_key_set.public_key();
        let ciphertext = public_key.encrypt(plaintext.as_bytes());
        
        // Serialize ciphertext for network transmission
        bincode::serialize(&ciphertext)
            .map_err(|e| CryptoError::EncryptionFailed(e.to_string()))
    }

    pub fn create_decryption_share(&self, ciphertext_bytes: &[u8]) -> Result<DecryptionShare, CryptoError> {
        // Deserialize ciphertext
        let ciphertext: threshold_crypto::Ciphertext = bincode::deserialize(ciphertext_bytes)
            .map_err(|e| CryptoError::DecryptionFailed(e.to_string()))?;
        
        // Create decryption share
        let dec_share = self.my_secret_share.decrypt_share(&ciphertext)
            .ok_or_else(|| CryptoError::DecryptionFailed("Failed to create decryption share".to_string()))?;
        
        // Serialize the share
        let share_data = bincode::serialize(&dec_share)
            .map_err(|e| CryptoError::DecryptionFailed(e.to_string()))?;
        
        Ok(DecryptionShare {
            shard_id: self.my_shard_id,
            share_data,
        })
    }

    pub fn combine_shares_and_decrypt(
        &self,
        ciphertext_bytes: &[u8],
        shares: &[DecryptionShare]
    ) -> Result<String, CryptoError> {
        if shares.len() < self.threshold {
            return Err(CryptoError::DecryptionFailed(
                format!("Need {} shares, have {}", self.threshold, shares.len())
            ));
        }
        
        // Deserialize ciphertext
        let ciphertext: threshold_crypto::Ciphertext = bincode::deserialize(ciphertext_bytes)
            .map_err(|e| CryptoError::DecryptionFailed(e.to_string()))?;
        
        // Deserialize shares into BTreeMap
        let mut share_map = BTreeMap::new();
        for dec_share in shares.iter().take(self.threshold) {
            let share: threshold_crypto::DecryptionShare = bincode::deserialize(&dec_share.share_data)
                .map_err(|e| CryptoError::DecryptionFailed(e.to_string()))?;
            share_map.insert(dec_share.shard_id, share);
        }
        
        // Decrypt
        let decrypted_bytes = self.public_key_set.decrypt(&share_map, &ciphertext)
            .map_err(|e| CryptoError::DecryptionFailed(e.to_string()))?;
        
        String::from_utf8(decrypted_bytes)
            .map_err(|e| CryptoError::DecryptionFailed(e.to_string()))
    }
}