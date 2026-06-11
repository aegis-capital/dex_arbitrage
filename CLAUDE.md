# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

Despite the repository name (`dex_arbitrage`), the Cargo package is named `eth-mempool-listener-rs`. It is a single-binary Rust program (`src/main.rs`) that subscribes to pending Ethereum transactions and prints those addressed to two hardcoded DEX router contracts:

- `0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D` — Uniswap V2 Router 02
- `0x3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD` — Uniswap Universal Router

The flow in `main.rs`: connect to an Ethereum node over WebSocket → `eth_subscribe` to new pending transaction hashes → fetch each full transaction via `eth().transaction()` → print hash/to/from/value when the `to` address matches a target. Matches go to stdout via `println!`; everything else (missing transactions, RPC errors) goes through the `log` crate (`warn!`/`error!`).

## Runtime requirements

- The program connects to a hardcoded WebSocket endpoint `ws://localhost:3334` (see `src/main.rs`). An Ethereum node (or a proxy/tunnel to one) must be listening there with pubsub support, or the program fails immediately on startup.
- Logging uses `env_logger`, so set `RUST_LOG` to see `warn!`/`error!` output, e.g. `RUST_LOG=info cargo run`.

## Commands

- Build: `cargo build`
- Run: `RUST_LOG=info cargo run`
- Lint: `cargo clippy`
- Format: `cargo fmt`

There are no tests and no CI configuration in this repository.

## Dependencies

Async runtime is `tokio` (full features); Ethereum access is the `web3` crate (0.17) with its `WebSocket` transport and `TryStreamExt` for consuming the subscription stream. Rust edition 2021.
