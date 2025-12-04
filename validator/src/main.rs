use anyhow::{Result, anyhow};
use btclib::crypto::{PrivateKey, Signature};
use btclib::network::Message;
use btclib::util::Saveable;
use clap::{Parser, arg, command};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::{Duration, interval};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[arg(short, long)]
    address: String,
    #[arg(short, long)]
    private_key_file: String,
}

struct Validator {
    private_key: PrivateKey,
    stream: Mutex<TcpStream>,
}

impl Validator {
    async fn new(address: String, private_key: PrivateKey) -> Result<Self> {
        let stream = TcpStream::connect(&address).await?;
        Ok(Self {
            private_key,
            stream: Mutex::new(stream),
        })
    }

    // fn verify_validator_eligibility(&self, private_key: &PrivateKey) -> Result<()> {
    //     let public_key = private_key.public_key();
    //     let stake_amount = btclib::types::Blockchain::get_validator_stake_amount(&public_key);
    //     let min_stake = btclib::types::Blockchain::get_min_stake_amount();
    //     if stake_amount < min_stake {
    //         return Err(anyhow!(
    //             "Validator not eligible: stake amount {} is less than minimum required {}",
    //             stake_amount,
    //             min_stake
    //         ));
    //     }
    //     Ok(())
    // }
    async fn run(&self) -> Result<()> {
        let mut template_interval = interval(Duration::from_secs(5));

        loop {
            template_interval.tick().await;
            if let Err(e) = self.fetch_and_validate_block().await {
                eprintln!("Error validating block: {}", e);
            }
        }
    }

    async fn fetch_and_validate_block(&self) -> Result<()> {
        println!("Fetching new template");
        let message = Message::FetchTemplate(self.private_key.public_key());

        let mut stream_lock = self.stream.lock().await;
        message.send_async(&mut *stream_lock).await?;
        drop(stream_lock);

        let mut stream_lock = self.stream.lock().await;
        match Message::receive_async(&mut *stream_lock).await? {
            Message::Template(mut block) => {
                drop(stream_lock);
                println!(
                    "Received new template with merkle root: {:?}",
                    block.header.merkle_root
                );

                // Sign the block
                let signature = Signature::sign_output(&block.header.hash(), &self.private_key);
                block.signature = signature;

                self.submit_block(block).await?;
                Ok(())
            }
            _ => Err(anyhow!(
                "Unexpected message received when fetching template"
            )),
        }
    }

    async fn submit_block(&self, block: btclib::types::Block) -> Result<()> {
        println!("Submitting validated block");
        let message = Message::SubmitTemplate(block);
        let mut stream_lock = self.stream.lock().await;
        message.send_async(&mut *stream_lock).await?;
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let private_key = PrivateKey::load_from_file(&cli.private_key_file)
        .map_err(|e| anyhow!("Error reading private key: {}", e))?;

    let validator = Validator::new(cli.address, private_key).await?;
    validator.run().await
}
