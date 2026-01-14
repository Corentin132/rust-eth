use anyhow::Result;
use crossbeam_skiplist::SkipMap;
use poslib::STAKE_MINIMUM_AMOUNT;
use poslib::crypto::{PrivateKey, PublicKey};
use poslib::network::Message;
use poslib::types::{Transaction, TransactionOutput};
use poslib::util::Saveable;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpStream;

use kanal::AsyncSender;

#[derive(Serialize, Deserialize, Clone)]
pub struct Key {
    public: PathBuf,
    private: PathBuf,
}
#[derive(Clone)]
struct LoadedKey {
    public: PublicKey,
    private: PrivateKey,
}
#[derive(Serialize, Deserialize, Clone)]
pub struct Recipient {
    pub name: String,
    pub key: PathBuf,
}
#[derive(Clone)]
pub struct LoadedRecipient {
    pub name: String,
    pub key: PublicKey,
}
impl Recipient {
    pub fn load(&self) -> Result<LoadedRecipient> {
        let key = PublicKey::load_from_file(&self.key)?;
        Ok(LoadedRecipient {
            name: self.name.clone(),
            key,
        })
    }
}
#[derive(Serialize, Deserialize, Clone)]
pub enum FeeType {
    Fixed,
    Percent,
}
#[derive(Serialize, Deserialize, Clone)]
pub struct FeeConfig {
    pub fee_type: FeeType,
    pub value: f64,
}
#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    pub my_keys: Vec<Key>,
    pub contacts: Vec<Recipient>,
    pub default_node: String,
    pub fee_config: FeeConfig,
}

#[derive(Clone)]
struct UtxoStore {
    my_keys: Vec<LoadedKey>,
    utxos: Arc<SkipMap<PublicKey, Vec<(bool, TransactionOutput)>>>,
}
impl UtxoStore {
    fn new() -> Self {
        UtxoStore {
            my_keys: vec![],
            utxos: Arc::new(SkipMap::new()),
        }
    }
    fn add_key(&mut self, key: LoadedKey) {
        self.my_keys.push(key);
    }
}
#[derive(Clone)]
pub struct Core {
    pub config: Config,
    utxos: UtxoStore,
    pub tx_sender: AsyncSender<Transaction>,
}
impl Core {
    // ...
    fn new(config: Config, utxos: UtxoStore) -> Self {
        let (tx_sender, _) = kanal::bounded(10);
        Core {
            config,
            utxos,
            tx_sender: tx_sender.clone_async(),
        }
    }
    pub fn load(config_path: PathBuf) -> Result<Self> {
        let config: Config = toml::from_str(&fs::read_to_string(&config_path)?)?;
        if !config.my_keys.is_empty() {
            println!("Loaded wallet config from {}", config_path.display());
        } else {
            println!(
                "Warning: No keys found in wallet config {}",
                config_path.display()
            );
        }
        let mut utxos = UtxoStore::new();
        // Load keys from config
        for key in &config.my_keys {
            let public = PublicKey::load_from_file(&key.public)?;
            let private = PrivateKey::load_from_file(&key.private)?;
            utxos.add_key(LoadedKey { public, private });
        }
        Ok(Core::new(config, utxos))
    }
    pub async fn fetch_utxos(&self) -> Result<()> {
        let mut stream = TcpStream::connect(&self.config.default_node).await?;
        for key in &self.utxos.my_keys {
            let message = Message::FetchUTXOs(key.public.clone());
            message.send_async(&mut stream).await?;
            if let Message::UTXOs(utxos) = Message::receive_async(&mut stream).await? {
                // Replace the entire UTXO set for this key
                self.utxos.utxos.insert(
                    key.public.clone(),
                    utxos
                        .into_iter()
                        .map(|(output, marked)| (marked, output))
                        .collect(),
                );
            } else {
                return Err(anyhow::anyhow!("Unexpected response from node"));
            }
        }
        Ok(())
    }
    pub async fn send_transaction(&self, transaction: Transaction) -> Result<()> {
        let mut stream = TcpStream::connect(&self.config.default_node).await?;
        let message = Message::SubmitTransaction(transaction);
        message.send_async(&mut stream).await?;
        Ok(())
    }

    /// Fetch current block height from the node (source of truth)
    pub async fn fetch_block_height(&self) -> Result<u64> {
        let mut stream = TcpStream::connect(&self.config.default_node).await?;
        let message = Message::FetchBlockHeight;
        message.send_async(&mut stream).await?;

        if let Message::BlockHeight(height) = Message::receive_async(&mut stream).await? {
            Ok(height)
        } else {
            Err(anyhow::anyhow!("Unexpected response from node"))
        }
    }

    pub async fn create_transaction(
        &self,
        recipient: &PublicKey,
        amount: u64,
    ) -> Result<Transaction> {
        let fee = self.calculate_fee(amount);
        let total_amount = amount + fee;
        let mut inputs = Vec::new();
        let mut input_sum = 0;

        // Fetch current block height to check stake lock status
        let current_height = self.fetch_block_height().await?;

        // Debug: show UTXO state
        println!("=== DEBUG UTXO State ===");
        println!("Current block height: {}", current_height);
        for entry in self.utxos.utxos.iter() {
            let pubkey = entry.key();
            let utxos = entry.value();
            println!("Key: {:?}", pubkey);
            for (i, (marked, utxo)) in utxos.iter().enumerate() {
                println!(
                    "  UTXO {}: value={}, marked={}, is_stake={}, locked_until={}",
                    i, utxo.value, marked, utxo.is_stake, utxo.locked_until
                );
                let can_spend = !marked && !(utxo.is_stake && utxo.locked_until > current_height);
                println!("    -> can_spend: {}", can_spend);
            }
        }
        println!("========================");

        for entry in self.utxos.utxos.iter() {
            let pubkey = entry.key();
            let utxos = entry.value();
            for (marked, utxo) in utxos.iter() {
                if *marked {
                    continue; // Skip used UTXOs
                }
                // Skip zero-value UTXOs - they are useless and may cause validation errors
                if utxo.value == 0 {
                    continue;
                }
                // Skip locked staked UTXOs - they can't be spent until unlocked
                if utxo.is_stake && utxo.locked_until > current_height {
                    continue;
                }
                if input_sum >= total_amount {
                    break;
                }
                inputs.push(poslib::types::TransactionInput {
                    prev_transaction_output_hash: utxo.hash(),
                    signature: poslib::crypto::Signature::sign_output(
                        &utxo.hash(),
                        &self
                            .utxos
                            .my_keys
                            .iter()
                            .find(|k| k.public == *pubkey)
                            .unwrap()
                            .private,
                    ),
                });
                input_sum += utxo.value;
            }
            if input_sum >= total_amount {
                break;
            }
        }
        println!("Total input_sum collected: {}", input_sum);
        println!("Total amount needed: {}", total_amount);

        if input_sum < total_amount {
            return Err(anyhow::anyhow!(format!(
                "Insufficient funds, total amount : {} (note: locked staked coins cannot be spent)",
                total_amount
            )));
        }
        let mut outputs = vec![TransactionOutput {
            value: amount,
            unique_id: uuid::Uuid::new_v4(),
            pubkey: recipient.clone(),
            is_stake: false,
            locked_until: 0,
        }];
        if input_sum > total_amount {
            outputs.push(TransactionOutput {
                value: input_sum - total_amount,
                unique_id: uuid::Uuid::new_v4(),
                pubkey: self.utxos.my_keys[0].public.clone(),
                is_stake: false,
                locked_until: 0,
            });
        }
        Ok(Transaction::new(inputs, outputs))
    }

    pub async fn create_stake_transaction(&self, amount: u64) -> Result<Transaction> {
        let fee = self.calculate_fee(amount);
        let total_amount = amount + fee;
        let mut inputs = Vec::new();
        let mut input_sum = 0;

        // Fetch current block height to check stake lock status
        let current_height = self.fetch_block_height().await?;

        // We use the first key for staking for simplicity, or we could iterate
        // For now, let's assume we stake from the first available funds found
        for entry in self.utxos.utxos.iter() {
            let pubkey = entry.key();
            let utxos = entry.value();
            for (marked, utxo) in utxos.iter() {
                if *marked {
                    continue;
                }
                // Skip UTXOs with no value
                if utxo.value == 0 {
                    continue;
                }
                // Skip locked staked UTXOs - they can't be spent until unlocked
                if utxo.is_stake && utxo.locked_until > current_height {
                    continue;
                }
                if input_sum >= total_amount {
                    break;
                }
                inputs.push(poslib::types::TransactionInput {
                    prev_transaction_output_hash: utxo.hash(),
                    signature: poslib::crypto::Signature::sign_output(
                        &utxo.hash(),
                        &self
                            .utxos
                            .my_keys
                            .iter()
                            .find(|k| k.public == *pubkey)
                            .unwrap()
                            .private,
                    ),
                });
                input_sum += utxo.value;
            }
            if input_sum >= total_amount {
                break;
            }
        }

        if input_sum < total_amount {
            return Err(anyhow::anyhow!("Insufficient funds"));
        }

        // The output is sent back to ourselves (the first key), but marked as stake
        let my_pubkey = self.utxos.my_keys[0].public.clone();

        // Fetch current block height from the node (source of truth)
        let current_height = self.fetch_block_height().await?;
        // Calculate lock period: current block height + STAKE_LOCK_PERIOD
        let lock_until = current_height + poslib::STAKE_LOCK_PERIOD;

        let mut outputs = vec![TransactionOutput {
            value: amount,
            unique_id: uuid::Uuid::new_v4(),
            pubkey: my_pubkey.clone(),
            is_stake: true,           // This is the key difference
            locked_until: lock_until, // Stake is locked for STAKE_LOCK_PERIOD blocks
        }];

        // Change output (not staked)
        if input_sum > total_amount {
            outputs.push(TransactionOutput {
                value: input_sum - total_amount,
                unique_id: uuid::Uuid::new_v4(),
                pubkey: my_pubkey,
                is_stake: false,
                locked_until: 0,
            });
        }
        Ok(Transaction::new(inputs, outputs))
    }

    /// Create a transaction to unstake coins (convert staked UTXOs back to regular UTXOs)
    /// Note: The node will validate that the stake lock period has passed
    pub async fn create_unstake_transaction(&self, amount: u64) -> Result<Transaction> {
        let fee = self.calculate_fee(amount);
        let total_amount = amount + fee;
        let mut inputs = Vec::new();
        let mut input_sum = 0;

        // Fetch current height from node for display purposes only
        // The actual validation is done by the node in add_to_mempool
        let current_height = self.fetch_block_height().await?;

        // Find staked UTXOs that appear unlocked (lock period has passed)
        for entry in self.utxos.utxos.iter() {
            let pubkey = entry.key();
            let utxos = entry.value();
            for (marked, utxo) in utxos.iter() {
                if *marked {
                    continue; // Skip marked UTXOs
                }
                // Skip UTXOs with no value
                if utxo.value == 0 {
                    continue;
                }
                // Only use staked UTXOs that are unlocked
                if !utxo.is_stake {
                    continue;
                }
                // Check if lock period has passed (display check only, node validates)
                if utxo.locked_until > current_height {
                    continue; // Still locked
                }
                if input_sum >= total_amount {
                    break;
                }
                inputs.push(poslib::types::TransactionInput {
                    prev_transaction_output_hash: utxo.hash(),
                    signature: poslib::crypto::Signature::sign_output(
                        &utxo.hash(),
                        &self
                            .utxos
                            .my_keys
                            .iter()
                            .find(|k| k.public == *pubkey)
                            .unwrap()
                            .private,
                    ),
                });
                input_sum += utxo.value;
            }
            if input_sum >= total_amount {
                break;
            }
        }

        if input_sum < total_amount {
            return Err(anyhow::anyhow!(
                "Insufficient unlocked staked funds. You may need to wait for the lock period to expire."
            ));
        }

        let my_pubkey = self.utxos.my_keys[0].public.clone();

        // Output is NOT staked anymore
        let mut outputs = vec![TransactionOutput {
            value: amount,
            unique_id: uuid::Uuid::new_v4(),
            pubkey: my_pubkey.clone(),
            is_stake: false, // No longer staked
            locked_until: 0,
        }];

        // Change output (also not staked)
        if input_sum > total_amount {
            outputs.push(TransactionOutput {
                value: input_sum - total_amount,
                unique_id: uuid::Uuid::new_v4(),
                pubkey: my_pubkey,
                is_stake: false,
                locked_until: 0,
            });
        }
        Ok(Transaction::new(inputs, outputs))
    }

    // Get the amount of currently locked staked coins
    pub async fn get_active_stake_balance(&self) -> Result<u64> {
        let current_height = self.fetch_block_height().await?;
        Ok(self
            .utxos
            .utxos
            .iter()
            .map(|entry| {
                entry
                    .value()
                    .iter()
                    .filter(|(_, utxo)| utxo.is_stake && utxo.locked_until > current_height)
                    .map(|(_, utxo)| utxo.value)
                    .sum::<u64>()
            })
            .sum())
    }

    // Get the amount of currently unlocked staked coins -> Not available for staking anymore
    pub async fn get_unlocked_stake_balance(&self) -> Result<u64> {
        let current_height = self.fetch_block_height().await?;
        Ok(self
            .utxos
            .utxos
            .iter()
            .map(|entry| {
                entry
                    .value()
                    .iter()
                    .filter(|(_, utxo)| utxo.is_stake && utxo.locked_until <= current_height)
                    .map(|(_, utxo)| utxo.value)
                    .sum::<u64>()
            })
            .sum())
    }

    fn calculate_fee(&self, amount: u64) -> u64 {
        match self.config.fee_config.fee_type {
            FeeType::Fixed => self.config.fee_config.value as u64,
            FeeType::Percent => (amount as f64 * self.config.fee_config.value / 100.0) as u64,
        }
    }

    pub async fn get_balance(&self) -> Result<u64> {
        let current_height = self.fetch_block_height().await?;
        Ok(self
            .utxos
            .utxos
            .iter()
            .map(|entry| {
                entry
                    .value()
                    .iter()
                    .filter(|(marked, utxo)| {
                        // Skip marked UTXOs and locked staked UTXOs
                        !marked && !(utxo.is_stake && utxo.locked_until > current_height)
                    })
                    .map(|(_, utxo)| utxo.value)
                    .sum::<u64>()
            })
            .sum())
    }
    pub fn get_min_stake_amount(&self) -> u64 {
        STAKE_MINIMUM_AMOUNT
    }
}
