# ETH Rust implementation (Proof of Stake)

## Architecture

- **node** == verify blocks and propagate transactions/blocks to the network
- **validator** == verify blocks && can forge new blocks when selected by the PoS algorithm
- **wallet** == classic wallet with staking support

## Proof of Stake Mechanism

- Validators must stake a minimum amount (`STAKE_MINIMUM_AMOUNT`) to participate
- Validator selection is weighted by stake amount
- Stakes are locked for `STAKE_LOCK_PERIOD` blocks after staking
- Slashing mechanism penalizes malicious validators (double-signing, downtime)