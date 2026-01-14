//! Node library - provides core blockchain node functionality
//!
//! This library exposes the core node functionality that can be reused by:
//! - The standalone node binary
//! - Validators (which are nodes with additional proposer capabilities)
//! - Other node types

pub mod handler;
pub mod util;

use dashmap::DashMap;
use poslib::types::Blockchain;
use static_init::dynamic;
use tokio::net::TcpStream;
use tokio::sync::RwLock;

// ============================================================================
// Shared State
// ============================================================================

/// Global blockchain state - thread-safe read/write access
#[dynamic]
pub static BLOCKCHAIN: RwLock<Blockchain> = RwLock::new(Blockchain::new());
/// Connected peer nodes
#[dynamic]
pub static NODES: DashMap<String, TcpStream> = DashMap::new();

pub const NODES_SELF_ADDRESS: &str = "127.0.0.1";
// ============================================================================
// Re-exports for convenience
// ============================================================================

pub use dashmap;
pub use poslib;
pub use tokio;
