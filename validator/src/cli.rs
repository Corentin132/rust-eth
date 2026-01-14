//! Command-line interface definition for the validator

use clap::{Parser, arg, command};

#[derive(Parser)]
#[command(
    author, 
    version, 
    about = "Proof of Stake Validator - Full Node + Block Proposer", 
    long_about = None
)]
pub struct Cli {
    /// Port to listen on for incoming connections
    #[arg(long, default_value = "9001")]
    pub port: u16,
    
    /// Path to the validator's private key file
    #[arg(short, long)]
    pub private_key_file: String,
    
    /// Path to the local blockchain file
    #[arg(short, long, default_value = "validator_blockchain.cbor")]
    pub blockchain_file: String,
    
    /// Addresses of peer nodes to connect to (comma-separated, e.g. "127.0.0.1:9001,127.0.0.1:9002")
    #[arg(short, long, default_value = "")]
    pub nodes: String,
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
