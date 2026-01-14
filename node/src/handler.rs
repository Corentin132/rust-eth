use chrono::Utc;
use poslib::crypto::{PublicKey, Signature};
use poslib::network::Message;
use poslib::sha256::Hash;
use poslib::types::{Block, BlockHeader, Transaction, TransactionOutput};
use poslib::util::MerkleRoot;
use tokio::net::TcpStream;
use uuid::Uuid;
pub async fn handle_connection(mut socket: TcpStream) {
    loop {
        // read a message from the socket
        let message = match Message::receive_async(&mut socket).await {
            Ok(message) => message,
            Err(e) => {
                // Check if it's just a clean disconnect (EOF)
                let err_str = e.to_string();
                if err_str.contains("eof") || err_str.contains("UnexpectedEof") {
                    // Normal disconnect - peer closed the connection
                    return;
                }
                println!("invalid message from peer: {e}, closing that connection");
                return;
            }
        };

        use poslib::network::Message::*;
        match message {
            UTXOs(_) | Template(_) | Difference(_) | TemplateValidity(_) | NodeList(_)
            | BlockHeight(_) | NextValidator(_) => {
                println!("I am neither a validator nor a wallet! Goodbye peer 💅");
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

            DiscoverNodes(sender_port) => {
                // Get the peer's IP address from the socket
                let peer_addr = match socket.peer_addr() {
                    Ok(addr) => addr,
                    Err(_) => {
                        println!("❌ Could not get peer address");
                        continue;
                    }
                };
                let peer_ip = peer_addr.ip();
                let peer_connect_addr = format!("{}:{}", peer_ip, sender_port);

                // Add the peer to our node list if not already present
                if !crate::NODES.contains_key(&peer_connect_addr) {
                    println!(
                        "🤝 New peer discovered: {}, connecting back...",
                        peer_connect_addr
                    );
                    match tokio::net::TcpStream::connect(&peer_connect_addr).await {
                        Ok(new_stream) => {
                            crate::NODES.insert(peer_connect_addr.clone(), new_stream);
                            println!("✅ Connected back to peer: {}", peer_connect_addr);
                        }
                        Err(e) => {
                            println!("❌ Failed to connect back to {}: {}", peer_connect_addr, e);
                        }
                    }
                }

                let nodes = crate::NODES
                    .iter()
                    .map(|x| x.key().clone())
                    .collect::<Vec<_>>();
                let message = NodeList(nodes);
                println!("👐 sending node list to peer");
                message.send_async(&mut socket).await.unwrap();
            }
            AskDifference(height) => {
                let blockchain = crate::BLOCKCHAIN.read().await;
                let count = blockchain.block_height() as i32 - height as i32;
                let message = Difference(count);
                message.send_async(&mut socket).await.unwrap();
            }
            FetchBlockHeight => {
                let blockchain = crate::BLOCKCHAIN.read().await;
                let height = blockchain.block_height();
                let message = BlockHeight(height);
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
                println!("█ Received new block");
                if blockchain.add_block(block).is_err() {
                    println!("New block rejected");
                } else {
                    // Rebuild UTXOs after accepting a new block
                    blockchain.rebuild_utxos();
                    println!("Block accepted, UTXOs rebuilt");
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
            // 🚨🚨🚨🚨🚨 Verification du block ou ça ????
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
                        if message.send_async(&mut *stream).await.is_err() {
                            println!("failed to send block to {}", node);
                        }
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

            SlashValidator {
                validator,
                reason,
                evidence: _,
            } => {
                use poslib::types::SlashingReason;
                let mut blockchain = crate::BLOCKCHAIN.write().await;

                let slashing_reason = if reason.contains("double") {
                    SlashingReason::DoubleSigning
                } else {
                    SlashingReason::Downtime
                };

                match blockchain.slash_validator(&validator, slashing_reason) {
                    Ok(penalty) => {
                        println!("🔪 Validator slashed! Penalty: {} coins", penalty);
                    }
                    Err(e) => {
                        println!("Failed to slash validator: {}", e);
                    }
                }
            }
        }
    }
}
