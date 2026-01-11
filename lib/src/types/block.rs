use super::{Transaction, TransactionOutput};
use crate::crypto::{PublicKey, Signature};
use crate::error::{EthError, Result};
use crate::sha256::Hash;
use crate::util::MerkleRoot;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::util::Saveable;
use std::io::{Error as IoError, ErrorKind as IoErrorKind, Read, Result as IoResult, Write};
// save and load expecting CBOR from ciborium as format
impl Saveable for Block {
    fn load<I: Read>(reader: I) -> IoResult<Self> {
        ciborium::de::from_reader(reader)
            .map_err(|_| IoError::new(IoErrorKind::InvalidData, "Failed to deserialize Block"))
    }
    fn save<O: Write>(&self, writer: O) -> IoResult<()> {
        ciborium::ser::into_writer(self, writer)
            .map_err(|_| IoError::new(IoErrorKind::InvalidData, "Failed to serialize Block"))
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<Transaction>,
    pub signature: Signature,
}

impl Block {
    pub fn new(header: BlockHeader, transactions: Vec<Transaction>, signature: Signature) -> Self {
        Block {
            header,
            transactions,
            signature,
        }
    }
    pub fn hash(&self) -> Hash {
        Hash::hash(self)
    }
    pub fn verify_transactions(
        &self,
        utxos: &HashMap<Hash, (bool, TransactionOutput)>,
    ) -> Result<()> {
        let mut inputs: HashMap<Hash, TransactionOutput> = HashMap::new();
        if self.transactions.is_empty() {
            return Err(EthError::InvalidBlock);
        }
        self.verify_coinbase_transaction(utxos)?;
        for transaction in self.transactions.iter().skip(1) {
            let mut input_value = 0;
            let mut output_value = 0;
            for input in &transaction.inputs {
                let prev_output = utxos
                    .get(&input.prev_transaction_output_hash)
                    .map(|(_, output)| output);
                if prev_output.is_none() {
                    return Err(EthError::InvalidTransaction);
                }
                let prev_output = prev_output.unwrap();
                // 🚨 prevent same-block double-spending
                if inputs.contains_key(&input.prev_transaction_output_hash) {
                    return Err(EthError::InvalidTransaction);
                }
                if !input
                    .signature
                    .verify(&input.prev_transaction_output_hash, &prev_output.pubkey)
                {
                    return Err(EthError::InvalidSignature);
                }
                input_value += prev_output.value;
                inputs.insert(input.prev_transaction_output_hash, prev_output.clone());
            }
            for output in &transaction.outputs {
                output_value += output.value;
            }
            if input_value < output_value {
                return Err(EthError::InvalidTransaction);
            }
        }
        Ok(())
    }
    pub fn calculate_miner_fees(
        &self,
        utxos: &HashMap<Hash, (bool, TransactionOutput)>,
    ) -> Result<u64> {
        let mut inputs: HashMap<Hash, TransactionOutput> = HashMap::new();
        let mut outputs: HashMap<Hash, TransactionOutput> = HashMap::new();
        for transaction in self.transactions.iter().skip(1) {
            for input in &transaction.inputs {
                let prev_output = utxos
                    .get(&input.prev_transaction_output_hash)
                    .map(|(_, output)| output);
                if prev_output.is_none() {
                    return Err(EthError::InvalidTransaction);
                }
                let prev_output = prev_output.unwrap();
                if inputs.contains_key(&input.prev_transaction_output_hash) {
                    return Err(EthError::InvalidTransaction);
                }
                inputs.insert(input.prev_transaction_output_hash, prev_output.clone());
            }
            for output in &transaction.outputs {
                if outputs.contains_key(&output.hash()) {
                    return Err(EthError::InvalidTransaction);
                }
                outputs.insert(output.hash(), output.clone());
            }
        }
        let input_value: u64 = inputs.values().map(|output| output.value).sum();
        let output_value: u64 = outputs.values().map(|output| output.value).sum();
        // Ex : send 100  -> received  90 = 10 fees 🐢
        Ok(input_value - output_value)
    }
    pub fn verify_coinbase_transaction(
        &self,
        utxos: &HashMap<Hash, (bool, TransactionOutput)>,
    ) -> Result<()> {
        // coinbase tx is the first transaction in the block
        let coinbase_transaction = &self.transactions[0];
        if coinbase_transaction.inputs.len() != 0 {
            return Err(EthError::InvalidTransaction);
        }
        if coinbase_transaction.outputs.len() == 0 {
            return Err(EthError::InvalidTransaction);
        }
        let miner_fees = self.calculate_miner_fees(utxos)?;
        let total_coinbase_outputs: u64 = coinbase_transaction
            .outputs
            .iter()
            .map(|output| output.value)
            .sum();
        if total_coinbase_outputs != miner_fees {
            return Err(EthError::InvalidTransaction);
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BlockHeader {
    pub timestamp: DateTime<Utc>,
    pub prev_block_hash: Hash,
    pub merkle_root: MerkleRoot,
    pub validator: PublicKey,
}
impl BlockHeader {
    pub fn new(
        timestamp: DateTime<Utc>,
        prev_block_hash: Hash,
        merkle_root: MerkleRoot,
        validator: PublicKey,
    ) -> Self {
        BlockHeader {
            timestamp,
            prev_block_hash,
            merkle_root,
            validator,
        }
    }
    pub fn hash(&self) -> Hash {
        Hash::hash(self)
    }
}
