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
    
    /// Addresses of peer nodes to connect to
    #[arg(short, long)]
    pub nodes: Vec<String>,
}
