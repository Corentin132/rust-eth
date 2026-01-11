use super::{Block, Transaction, TransactionOutput};
use crate::crypto::PublicKey;
use crate::error::{EthError, Result};
use crate::sha256::Hash;
use crate::util::MerkleRoot;
use crate::util::Saveable;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{Error as IoError, ErrorKind as IoErrorKind, Read, Result as IoResult, Write};

impl Saveable for Blockchain {
    fn load<I: Read>(reader: I) -> IoResult<Self> {
        ciborium::de::from_reader(reader)
            .map_err(|_| IoError::new(IoErrorKind::InvalidData, "Failed to deserialize Blockchain"))
    }
    fn save<O: Write>(&self, writer: O) -> IoResult<()> {
        ciborium::ser::into_writer(self, writer)
            .map_err(|_| IoError::new(IoErrorKind::InvalidData, "Failed to serialize Blockchain"))
    }
}

/// Record of a slashing event
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SlashingRecord {
    pub validator: PublicKey,
    pub block_height: u64,
    pub reason: SlashingReason,
    pub penalty_amount: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum SlashingReason {
    DoubleSigning,
    Downtime,
}

#[derive(Serialize, Deserialize, Clone, Debug)]

pub struct Blockchain {
    utxos: HashMap<Hash, (bool, TransactionOutput)>,
    blocks: Vec<Block>,
    #[serde(default, skip_serializing)]
    mempool: Vec<(DateTime<Utc>, Transaction)>,
    #[serde(default, skip_serializing)]
    orphan_children: HashMap<Hash, Vec<Block>>,
    /// Slashing records for accountability
    #[serde(default)]
    slashing_history: Vec<SlashingRecord>,
    /// Slashed validators - reduced stake amounts (pubkey -> slashed amount)
    #[serde(default)]
    slashed_amounts: HashMap<PublicKey, u64>,
}
impl Blockchain {
    pub fn new() -> Self {
        Blockchain {
            blocks: vec![],
            utxos: HashMap::new(),
            mempool: vec![],
            orphan_children: HashMap::new(),
            slashing_history: vec![],
            slashed_amounts: HashMap::new(),
        }
    }
    pub fn add_block(&mut self, block: Block) -> Result<()> {
        if self.blocks.is_empty() {
            if block.header.prev_block_hash != Hash::zero() {
                println!("zero hash");
                self.orphan_children
                    .entry(block.header.prev_block_hash)
                    .or_default()
                    .push(block);
                return Ok(());
            }
        } else {
            let last_block = self.blocks.last().unwrap();
            if block.header.prev_block_hash != last_block.hash() {
                self.orphan_children
                    .entry(block.header.prev_block_hash)
                    .or_default()
                    .push(block);
                return Ok(());
            }
            // check if the block's validator is the expected one
            let expected_validator = self.get_next_validator(&block.header.prev_block_hash);
            if let Some(validator) = expected_validator {
                if block.header.validator != validator {
                    println!("invalid validator");
                    return Err(EthError::InvalidValidator);
                }
            } else {
                println!("no stakes found");
                return Err(EthError::InvalidValidator);
            }
            // check if the block's signature is valid
            if !block
                .signature
                .verify(&block.header.hash(), &block.header.validator)
            {
                println!("invalid signature");
                return Err(EthError::InvalidSignature);
            }
            let calculated_merkle_root = MerkleRoot::calculate(&block.transactions);
            if calculated_merkle_root != block.header.merkle_root {
                println!("invalid merkle root");
                return Err(EthError::InvalidMerkleRoot);
            }
            // check if the block's timestamp is after the
            // last block's timestamp
            if block.header.timestamp <= last_block.header.timestamp {
                return Err(EthError::InvalidBlock);
            }
            // Verify all transactions in the block
            block.verify_transactions(&self.utxos)?;
        }
        let block_transactions: HashSet<_> =
            block.transactions.iter().map(|tx| tx.hash()).collect();
        self.mempool
            .retain(|(_, tx)| !block_transactions.contains(&tx.hash()));
        self.blocks.push(block);

        let new_tip_hash = self.blocks.last().unwrap().hash();
        self.process_orphans(new_tip_hash);

        Ok(())
    }
    pub fn calculate_stakes(&self) -> HashMap<PublicKey, u64> {
        let mut stakes = HashMap::new();
        let current_height = self.block_height();

        for (_, (_, output)) in self.utxos.values().enumerate() {
            if output.is_stake {
                // Only count stakes that are locked (active validators must have locked stake)
                if output.locked_until > current_height {
                    *stakes.entry(output.pubkey.clone()).or_insert(0) += output.value;
                }
            }
        }

        // Subtract slashed amounts from stakes
        for (pubkey, slashed_amount) in &self.slashed_amounts {
            if let Some(stake) = stakes.get_mut(pubkey) {
                *stake = stake.saturating_sub(*slashed_amount);
            }
        }

        stakes.retain(|_, v| *v >= Self::get_min_stake_amount());
        stakes
    }
    pub fn get_min_stake_amount() -> u64 {
        crate::STAKE_MINIMUM_AMOUNT
    }
    pub fn get_next_validator(&self, seed: &Hash) -> Option<PublicKey> {
        let stakes = self.calculate_stakes();
        let total_stake: u64 = stakes.values().sum();

        if total_stake == 0 {
            println!("0 crypto staked 🐒");
            return None;
        }

        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&seed.as_bytes()[0..8]);
        let random_value = u64::from_be_bytes(bytes) % total_stake;

        let mut current_sum = 0;
        // sort stakes by pubkey to ensure deterministic behavior !!!!
        let mut sorted_stakes: Vec<_> = stakes.into_iter().collect();
        sorted_stakes.sort_by(|a, b| a.0.cmp(&b.0));

        for (pubkey, stake) in sorted_stakes {
            current_sum += stake;
            if current_sum > random_value {
                return Some(pubkey);
            }
        }
        None
    }
    pub fn block_height(&self) -> u64 {
        self.blocks.len() as u64
    }
    pub fn rebuild_utxos(&mut self) {
        for block in &self.blocks {
            for transaction in &block.transactions {
                for input in &transaction.inputs {
                    self.utxos.remove(&input.prev_transaction_output_hash);
                }
                for output in transaction.outputs.iter() {
                    self.utxos.insert(output.hash(), (false, output.clone()));
                }
            }
        }
    }

    pub fn process_orphans(&mut self, parent_hash: Hash) {
        let mut stack = vec![parent_hash];
        while let Some(current_parent) = stack.pop() {
            if let Some(children) = self.orphan_children.remove(&current_parent) {
                for child in children {
                    // Try to add each child. add_block may in turn call process_orphans
                    // recursively when it succeeds. If it fails validation, we drop it
                    // (or you can store it elsewhere for debugging).
                    match self.add_block(child) {
                        Ok(()) => {
                            // child appended: push its hash to stack to process grandchildren
                            let tip_hash = self.blocks.last().unwrap().hash();
                            stack.push(tip_hash);
                        }
                        Err(e) => {
                            eprintln!("Failed to attach orphan child: {:?}", e);
                            // If add_block fails (invalid merkle/target/etc.), we simply skip it.
                        }
                    }
                }
            }
        }
    }
    // mempool
    pub fn mempool(&self) -> &[(DateTime<Utc>, Transaction)] {
        // later, we will also need to keep track
        &self.mempool
    }

    // add a transaction to mempool
    pub fn add_to_mempool(&mut self, transaction: Transaction) -> Result<()> {
        // validate transaction before insertion
        // all inputs must match known UTXOs, and must be unique
        let current_height = self.block_height();
        let mut known_inputs = HashSet::new();

        for input in &transaction.inputs {
            if !self.utxos.contains_key(&input.prev_transaction_output_hash) {
                println!("UTXO not found");
                dbg!(&self.utxos);
                return Err(EthError::InvalidTransaction);
            }

            // Check if the UTXO is a locked stake
            if let Some((_, utxo)) = self.utxos.get(&input.prev_transaction_output_hash) {
                if utxo.is_stake && utxo.locked_until > current_height {
                    println!(
                        "Stake is still locked until block {}, current height is {}",
                        utxo.locked_until, current_height
                    );
                    return Err(EthError::StakeLocked);
                }
            }

            if known_inputs.contains(&input.prev_transaction_output_hash) {
                println!("duplicate input");
                return Err(EthError::InvalidTransaction);
            }

            known_inputs.insert(input.prev_transaction_output_hash);
        }

        // check if any of the utxos have the bool mark set to true
        // and if so, find the transaction that references them
        // in mempool, remove it, and set all the utxos it references
        // to false
        for input in &transaction.inputs {
            if let Some((true, _)) = self.utxos.get(&input.prev_transaction_output_hash) {
                // find the transaction that references the UTXO
                // we are trying to reference
                let referencing_transaction =
                    self.mempool
                        .iter()
                        .enumerate()
                        .find(|(_, (_, transaction))| {
                            transaction
                                .outputs
                                .iter()
                                .any(|output| output.hash() == input.prev_transaction_output_hash)
                        });

                // If we have found one, unmark all of its UTXOs
                if let Some((idx, (_, referencing_transaction))) = referencing_transaction {
                    for input in &referencing_transaction.inputs {
                        // set all utxos from this transaction to false
                        self.utxos
                            .entry(input.prev_transaction_output_hash)
                            .and_modify(|(marked, _)| {
                                *marked = false;
                            });
                    }

                    // remove the transaction from the mempool
                    self.mempool.remove(idx);
                } else {
                    // if, somehow, there is no matching transaction,
                    // set this utxo to false
                    self.utxos
                        .entry(input.prev_transaction_output_hash)
                        .and_modify(|(marked, _)| {
                            *marked = false;
                        });
                }
            }
        }

        // all inputs must be lower than all outputs
        let all_inputs = transaction
            .inputs
            .iter()
            .map(|input| {
                self.utxos
                    .get(&input.prev_transaction_output_hash)
                    .expect("BUG: impossible")
                    .1
                    .value
            })
            .sum::<u64>();
        let all_outputs = transaction.outputs.iter().map(|output| output.value).sum();

        if all_inputs < all_outputs {
            print!("inputs are lower than outputs");
            return Err(EthError::InvalidTransaction);
        }

        // Mark the UTXOs as used
        for input in &transaction.inputs {
            self.utxos
                .entry(input.prev_transaction_output_hash)
                .and_modify(|(marked, _)| {
                    *marked = true;
                });
        }

        // push the transaction to the mempool
        self.mempool.push((Utc::now(), transaction));

        // sort by miner fee
        self.mempool.sort_by_key(|(_, transaction)| {
            let all_inputs = transaction
                .inputs
                .iter()
                .map(|input| {
                    self.utxos
                        .get(&input.prev_transaction_output_hash)
                        .expect("BUG: impossible")
                        .1
                        .value
                })
                .sum::<u64>();

            let all_outputs: u64 = transaction.outputs.iter().map(|output| output.value).sum();

            let miner_fee = all_inputs - all_outputs;
            miner_fee
        });

        Ok(())
    }
    pub fn clean_mempool(&mut self) {
        let now = Utc::now();
        let mut utxo_hashes_to_unmark: Vec<Hash> = vec![];

        self.mempool.retain(|(timestamp, transaction)| {
            if now - *timestamp
                > chrono::Duration::seconds(crate::MAX_MEMPOOL_TRANSACTION_AGE as i64)
            {
                utxo_hashes_to_unmark.extend(
                    transaction
                        .inputs
                        .iter()
                        .map(|input| input.prev_transaction_output_hash),
                );
                false
            } else {
                true
            }
        });
        for hash in utxo_hashes_to_unmark {
            self.utxos.entry(hash).and_modify(|(marked, _)| {
                *marked = false;
            });
        }
    }
    pub fn calculate_block_reward(&self) -> u64 {
        let block_height = self.block_height();
        let halvings = block_height / crate::HALVING_INTERVAL;
        (crate::INITIAL_REWARD * 10u64.pow(8)) >> halvings
    }
    //🚨 Better to have getters than public fields --> for futur stockage purposes

    /// Slash a validator for misbehavior (double-signing, downtime, etc.)
    pub fn slash_validator(&mut self, pubkey: &PublicKey, reason: SlashingReason) -> Result<u64> {
        let stakes = self.calculate_stakes();
        let stake = stakes.get(pubkey).cloned().unwrap_or(0);

        if stake == 0 {
            return Err(EthError::InvalidValidator);
        }

        let penalty_rate = match reason {
            SlashingReason::DoubleSigning => crate::SLASHING_PENALTY_DOUBLE_SIGN,
            SlashingReason::Downtime => crate::SLASHING_PENALTY_DOWNTIME,
        };

        // Calculate penalty (basis points: 10000 = 100%)
        let penalty_amount = (stake * penalty_rate) / 10000;

        // Record the slashing
        let record = SlashingRecord {
            validator: pubkey.clone(),
            block_height: self.block_height(),
            reason,
            penalty_amount,
        };
        self.slashing_history.push(record);

        // Add to slashed amounts
        *self.slashed_amounts.entry(pubkey.clone()).or_insert(0) += penalty_amount;

        println!(
            "🔪 Validator {:?} slashed for {} coins",
            pubkey, penalty_amount
        );
        Ok(penalty_amount)
    }

    /// Check if a validator is currently slashed (has any pending slashing)
    pub fn is_validator_slashed(&self, pubkey: &PublicKey) -> bool {
        self.slashed_amounts
            .get(pubkey)
            .map_or(false, |&amt| amt > 0)
    }

    /// Get the effective stake after slashing penalties
    pub fn get_effective_stake(&self, pubkey: &PublicKey) -> u64 {
        let stakes = self.calculate_stakes();
        let stake = stakes.get(pubkey).cloned().unwrap_or(0);
        let slashed = self.slashed_amounts.get(pubkey).cloned().unwrap_or(0);
        stake.saturating_sub(slashed)
    }

    /// Get slashing history
    pub fn slashing_history(&self) -> &[SlashingRecord] {
        &self.slashing_history
    }

    pub fn utxos(&self) -> &HashMap<Hash, (bool, TransactionOutput)> {
        &self.utxos
    }
    // blocks
    pub fn blocks(&self) -> impl Iterator<Item = &Block> {
        self.blocks.iter()
    }
}
