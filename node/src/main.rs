use anyhow::Result;
use argh::FromArgs;
use poslib::types::Blockchain;
use dashmap::DashMap;
use static_init::dynamic;
use std::path::Path;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;

mod handler;
mod util;

#[derive(FromArgs)]
/// A toy blockchain node
struct Args {
    #[argh(option, default = "9000")]
    /// port number
    port: u16,
    #[argh(option, default = "String::from(\"./blockchain.cbor\")")]
    /// blockchain file location
    blockchain_file: String,
    #[argh(positional)]
    /// addresses of initial nodes
    nodes: Vec<String>,
}

#[dynamic]
pub static BLOCKCHAIN: RwLock<Blockchain> = RwLock::new(Blockchain::new());
// Node pool
#[dynamic]
pub static NODES: DashMap<String, TcpStream> = DashMap::new();
#[tokio::main]
async fn main() -> Result<()> {
    // Parse command line arguments
    let args: Args = argh::from_env();
    let port = args.port;
    let blockchain_file = args.blockchain_file;
    let nodes = args.nodes;
    util::populate_connections(&nodes).await?;
    println!("Total amount of known nodes: {}", NODES.len());
    if Path::new(&blockchain_file).exists() {
        println!("Loading blockchain from file: {}", blockchain_file);
        util::load_blockchain(&blockchain_file).await?;
    } else {
        println!("No existing blockchain found 😫, checking with other node .. ");
        if nodes.is_empty() {
            println!("no initial nodes provided, starting as a seed node 🤴");
            let genesis_block = util::create_genesis_block();
            let mut blockchain = BLOCKCHAIN.write().await;
            blockchain
                .add_block(genesis_block)
                .expect("Failed to add genesis block");
        } else {
            let (longest_name, longest_count) = util::find_longest_chain_node().await?;
            // request the blockchain from the node with the lon-gest blockchain
            util::download_blockchain(&longest_name, longest_count).await?;
            println!("blockchain downloaded from {}", longest_name);
            {
                // recalculate utxos
                let mut blockchain = BLOCKCHAIN.write().await;
                blockchain.rebuild_utxos();
            }
        }
    }
    let addr = format!("0.0.0.0:{}", port);
    let listener = TcpListener::bind(&addr).await?;
    println!("Listening on {}", addr);
    // start a task to periodically cleanup the mempool
    // normally, you would want to keep and join the handle
    tokio::spawn(util::cleanup());

    // and a task to periodically save the blockchain
    tokio::spawn(util::save(blockchain_file.clone()));
    loop {
        let (socket, _) = listener.accept().await?;
        tokio::spawn(handler::handle_connection(socket));
    }
}
