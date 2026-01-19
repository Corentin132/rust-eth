//! Proof of Stake Validator
//!
//! A validator is a **full node** with additional capabilities:
//! - Can propose new blocks when selected (based on slot timing)
//! - Must maintain sufficient stake
//! - Attests to blocks proposed by other validators
//! - Can be slashed for misbehavior (double voting, etc.)
//!
//! This implementation reuses the node library for all common functionality
//! and only adds validator-specific logic.

mod cli;
mod proposer;

use anyhow::{Result, anyhow};
use clap::Parser;
use node_lib::{BLOCKCHAIN, NODES, handler, util};
use poslib::crypto::PrivateKey;
use poslib::types::{Blockchain, SLOT_DURATION_SECS};
use poslib::util::Saveable;
use std::path::Path;
use tokio::net::TcpListener;
use tokio::time::{Duration, interval};

use crate::cli::Cli;
use crate::proposer::BlockProposer;

fn print_banner() {
    println!("═══════════════════════════════════════════════");
    println!("    Proof of Stake Validator v0.3.0           ");
    println!("    (Full Node + Block Proposer)              ");
    println!("═══════════════════════════════════════════════");
    println!();
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    print_banner();

    // Load private key for signing blocks
    let private_key = PrivateKey::load_from_file(&cli.private_key_file).map_err(|e| {
        anyhow!(
            "Error reading private key from '{}': {}",
            cli.private_key_file,
            e
        )
    })?;

    let public_key = private_key.public_key();
    println!("🔑 Validator public key: {:?}", public_key);

    // =========================================================================
    // REUSE NODE INITIALIZATION (from node_lib)
    // =========================================================================

    let nodes = cli.get_nodes();

    // Connect to peer nodes
    println!("📡 Connected to {} peer nodes", NODES.len());

    // Load or initialize blockchain
    if Path::new(&cli.blockchain_file).exists() {
        println!("📂 Loading blockchain from: {}", cli.blockchain_file);
        util::load_blockchain(&cli.blockchain_file).await?;
    } else {
        println!("📂 No blockchain found, syncing from network...");
        if nodes.is_empty() {
            println!("🌱 No peers provided, creating genesis block as seed validator");
            let genesis_block = util::create_genesis_block();
            let mut blockchain = BLOCKCHAIN.write().await;
            blockchain
                .add_block(genesis_block)
                .expect("Failed to add genesis block");
            // Rebuild UTXOs after creating genesis block
            blockchain.rebuild_utxos();
        } else {
            let (longest_name, longest_count) = util::find_longest_chain_node().await?;
            util::download_blockchain(&longest_name, longest_count).await?;
            println!("✅ Downloaded blockchain from {}", longest_name);

            let mut blockchain = BLOCKCHAIN.write().await;
            blockchain.rebuild_utxos();
        }
    }

    // Display validator status
    {
        let blockchain = BLOCKCHAIN.read().await;
        let stakes = blockchain.calculate_stakes();
        let our_stake = stakes.get(&public_key).cloned().unwrap_or(0);
        let min_stake = Blockchain::get_min_stake_amount();
        let current_slot = blockchain.current_slot();
        let genesis_time = blockchain.genesis_time();

        println!("\n💰 Stake status:");
        println!("   Our stake: {}", our_stake);
        println!("   Minimum required: {}", min_stake);
        
        println!("\n⏱️  Consensus timing:");
        println!("   Genesis time: {}", genesis_time);
        println!("   Current slot: {}", current_slot);
        println!("   Slot duration: {}s", SLOT_DURATION_SECS);

        if our_stake < min_stake {
            println!("⚠️  WARNING: Insufficient stake! You cannot propose or attest blocks.");
        } else {
            println!("✅ Sufficient stake to be a validator");
        }
    }

    // =========================================================================
    // START NODE SERVICES (from node_lib)
    // =========================================================================

    // Start listening for connections (node functionality)
    let addr = format!("0.0.0.0:{}", cli.port);
    let listener = TcpListener::bind(&addr).await?;
    println!("\n🌐 Node listening on {}", addr);

    // Start background tasks (reusing node code)
    tokio::spawn(util::cleanup());
    tokio::spawn(util::save(cli.blockchain_file.clone()));
    // DEV : async func so listener port is passed correctly
    // In Eth, the validator connects to other nodes rather than other nodes connecting to it --> with a trusted boot node logicic 🫡
    tokio::spawn(util::populate_connections(nodes, cli.port));
    // Spawn connection handler (node functionality)
    let listener_handle = tokio::spawn(async move {
        loop {
            if let Ok((socket, _)) = listener.accept().await {
                tokio::spawn(handler::handle_connection(socket));
            }
        }
    });

    // =========================================================================
    // VALIDATOR-SPECIFIC: BLOCK PROPOSAL LOOP
    // =========================================================================

    let proposer = BlockProposer::new(private_key);

    println!(
        "\n🚀 Validator started. Slot duration: {}s",
        SLOT_DURATION_SECS
    );
    println!("═══════════════════════════════════════════════\n");

    let mut slot_timer = interval(Duration::from_secs(SLOT_DURATION_SECS));
    let mut last_slot = 0u64;

    loop {
        tokio::select! {
            _ = slot_timer.tick() => {
                // Get current slot and consensus info
                let (current_slot, finalized_height, is_our_turn) = {
                    let blockchain = BLOCKCHAIN.read().await;
                    let slot = blockchain.current_slot();
                    let finalized = blockchain.finalized_height();
                    let our_turn = proposer.is_our_turn(&blockchain);
                    (slot, finalized, our_turn)
                };

                // Only print slot change once
                if current_slot != last_slot {
                    println!("\n⏱️  Slot {} | Finalized height: {}", current_slot, finalized_height);
                    last_slot = current_slot;
                }

                if is_our_turn {
                    println!("\n🔔 IT'S OUR TURN TO PROPOSE A BLOCK!");

                    if let Err(e) = proposer.propose_block().await {
                        eprintln!("❌ Block proposal failed: {}", e);
                    }
                } else {
                    println!("⏳ Not our turn to propose a block this slot.");
                }
                
                // Cleanup old consensus data periodically
                {
                    let mut blockchain = BLOCKCHAIN.write().await;
                    blockchain.cleanup_consensus();
                }
            }
            _ = tokio::signal::ctrl_c() => {
                println!("\n👋 Shutting down validator...");
                break;
            }
        }
    }

    listener_handle.abort();
    Ok(())
}
