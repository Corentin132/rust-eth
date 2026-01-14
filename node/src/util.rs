use anyhow::{Context, Result};
use chrono::Utc;
use poslib::crypto::{PrivateKey, PublicKey, Signature};
use poslib::network::Message;
use poslib::sha256::Hash;
use poslib::types::{Block, BlockHeader, Blockchain, Transaction, TransactionOutput};
use poslib::util::{MerkleRoot, Saveable};
use tokio::net::TcpStream;
use tokio::time;
use uuid::Uuid;

pub fn create_genesis_block() -> Block {
    let mut outputs = Vec::new();

    // Try to load pre-defined validators
    let validators = vec!["validator/alice.pub.pem", "validator/bob.pub.pem"];
    let validator_count = validators.len() as u64;
    for path in validators {
        if let Ok(pubkey) = PublicKey::load_from_file(path) {
            outputs.push(TransactionOutput {
                unique_id: Uuid::new_v4(),
                value: poslib::TOTAL_SUPPLY_CAP / validator_count,
                pubkey: pubkey.clone(),
                is_stake: false, // Regular spendable coins
                locked_until: 0,
            });
            println!(
                "  - Allocated {} spendable coins",
                poslib::TOTAL_SUPPLY_CAP / validator_count
            );

            println!("Allocating genesis stake to {}", path);

            outputs.push(TransactionOutput {
                unique_id: Uuid::new_v4(),
                value: poslib::STAKE_MINIMUM_AMOUNT,
                pubkey: pubkey.clone(),
                is_stake: true,
                locked_until: 100, // Locked for  the first 100 blocks
            });
            println!(
                "  - Allocated {} staked coins (locked until block 100)",
                poslib::STAKE_MINIMUM_AMOUNT
            );
        }
    }

    let transactions = vec![Transaction::new(vec![], outputs)];

    let merkle_root = MerkleRoot::calculate(&transactions);
    let header = BlockHeader::new(
        Utc::now(),
        Hash::zero(),
        merkle_root,
        PublicKey::load_from_file("validator/alice.pub.pem")
            .expect("Failed to load genesis validator public key"),
    );

    let signature = Signature::sign_output(
        &header.hash(),
        &PrivateKey::load_from_file("validator/alice.priv.cbor")
            .expect("Failed to load genesis validator private key"),
    );
    Block::new(header, transactions, signature)
}

pub async fn load_blockchain(blockchain_file: &str) -> Result<()> {
    println!("blockchain file exists, loading...");
    let new_blockchain = Blockchain::load_from_file(blockchain_file)?;
    println!("blockchain loaded");
    let mut blockchain = crate::BLOCKCHAIN.write().await;
    *blockchain = new_blockchain;
    println!("rebuilding utxos...");
    blockchain.rebuild_utxos();
    println!("utxos rebuilt");
    println!("initialization complete");
    Ok(())
}

pub async fn populate_connections(nodes: Vec<String>, port: u16) -> Result<()> {
    println!("trying to connect to other nodes...");
    'node_loop: for node in nodes {
        println!("connecting to {}", node);
        // Skip connecting to ourselves
        if node.contains(&format!("127.0.0.1:{}", port))
            || node.contains(&format!("localhost:{}", port))
        {
            println!("  - skipping self (127.0.0.1:{})", port);
            continue 'node_loop;
        }
        // Try to connect with retry
        let mut retries = 5;
        let stream = loop {
            match TcpStream::connect(&node).await {
                Ok(s) => break s,
                Err(e) => {
                    retries -= 1;
                    if retries == 0 {
                        println!("  - failed to connect to {} after 3 attempts: {}", node, e);
                        continue 'node_loop;
                    }
                    println!(
                        "  - connection failed, retrying... ({} attempts left)",
                        retries
                    );
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
            }
        };

        let mut stream = stream;
        let message = Message::DiscoverNodes(port);
        if let Err(e) = message.send_async(&mut stream).await {
            println!("  - failed to send DiscoverNodes to {}: {}", node, e);
            continue;
        }
        println!("sent DiscoverNodes to {}", node);

        let message = match Message::receive_async(&mut stream).await {
            Ok(m) => m,
            Err(e) => {
                println!("  - failed to receive response from {}: {}", node, e);
                continue;
            }
        };
        match message {
            Message::NodeList(child_nodes) => {
                println!("received NodeList from {}", node);
                for child_node in child_nodes {
                    println!("adding node {}", child_node);

                    let new_stream = TcpStream::connect(&child_node).await?;
                    crate::NODES.insert(child_node, new_stream);
                }
            }
            _ => {
                println!("unexpected message from {}", node);
            }
        }
        crate::NODES.insert(node.clone(), stream);
    }
    Ok(())
}

pub async fn find_longest_chain_node() -> Result<(String, u32)> {
    println!("finding nodes with the highest blockchainlength...");
    let mut longest_name = String::new();
    let mut longest_count = 0;
    let all_nodes = crate::NODES
        .iter()
        .map(|x| x.key().clone())
        .collect::<Vec<_>>();
    for node in all_nodes {
        println!("asking {} for blockchain length", node);
        let mut stream = crate::NODES.get_mut(&node).context("no node")?;
        let message = Message::AskDifference(0);
        message.send_async(&mut *stream).await.unwrap();
        println!("sent AskDifference to {}", node);
        let message = Message::receive_async(&mut *stream).await?;
        match message {
            Message::Difference(count) => {
                println!("received Difference from {}", node);
                if count > longest_count {
                    println!(
                        "new longest blockchain: \
{} blocks from {node}",
                        count
                    );
                    longest_count = count;
                    longest_name = node;
                }
            }
            e => {
                println!("unexpected message from {}: {:?}", node, e);
            }
        }
    }
    Ok((longest_name, longest_count as u32))
}

// TODO :: immplement a better to download the blockchains (with one message to feetch the whole blockchain ) rnd (using multiple connections and parallel downloads)
pub async fn download_blockchain(node: &str, count: u32) -> Result<()> {
    let mut stream = crate::NODES.get_mut(node).unwrap();
    for i in 0..count as usize {
        let message = Message::FetchBlock(i);
        message.send_async(&mut *stream).await?;
        let message = Message::receive_async(&mut *stream).await?;
        match message {
            Message::NewBlock(block) => {
                let mut blockchain = crate::BLOCKCHAIN.write().await;
                blockchain.add_block(block)?;
            }
            _ => {
                println!("unexpected message from {}", node);
            }
        }
    }
    Ok(())
}

pub async fn cleanup() {
    let mut interval = time::interval(time::Duration::from_secs(30));
    loop {
        interval.tick().await;
        println!("cleaning the mempool from old transactions");
        let mut blockchain = crate::BLOCKCHAIN.write().await;
        blockchain.clean_mempool();
    }
}
pub async fn save(name: String) {
    let mut interval = time::interval(time::Duration::from_secs(15));
    loop {
        interval.tick().await;
        println!("saving blockchain to drive...");
        let blockchain = crate::BLOCKCHAIN.read().await;
        blockchain.save_to_file(name.clone()).unwrap();
    }
}
