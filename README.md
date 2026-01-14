# ETH Rust implementation (Proof of Stake)

This project is a simple implementation of a blockchain using Proof of Stake (PoS) consensus, written in Rust. 

Objective : 
 - Learn blockchain PoS 
 - Learn Rust 🦀 THE CHAD language 👺

## Architecture

*   **Node**: Verifies blocks and propagates transactions/blocks to the network.
*   **Validator**: A full node that participates in consensus. It must hold a "stake" (stake amount) and can propose new blocks when selected by the PoS algorithm.
*   **Wallet**: A client application to manage keys, send transactions, and manage staking.

## Proof of Stake Mechanism

*   Validators must "stake" (lock) a minimum amount (`STAKE_MINIMUM_AMOUNT`) to participate.
*   Validator selection is weighted by the stake amount.
*   Stakes are locked for a period (`STAKE_LOCK_PERIOD`) after staking.
*   A "slashing" mechanism penalizes malicious validators (double signing, downtime).

---

## Usage

Ensure you have Rust and Cargo installed. Compile the project with:

```bash
cargo build --release
```

### 1. Node (Standard Node)

The standard node connects to the P2P network, downloads the blockchain, and relays information.

**Command:**
```bash
cargo run --bin node -- [OPTIONS]
```

**Options:**
*   `--port <PORT>`: Listening port (default: 9000).
*   `--blockchain-file <FILE>`: Blockchain save file (default: `./blockchain.cbor`).
*   `--nodes <LIST>`: Comma-separated list of peer addresses to join the network.

**Example:**
```bash
cargo run --bin node -- --port 9000 --nodes "127.0.0.1:9001"
```

### 2. Validator

The validator requires a private key to sign proposed blocks.

**Command:**
```bash
cargo run --bin validator -- [OPTIONS]
```

**Options:**
*   `--port <PORT>`: Listening port (default: 9001).
*   `--private-key-file <FILE>`: Path to the private key file (Required).
*   `--blockchain-file <FILE>`: Blockchain save file (default: `validator_blockchain.cbor`).
*   `--nodes <LIST>`: List of peer addresses.

**Example (Start as the first validator "Boot node"):**
```bash
cargo run --bin validator -- --private-key-file ./validator/alice.priv.cbor --port 9001
```

**Example (Join as a validator):**
```bash
cargo run --bin validator -- --private-key-file ./validator/bob.priv.cbor --port 9999 --nodes "127.0.0.1:9001"
```

### 3. Wallet

The wallet is an interactive command-line interface to manage your funds.

**Command:**
```bash
cargo run --bin wallet -- [OPTIONS]
```

**Options:**
*   `--config <FILE>`: Configuration file (default: `wallet_config.toml`).
*   `--node <ADDRESS>`: Node address to connect to (overrides config value).
*   `generate-config`: (Subcommand) Generates a default configuration file.

**Example:**
```bash
# Use a specific config file
cargo run --bin wallet -- --config ./wallet/bob_wallet_config.toml --node 127.0.0.1:9001
```

**Configuration (`wallet_config.toml`):**
The configuration file defines your keys, contacts, and default node.

```toml
default_node = "127.0.0.1:9001"

[fee_config]
fee_type = "Percent"
value = 0.1

[[contacts]]
name = "Bob"
key = "../validator/bob.pub.pem"

[[my_keys]]
public = "../validator/alice.pub.pem"
private = "../validator/alice.priv.cbor"
```

