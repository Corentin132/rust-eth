//! Block Proposer - Validator-specific functionality
//!
//! This module contains ONLY the logic specific to validators:
//! - Checking if it's our turn to propose (LOCAL calculation based on slots)
//! - Building and signing blocks
//! - Broadcasting proposed blocks
//! - Attesting to blocks proposed by others
//!
//! All other functionality is inherited from node_lib.

use anyhow::{Result, anyhow};
use chrono::Utc;
use node_lib::{BLOCKCHAIN, NODES};
use poslib::crypto::{PrivateKey, PublicKey, Signature};
use poslib::network::Message;
use poslib::sha256::Hash;
use poslib::types::{Attestation, Block, BlockHeader, Blockchain, Transaction, TransactionOutput};
use poslib::util::MerkleRoot;
use std::sync::atomic::{AtomicU64, Ordering};
use uuid::Uuid;

pub struct BlockProposer {
    private_key: PrivateKey,
    public_key: PublicKey,
    blocks_proposed: AtomicU64,
    attestations_made: AtomicU64,
    last_attested_slot: AtomicU64,
}

impl BlockProposer {
    pub fn new(private_key: PrivateKey) -> Self {
        let public_key = private_key.public_key();
        Self {
            private_key,
            public_key,
            blocks_proposed: AtomicU64::new(0),
            attestations_made: AtomicU64::new(0),
            last_attested_slot: AtomicU64::new(0),
        }
    }

    pub fn public_key(&self) -> &PublicKey {
        &self.public_key
    }

    /// Check if it's our turn to propose a block for the current slot
    pub fn is_our_turn(&self, blockchain: &Blockchain) -> bool {
        // Use slot-based validator selection
        blockchain.is_slot_for_proposal(&self.public_key)
    }

    /// Check if it's our turn using the old method (for backward compatibility)
    pub fn is_our_turn_legacy(&self, blockchain: &Blockchain) -> bool {
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

    /// Get the current slot from the blockchain
    pub fn current_slot(&self, blockchain: &Blockchain) -> u64 {
        blockchain.current_slot()
    }

    /// Propose a new block
    ///
    /// This builds the block locally, signs it, adds it to our chain,
    /// and broadcasts it to peers. Also registers for consensus tracking.
    pub async fn propose_block(&self) -> Result<()> {
        // Build block from our local state
        let block = self.build_block().await?;
        let block_hash = block.hash();
        let slot = block.header.slot;

        // Add to our own blockchain first (this validates it)
        {
            let mut blockchain = BLOCKCHAIN.write().await;
            blockchain
                .add_block(block.clone())
                .map_err(|e| anyhow!("Our own block was rejected: {:?}", e))?;
            blockchain.rebuild_utxos();
            
            // Register block for consensus tracking
            let _ = blockchain.propose_block(&block);
        }

        // Broadcast to all peers using ProposeBlock message
        self.broadcast_proposed_block(block.clone()).await?;

        // Self-attest to our own block
        self.attest_to_block(&block_hash, slot).await?;

        let count = self.blocks_proposed.fetch_add(1, Ordering::SeqCst) + 1;
        println!(
            "🎉 Block proposed and broadcast! (Total proposed: {}, slot: {})",
            count, slot
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

        // Get current slot
        let current_slot = blockchain.current_slot();

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

        // Build header with slot
        let prev_hash = blockchain
            .blocks()
            .last()
            .map(|b| b.hash())
            .unwrap_or(Hash::zero());

        // Get attestations from the previous block's consensus (if any)
        let parent_attestations = if let Some(last_block) = blockchain.blocks().last() {
            blockchain.get_attestations(&last_block.hash())
        } else {
            Vec::new()
        };

        let header = BlockHeader::new_with_slot(
            Utc::now(),
            prev_hash,
            merkle_root,
            self.public_key.clone(),
            current_slot,
        );

        // Sign the block
        let signature = Signature::sign_output(&header.hash(), &self.private_key);

        let block = Block::new_with_attestations(header, transactions, signature, parent_attestations);

        println!("📦 Built block:");
        println!("   - Slot: {}", current_slot);
        println!("   - Transactions: {}", block.transactions.len());
        println!("   - Reward: {}", validator_fees);
        println!("   - Prev hash: {}", prev_hash);
        println!("   - Parent attestations: {}", block.parent_attestations.len());

        Ok(block)
    }

    /// Broadcast a proposed block to all connected peers
    async fn broadcast_proposed_block(&self, block: Block) -> Result<()> {
        // Use ProposeBlock for consensus-aware broadcast
        let message = Message::ProposeBlock(block);

        let nodes: Vec<String> = NODES.iter().map(|x| x.key().clone()).collect();

        let mut success_count = 0;

        for node in &nodes {
            if let Some(mut stream) = NODES.get_mut(node) {
                if message.send_async(&mut *stream).await.is_ok() {
                    success_count += 1;
                } else {
                    eprintln!("⚠️  Failed to send proposed block to {}", node);
                }
            }
        }

        println!(
            "📡 Proposed block broadcast to {}/{} peers",
            success_count,
            nodes.len()
        );

        Ok(())
    }

    /// Attest to a block (vote for it)
    /// 
    /// Called when we receive a valid block from another validator,
    /// or after proposing our own block.
    pub async fn attest_to_block(&self, block_hash: &Hash, slot: u64) -> Result<()> {
        // Check if we already attested to this slot
        let last_attested = self.last_attested_slot.load(Ordering::SeqCst);
        if slot <= last_attested {
            println!("⏭️ Already attested to slot {}, skipping", slot);
            return Ok(());
        }

        // Check if we have stake (are we a valid attester?)
        {
            let blockchain = BLOCKCHAIN.read().await;
            let stakes = blockchain.calculate_stakes();
            if !stakes.contains_key(&self.public_key) {
                println!("⚠️ We don't have stake, cannot attest");
                return Ok(());
            }
        }

        // Create the attestation
        let signing_data = Attestation::signing_data(block_hash, slot);
        let signature = Signature::sign_output(&signing_data, &self.private_key);
        
        let attestation = Attestation::new(
            *block_hash,
            slot,
            self.public_key.clone(),
            signature,
        );

        // Add attestation to our own blockchain
        {
            let mut blockchain = BLOCKCHAIN.write().await;
            match blockchain.add_attestation(attestation.clone()) {
                Ok(true) => {
                    println!("✅ Self-attestation added for slot {}", slot);
                }
                Ok(false) => {
                    println!("⚠️ Attestation already exists");
                    return Ok(());
                }
                Err(e) => {
                    println!("❌ Failed to add attestation: {:?}", e);
                    return Err(anyhow!("Failed to add attestation: {:?}", e));
                }
            }
        }

        // Broadcast attestation to peers
        self.broadcast_attestation(attestation).await?;

        // Update last attested slot
        self.last_attested_slot.store(slot, Ordering::SeqCst);
        let count = self.attestations_made.fetch_add(1, Ordering::SeqCst) + 1;
        println!("🗳️ Attestation broadcast (Total attestations: {})", count);

        Ok(())
    }

    /// Attest to a received block if it's valid
    /// 
    /// Called when we receive a ProposeBlock message from another validator
    pub async fn attest_to_received_block(&self, block: &Block) -> Result<()> {
        let block_hash = block.hash();
        let slot = block.header.slot;

        println!("🗳️ Attesting to received block at slot {}", slot);
        self.attest_to_block(&block_hash, slot).await
    }

    /// Broadcast an attestation to all peers
    async fn broadcast_attestation(&self, attestation: Attestation) -> Result<()> {
        let message = Message::AttestBlock(attestation);

        let nodes: Vec<String> = NODES.iter().map(|x| x.key().clone()).collect();

        let mut success_count = 0;

        for node in &nodes {
            if let Some(mut stream) = NODES.get_mut(node) {
                if message.send_async(&mut *stream).await.is_ok() {
                    success_count += 1;
                }
            }
        }

        println!(
            "📡 Attestation broadcast to {}/{} peers",
            success_count,
            nodes.len()
        );

        Ok(())
    }

    /// Get consensus status for reporting
    pub async fn get_consensus_status(&self) -> (u64, u64, u64) {
        let blockchain = BLOCKCHAIN.read().await;
        let current_slot = blockchain.current_slot();
        let finalized_height = blockchain.finalized_height();
        let total_stake = blockchain.total_stake();
        (current_slot, finalized_height, total_stake)
    }
}
