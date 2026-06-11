# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

A Rust DEX arbitrage bot (Cargo package `eth-mempool-listener-rs`) for Uniswap-V2-style pools. It monitors WETH pairs (USDC, DAI, USDT by default) on Uniswap V2 and SushiSwap, computes the profit-optimal trade size for two-pool arbitrage, and — when profit exceeds gas cost plus a configurable floor — executes the round trip atomically through a small on-chain executor contract.

## Architecture

The flow: `main.rs` resolves pair contracts from both factories at startup, then rescans reserves on every new block and whenever a pending transaction targeting a known DEX router appears in the mempool (debounced to one scan per 200ms). Each scan evaluates both directions (buy on Uniswap/sell on Sushi and vice versa) per market.

- `src/arb.rs` — pure math, the core of the system. Closed-form optimal input for two constant-product pools with 0.3% fees: the optimum is located in f64, then the profit is **verified with exact integer math** before acting. All unit tests live here (plus one in `executor.rs`). Reserves are always oriented as `PoolReserves { base, quote }` where base is WETH.
- `src/dex.rs` — minimal-ABI contract bindings: factory `getPair`, pair `getReserves`/`token0`, ERC20 `balanceOf`.
- `src/executor.rs` — signs and submits `ArbExecutor.execute(...)` transactions (legacy gas-price txs via `web3.accounts().sign_transaction`).
- `src/mempool.rs` — pending-transaction watcher; fires a rescan trigger via an mpsc channel when a transaction targets a router. Resubscribes itself on errors so the channel never closes.
- `src/config.rs` — all configuration from env vars with mainnet defaults.
- `contracts/ArbExecutor.sol` — owner-only contract holding WETH inventory; does both swap legs atomically and reverts unless profit ≥ `minProfit`, so stale opportunities cost only gas. Deployment instructions are in the file header. There is no Solidity build tooling in this repo; deploy with Foundry/Remix.

Failure handling convention: `main()` wraps `run()` in an infinite reconnect loop; any subscription/transport error tears down and reconnects after 5s. Per-pair scan errors are logged and skipped, never fatal.

## Configuration

Everything is env-var driven (see `src/config.rs` for the full list and defaults):

- `CHAIN` (default `mainnet`, also accepts `base`) — selects a per-chain preset (`ChainPreset` in `config.rs`) of factory/token/router addresses, expected chain id, and block time. The node's chain id is checked at startup and mismatches are warned. On Base there is no public mempool, so mempool triggers never fire; the 2s block cadence drives scanning instead, and the watcher backs off exponentially rather than spamming warnings.
- `ETH_WS_URL` (default `ws://localhost:3334`) — needs a node (for the selected chain) with pubsub; mempool triggers additionally need pending-transaction visibility.
- `DRY_RUN` (default `true`) — opportunities are only logged. Setting `DRY_RUN=false` requires `PRIVATE_KEY` and `EXECUTOR_CONTRACT` (a deployed, WETH-funded `ArbExecutor` owned by that key).
- `MAX_TRADE_WEI`, `MIN_PROFIT_WEI`, `GAS_LIMIT` — sizing and profitability thresholds. Profit must exceed `gas_price * GAS_LIMIT + MIN_PROFIT_WEI`.
- `UNI_V2_FACTORY`, `SUSHI_FACTORY`, `WETH_ADDRESS` — preset defaults per chain, overridable for forks/testnets.
- On OP-stack chains (Base) the gas threshold `gas_price * GAS_LIMIT` excludes the L1 data fee; it is currently small enough to be absorbed by `MIN_PROFIT_WEI`, but keep that in mind before lowering the floor.

## Commands

- Build: `cargo build`
- Test: `cargo test` (pure-math tests; no node needed). Single test: `cargo test finds_near_optimal_input`
- Run: `cargo run` (defaults to dry-run; logs at info level by default, `RUST_LOG=debug` for per-transaction mempool detail)
- Lint: `cargo clippy`
- Format: `cargo fmt`

There is no CI configuration in this repository.

## Conventions

- Keep `optimal_arb`/`swap_out` pure and side-effect free — they are the only tested surface; anything touching the chain stays in `dex.rs`/`executor.rs`/`mempool.rs`.
- Amounts are wei-denominated `U256` end to end; f64 is allowed only for locating the optimum and for log formatting, never for profit decisions.
- On-chain addresses hardcoded as defaults must be verified against Etherscan before changing (the SushiSwap factory is `0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac`, easily confused with similar-looking addresses).
- The Rust-side `EXECUTOR_ABI` in `executor.rs` must stay in sync with `contracts/ArbExecutor.sol`.
