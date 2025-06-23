use libp2p::{
    gossipsub, identity,
    mdns, noise, swarm::{Swarm, SwarmEvent}, tcp, yamux, PeerId, Transport
};
use std::{time::Duration, hash::{Hash, Hasher, DefaultHasher}};
use tokio::io::{self, AsyncBufReadExt};
use tracing::{info, warn};
use libp2p::swarm::NetworkBehaviour;
use libp2p::futures::StreamExt;
use serde::{Serialize, Deserialize};
use uuid::Uuid;
use chrono::{DateTime, Utc};
use std::collections::HashSet;

use crate::crypto::{ThresholdCrypto, EncryptedMessage };

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

pub struct PhoenixNode {
    swarm: Swarm<PhoenixBehaviour>,
    local_peer_id: PeerId,
    seen_messages: HashSet<String>,
    crypto: ThresholdCrypto,
}

impl PhoenixNode {
    pub async fn new(topic_str: &str, shard_id: usize) -> anyhow::Result<Self> {
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
        
        info!("Crypto initialized: {}", crypto.get_info());

        Ok(Self { 
            swarm, 
            local_peer_id, 
            seen_messages: HashSet::new(),
            crypto, 
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
                // NEW: Handle encrypted messages
                if self.seen_messages.contains(&encrypted_msg.id) {
                    return;
                }
                self.seen_messages.insert(encrypted_msg.id.clone());
                
                info!("🔒 Encrypted message from {} (ID: {}, needs {}/{} shares)", 
                      encrypted_msg.sender, encrypted_msg.id, encrypted_msg.threshold_needed, 5);
                
                // For now, just forward the message (decryption comes later)
                if encrypted_msg.decrement_ttl() {
                    let msg_type = PhoenixMessageType::Encrypted(encrypted_msg);
                    if let Ok(serialized) = serde_json::to_vec(&msg_type) {
                        let _ = self.swarm.behaviour_mut().gossipsub.publish(topic.clone(), serialized);
                    }
                }
            }
            Err(_) => {
                // Fallback for old format messages
                info!("📨 Raw: {}", String::from_utf8_lossy(msg_bytes));
            }
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
                            // NEW: Handle encrypted message command
                            let content = line.strip_prefix("/encrypt ").unwrap_or("");
                            let connected_peers: Vec<_> = self.swarm.connected_peers().cloned().collect();
                            
                            if connected_peers.is_empty() {
                                warn!("❌ No connected peers yet, try again in a moment");
                            } else {
                                if let Err(e) = self.send_encrypted_message(&topic, content, 5).await {
                                    warn!("❌ Failed to send encrypted message: {:?}", e);
                                }
                            }
                        } else if line == "/test-encrypt" {
                            // ADD: Test encryption command
                            println!("🔐 Testing message encryption...");
                            match self.crypto.encrypt_message("test message") {
                                Ok(encrypted_bytes) => {
                                    println!("✅ Encrypted 'test message' -> {} bytes", encrypted_bytes.len());
                                    println!("✅ Message serialization works");
                                }
                                Err(e) => {
                                    println!("❌ Encryption failed: {:?}", e);
                                }
                            }
                        } else {
                            // Existing plain text message logic
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