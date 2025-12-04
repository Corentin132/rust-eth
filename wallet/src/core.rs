use anyhow::Result;
use btclib::crypto::{PrivateKey, PublicKey};
use btclib::network::Message;
use btclib::types::{Transaction, TransactionOutput};
use btclib::util::Saveable;
use crossbeam_skiplist::SkipMap;
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

    pub async fn create_transaction(
        &self,
        recipient: &PublicKey,
        amount: u64,
    ) -> Result<Transaction> {
        let fee = self.calculate_fee(amount);
        let total_amount = amount + fee;
        let mut inputs = Vec::new();
        let mut input_sum = 0;
        for entry in self.utxos.utxos.iter() {
            let pubkey = entry.key();
            let utxos = entry.value();
            for (marked, utxo) in utxos.iter() {
                if *marked {
                    continue; // Skip marked UTXOs
                }
                if input_sum >= total_amount {
                    break;
                }
                inputs.push(btclib::types::TransactionInput {
                    prev_transaction_output_hash: utxo.hash(),
                    signature: btclib::crypto::Signature::sign_output(
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
        let mut outputs = vec![TransactionOutput {
            value: amount,
            unique_id: uuid::Uuid::new_v4(),
            pubkey: recipient.clone(),
            is_stake: false,
        }];
        if input_sum > total_amount {
            outputs.push(TransactionOutput {
                value: input_sum - total_amount,
                unique_id: uuid::Uuid::new_v4(),
                pubkey: self.utxos.my_keys[0].public.clone(),
                is_stake: false,
            });
        }
        Ok(Transaction::new(inputs, outputs))
    }

    pub async fn create_stake_transaction(&self, amount: u64) -> Result<Transaction> {
        let fee = self.calculate_fee(amount);
        let total_amount = amount + fee;
        let mut inputs = Vec::new();
        let mut input_sum = 0;

        // We use the first key for staking for simplicity, or we could iterate
        // For now, let's assume we stake from the first available funds found
        for entry in self.utxos.utxos.iter() {
            let pubkey = entry.key();
            let utxos = entry.value();
            for (marked, utxo) in utxos.iter() {
                if *marked {
                    continue;
                }
                if input_sum >= total_amount {
                    break;
                }
                inputs.push(btclib::types::TransactionInput {
                    prev_transaction_output_hash: utxo.hash(),
                    signature: btclib::crypto::Signature::sign_output(
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

        let mut outputs = vec![TransactionOutput {
            value: amount,
            unique_id: uuid::Uuid::new_v4(),
            pubkey: my_pubkey.clone(),
            is_stake: true, // This is the key difference
        }];

        // Change output (not staked)
        if input_sum > total_amount {
            outputs.push(TransactionOutput {
                value: input_sum - total_amount,
                unique_id: uuid::Uuid::new_v4(),
                pubkey: my_pubkey,
                is_stake: false,
            });
        }
        Ok(Transaction::new(inputs, outputs))
    }

    fn calculate_fee(&self, amount: u64) -> u64 {
        match self.config.fee_config.fee_type {
            FeeType::Fixed => self.config.fee_config.value as u64,
            FeeType::Percent => (amount as f64 * self.config.fee_config.value / 100.0) as u64,
        }
    }
    pub fn get_staked_balance(&self) -> u64 {
        self.utxos
            .utxos
            .iter()
            .map(|entry| {
                entry
                    .value()
                    .iter()
                    .filter(|(_, utxo)| utxo.is_stake)
                    .map(|(_, utxo)| utxo.value)
                    .sum::<u64>()
            })
            .sum()
    }
    pub fn get_balance(&self) -> u64 {
        self.utxos
            .utxos
            .iter()
            .map(|entry| entry.value().iter().map(|utxo| utxo.1.value).sum::<u64>())
            .sum()
    }
}
