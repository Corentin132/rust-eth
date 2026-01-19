//! Attestation and Consensus types for Proof of Stake
//!
//! Implements a slot-based consensus with validator attestations.
//! A block is finalized when >2/3 of the total stake has attested to it.

use crate::crypto::{PublicKey, Signature};
use crate::sha256::Hash;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Duration of a slot in seconds
pub const SLOT_DURATION_SECS: u64 = 12;

/// Number of slots per epoch (for finality)
pub const SLOTS_PER_EPOCH: u64 = 32;

/// Minimum attestations required before a block can be considered (as fraction of total)
/// We require > 2/3 of total stake (supermajority)
pub const SUPERMAJORITY_THRESHOLD: u64 = 2; // numerator
pub const SUPERMAJORITY_DIVISOR: u64 = 3; // denominator

/// An attestation is a validator's vote for a specific block
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Attestation {
    /// The hash of the block being attested to
    pub block_hash: Hash,
    /// The slot number this attestation is for
    pub slot: u64,
    /// The validator's public key
    pub validator: PublicKey,
    /// Signature of (block_hash || slot) by the validator
    pub signature: Signature,
}

impl Attestation {
    /// Create a new attestation
    pub fn new(block_hash: Hash, slot: u64, validator: PublicKey, signature: Signature) -> Self {
        Self {
            block_hash,
            slot,
            validator,
            signature,
        }
    }

    /// Get the data that should be signed for this attestation
    pub fn signing_data(block_hash: &Hash, slot: u64) -> Hash {
        let mut data = block_hash.as_bytes().to_vec();
        data.extend_from_slice(&slot.to_be_bytes());
        Hash::hash_bytes(&data)
    }

    /// Verify the attestation signature
    pub fn verify(&self) -> bool {
        let signing_hash = Self::signing_data(&self.block_hash, self.slot);
        self.signature.verify(&signing_hash, &self.validator)
    }
}

/// Block status in the consensus process
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum BlockStatus {
    /// Block has been proposed but not yet attested
    Proposed,
    /// Block has received attestations but not yet supermajority
    Attesting,
    /// Block has received supermajority (>2/3 stake) - justified
    Justified,
    /// Block is finalized (justified block built upon by another justified block)
    Finalized,
}

/// Pending block awaiting consensus
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PendingBlock {
    /// The block hash
    pub block_hash: Hash,
    /// The slot this block was proposed for
    pub slot: u64,
    /// Current status
    pub status: BlockStatus,
    /// Collected attestations
    pub attestations: Vec<Attestation>,
    /// Total stake that has attested (cached for efficiency)
    pub attested_stake: u64,
}

impl PendingBlock {
    pub fn new(block_hash: Hash, slot: u64) -> Self {
        Self {
            block_hash,
            slot,
            status: BlockStatus::Proposed,
            attestations: Vec::new(),
            attested_stake: 0,
        }
    }

    /// Add an attestation and return true if it's new
    pub fn add_attestation(&mut self, attestation: Attestation, validator_stake: u64) -> bool {
        // Check if this validator already attested
        if self
            .attestations
            .iter()
            .any(|a| a.validator == attestation.validator)
        {
            return false;
        }

        self.attestations.push(attestation);
        self.attested_stake += validator_stake;
        true
    }

    /// Check if we have supermajority
    pub fn has_supermajority(&self, total_stake: u64) -> bool {
        if total_stake == 0 {
            return false;
        }
        // Check if attested_stake * DIVISOR > total_stake * THRESHOLD
        // This is equivalent to attested_stake / total_stake > THRESHOLD / DIVISOR
        // i.e., attested_stake / total_stake > 2/3
        self.attested_stake * SUPERMAJORITY_DIVISOR > total_stake * SUPERMAJORITY_THRESHOLD
    }
}

/// Consensus state tracking
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ConsensusState {
    /// Current slot number (based on genesis time)
    pub current_slot: u64,

    /// Genesis timestamp (Unix timestamp in seconds)
    pub genesis_time: u64,

    /// Pending blocks awaiting attestations: block_hash -> PendingBlock
    #[serde(default)]
    pub pending_blocks: HashMap<Hash, PendingBlock>,

    /// Justified block hashes by slot
    #[serde(default)]
    pub justified_blocks: HashMap<u64, Hash>,

    /// Finalized block height (all blocks up to this height are final)
    pub finalized_height: u64,

    /// Last finalized block hash
    pub last_finalized_hash: Option<Hash>,

    /// Attestations seen (to detect double voting)
    /// Maps (validator, slot) -> block_hash they attested to
    #[serde(default)]
    pub seen_attestations: HashMap<(PublicKey, u64), Hash>,
}

impl ConsensusState {
    pub fn new(genesis_time: u64) -> Self {
        Self {
            current_slot: 0,
            genesis_time,
            pending_blocks: HashMap::new(),
            justified_blocks: HashMap::new(),
            finalized_height: 0,
            last_finalized_hash: None,
            seen_attestations: HashMap::new(),
        }
    }

    /// Calculate the current slot based on time
    pub fn calculate_current_slot(&self, current_time: u64) -> u64 {
        if current_time < self.genesis_time {
            return 0;
        }
        (current_time - self.genesis_time) / SLOT_DURATION_SECS
    }

    /// Update current slot based on timestamp
    pub fn update_slot(&mut self, current_time: u64) {
        self.current_slot = self.calculate_current_slot(current_time);
    }

    /// Get the slot for a given timestamp
    pub fn slot_for_time(&self, timestamp: u64) -> u64 {
        if timestamp < self.genesis_time {
            return 0;
        }
        (timestamp - self.genesis_time) / SLOT_DURATION_SECS
    }

    /// Get the start time for a given slot
    pub fn time_for_slot(&self, slot: u64) -> u64 {
        self.genesis_time + (slot * SLOT_DURATION_SECS)
    }

    /// Get the current epoch
    pub fn current_epoch(&self) -> u64 {
        self.current_slot / SLOTS_PER_EPOCH
    }

    /// Check if a validator double-voted (attested to different blocks at same slot)
    pub fn check_double_vote(&self, validator: &PublicKey, slot: u64, block_hash: &Hash) -> bool {
        if let Some(existing_hash) = self.seen_attestations.get(&(validator.clone(), slot)) {
            return existing_hash != block_hash;
        }
        false
    }

    /// Record an attestation for double-vote detection
    pub fn record_attestation(&mut self, validator: PublicKey, slot: u64, block_hash: Hash) {
        self.seen_attestations.insert((validator, slot), block_hash);
    }

    /// Add a pending block for consensus
    pub fn add_pending_block(&mut self, block_hash: Hash, slot: u64) {
        if !self.pending_blocks.contains_key(&block_hash) {
            self.pending_blocks
                .insert(block_hash, PendingBlock::new(block_hash, slot));
        }
    }

    /// Try to justify a block (called when attestations are added)
    pub fn try_justify(&mut self, block_hash: &Hash, total_stake: u64) -> bool {
        if let Some(pending) = self.pending_blocks.get_mut(block_hash) {
            if pending.status == BlockStatus::Proposed || pending.status == BlockStatus::Attesting {
                if pending.has_supermajority(total_stake) {
                    pending.status = BlockStatus::Justified;
                    self.justified_blocks.insert(pending.slot, *block_hash);
                    println!(
                        "✅ Block at slot {} JUSTIFIED with {}/{} stake",
                        pending.slot, pending.attested_stake, total_stake
                    );
                    return true;
                } else {
                    pending.status = BlockStatus::Attesting;
                }
            }
        }
        false
    }

    /// Check and update finalization
    /// A block is finalized if it's justified and the next justified block builds on it
    pub fn try_finalize(&mut self, current_height: u64) -> Option<u64> {
        // Find consecutive justified blocks
        let mut justified_slots: Vec<u64> = self.justified_blocks.keys().cloned().collect();
        justified_slots.sort();

        // If we have at least 2 justified blocks, we can finalize the earlier ones
        if justified_slots.len() >= 2 {
            // Finalize all but the last justified block
            let finalize_up_to = justified_slots[justified_slots.len() - 2];

            if finalize_up_to > self.finalized_height {
                let old_finalized = self.finalized_height;
                self.finalized_height = finalize_up_to;

                // Update status of finalized blocks
                for slot in justified_slots.iter() {
                    if *slot <= finalize_up_to {
                        if let Some(hash) = self.justified_blocks.get(slot) {
                            if let Some(pending) = self.pending_blocks.get_mut(hash) {
                                pending.status = BlockStatus::Finalized;
                            }
                            self.last_finalized_hash = Some(*hash);
                        }
                    }
                }

                println!(
                    "🔒 FINALIZED: blocks up to slot {} (was {})",
                    finalize_up_to, old_finalized
                );
                return Some(finalize_up_to);
            }
        }
        None
    }

    /// Clean up old pending blocks that are now finalized
    pub fn cleanup_old_pending(&mut self) {
        self.pending_blocks
            .retain(|_, pending| pending.slot > self.finalized_height);

        // Also clean up old attestation records (keep last 2 epochs)
        let min_slot = self.current_slot.saturating_sub(SLOTS_PER_EPOCH * 2);
        self.seen_attestations
            .retain(|(_, slot), _| *slot >= min_slot);
    }

    /// Get the status of a block
    pub fn get_block_status(&self, block_hash: &Hash) -> Option<&BlockStatus> {
        self.pending_blocks.get(block_hash).map(|p| &p.status)
    }

    /// Check if a block is at least justified
    pub fn is_justified(&self, block_hash: &Hash) -> bool {
        self.pending_blocks.get(block_hash).map_or(false, |p| {
            p.status == BlockStatus::Justified || p.status == BlockStatus::Finalized
        })
    }

    /// Check if a block is finalized
    pub fn is_finalized(&self, block_hash: &Hash) -> bool {
        self.pending_blocks
            .get(block_hash)
            .map_or(false, |p| p.status == BlockStatus::Finalized)
    }
}

/// Evidence of double voting (for slashing)
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DoubleVoteEvidence {
    pub attestation1: Attestation,
    pub attestation2: Attestation,
}

impl DoubleVoteEvidence {
    /// Verify this is valid double-vote evidence
    pub fn verify(&self) -> bool {
        // Same validator, same slot, different blocks
        self.attestation1.validator == self.attestation2.validator
            && self.attestation1.slot == self.attestation2.slot
            && self.attestation1.block_hash != self.attestation2.block_hash
            && self.attestation1.verify()
            && self.attestation2.verify()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slot_calculation() {
        let genesis = 1000;
        let state = ConsensusState::new(genesis);

        assert_eq!(state.calculate_current_slot(1000), 0);
        assert_eq!(state.calculate_current_slot(1011), 0);
        assert_eq!(state.calculate_current_slot(1012), 1);
        assert_eq!(state.calculate_current_slot(1024), 2);
    }

    #[test]
    fn test_supermajority() {
        let pending = PendingBlock {
            block_hash: Hash::zero(),
            slot: 0,
            status: BlockStatus::Proposed,
            attestations: vec![],
            attested_stake: 67,
        };

        // 67/100 > 2/3 ≈ 66.67%
        assert!(pending.has_supermajority(100));

        let pending2 = PendingBlock {
            attested_stake: 66,
            ..pending.clone()
        };
        // 66/100 = 66% which is NOT > 2/3
        assert!(!pending2.has_supermajority(100));
    }
}
