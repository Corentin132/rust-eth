//! Shadow Chain - Private blockchain for 51% attack
//!
//! Maintains a secret chain of blocks that are not broadcast to the network.
//! When the attack is triggered, all blocks are released at once.
//!
//! ⚠️ NOTE: With PoS consensus and finality, this attack is blocked once
//! the fork point has been finalized (>2/3 stake attestations).

use anyhow::Result;
use chrono::{DateTime, Utc};
use poslib::crypto::{PrivateKey, PublicKey, Signature};
use poslib::sha256::Hash;
use poslib::types::{Block, BlockHeader, Transaction, TransactionOutput};
use poslib::util::MerkleRoot;
use std::collections::VecDeque;
use uuid::Uuid;

/// A block in the shadow chain with metadata
#[derive(Clone, Debug)]
pub struct ShadowBlock {
    pub block: Block,
    pub mined_at: DateTime<Utc>,
    pub would_be_height: u64,
}

/// The shadow chain
pub struct ShadowChain {
    /// Blocks mined secretly (not broadcast)
    blocks: VecDeque<ShadowBlock>,
    /// The hash of the block we forked from (on the public chain)
    fork_point_hash: Hash,
    /// Height of the fork point on the public chain
    fork_point_height: u64,
    /// Slot of the last block (for building next block with correct slot)
    last_slot: u64,
    /// Total blocks we've mined secretly
    total_mined: u64,
    /// Blocks released so far
    total_released: u64,
}

impl ShadowChain {
    pub fn new() -> Self {
        Self {
            blocks: VecDeque::new(),
            fork_point_hash: Hash::zero(),
            fork_point_height: 0,
            last_slot: 0,
            total_mined: 0,
            total_released: 0,
        }
    }

    /// Initialize/reset the shadow chain from a fork point
    pub fn fork_from(&mut self, hash: Hash, height: u64) {
        self.blocks.clear();
        self.fork_point_hash = hash;
        self.fork_point_height = height;
        self.last_slot = 0; // Will be set when building first block
        self.total_released = 0;
    }

    /// Set the current slot (called from proposer with blockchain.current_slot())
    pub fn set_current_slot(&mut self, slot: u64) {
        self.last_slot = slot;
    }

    /// Get the hash to build the next shadow block on
    pub fn get_tip_hash(&self) -> Hash {
        self.blocks
            .back()
            .map(|sb| sb.block.hash())
            .unwrap_or(self.fork_point_hash)
    }

    /// Get the height of the next shadow block
    pub fn get_next_height(&self) -> u64 {
        self.fork_point_height + self.blocks.len() as u64 + 1
    }

    /// Build a shadow block (same logic as normal proposer but stored secretly)
    /// Now includes slot for consensus compatibility
    pub fn build_shadow_block(
        &mut self,
        private_key: &PrivateKey,
        public_key: &PublicKey,
        transactions: Vec<Transaction>,
        current_slot: u64,
    ) -> Result<Block> {
        // Calculate fees
        let validator_fees: u64 = transactions
            .iter()
            .map(|tx| {
                let outputs: u64 = tx.outputs.iter().map(|o| o.value).sum();
                // In a real implementation, we'd track inputs too
                outputs / 100 // Simplified fee calculation
            })
            .sum();

        // Create coinbase transaction
        let coinbase = Transaction {
            inputs: vec![],
            outputs: vec![TransactionOutput {
                pubkey: public_key.clone(),
                unique_id: Uuid::new_v4(),
                value: validator_fees,
                is_stake: false,
                locked_until: 0,
            }],
        };

        let mut all_transactions = vec![coinbase];
        all_transactions.extend(transactions);

        // Calculate merkle root
        let merkle_root = MerkleRoot::calculate(&all_transactions);

        // Build header with slot
        let prev_hash = self.get_tip_hash();
        let header = BlockHeader::new_with_slot(
            Utc::now(), 
            prev_hash, 
            merkle_root, 
            public_key.clone(),
            current_slot,
        );

        // Sign the block
        let signature = Signature::sign_output(&header.hash(), private_key);
        
        // Update last slot
        self.last_slot = current_slot;

        Ok(Block::new(header, all_transactions, signature))
    }

    /// Add a secretly mined block
    pub fn add_shadow_block(&mut self, block: Block) {
        let shadow_block = ShadowBlock {
            block,
            mined_at: Utc::now(),
            would_be_height: self.get_next_height(),
        };
        self.blocks.push_back(shadow_block);
        self.total_mined += 1;
    }

    /// Get all blocks ready for release (drains the queue)
    pub fn release_all(&mut self) -> Vec<Block> {
        let blocks: Vec<Block> = self.blocks.iter().map(|sb| sb.block.clone()).collect();
        self.total_released += blocks.len() as u64;
        self.blocks.clear();
        blocks
    }

    /// Get advantage over public chain
    pub fn calculate_advantage(&self, public_chain_height: u64) -> i64 {
        let our_height = self.fork_point_height + self.blocks.len() as u64;
        our_height as i64 - public_chain_height as i64
    }

    /// Check if we should release (based on threshold)
    pub fn should_release(&self, public_chain_height: u64, threshold: u64) -> bool {
        self.calculate_advantage(public_chain_height) >= threshold as i64
    }

    // Getters for TUI
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn blocks(&self) -> impl Iterator<Item = &ShadowBlock> {
        self.blocks.iter()
    }

    pub fn fork_point_height(&self) -> u64 {
        self.fork_point_height
    }

    pub fn fork_point_hash(&self) -> Hash {
        self.fork_point_hash
    }

    pub fn total_mined(&self) -> u64 {
        self.total_mined
    }

    pub fn total_released(&self) -> u64 {
        self.total_released
    }
}

impl Default for ShadowChain {
    fn default() -> Self {
        Self::new()
    }
}
