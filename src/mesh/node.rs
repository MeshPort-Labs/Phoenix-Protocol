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

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PhoenixMessage {
    pub id: String,
    pub sender: String,  
    pub content: Vec<u8>,
    pub timestamp: DateTime<Utc>,
    pub ttl: u32,
    pub hop_count: u32,
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
    seen_messages: HashSet<String>
}

impl PhoenixNode {
    pub async fn new(topic_str: &str) -> anyhow::Result<Self> {
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

        Ok(Self { swarm, local_peer_id, seen_messages: HashSet::new() })
    }

    pub async fn send_message(&mut self, topic: &gossipsub::IdentTopic, content: Vec<u8>, ttl: u32) -> anyhow::Result<()> {
        let phoenix_msg = PhoenixMessage::new(self.local_peer_id, content, ttl);
        info!("i8Sending message ID: {} with TTL: {}", phoenix_msg.id, phoenix_msg.ttl);

        let serialized = serde_json::to_vec(&phoenix_msg)?;

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

    fn route_message(&mut self, topic: &gossipsub::IdentTopic, mut phoenix_msg: PhoenixMessage) {
        // Check if we've seen this message before
        if self.seen_messages.contains(&phoenix_msg.id) {
            info!("Already seen message {}, skipping", phoenix_msg.id);
            return;
        }

        self.seen_messages.insert(phoenix_msg.id.clone());

        info!(
            "Received message ID: {} from {} (TTL: {}, Hops: {}): {}", 
            phoenix_msg.id,
            phoenix_msg.sender,
            phoenix_msg.ttl,
            phoenix_msg.hop_count,
            String::from_utf8_lossy(&phoenix_msg.content)
        );

        // Check if we should forward this message
        if phoenix_msg.decrement_ttl() {
            info!("Forwarding message {} with new TTL: {}", phoenix_msg.id, phoenix_msg.ttl);
            
            if let Ok(serialized) = serde_json::to_vec(&phoenix_msg) {
                if let Err(e) = self.swarm.behaviour_mut().gossipsub.publish(topic.clone(), serialized) {
                    warn!("Failed to forward message: {:?}", e);
                }
            }
        } else {
            info!("Message {} TTL expired, not forwarding", phoenix_msg.id);
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
                        // Parse input: either "ttl:message" or just "message"
                        let (ttl, content) = if line.contains(':') {
                            let parts: Vec<&str> = line.splitn(2, ':').collect();
                            if parts.len() == 2 {
                                if let Ok(ttl) = parts[0].parse::<u32>() {
                                    (ttl, parts[1].to_string())
                                } else {
                                    (5, line.clone()) // Default TTL if parse fails
                                }
                            } else {
                                (5, line.clone()) // Default TTL
                            }
                        } else {
                            (5, line.clone()) // Default TTL
                        };

                        // Check connected peers before publishing
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
                },
                event = self.swarm.select_next_some() => match event {
                    SwarmEvent::Behaviour(PhoenixEvent::Gossipsub(gossipsub::Event::Subscribed { peer_id, topic })) => {
                        info!("Peer {:?} subscribed to topic {:?}", peer_id, topic);
                    }
                    SwarmEvent::Behaviour(PhoenixEvent::Gossipsub(gossipsub::Event::Message {
                        propagation_source,
                        message,
                        ..
                    })) => {
                        // parse Phoenix message and route it
                        match serde_json::from_slice::<PhoenixMessage>(&message.data) {
                            Ok(phoenix_msg) => {
                                // Only route if it's not from ourselves
                                if phoenix_msg.sender != self.local_peer_id.to_string() {
                                    self.route_message(&topic, phoenix_msg);
                                }
                            }
                            Err(e) => {
                                // Fallback for non-Phoenix messages
                                info!(
                                    "Raw message from {:?}: {} (Parse error: {})",
                                    propagation_source,
                                    String::from_utf8_lossy(&message.data),
                                    e
                                );
                            }
                        }
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
}