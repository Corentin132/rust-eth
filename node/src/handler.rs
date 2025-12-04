use btclib::crypto::Signature;
use btclib::network::Message;
use btclib::sha256::Hash;
use btclib::types::{Block, BlockHeader, Transaction, TransactionOutput};
use btclib::util::MerkleRoot;
use chrono::Utc;
use tokio::net::TcpStream;
use uuid::Uuid;
pub async fn handle_connection(mut socket: TcpStream) {
    loop {
        // read a message from the socket
        let message = match Message::receive_async(&mut socket).await {
            Ok(message) => message,
            Err(e) => {
                println!("invalid message from peer: {e}, closing that connection");
                return;
            }
        };

        use btclib::network::Message::*;
        match message {
            UTXOs(_) | Template(_) | Difference(_) | TemplateValidity(_) | NodeList(_) => {
                println!("I am neither a miner nor a wallet! Goodbye peer 💅");
                return;
            }
            FetchBlock(height) => {
                let blockchain = crate::BLOCKCHAIN.read().await;
                let Some(block) = blockchain.blocks().nth(height as usize).cloned() else {
                    return;
                };
                let message = NewBlock(block);
                message.send_async(&mut socket).await.unwrap();
            }

            DiscoverNodes => {
                let nodes = crate::NODES
                    .iter()
                    .map(|x| x.key().clone())
                    .collect::<Vec<_>>();
                let message = NodeList(nodes);
                message.send_async(&mut socket).await.unwrap();
            }
            AskDifference(height) => {
                let blockchain = crate::BLOCKCHAIN.read().await;
                let count = blockchain.block_height() as i32 - height as i32;
                let message = Difference(count);
                message.send_async(&mut socket).await.unwrap();
            }
            FetchUTXOs(key) => {
                println!("received request to fetch UTXOs");
                let blockchain = crate::BLOCKCHAIN.read().await;
                let utxos = blockchain
                    .utxos()
                    .iter()
                    .filter(|(_, (_, txout))| txout.pubkey == key)
                    .map(|(_, (marked, txout))| (txout.clone(), *marked))
                    .collect::<Vec<_>>();
                let message = UTXOs(utxos);
                message.send_async(&mut socket).await.unwrap();
            }
            NewBlock(block) => {
                let mut blockchain = crate::BLOCKCHAIN.write().await;
                println!("received new block");
                if blockchain.add_block(block).is_err() {
                    println!("New block rejected");
                }
            }
            NewTransaction(tx) => {
                let mut blockchain = crate::BLOCKCHAIN.write().await;
                println!("received transaction from friend");
                if blockchain.add_to_mempool(tx).is_err() {
                    println!("transaction rejected, closing connection");
                    return;
                }
            }
            ValidateTemplate(block_template) => {
                let blockchain = crate::BLOCKCHAIN.read().await;
                let status = block_template.header.prev_block_hash
                    == blockchain
                        .blocks()
                        .last()
                        .map(|last_block| last_block.hash())
                        .unwrap_or(Hash::zero());
                let message = TemplateValidity(status);
                message.send_async(&mut socket).await.unwrap();
            }
            SubmitTemplate(block) => {
                println!("received allegedly validated block");
                let mut blockchain = crate::BLOCKCHAIN.write().await;
                if let Err(e) = blockchain.add_block(block.clone()) {
                    println!("block rejected: {e}, closing connection");
                    continue;
                }
                blockchain.rebuild_utxos();
                println!("block looks good, broadcasting");
                // send block to all friend nodes
                let nodes = crate::NODES
                    .iter()
                    .map(|x| x.key().clone())
                    .collect::<Vec<_>>();
                for node in nodes {
                    if let Some(mut stream) = crate::NODES.get_mut(&node) {
                        let message = Message::NewBlock(block.clone());
                        if message.send_async(&mut *stream).await.is_err() {}
                        println!("failed to send block to {}", node);
                    }
                }
            }
            SubmitTransaction(tx) => {
                println!("submit tx");
                let mut blockchain = crate::BLOCKCHAIN.write().await;
                if let Err(e) = blockchain.add_to_mempool(tx.clone()) {
                    println!("transaction rejected, closing connection: {e}");
                    return;
                }
                println!("added transaction to mempool");
                // send transaction to all friend nodes
                let nodes = crate::NODES
                    .iter()
                    .map(|x| x.key().clone())
                    .collect::<Vec<_>>();
                for node in nodes {
                    println!("sending to friend: {node}");
                    if let Some(mut stream) = crate::NODES.get_mut(&node) {
                        let message = Message::NewTransaction(tx.clone());
                        if message.send_async(&mut *stream).await.is_err() {
                            println!("failed to send transaction to {}", node);
                        }
                    }
                }
                println!("transaction sent to friends");
            }
            FetchTemplate(pubkey) => {
                let blockchain = crate::BLOCKCHAIN.read().await;

                let last_block_hash = blockchain
                    .blocks()
                    .last()
                    .map(|last_block| last_block.hash())
                    .unwrap_or(Hash::zero());

                // Check if the requester is the allowed validator
                if let Some(expected_validator) = blockchain.get_next_validator(&last_block_hash) {
                    if expected_validator != pubkey {
                        println!(
                            "Requester {:?} is not the expected validator {:?}",
                            pubkey, expected_validator
                        );
                        return;
                    }
                }

                // 1. Build candidate transactions list (without coinbase)
                let transactions = blockchain
                    .mempool()
                    .iter()
                    .take(btclib::BLOCK_TRANSACTION_CAP)
                    .map(|(_, tx)| tx)
                    .cloned()
                    .collect::<Vec<_>>();

                // 2. Calculate fees from these transactions
                let mut miner_fees = 0;
                let mut valid_transactions = Vec::new();

                for tx in transactions {
                    let mut input_sum = 0;
                    let mut output_sum = 0;
                    let mut is_valid = true;

                    for input in &tx.inputs {
                        if let Some((_, output)) =
                            blockchain.utxos().get(&input.prev_transaction_output_hash)
                        {
                            input_sum += output.value;
                        } else {
                            eprintln!(
                                "Error: UTXO not found for transaction input. Skipping transaction."
                            );
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

                    if input_sum < output_sum {
                        eprintln!("Error: Transaction inputs < outputs. Skipping transaction.");
                        continue;
                    }

                    miner_fees += input_sum - output_sum;
                    valid_transactions.push(tx);
                }

                let mut transactions = valid_transactions;

                let reward = blockchain.calculate_block_reward();

                // 3. Create coinbase with reward + fees
                let coinbase = Transaction {
                    inputs: vec![],
                    outputs: vec![TransactionOutput {
                        pubkey: pubkey.clone(),
                        unique_id: Uuid::new_v4(),
                        value: reward + miner_fees,
                        is_stake: false,
                    }],
                };

                // 4. Prefix coinbase
                transactions.insert(0, coinbase);

                // 5. Calculate merkle root once
                let merkle_root = MerkleRoot::calculate(&transactions);

                // 6. Construct block
                let header = BlockHeader::new(
                    Utc::now(),
                    blockchain
                        .blocks()
                        .last()
                        .map(|last_block| last_block.hash())
                        .unwrap_or(Hash::zero()),
                    merkle_root,
                    pubkey,
                );
                // Create a dummy signature for the template, the validator will replace it
                let dummy_signature =
                    Signature::sign_output(&header.hash(), &btclib::crypto::PrivateKey::new_key());
                let block = Block::new(header, transactions, dummy_signature);

                let message = Template(block);
                if let Err(e) = message.send_async(&mut socket).await {
                    println!("Failed to send template to validator: {}", e);
                    return;
                }
            }
        }
    }
}
