use libp2p::{
    gossipsub, identity,
    mdns, noise, swarm::{Swarm, SwarmEvent}, tcp, yamux, PeerId, Transport
};
use std::{time::Duration, hash::{Hash, Hasher, DefaultHasher}};
use tokio::io::{self, AsyncBufReadExt};
use tracing::{info, warn};
use libp2p::swarm::NetworkBehaviour;
use libp2p::futures::StreamExt;

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
    _local_peer_id: PeerId,
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

        Ok(Self { swarm, _local_peer_id: local_peer_id })
    }

    pub async fn run(mut self, topic_str: &str) -> anyhow::Result<()> {
        let topic = gossipsub::IdentTopic::new(topic_str);

        self.swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

        let mut stdin = io::BufReader::new(io::stdin()).lines();

        loop {
            tokio::select! {
                line = stdin.next_line() => {
                    let line = line?.unwrap_or_default();
                    if !line.is_empty() {
                        // Checking connected peers before publishing
                        let connected_peers: Vec<_> = self.swarm.connected_peers().cloned().collect();
                        info!("Connected peers: {} total", connected_peers.len());
                        
                        if connected_peers.is_empty() {
                            warn!("No connected peers yet, try again in a moment");
                        } else {
                            match self.swarm.behaviour_mut().gossipsub.publish(topic.clone(), line.as_bytes()) {
                                Ok(_) => info!("Message published successfully to {} peers", connected_peers.len()),
                                Err(e) => warn!("Failed to publish message: {:?}", e),
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
                        info!(
                             "Received from {:?}: {}",
                            propagation_source,
                            String::from_utf8_lossy(&message.data)
                        );
                    }
                    SwarmEvent::Behaviour(PhoenixEvent::Mdns(mdns::Event::Discovered(list))) => {
                        for (peer_id, multiaddr) in list {
                            info!("mDNS discovered peer: {}", peer_id);
                            
                            // Add peer to gossipsub
                            self.swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                            
                            // Dial the peer with explicit error handling
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
                        
                        // Force gossipsub to recognize this peer 
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