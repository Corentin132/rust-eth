mod core;
use anyhow::Result;
use clap::{Parser, Subcommand};
use core::{Config, Core, FeeConfig, FeeType, Recipient};
use kanal::bounded;
use poslib::types::Transaction;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::time::{self, Duration};
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(short, long, value_name = "FILE", default_value_os_t = PathBuf::from("wallet_config.toml"))]
    config: PathBuf,

    #[arg(short, long, value_name = "ADDRESS")]
    node: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    GenerateConfig {
        #[arg(short, long, value_name = "FILE", default_value_os_t = PathBuf::from("wallet_config.toml"))]
        output: PathBuf,
    },
}

fn generate_dummy_config(path: &PathBuf) -> Result<()> {
    let dummy_config = Config {
        my_keys: vec![],
        contacts: vec![
            Recipient {
                name: "Alice".to_string(),
                key: PathBuf::from("alice.pub.pem"),
            },
            Recipient {
                name: "Bob".to_string(),
                key: PathBuf::from("bob.pub.pem"),
            },
        ],
        default_node: "127.0.0.1:9000".to_string(),
        fee_config: FeeConfig {
            fee_type: FeeType::Percent,
            value: 0.1,
        },
    };
    let config_str = toml::to_string_pretty(&dummy_config)?;
    std::fs::write(path, config_str)?;
    println!("Dummy config generated at: {}", path.display());
    Ok(())
}
async fn update_utxos(core: Arc<Core>) {
    let mut interval = time::interval(Duration::from_secs(20));
    loop {
        interval.tick().await;
        if let Err(e) = core.fetch_utxos().await {
            eprintln!("Failed to update UTXOs: {}", e);
        }
    }
}
async fn handle_transactions(rx: kanal::AsyncReceiver<Transaction>, core: Arc<Core>) {
    while let Ok(transaction) = rx.recv().await {
        if let Err(e) = core.send_transaction(transaction).await {
            eprintln!("Failed to send transaction: {}", e);
        }
    }
}
async fn run_cli(core: Arc<Core>) -> Result<()> {
    loop {
        print!("> ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let parts: Vec<&str> = input.trim().split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }
        match parts[0] {
            "balance" => {
                println!("Current balance: {} satoshis", core.get_balance());
                println!(
                    "Stakable balance: {} satoshis",
                    core.get_unlocked_stake_balance().await?
                );
            }

            "send" => {
                if parts.len() != 3 {
                    println!("Usage: send <recipient> <amount>");
                    continue;
                }
                let recipient = parts[1];
                let amount: u64 = parts[2].parse()?;
                let recipient_key = core
                    .config
                    .contacts
                    .iter()
                    .find(|r| r.name == recipient)
                    .ok_or_else(|| anyhow::anyhow!("Recipient not found"))?
                    .load()?
                    .key;
                if let Err(e) = core.fetch_utxos().await {
                    println!("failed to fetch utxos: {e}");
                };
                let transaction = core.create_transaction(&recipient_key, amount).await?;
                core.tx_sender.send(transaction).await?;
                println!("Transaction sent successfully");
                core.fetch_utxos().await?;
            }
            "stake" => {
                if parts.len() == 1 {
                    println!(
                        "You need {} to be a validator node",
                        core.get_min_stake_amount()
                    );
                    println!(
                        "Your Stake amount is : {} satoshis",
                        core.get_active_stake_balance().await?
                    );
                    continue;
                }
                if parts.len() != 2 {
                    println!("Usage: stake or stake <amount> to stake coins");
                    continue;
                }
                let amount: u64 = parts[1].parse()?;
                if let Err(e) = core.fetch_utxos().await {
                    println!("failed to fetch utxos: {e}");
                };
                let transaction = core.create_stake_transaction(amount).await?;
                core.tx_sender.send(transaction).await?;
                println!("Stake transaction sent successfully");
                core.fetch_utxos().await?;
            }
            "unstake" => {
                if parts.len() == 1 {
                    println!(
                        "Your unstakable balance is : {} satoshis",
                        core.get_unlocked_stake_balance().await?
                    );
                    continue;
                }
                if parts.len() != 2 {
                    println!("Usage: unstake <amount>");
                    continue;
                }
                let amount: u64 = parts[1].parse()?;
                if let Err(e) = core.fetch_utxos().await {
                    println!("failed to fetch utxos: {e}");
                };
                let transaction = core.create_unstake_transaction(amount).await?;
                core.tx_sender.send(transaction).await?;
                println!("Unstake transaction sent successfully");
                core.fetch_utxos().await?;
            }
            "help" => {
                println!("Available commands:");
                println!("  balance               - Show current balance and staked balance");
                println!("  send <recipient> <amount> - Send amount to recipient");
                println!(
                    "  stake <amount>        - Send your coins to stake (or just 'stake' to view stakable balance)"
                );
                println!("  help                  - Show this help message");
                println!("  exit                  - Exit the wallet");
            }
            "exit" => break,
            _ => println!("Unknown command"),
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match &cli.command {
        Some(Commands::GenerateConfig { output }) => {
            return generate_dummy_config(output);
        }
        None => {}
    }
    let config_path = cli.config;
    let mut core = Core::load(config_path.clone())?;
    if let Some(node) = cli.node {
        core.config.default_node = node;
    }
    let (tx_sender, tx_receiver) = kanal::bounded(10);
    core.tx_sender = tx_sender.clone_async();
    let core = Arc::new(core);
    tokio::spawn(update_utxos(core.clone()));
    tokio::spawn(handle_transactions(tx_receiver.clone_async(), core.clone()));
    run_cli(core).await?;
    Ok(())
}
