use poslib::crypto::PrivateKey;
use poslib::types::{Transaction, TransactionOutput};
use poslib::util::Saveable;
use std::env;
use std::process::exit;
use uuid::Uuid;
fn main() {
    let path = if let Some(arg) = env::args().nth(1) {
        arg
    } else {
        eprintln!("Usage: tx_gen <output_transaction_file_path>");
        exit(1);
    };
    let private_key = PrivateKey::new_key();
    let transaction = Transaction::new(
        vec![],
        vec![TransactionOutput {
            unique_id: Uuid::new_v4(),
            value: poslib::INITIAL_REWARD * 10u64.pow(8),
            pubkey: private_key.public_key(),
            is_stake: false,
            locked_until: 0,
        }],
    );
    transaction
        .save_to_file(path)
        .expect("Failed to save transaction");
}
