//! Attack Coordinator - Synchronize multiple evil validators
//!
//! Allows 2 evil validators to coordinate their 51% attack
//! by sharing private blocks and synchronizing release timing.

use anyhow::Result;
use poslib::sha256::Hash;
use poslib::types::Block;
use serde::{Deserialize, Serialize};
use std::io::{Error as IoError, ErrorKind};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;

/// Messages between evil validators
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EvilSync {
    /// Share a privately mined block
    SharePrivateBlock(Block),
    /// Signal to release all private blocks NOW
    TriggerRelease,
    /// Share current shadow chain length
    ShadowChainStatus {
        length: u64,
        fork_point: Hash,
    },
    /// Acknowledge receipt
    Ack,
    /// Heartbeat to maintain connection
    Ping,
    Pong,
}

impl EvilSync {
    pub fn encode(&self) -> Result<Vec<u8>, ciborium::ser::Error<IoError>> {
        let mut bytes = Vec::new();
        ciborium::into_writer(self, &mut bytes)?;
        Ok(bytes)
    }

    pub fn decode(data: &[u8]) -> Result<Self, ciborium::de::Error<IoError>> {
        ciborium::from_reader(data)
    }

    pub async fn send_async<W: AsyncWriteExt + Unpin>(&self, stream: &mut W) -> Result<()> {
        let bytes = self
            .encode()
            .map_err(|e| anyhow::anyhow!("Encode error: {}", e))?;
        let len = bytes.len() as u64;
        stream.write_all(&len.to_be_bytes()).await?;
        stream.write_all(&bytes).await?;
        stream.flush().await?;
        Ok(())
    }

    pub async fn receive_async<R: AsyncReadExt + Unpin>(stream: &mut R) -> Result<Self> {
        let mut len_bytes = [0u8; 8];
        stream.read_exact(&mut len_bytes).await?;
        let len = u64::from_be_bytes(len_bytes) as usize;

        if len > 10_000_000 {
            return Err(anyhow::anyhow!("Message too large"));
        }

        let mut data = vec![0u8; len];
        stream.read_exact(&mut data).await?;

        Self::decode(&data).map_err(|e| anyhow::anyhow!("Decode error: {}", e))
    }
}

/// Coordinator state shared between evil validators
#[derive(Debug, Default)]
pub struct CoordinatorState {
    /// Blocks received from partner
    pub partner_blocks: Vec<Block>,
    /// Partner's shadow chain length
    pub partner_shadow_length: u64,
    /// Whether partner triggered release
    pub release_triggered: bool,
    /// Connection established
    pub connected: bool,
}

/// Attack coordinator for synchronizing evil validators
/// Uses automatic sync port = normal_port + 10000
pub struct AttackCoordinator {
    pub state: Arc<RwLock<CoordinatorState>>,
    pub evil_id: u8,
    pub our_port: u16,
    partner_addr: Option<String>,
    partner_stream: Arc<RwLock<Option<TcpStream>>>,
}

impl AttackCoordinator {
    /// Create new coordinator
    /// - our_port: our normal network port (sync port will be our_port + 10000)
    /// - partner_addr: partner's normal port address (e.g., "127.0.0.1:9003")
    pub fn new(evil_id: u8, our_port: u16, partner_addr: Option<String>) -> Self {
        Self {
            state: Arc::new(RwLock::new(CoordinatorState::default())),
            evil_id,
            our_port,
            partner_addr,
            partner_stream: Arc::new(RwLock::new(None)),
        }
    }

    /// Calculate sync port from normal port
    fn sync_port(normal_port: u16) -> u16 {
        normal_port + 10000
    }

    /// Extract port from address and convert to sync port
    fn partner_sync_addr(partner_addr: &str) -> String {
        // Parse "127.0.0.1:9003" -> "127.0.0.1:19003"
        if let Some((host, port_str)) = partner_addr.rsplit_once(':') {
            if let Ok(port) = port_str.parse::<u16>() {
                return format!("{}:{}", host, Self::sync_port(port));
            }
        }
        partner_addr.to_string()
    }

    /// Start the coordinator:
    /// 1. Listen on our sync port (our_port + 10000)
    /// 2. Try to connect to partner's sync port in background
    pub async fn start(&self) -> Result<()> {
        let our_sync_port = Self::sync_port(self.our_port);

        // Start listener for incoming evil sync connections
        let listener = TcpListener::bind(format!("0.0.0.0:{}", our_sync_port)).await?;
        println!(
            "👿 Evil sync listening on port {} (auto-calculated)",
            our_sync_port
        );

        let state = self.state.clone();
        let partner_stream = self.partner_stream.clone();
        let evil_id = self.evil_id;

        // Spawn listener task - accepts connection from partner
        tokio::spawn(async move {
            loop {
                if let Ok((socket, addr)) = listener.accept().await {
                    println!("👿 Evil #{} - Partner connected from {}", evil_id, addr);

                    let mut state_guard = state.write().await;
                    if state_guard.connected {
                        // Already connected, ignore duplicate
                        continue;
                    }
                    state_guard.connected = true;
                    drop(state_guard);

                    let mut stream_guard = partner_stream.write().await;
                    *stream_guard = Some(socket);
                    drop(stream_guard);

                    // Start message receiver
                    Self::spawn_message_receiver(state.clone(), partner_stream.clone(), evil_id);
                    break; // Only accept one partner
                }
            }
        });

        // Try to connect to partner in background
        if let Some(addr) = &self.partner_addr {
            let partner_sync = Self::partner_sync_addr(addr);
            let state = self.state.clone();
            let partner_stream = self.partner_stream.clone();
            let evil_id = self.evil_id;

            tokio::spawn(async move {
                // Small delay to let partner start their listener
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

                for attempt in 1..=30 {
                    // Check if already connected (partner connected to us)
                    {
                        let state_guard = state.read().await;
                        if state_guard.connected {
                            println!("👿 Evil #{} - Already connected via listener", evil_id);
                            return;
                        }
                    }

                    match TcpStream::connect(&partner_sync).await {
                        Ok(stream) => {
                            println!(
                                "👿 Evil #{} - Connected to partner at {}",
                                evil_id, partner_sync
                            );

                            let mut state_guard = state.write().await;
                            state_guard.connected = true;
                            drop(state_guard);

                            let mut stream_guard = partner_stream.write().await;
                            *stream_guard = Some(stream);
                            drop(stream_guard);

                            // Start message receiver
                            Self::spawn_message_receiver(
                                state.clone(),
                                partner_stream.clone(),
                                evil_id,
                            );
                            return;
                        }
                        Err(e) => {
                            if attempt % 5 == 0 {
                                println!(
                                    "👿 Evil #{} - Connection attempt {}/30 to {} failed: {}",
                                    evil_id, attempt, partner_sync, e
                                );
                            }
                            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                        }
                    }
                }
                println!(
                    "⚠️  Evil #{} - Could not connect to partner, running solo",
                    evil_id
                );
            });
        }

        Ok(())
    }

    /// Spawn background task to receive messages from partner
    fn spawn_message_receiver(
        state: Arc<RwLock<CoordinatorState>>,
        partner_stream: Arc<RwLock<Option<TcpStream>>>,
        evil_id: u8,
    ) {
        tokio::spawn(async move {
            loop {
                let msg = {
                    let mut stream_guard = partner_stream.write().await;
                    if let Some(stream) = stream_guard.as_mut() {
                        match tokio::time::timeout(
                            tokio::time::Duration::from_millis(100),
                            EvilSync::receive_async(stream),
                        )
                        .await
                        {
                            Ok(Ok(msg)) => Some(msg),
                            _ => None,
                        }
                    } else {
                        None
                    }
                };

                if let Some(msg) = msg {
                    match msg {
                        EvilSync::SharePrivateBlock(block) => {
                            println!("👿 Evil #{} - Received block from partner", evil_id);
                            let mut state_guard = state.write().await;
                            state_guard.partner_blocks.push(block);
                        }
                        EvilSync::TriggerRelease => {
                            println!("👿 Evil #{} - Partner triggered release!", evil_id);
                            let mut state_guard = state.write().await;
                            state_guard.release_triggered = true;
                        }
                        EvilSync::ShadowChainStatus { length, .. } => {
                            let mut state_guard = state.write().await;
                            state_guard.partner_shadow_length = length;
                        }
                        EvilSync::Ping => {
                            let mut stream_guard = partner_stream.write().await;
                            if let Some(stream) = stream_guard.as_mut() {
                                let _ = EvilSync::Pong.send_async(stream).await;
                            }
                        }
                        EvilSync::Pong | EvilSync::Ack => {}
                    }
                }

                tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            }
        });
    }

    /// Share a privately mined block with partner
    pub async fn share_block(&self, block: Block) -> Result<()> {
        let mut stream_guard = self.partner_stream.write().await;
        if let Some(stream) = stream_guard.as_mut() {
            let msg = EvilSync::SharePrivateBlock(block);
            msg.send_async(stream).await?;
        }
        Ok(())
    }

    /// Signal partner to release all blocks
    pub async fn trigger_release(&self) -> Result<()> {
        let mut stream_guard = self.partner_stream.write().await;
        if let Some(stream) = stream_guard.as_mut() {
            let msg = EvilSync::TriggerRelease;
            msg.send_async(stream).await?;
        }

        let mut state_guard = self.state.write().await;
        state_guard.release_triggered = true;

        Ok(())
    }

    /// Update partner about our shadow chain status
    pub async fn send_status(&self, length: u64, fork_point: Hash) -> Result<()> {
        let mut stream_guard = self.partner_stream.write().await;
        if let Some(stream) = stream_guard.as_mut() {
            let msg = EvilSync::ShadowChainStatus { length, fork_point };
            msg.send_async(stream).await?;
        }
        Ok(())
    }

    /// Process incoming messages from partner
    /// Note: This is now handled automatically by spawn_message_receiver()
    pub async fn process_incoming(&self) -> Result<()> {
        Ok(())
    }

    /// Check if release was triggered by partner
    pub async fn is_release_triggered(&self) -> bool {
        self.state.read().await.release_triggered
    }

    /// Check if connected to partner
    pub async fn is_connected(&self) -> bool {
        self.state.read().await.connected
    }

    /// Get partner's shadow chain length
    pub async fn partner_shadow_length(&self) -> u64 {
        self.state.read().await.partner_shadow_length
    }
}
