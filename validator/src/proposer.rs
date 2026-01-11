//! Block Proposer - Validator-specific functionality
//!
//! This module contains ONLY the logic specific to validators:
//! - Checking if it's our turn to propose (LOCAL calculation)
//! - Building and signing blocks
//! - Broadcasting proposed blocks
//!
//! All other functionality is inherited from node_lib.

use anyhow::{Result, anyhow};
use chrono::Utc;
use node_lib::{BLOCKCHAIN, NODES};
use poslib::crypto::{PrivateKey, PublicKey, Signature};
use poslib::network::Message;
use poslib::sha256::Hash;
use poslib::types::{Block, BlockHeader, Blockchain, Transaction, TransactionOutput};
use poslib::util::MerkleRoot;
use std::sync::atomic::{AtomicU64, Ordering};
use uuid::Uuid;

pub struct BlockProposer {
    private_key: PrivateKey,
    public_key: PublicKey,
    blocks_proposed: AtomicU64,
}

impl BlockProposer {
    pub fn new(private_key: PrivateKey) -> Self {
        let public_key = private_key.public_key();
        Self {
            private_key,
            public_key,
            blocks_proposed: AtomicU64::new(0),
        }
    }

    /// Check if it's our turn to propose a blocks
    pub fn is_our_turn(&self, blockchain: &Blockchain) -> bool {
        let last_block_hash = blockchain
            .blocks()
            .last()
            .map(|b| b.hash())
            .unwrap_or(Hash::zero());

        match blockchain.get_next_validator(&last_block_hash) {
            Some(expected_validator) => expected_validator == self.public_key,
            None => false,
        }
    }

    /// Propose a new block
    ///
    /// This builds the block locally, signs it, adds it to our chain,
    /// and broadcasts it to peers.
    pub async fn propose_block(&self) -> Result<()> {
        // Build block from our local state
        let block = self.build_block().await?;

        // Add to our own blockchain first (this validates it)
        {
            let mut blockchain = BLOCKCHAIN.write().await;
            blockchain
                .add_block(block.clone())
                .map_err(|e| anyhow!("Our own block was rejected: {:?}", e))?;
            blockchain.rebuild_utxos();
        }

        // Broadcast to all peers
        self.broadcast_block(block).await?;

        let count = self.blocks_proposed.fetch_add(1, Ordering::SeqCst) + 1;
        println!(
            "🎉 Block proposed and broadcast! (Total proposed: {})",
            count
        );

        Ok(())
    }

    /// Build a new block from local state
    ///
    /// The block is built entirely from our local blockchain state.
    /// We don't ask any node for a template - we build it ourselves.
    async fn build_block(&self) -> Result<Block> {
        let blockchain = BLOCKCHAIN.read().await;

        // Double-check we're still the expected validator
        if !self.is_our_turn(&blockchain) {
            return Err(anyhow!("No longer our turn to propose"));
        }

        // Get transactions from mempool
        let mempool_txs: Vec<Transaction> = blockchain
            .mempool()
            .iter()
            .take(poslib::BLOCK_TRANSACTION_CAP)
            .map(|(_, tx)| tx.clone())
            .collect();

        // Calculate fees from transactions
        let mut validator_fees = 0u64;
        let mut valid_transactions = Vec::new();

        for tx in mempool_txs {
            let mut input_sum = 0u64;
            let mut output_sum = 0u64;
            let mut is_valid = true;

            for input in &tx.inputs {
                if let Some((_, output)) =
                    blockchain.utxos().get(&input.prev_transaction_output_hash)
                {
                    input_sum += output.value;
                } else {
                    is_valid = false;
                    break;
                }
            }

            if !is_valid {
                continue;
            }

            for output in &tx.outputs {
                output_sum += output.value;
            }

            if input_sum >= output_sum {
                validator_fees += input_sum - output_sum;
                valid_transactions.push(tx);
            }
        }

        // Create coinbase transaction (our reward)
        let coinbase = Transaction {
            inputs: vec![],
            outputs: vec![TransactionOutput {
                pubkey: self.public_key.clone(),
                unique_id: Uuid::new_v4(),
                value: validator_fees,
                is_stake: false,
                locked_until: 0,
            }],
        };

        // Build transaction list with coinbase first
        let mut transactions = vec![coinbase];
        transactions.extend(valid_transactions);

        // Calculate merkle root
        let merkle_root = MerkleRoot::calculate(&transactions);

        // Build header
        let prev_hash = blockchain
            .blocks()
            .last()
            .map(|b| b.hash())
            .unwrap_or(Hash::zero());

        let header = BlockHeader::new(Utc::now(), prev_hash, merkle_root, self.public_key.clone());

        // Sign the block
        let signature = Signature::sign_output(&header.hash(), &self.private_key);

        let block = Block::new(header, transactions, signature);

        println!("📦 Built block:");
        println!("   - Transactions: {}", block.transactions.len());
        println!("   - Reward: {}", validator_fees);
        println!("   - Prev hash: {}", prev_hash);

        Ok(block)
    }

    /// Broadcast a block to all connected peers
    async fn broadcast_block(&self, block: Block) -> Result<()> {
        let message = Message::NewBlock(block);

        let nodes: Vec<String> = NODES.iter().map(|x| x.key().clone()).collect();

        let mut success_count = 0;

        for node in &nodes {
            if let Some(mut stream) = NODES.get_mut(node) {
                if message.send_async(&mut *stream).await.is_ok() {
                    success_count += 1;
                } else {
                    eprintln!("⚠️  Failed to send block to {}", node);
                }
            }
        }

        println!(
            "📡 Block broadcast to {}/{} peers",
            success_count,
            nodes.len()
        );

        Ok(())
    }
}
