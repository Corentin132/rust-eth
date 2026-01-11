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
            println!("Allocating genesis stake to {}", path);
            // Staked coins - MUST have locked_until > current_height to be counted!
            outputs.push(TransactionOutput {
                unique_id: Uuid::new_v4(),
                value: poslib::TOTAL_SUPPLY_CAP / validator_count,
                pubkey: pubkey.clone(),
                is_stake: true,
                locked_until: 1_000_000, // Locked for a very long time (active stake)
            });
            println!(
                "  - Allocated {} staked coins (locked until block 1,000,000)",
                poslib::TOTAL_SUPPLY_CAP / validator_count
            );
            // Spendable coins - NOT staked, can be used immediately
            outputs.push(TransactionOutput {
                unique_id: Uuid::new_v4(),
                value: 100_000_000_000,
                pubkey: pubkey.clone(),
                is_stake: false, // Regular spendable coins
                locked_until: 0, // Not locked
            });
            println!("  - Allocated {} spendable coins", 100_000_000_000u64);
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

pub async fn populate_connections(nodes: &[String]) -> Result<()> {
    println!("trying to connect to other nodes...");
    for node in nodes {
        println!("connecting to {}", node);
        let mut stream = TcpStream::connect(&node).await?;
        let message = Message::DiscoverNodes;
        message.send_async(&mut stream).await?;
        println!("sent DiscoverNodes to {}", node);
        let message = Message::receive_async(&mut stream).await?;
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
