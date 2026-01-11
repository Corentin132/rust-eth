use serde::{Deserialize, Serialize};
use uint::construct_uint;
construct_uint! {
// consisting of 4 x 64-bit words
#[derive(Serialize, Deserialize)]
pub struct U256(4);
}
pub mod crypto;
pub mod error;
pub mod network;
pub mod sha256;
pub mod types;
pub mod util;

// initial reward in bitcoin - multiply by 10^8 to get satoshis
pub const INITIAL_REWARD: u64 = 50;
// halving interval in blocks
pub const HALVING_INTERVAL: u64 = 210;
pub const STAKE_MINIMUM_AMOUNT: u64 = 1000 * 10u64.pow(8); // 1000 coins in satoshis
// maximum age of a transaction in the mempool in seconds -> btc 72h
pub const MAX_MEMPOOL_TRANSACTION_AGE: u64 = 600;

// maximum number of transactions in a block
pub const BLOCK_TRANSACTION_CAP: usize = 20;
pub const TOTAL_SUPPLY_CAP: u64 = 21_000_000 * 10u64.pow(8); // 21 million coins in satoshis
// ===== Proof of Stake Constants =====
// Stake lock period in blocks (unbonding period)
pub const STAKE_LOCK_PERIOD: u64 = 100; // ~100 blocks before stake can be withdrawn
// Slashing percentage for double-signing (in basis points, 1000 = 10%)
pub const SLASHING_PENALTY_DOUBLE_SIGN: u64 = 1000;
// Slashing percentage for being offline/missing blocks (in basis points)
pub const SLASHING_PENALTY_DOWNTIME: u64 = 100; // 1%
