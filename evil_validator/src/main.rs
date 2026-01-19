//! Evil Validator - 51% Attack Simulator
//!
//! This validator simulates blockchain attacks for educational purposes.
//! Use with 2 evil validators against 1 honest validator to demonstrate
//! the importance of decentralization and stake distribution.
//!
//! ⚠️  FOR EDUCATIONAL PURPOSES ONLY ⚠️

mod attack_coordinator;
mod cli;
mod evil_proposer;
mod shadow_chain;
mod tui;

use anyhow::{Result, anyhow};
use clap::Parser;
use core::panic;
use node_lib::{BLOCKCHAIN, NODES, handler, util};
use poslib::crypto::PrivateKey;
use poslib::types::{Blockchain, SLOT_DURATION_SECS};
use poslib::util::Saveable;
use std::path::Path;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tokio::time::{Duration, interval};

use crate::attack_coordinator::AttackCoordinator;
use crate::cli::Cli;
use crate::evil_proposer::EvilProposer;
use crate::tui::{TuiApp, run_simple_output, run_tui};

fn print_evil_banner() {
    println!("\x1b[31m");
    println!("╔═══════════════════════════════════════════════════════════╗");
    println!("║                                                           ║");
    println!("║     🤡  EVIL VALIDATOR - 51% ATTACK SIMULATOR  🤡        ║");
    println!("║                                                           ║");
    println!("║     ⚠️  FOR EDUCATIONAL PURPOSES ONLY ⚠️                 ║");
    println!("║                                                           ║");
    println!("╚═══════════════════════════════════════════════════════════╝");
    println!("\x1b[0m");
    println!();
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    print_evil_banner();

    // Load private key
    let private_key = PrivateKey::load_from_file(&cli.private_key_file).map_err(|e| {
        anyhow!(
            "Error reading private key from '{}': {}",
            cli.private_key_file,
            e
        )
    })?;

    let public_key = private_key.public_key();
    println!(
        "🔑 Evil Validator #{} public key: {:?}",
        cli.evil_id, public_key
    );
    println!("👿 Attack mode: {:?}", cli.attack_mode);
    println!(
        "🎯 Release threshold: {} blocks ahead",
        cli.release_threshold
    );

    // =========================================================================
    // NODE INITIALIZATION (same as normal validator)
    // =========================================================================

    let nodes = cli.get_nodes();

    // Load or initialize blockchain
    if Path::new(&cli.blockchain_file).exists() {
        println!("📂 Loading blockchain from: {}", cli.blockchain_file);
        util::load_blockchain(&cli.blockchain_file).await?;
    } else {
        println!("📂 No blockchain found, syncing from network...");
        if nodes.is_empty() {
            panic!(
                "Genesis block creation not implemented in evil validator, for realistic simulation please provide peer nodes to sync from."
            );
        } else {
            let (longest_name, longest_count) = util::find_longest_chain_node().await?;
            util::download_blockchain(&longest_name, longest_count).await?;
            println!("✅ Downloaded blockchain from {}", longest_name);

            let mut blockchain = BLOCKCHAIN.write().await;
            blockchain.rebuild_utxos();
        }
    }

    // Display stake status
    {
        let blockchain = BLOCKCHAIN.read().await;
        let stakes = blockchain.calculate_stakes();
        let our_stake = stakes.get(&public_key).cloned().unwrap_or(0);
        let total_stake: u64 = stakes.values().sum();
        let min_stake = Blockchain::get_min_stake_amount();
        let current_slot = blockchain.current_slot();
        let finalized_height = blockchain.finalized_height();

        println!("\n💰 Stake status:");
        println!(
            "   Our stake: {} ({:.1}% of total)",
            our_stake,
            if total_stake > 0 {
                (our_stake as f64 / total_stake as f64) * 100.0
            } else {
                0.0
            }
        );
        println!("   Total staked: {}", total_stake);
        println!("   Minimum required: {}", min_stake);
        
        println!("\n⏱️  Consensus status:");
        println!("   Current slot: {}", current_slot);
        println!("   Finalized height: {}", finalized_height);
        println!("   Slot duration: {}s", SLOT_DURATION_SECS);
        println!("   ⚠️  Blocks < finalized height CANNOT be reverted!");

        if our_stake < min_stake {
            println!("\x1b[33m⚠️  WARNING: Insufficient stake! Attack may not work.\x1b[0m");
        } else {
            let stake_percentage = (our_stake as f64 / total_stake as f64) * 100.0;
            if stake_percentage > 50.0 {
                println!(
                    "\x1b[31m😈 You control {:.1}% - 51% attack is possible!\x1b[0m",
                    stake_percentage
                );
            } else {
                println!(
                    "\x1b[33m⚠️  You only control {:.1}% - need >50% for reliable attack\x1b[0m",
                    stake_percentage
                );
            }
        }
    }

    // =========================================================================
    // SETUP ATTACK COORDINATOR (for multi-validator coordination)
    // =========================================================================

    let coordinator = if cli.evil_partner.is_some() {
        let coord = Arc::new(AttackCoordinator::new(
            cli.evil_id,
            cli.port,  // Pass our normal port, sync port = port + 10000
            cli.evil_partner.clone(),
        ));
        coord.start().await?;
        coord.process_incoming().await?;
        println!(
            "👿 Attack coordinator ready, partner target: {:?}",
            cli.evil_partner
        );
        Some(coord)
    } else {
        println!("👿 Running solo (no evil partner specified)");
        None
    };

    // =========================================================================
    // CREATE EVIL PROPOSER
    // =========================================================================

    let proposer = Arc::new(EvilProposer::new(
        private_key,
        cli.attack_mode,
        cli.release_threshold,
        cli.network_delay_ms,
        coordinator.clone(),
    ));

    // Initialize shadow chain
    proposer.init_shadow_chain().await;

    // =========================================================================
    // START NODE SERVICES
    // =========================================================================

    let addr = format!("0.0.0.0:{}", cli.port);
    let listener = TcpListener::bind(&addr).await?;
    println!("\n🌐 Evil node listening on {}", addr);

    // Background tasks
    tokio::spawn(util::cleanup());
    tokio::spawn(util::save(cli.blockchain_file.clone()));
    tokio::spawn(util::populate_connections(nodes, cli.port));

    // Connection handler
    tokio::spawn(async move {
        loop {
            if let Ok((socket, _)) = listener.accept().await {
                tokio::spawn(handler::handle_connection(socket));
            }
        }
    });

    // =========================================================================
    // SETUP TUI OR SIMPLE OUTPUT
    // =========================================================================

    let tui_app = Arc::new(RwLock::new(TuiApp::new(
        proposer.clone(),
        coordinator.clone(),
        cli.attack_mode,
        cli.evil_id,
    )));

    // Start TUI or simple output in background
    let tui_app_clone = tui_app.clone();
    let no_tui = cli.no_tui;

    tokio::spawn(async move {
        if no_tui {
            run_simple_output(tui_app_clone).await;
        } else {
            if let Err(e) = run_tui(tui_app_clone).await {
                eprintln!("TUI error: {}", e);
            }
        }
    });

    // =========================================================================
    // EVIL BLOCK PROPOSAL LOOP
    // =========================================================================

    println!(
        "\n\x1b[31m🚀 Evil validator started. Checking for slot every {}s\x1b[0m",
        SLOT_DURATION_SECS
    );
    println!("═══════════════════════════════════════════════════════════\n");

    let mut slot_timer = interval(Duration::from_secs(SLOT_DURATION_SECS));
    let mut last_slot = 0u64;

    loop {
        tokio::select! {
            _ = slot_timer.tick() => {
                // Get consensus info
                let (current_slot, finalized_height, is_our_turn) = {
                    let blockchain = BLOCKCHAIN.read().await;
                    let slot = blockchain.current_slot();
                    let finalized = blockchain.finalized_height();
                    let our_turn = proposer.is_our_turn(&blockchain);
                    (slot, finalized, our_turn)
                };

                // Show slot change
                if current_slot != last_slot {
                    println!("\n⏱️  Slot {} | Finalized height: {}", current_slot, finalized_height);
                    last_slot = current_slot;
                }

                if is_our_turn {
                    println!("\x1b[31m👿 It's our turn to be EVIL!\x1b[0m");

                    // Log to TUI
                    {
                        let app = tui_app.read().await;
                        app.add_log("Our slot - mining evil block!".to_string()).await;
                    }

                    // Propose block based on attack mode
                    match proposer.propose_block_evil().await {
                        Ok(_) => {
                            println!("\x1b[31m✅ Evil block operation completed\x1b[0m");
                        }
                        Err(e) => {
                            eprintln!("\x1b[33m⚠️  Evil proposal failed: {}\x1b[0m", e);
                        }
                    }
                } else {
                    // Monitor the network when it's not our turn
                    let blockchain = BLOCKCHAIN.read().await;
                    let height = blockchain.block_height();
                    let finalized = blockchain.finalized_height();
                    drop(blockchain);

                    let shadow = proposer.shadow_chain.read().await;
                    let advantage = shadow.calculate_advantage(height);
                    let shadow_len = shadow.len();
                    let fork_height = shadow.fork_point_height();
                    drop(shadow);

                    if shadow_len > 0 {
                        // Check if our fork point is now finalized (attack failed!)
                        if fork_height < finalized {
                            println!("\x1b[33m⚠️  ATTACK BLOCKED! Fork point {} is now finalized ({})\x1b[0m", 
                                fork_height, finalized);
                            println!("\x1b[33m   Shadow chain is now invalid, reinitializing...\x1b[0m");
                            proposer.init_shadow_chain().await;
                        } else {
                            println!(
                                "👁️  Watching... Public height: {}, Finalized: {}, Shadow: {} blocks, Advantage: {}",
                                height, finalized, shadow_len, advantage
                            );
                        }
                    }

                    // Check if we should release based on partner signal
                    if let Some(coord) = &coordinator {
                        if coord.is_release_triggered().await {
                            println!("\x1b[31m👿 Partner triggered release!\x1b[0m");
                            let _ = proposer.release_attack().await;
                        }
                    }
                }
                
                // Cleanup old consensus data
                {
                    let mut blockchain = BLOCKCHAIN.write().await;
                    blockchain.cleanup_consensus();
                }
            }
        }
    }
}
