use libp2p::{
    gossipsub, identity,
    mdns, noise, swarm::{Swarm, SwarmEvent}, tcp, yamux, PeerId, Transport
};
use std::{collections::HashMap, hash::{DefaultHasher, Hash, Hasher}, time::Duration};
use tokio::io::{self, AsyncBufReadExt};
use tracing::{info, warn};
use libp2p::swarm::NetworkBehaviour;
use libp2p::futures::StreamExt;
use serde::{Serialize, Deserialize};
use uuid::Uuid;
use chrono::{DateTime, Utc};
use std::collections::HashSet;

use crate::crypto::{DecryptionShare, EncryptedMessage, ShareRequest, ShareResponse, ThresholdCrypto };
use crate::filecoin::{FilecoinClient, ThresholdMessage};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PhoenixMessage {
    pub id: String,
    pub sender: String,  
    pub content: Vec<u8>,
    pub timestamp: DateTime<Utc>,
    pub ttl: u32,
    pub hop_count: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum PhoenixMessageType {
    PlainText(PhoenixMessage),
    Encrypted(EncryptedMessage),
    ShareRequest(ShareRequest),
    ShareResponse(ShareResponse),
}

impl PhoenixMessage {
    pub fn new(sender: PeerId, content: Vec<u8>, initial_ttl: u32) -> Self {
        Self { 
            id: Uuid::new_v4().to_string(), 
            sender: sender.to_string(), 
            content, 
            timestamp: Utc::now(), 
            ttl: initial_ttl, 
            hop_count: 0 
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

#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "PhoenixEvent")]
struct PhoenixBehaviour {
    gossipsub: gossipsub::Behaviour,
    mdns: mdns::tokio::Behaviour,
}

pub enum PhoenixEvent {
    Gossipsub(gossipsub::Event),
    Mdns(mdns::Event),
}

impl From<gossipsub::Event> for PhoenixEvent {
    fn from(event: gossipsub::Event) -> Self {
        PhoenixEvent::Gossipsub(event)
    }
}

impl From<mdns::Event> for PhoenixEvent {
    fn from(event: mdns::Event) -> Self {
        PhoenixEvent::Mdns(event)
    }
}

#[derive(Clone, Debug)]
pub struct DecryptedMessage {
    pub id: String,
    pub sender: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub emergency: bool,
}

pub struct PhoenixNode {
    swarm: Swarm<PhoenixBehaviour>,
    local_peer_id: PeerId,
    seen_messages: HashSet<String>,
    crypto: ThresholdCrypto,
    pending_encrypted_messages: HashMap<String, EncryptedMessage>,
    my_decryption_shares: HashMap<String, DecryptionShare>,
    _node_name: String,
    collected_shares: HashMap<String, Vec<DecryptionShare>>,
    message_history: Vec<DecryptedMessage>,
    pub filecoin_client: Option<FilecoinClient>,
}

impl PhoenixNode {
    pub async fn new(topic_str: &str, shard_id: usize, name: String) -> anyhow::Result<Self> {
        let id_keys = identity::Keypair::generate_ed25519();
        let local_peer_id = PeerId::from(id_keys.public());
        info!("Local peer id: {:?}", local_peer_id);

        let noise_keys = noise::Config::new(&id_keys)?;

        let transport = tcp::tokio::Transport::new(tcp::Config::default().nodelay(true))
            .upgrade(libp2p::core::upgrade::Version::V1Lazy)
            .authenticate(noise_keys)
            .multiplex(yamux::Config::default())
            .timeout(Duration::from_secs(20))
            .boxed();

        let message_id_fn = |message: &gossipsub::Message| {
            let mut s = DefaultHasher::new();
            message.data.hash(&mut s);
            gossipsub::MessageId::from(s.finish().to_string())
        };

        let gossipsub_config = gossipsub::ConfigBuilder::default()
            .heartbeat_interval(Duration::from_secs(1))
            .validation_mode(gossipsub::ValidationMode::Strict)
            .message_id_fn(message_id_fn)
            .build()
            .expect("Valid gossipsub config");

        let mut gossipsub = gossipsub::Behaviour::new(
            gossipsub::MessageAuthenticity::Signed(id_keys.clone()),
            gossipsub_config,
        ).expect("Valid gossipsub behaviour");
        
        let topic = gossipsub::IdentTopic::new(topic_str);
        gossipsub.subscribe(&topic)?;

        let mdns = mdns::tokio::Behaviour::new(mdns::Config::default(), local_peer_id)?;
        
        let behaviour = PhoenixBehaviour { gossipsub, mdns };
        
        let swarm_config = libp2p::swarm::Config::with_tokio_executor()
            .with_idle_connection_timeout(Duration::from_secs(60)); 
            
        let swarm = Swarm::new(
            transport,
            behaviour,
            local_peer_id,
            swarm_config,
        );

        let crypto = ThresholdCrypto::generate_keys(3, 5, shard_id)
            .map_err(|e| anyhow::anyhow!("Crypto initialization failed: {:?}", e))?;
        
        info!("🔐 Node '{}' crypto initialized: {}", name, crypto.get_info());

        let filecoin_client = Some(FilecoinClient::new(
            "http://localhost:8080".to_string(),
            3, // threshold
            9, // total shards
        ));

        Ok(Self { 
            swarm, 
            local_peer_id, 
            seen_messages: HashSet::new(),
            crypto, 
            pending_encrypted_messages: HashMap::new(),
            my_decryption_shares: HashMap::new(),
            _node_name: name,
            collected_shares: HashMap::new(),
            message_history: Vec::new(),
            filecoin_client
        })
    }

    pub async fn send_message(&mut self, topic: &gossipsub::IdentTopic, content: Vec<u8>, ttl: u32) -> anyhow::Result<()> {
        let phoenix_msg = PhoenixMessage::new(self.local_peer_id, content, ttl);
        info!("Sending message ID: {} with TTL: {}", phoenix_msg.id, phoenix_msg.ttl);

        let message_type = PhoenixMessageType::PlainText(phoenix_msg.clone());
        let serialized = serde_json::to_vec(&message_type)?;

        self.seen_messages.insert(phoenix_msg.id.clone());

        match self.swarm.behaviour_mut().gossipsub.publish(topic.clone(), serialized) {
            Ok(_) => {
                info!("Message published successfully");
                Ok(())
            }
            Err(e) => {
                warn!("Failed to publish message: {:?}", e);
                Err(anyhow::anyhow!("Failed to publish: {:?}", e))
            }
        }
    }

    pub async fn send_encrypted_message(&mut self, topic: &gossipsub::IdentTopic, content: &str, ttl: u32) -> anyhow::Result<()> {
        let encrypted_content = self.crypto.encrypt_message(content)
            .map_err(|e| anyhow::anyhow!("Encryption failed: {:?}", e))?;
        
        let encrypted_msg = EncryptedMessage::new(
            self.local_peer_id.to_string(),
            encrypted_content,
            self.crypto.threshold,
            ttl
        );
        
        let message_type = PhoenixMessageType::Encrypted(encrypted_msg.clone());
        let serialized = serde_json::to_vec(&message_type)?;
        
        info!("🔐 Sending encrypted message ID: {} (TTL: {})", encrypted_msg.id, encrypted_msg.ttl);
        
        match self.swarm.behaviour_mut().gossipsub.publish(topic.clone(), serialized) {
            Ok(_) => {
                info!("✅ Encrypted message published successfully");
                Ok(())
            }
            Err(e) => {
                warn!("❌ Failed to publish encrypted message: {:?}", e);
                Err(anyhow::anyhow!("Failed to publish: {:?}", e))
            }
        }
    }

    pub async fn send_encrypted_message_with_storage(&mut self, topic: &gossipsub::IdentTopic, content: &str, ttl: u32) -> anyhow::Result<()> {
        if let Some(ref mut filecoin_client) = self.filecoin_client {
            // Create threshold message
            let threshold_msg = filecoin_client.threshold_storage.create_threshold_message(
                &self.crypto,
                content,
                &self.local_peer_id.to_string(),
                content.starts_with("🚨") // Emergency if starts with alert emoji
            ).map_err(|e| anyhow::anyhow!("Failed to create threshold message: {}", e))?;
            
            // Store in Filecoin
            match filecoin_client.store_threshold_message(threshold_msg.clone()).await {
                Ok(manifest) => {
                    info!("✅ Message stored in Filecoin: {}", manifest.manifest_cid);
                    
                    // Still send the encrypted message via mesh for immediate delivery
                    self.send_encrypted_message(topic, content, ttl).await?;
                }
                Err(e) => {
                    warn!("⚠️ Filecoin storage failed, using mesh only: {}", e);
                    // Fallback to mesh-only
                    self.send_encrypted_message(topic, content, ttl).await?;
                }
            }
        } else {
            // No Filecoin client, use mesh only
            self.send_encrypted_message(topic, content, ttl).await?;
        }
        
        Ok(())
    }

    fn handle_encrypted_message(&mut self, topic: &gossipsub::IdentTopic, encrypted_msg: EncryptedMessage) {
        let msg_id = encrypted_msg.id.clone();
        
        match self.crypto.create_decryption_share(&encrypted_msg.encrypted_content) {
            Ok(share) => {
                info!("🔑 Created decryption share for message {} (shard {})", msg_id, share.shard_id);
                
                // Store the message and our share
                self.pending_encrypted_messages.insert(msg_id.clone(), encrypted_msg);
                self.my_decryption_shares.insert(msg_id.clone(), share.clone());
                
                // Automatically broadcast our share
                let response = ShareResponse::new(
                    msg_id,
                    self.local_peer_id.to_string(),
                    self._node_name.clone(),
                    share
                );
                
                let msg_type = PhoenixMessageType::ShareResponse(response);
                if let Ok(serialized) = serde_json::to_vec(&msg_type) {
                    if let Err(e) = self.swarm.behaviour_mut().gossipsub.publish(topic.clone(), serialized) {
                        warn!("Failed to broadcast share: {:?}", e);
                    } else {
                        info!("📤 Auto-broadcasted my share to network");
                    }
                }
            }
            Err(e) => {
                warn!("❌ Failed to create decryption share: {:?}", e);
            }
        }
    }

    // Auto-decrypt when threshold reached
    fn try_auto_decrypt(&mut self, message_id: &str) {
        if let Some(encrypted_msg) = self.pending_encrypted_messages.get(message_id) {
            let mut all_shares = Vec::new();
            
            // Add our own share
            if let Some(my_share) = self.my_decryption_shares.get(message_id) {
                all_shares.push(my_share.clone());
            }
            
            // Add collected shares from other nodes
            if let Some(collected) = self.collected_shares.get(message_id) {
                all_shares.extend(collected.clone());
            }
            
            info!("🔍 Checking decryption: have {}/{} shares for message {}", 
                  all_shares.len(), self.crypto.threshold, message_id);
            
            if all_shares.len() >= self.crypto.threshold {
                match self.crypto.combine_shares_and_decrypt(&encrypted_msg.encrypted_content, &all_shares) {
                    Ok(plaintext) => {
                        info!("🔓 Threshold reached! Decrypting message...");
                        info!("✅ Decrypted from {}: \"{}\"", encrypted_msg.sender, plaintext);

                        //saveing to history
                        let is_emergency = plaintext.starts_with("🚨");
                let decrypted_msg = DecryptedMessage {
                    id: message_id.to_string(),
                    sender: encrypted_msg.sender.clone(),
                    content: plaintext,
                    timestamp: Utc::now(),
                    emergency: is_emergency,
                };

                self.message_history.push(decrypted_msg);
                
                // Keep only last 50 messages
                if self.message_history.len() > 50 {
                    self.message_history.remove(0);
                }
                        
                        // Clean up
                        self.pending_encrypted_messages.remove(message_id);
                        self.my_decryption_shares.remove(message_id);
                        self.collected_shares.remove(message_id);
                    }
                    Err(e) => {
                        warn!("❌ Decryption failed: {:?}", e);
                    }
                }
            }
        }
    }
    
    // Handle share responses
    fn handle_share_response(&mut self, response: ShareResponse) {
        let msg_id = &response.message_id;
        info!("🔑 Received share from {} for message {} (shard {})", 
              response.provider_name, msg_id, response.share.shard_id);
        
        // Store the share
        self.collected_shares
            .entry(msg_id.clone())
            .or_insert_with(Vec::new)
            .push(response.share);
        
        // Try to decrypt automatically
        self.try_auto_decrypt(msg_id);
    }
    
    // Handle share requests
    fn handle_share_request(&mut self, topic: &gossipsub::IdentTopic, request: ShareRequest) {
        info!("📨 Share request from {} for message {}", request.requester_name, request.message_id);
        
        if let Some(my_share) = self.my_decryption_shares.get(&request.message_id).cloned() {
            let response = ShareResponse::new(
                request.message_id,
                self.local_peer_id.to_string(),
                self._node_name.clone(),
                my_share
            );
            
            let msg_type = PhoenixMessageType::ShareResponse(response);
            if let Ok(serialized) = serde_json::to_vec(&msg_type) {
                if let Err(e) = self.swarm.behaviour_mut().gossipsub.publish(topic.clone(), serialized) {
                    warn!("Failed to send share response: {:?}", e);
                } else {
                    info!("📤 Sent share to {}", request.requester_name);
                }
            }
        } else {
            info!("❌ No share available for message {}", request.message_id);
        }
    }

    fn route_message(&mut self, topic: &gossipsub::IdentTopic, msg_bytes: &[u8]) {
        // Try to parse as PhoenixMessageType first
        match serde_json::from_slice::<PhoenixMessageType>(msg_bytes) {
            Ok(PhoenixMessageType::PlainText(mut phoenix_msg)) => {
                // Existing plain text logic
                if self.seen_messages.contains(&phoenix_msg.id) {
                    return;
                }
                self.seen_messages.insert(phoenix_msg.id.clone());
                
                info!("📨 [{}]: {}", phoenix_msg.sender, String::from_utf8_lossy(&phoenix_msg.content));
                
                if phoenix_msg.decrement_ttl() {
                    let msg_type = PhoenixMessageType::PlainText(phoenix_msg);
                    if let Ok(serialized) = serde_json::to_vec(&msg_type) {
                        let _ = self.swarm.behaviour_mut().gossipsub.publish(topic.clone(), serialized);
                    }
                }
            }
            Ok(PhoenixMessageType::Encrypted(mut encrypted_msg)) => {
                if self.seen_messages.contains(&encrypted_msg.id) {
                    return;
                }
                self.seen_messages.insert(encrypted_msg.id.clone());
                
                info!("🔒 Encrypted message from {} (ID: {}, needs {}/{} shares)", 
                      encrypted_msg.sender, encrypted_msg.id, encrypted_msg.threshold_needed, 5);
                
                self.handle_encrypted_message(topic, encrypted_msg.clone());

                // auto-decryption immediately (in case we already have shares)
                self.try_auto_decrypt(&encrypted_msg.id);

                if encrypted_msg.decrement_ttl() {
                    let msg_type = PhoenixMessageType::Encrypted(encrypted_msg);
                    if let Ok(serialized) = serde_json::to_vec(&msg_type) {
                        let _ = self.swarm.behaviour_mut().gossipsub.publish(topic.clone(), serialized);
                    }
                }
            }
            Ok(PhoenixMessageType::ShareRequest(request)) => {
                self.handle_share_request(topic, request);
            }
            Ok(PhoenixMessageType::ShareResponse(response)) => {
                self.handle_share_response(response);
            }
            Err(_) => {
                // Fallback for raw messages
                info!("📨 Raw: {}", String::from_utf8_lossy(msg_bytes));
            }
        }
    }

    // Add command handlers for Filecoin operations
    pub async fn handle_filecoin_status(&mut self) {
        if let Some(ref filecoin_client) = self.filecoin_client {
            match filecoin_client.check_status().await {
                Ok(status) => {
                    info!("📊 Filecoin Status:");
                    info!("   Connected: {}", status.connected);
                    info!("   Network: {}", status.network);
                    if let Some(latency) = status.latency {
                        info!("   Latency: {}ms", latency);
                    }
                    
                    let (cache_count, _) = filecoin_client.get_emergency_cache_status();
                    info!("   Emergency Cache: {} messages", cache_count);
                }
                Err(e) => {
                    warn!("❌ Filecoin status check failed: {}", e);
                }
            }
        } else {
            info!("❌ Filecoin client not initialized");
        }
    }

    pub async fn run(mut self, topic_str: &str) -> anyhow::Result<()> {
        let topic = gossipsub::IdentTopic::new(topic_str);

        self.swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

        let mut stdin = io::BufReader::new(io::stdin()).lines();

        info!("🚀 Phoenix node started! Type messages to send (format: 'ttl:message' or just 'message' for TTL=5)");

        loop {
            tokio::select! {
                line = stdin.next_line() => {
                    let line = line?.unwrap_or_default();
                    if !line.is_empty() {
                        if line.starts_with("/encrypt ") {
                            let content = line.strip_prefix("/encrypt ").unwrap_or("");
                            let connected_peers: Vec<_> = self.swarm.connected_peers().cloned().collect();
                            
                            if connected_peers.is_empty() {
                                warn!("❌ No connected peers yet, try again in a moment");
                            } else {
                                if let Err(e) = self.send_encrypted_message(&topic, content, 5).await {
                                    warn!("❌ Failed to send encrypted message: {:?}", e);
                                }
                            }
                        } else if line.starts_with("/request-decrypt ") {
                            // ADD THIS: Manual share request
                            let message_id = line.strip_prefix("/request-decrypt ").unwrap_or("").trim();
                            if !message_id.is_empty() {
                                let request = ShareRequest::new(
                                    message_id.to_string(),
                                    self.local_peer_id.to_string(),
                                    self._node_name.clone()
                                );
                                
                                let msg_type = PhoenixMessageType::ShareRequest(request);
                                if let Ok(serialized) = serde_json::to_vec(&msg_type) {
                                    if let Err(e) = self.swarm.behaviour_mut().gossipsub.publish(topic.clone(), serialized) {
                                        warn!("Failed to request shares: {:?}", e);
                                    } else {
                                        info!("📨 Requesting decryption shares for message {}", message_id);
                                    }
                                }
                            }
                        } else if line == "/shares" {
                            info!("📊 Share Status:");
                            for (msg_id, encrypted_msg) in &self.pending_encrypted_messages {
                                let my_shares = if self.my_decryption_shares.contains_key(msg_id) { 1 } else { 0 };
                                let collected = self.collected_shares.get(msg_id).map(|v| v.len()).unwrap_or(0);
                                let total = my_shares + collected;
                                info!("   {} - {}/{} shares (from: {})", 
                                      &msg_id[..8], total, self.crypto.threshold, encrypted_msg.sender);
                            }
                        } else if line == "/status" {
                            info!("📊 Phoenix Node Status:");
                            info!("   🔐 Shard ID: {}", self.crypto.my_shard_id);
                            info!("   🌐 Connected Peers: {}", self.swarm.connected_peers().count());
                            info!("   📨 Pending Messages: {}", self.pending_encrypted_messages.len());
                            info!("   🔑 My Shares: {}", self.my_decryption_shares.len());
                            info!("   📥 Collected Shares: {}", self.collected_shares.len());
                            
                            let ready_to_decrypt: Vec<_> = self.pending_encrypted_messages.keys()
                                .filter(|msg_id| {
                                    let my_count = if self.my_decryption_shares.contains_key(*msg_id) { 1 } else { 0 };
                                    let collected = self.collected_shares.get(*msg_id).map(|v| v.len()).unwrap_or(0);
                                    (my_count + collected) >= self.crypto.threshold
                                })
                                .collect();
                            
                            info!("   ✅ Ready to Decrypt: {}", ready_to_decrypt.len());
                            
                        } else if line == "/peers" {
                            // List connected peers with details
                            info!("👥 Connected Peers:");
                            let peers: Vec<_> = self.swarm.connected_peers().collect();
                            if peers.is_empty() {
                                info!("   No peers connected");
                            } else {
                                for (i, peer) in peers.iter().enumerate() {
                                    info!("   {}. {}", i + 1, peer);
                                }
                            }
                            
                        } else if line.starts_with("/decrypt ") {
                            // Manually decrypt specific message
                            let message_id = line.strip_prefix("/decrypt ").unwrap_or("").trim();
                            if !message_id.is_empty() {
                                if self.pending_encrypted_messages.contains_key(message_id) {
                                    self.try_auto_decrypt(message_id);
                                } else {
                                    info!("❌ Message {} not found in pending messages", message_id);
                                }
                            }
                            
                        } else if line.starts_with("/emergency ") {
                            // Send emergency broadcast
                            let content = line.strip_prefix("/emergency ").unwrap_or("");
                            let connected_peers: Vec<_> = self.swarm.connected_peers().cloned().collect();
                            
                            if connected_peers.is_empty() {
                                warn!("❌ No connected peers for emergency broadcast");
                            } else {
                                // Create emergency message with special formatting
                                let emergency_content = format!("🚨 EMERGENCY: {}", content);
                                if let Err(e) = self.send_encrypted_message(&topic, &emergency_content, 10).await {
                                    warn!("❌ Failed to send emergency message: {:?}", e);
                                } else {
                                    info!("🚨 Emergency broadcast sent to {} peers", connected_peers.len());
                                }
                            }
                                for (i, msg) in self.message_history.iter().rev().take(10).enumerate() {
                                    let prefix = if msg.emergency { "🚨" } else { "📨" };
                                    info!("   {}. {} [{}]: {}", 
                                          i + 1, prefix, &msg.sender[..12], 
                                          if msg.content.len() > 50 { 
                                              format!("{}...", &msg.content[..50]) 
                                          } else { 
                                              msg.content.clone() 
                                          });
                                }
                            } else if line == "/history" {
                                info!("📜 Message History:");
                                if self.message_history.is_empty() {
                                    info!("   No messages in history");
                                } else {
                                    for (i, msg) in self.message_history.iter().rev().take(10).enumerate() {
                                        let prefix = if msg.emergency { "🚨" } else { "📨" };
                                        info!("   {}. {} [{}]: {}", 
                                              i + 1, prefix, &msg.sender[..12], 
                                              if msg.content.len() > 50 { 
                                                  format!("{}...", &msg.content[..50]) 
                                              } else { 
                                                  msg.content.clone() 
                                              });
                                    }
                                } 
                            } else if line == "/filecoin" {
                                self.handle_filecoin_status().await;
                            } else if line.starts_with("/store ") {
                                let content = line.strip_prefix("/store ").unwrap_or("");
                                if let Err(e) = self.send_encrypted_message_with_storage(&topic, content, 5).await {
                                    warn!("❌ Failed to store message: {:?}", e);
                                }
                            } else if line == "/emergency-mode" {
                                if let Some(ref mut filecoin_client) = self.filecoin_client {
                                    if let Err(e) = filecoin_client.emergency_broadcast_mode(true).await {
                                        warn!("❌ Failed to enable emergency mode: {}", e);
                                    }
                                }
                            } else if line == "/routing-check" {
                                if let Some(ref filecoin_client) = self.filecoin_client {
                                    match filecoin_client.adaptive_routing_check().await {
                                        Ok(mode) => info!("🔄 Current routing mode: {}", mode),
                                        Err(e) => warn!("❌ Routing check failed: {}", e),
                                    }
                                }
                            }
                        else {
                            // plain text message logic
                            let (ttl, content) = if line.contains(':') {
                                let parts: Vec<&str> = line.splitn(2, ':').collect();
                                if parts.len() == 2 {
                                    if let Ok(ttl) = parts[0].parse::<u32>() {
                                        (ttl, parts[1].to_string())
                                    } else {
                                        (5, line.clone())
                                    }
                                } else {
                                    (5, line.clone())
                                }
                            } else {
                                (5, line.clone())
                            };
                
                            let connected_peers: Vec<_> = self.swarm.connected_peers().cloned().collect();
                            info!("Connected peers: {} total", connected_peers.len());
                            
                            if connected_peers.is_empty() {
                                warn!("No connected peers yet, try again in a moment");
                            } else {
                                if let Err(e) = self.send_message(&topic, content.as_bytes().to_vec(), ttl).await {
                                    warn!("Failed to send message: {:?}", e);
                                }
                            }
                        }
                    }
                },
                event = self.swarm.select_next_some() => match event {
                    SwarmEvent::Behaviour(PhoenixEvent::Gossipsub(gossipsub::Event::Subscribed { peer_id, topic })) => {
                        info!("Peer {:?} subscribed to topic {:?}", peer_id, topic);
                    }
                    SwarmEvent::Behaviour(PhoenixEvent::Gossipsub(gossipsub::Event::Message {
                        propagation_source: _,
                        message,
                        ..
                    })) => {
                        self.route_message(&topic, &message.data);
                    }
                    SwarmEvent::Behaviour(PhoenixEvent::Mdns(mdns::Event::Discovered(list))) => {
                        for (peer_id, multiaddr) in list {
                            info!("🔍 mDNS discovered peer: {}", peer_id);
                            
                            self.swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                            
                            match self.swarm.dial(multiaddr.clone()) {
                                Ok(_) => info!("Dialing peer {}", peer_id),
                                Err(e) => warn!("Failed to dial peer {}: {:?}", peer_id, e),
                            }
                        }
                    }
                    SwarmEvent::Behaviour(PhoenixEvent::Mdns(mdns::Event::Expired(list))) => {
                        for (peer_id, _multiaddr) in list {
                            warn!("mDNS peer expired: {}", peer_id);
                            self.swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer_id);
                        }
                    }
                    SwarmEvent::ConnectionEstablished { peer_id, connection_id, .. } => {
                        info!("Connection established with: {:?} (connection: {:?})", peer_id, connection_id);
                        
                        self.swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                        
                        tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_secs(2)).await;
                        });
                    }
                    SwarmEvent::ConnectionClosed { peer_id, connection_id, cause, .. } => {
                        warn!("Connection closed with: {:?} (connection: {:?}) - Cause: {:?}", peer_id, connection_id, cause);
                    }
                    SwarmEvent::NewListenAddr { address, .. } => {
                        info!("Local node listening on {}", address);
                    }
                    SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                        warn!("Outgoing connection error to {:?}: {:?}", peer_id, error);
                    }
                    SwarmEvent::IncomingConnectionError { error, .. } => {
                        warn!("Incoming connection error: {:?}", error);
                    }
                    _ => {}
                }
            }
        }
    }

    pub fn get_crypto_info(&self) -> String {
        self.crypto.get_info()
    }
}