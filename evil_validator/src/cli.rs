//! Command-line interface definition for the evil validator

use clap::{Parser, ValueEnum, arg, command};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AttackMode {
    /// Mine blocks secretly, release when ahead
    PrivateChain,
    /// Attempt double-spend attack
    DoubleSpend,
    /// Selfish mining - withhold blocks strategically
    SelfishMining,
    /// Just observe (passive evil mode)
    Observer,
}

#[derive(Parser)]
#[command(
    author,
    version,
    about = "Evil Validator - 51% Attack Simulator",
    long_about = "Simulates blockchain attacks for educational purposes. Use with 2 evil validators against 1 honest validator."
)]
pub struct Cli {
    /// Port to listen on for incoming connections
    #[arg(long, default_value = "9001")]
    pub port: u16,

    /// Path to the validator's private key file
    #[arg(short, long)]
    pub private_key_file: String,

    /// Path to the local blockchain file
    #[arg(short, long, default_value = "evil_blockchain.cbor")]
    pub blockchain_file: String,

    /// Addresses of peer nodes to connect to (comma-separated)
    #[arg(short, long, default_value = "")]
    pub nodes: String,

    // =========================================================================
    // EVIL-SPECIFIC OPTIONS
    // =========================================================================
    /// Attack mode to use
    #[arg(long, value_enum, default_value = "private-chain")]
    pub attack_mode: AttackMode,

    /// Address of partner evil validator for coordination (e.g., "127.0.0.1:9003")
    /// Uses the partner's normal --port for communication
    #[arg(long)]
    pub evil_partner: Option<String>,

    /// Number of blocks ahead before releasing private chain
    #[arg(long, default_value = "2")]
    pub release_threshold: u64,

    /// Simulated network delay in milliseconds (for realism)
    #[arg(long, default_value = "100")]
    pub network_delay_ms: u64,

    /// Disable TUI (text output only)
    #[arg(long)]
    pub no_tui: bool,

    /// Evil validator ID (1 or 2) for coordination
    #[arg(long, default_value = "1")]
    pub evil_id: u8,
}

impl Cli {
    /// Parse the nodes string into a vector of addresses
    pub fn get_nodes(&self) -> Vec<String> {
        self.nodes
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }
}
