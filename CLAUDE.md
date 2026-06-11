# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

A Rust DEX arbitrage bot (Cargo package `eth-mempool-listener-rs`) for constant-product (x*y=k) pools, including Uniswap V3 treated as constant-product over its in-range virtual reserves. At startup it discovers every pool among a candidate token set (WETH, the chain's stablecoins, plus any `TOKENS` extras) across the chain's configured factories — Uniswap V2 / SushiSwap / Uniswap V3 (all fee tiers) everywhere, Aerodrome volatile pools on Base — then enumerates arbitrage **cycles** (WETH → … → WETH, up to `MAX_LEGS` swaps). Every scan finds the profit-optimal input per cycle and, when profit exceeds gas cost plus a configurable floor, executes the whole cycle atomically through a small on-chain executor contract.

## Architecture

The flow: `build_universe` in `main.rs` resolves pools and DFS-enumerates cycles once per connection; the scan loop re-fetches all pool reserves (one `getReserves` per pool per scan, shared by all cycles) on every new block and whenever a pending transaction targeting a known DEX router appears in the mempool (debounced to one scan per 200ms).

- `src/arb.rs` — pure math, the core of the system. Each swap leg is a fractional-linear map `E*x/(F+G*x)`; that family is closed under composition, so an N-leg cycle reduces to one closed-form optimum (`optimal_cycle`). Fees are per-leg in 1/10000 units (UniV2 = 30, Aerodrome read from its factory, V3 tier/100). `v3_virtual_reserves` maps a V3 pool's `slot0`/`liquidity` to virtual reserves (`L*2^96/sqrtP`, `L*sqrtP/2^96`, overflow-safe split at 2^96) — exact within the current tick range; tick-crossing deviation is absorbed by the executor's on-chain `minProfit` floor. The optimum is located in f64, then the profit is **verified with exact integer math** before acting. All unit tests live here (plus one in `executor.rs`).
- `src/dex.rs` — factory abstraction (`FactoryKind::UniV2` = `getPair`, `FactoryKind::Solidly` = `getPool(a,b,false)` + `getFee` (stable-curve pools deliberately excluded since their math differs), `FactoryKind::UniV3` = `getPool(a,b,fee)` per tier [100, 500, 3000, 10000]) and minimal-ABI pool bindings. `PoolKind` (V2Style = 0, V3 = 1) is the discriminant the executor contract receives. Pool reserves are decoded as uint256 words so V2 pairs (uint112) and Solidly pools (uint256) share one ABI; V3 `slot0` is decoded via a first-word-only `Detokenize` impl.
- `src/executor.rs` — signs and submits `ArbExecutor.execute(pairs, path, feesBps, poolKinds, amountIn, minProfit)` transactions (legacy gas-price txs).
- `src/mempool.rs` — pending-transaction watcher; fires a rescan trigger via an mpsc channel when a transaction targets a router. Resubscribes itself with exponential backoff so the channel never closes.
- `src/config.rs` — all configuration from env vars; per-chain presets (`ChainPreset`) for mainnet and Base.
- `contracts/ArbExecutor.sol` — owner-only contract holding WETH inventory; walks the swap path atomically (V2-style transfer-then-swap, or V3 exact-input swap paying via `uniswapV3SwapCallback` gated on an `expectedV3Pool` guard) and reverts unless profit ≥ `minProfit`, so stale opportunities cost only gas. Deployment instructions are in the file header. There is no Solidity build tooling in this repo; deploy with Foundry/Remix.

Failure handling convention: `main()` wraps `run()` in an infinite reconnect loop; any subscription/transport error tears down and reconnects after 5s. Per-pool reserve fetch errors skip only the cycles touching that pool, never fatal.

## Configuration

Everything is env-var driven (see `src/config.rs` for the full list and defaults):

- `CHAIN` (default `mainnet`, also accepts `base`) — selects the preset of factories/tokens/routers, expected chain id (warned on mismatch at startup), and block time. On Base there is no public mempool, so mempool triggers never fire; the 2s block cadence drives scanning instead.
- `TOKENS` (alias `TOKEN_ADDRESS`) — comma-separated extra token addresses. Their pools against WETH and the stablecoin set are discovered on all factories and folded into cycle search; symbols are resolved on-chain for logging. Pair discovery is candidate-based (`getPair`/`getPool` against the known token set), not an exhaustive factory enumeration.
- `MAX_LEGS` (default 3, clamped 2–4) — cycle length cap: 2 = direct cross-DEX, 3 = triangular, 4 also covers cross-DEX arbs between two non-WETH pools.
- `ETH_WS_URL` (default `ws://localhost:3334`) — needs a node (for the selected chain) with pubsub; mempool triggers additionally need pending-transaction visibility.
- `DRY_RUN` (default `true`) — opportunities are only logged. Setting `DRY_RUN=false` requires `PRIVATE_KEY` and `EXECUTOR_CONTRACT` (a deployed, WETH-funded `ArbExecutor` owned by that key).
- `MAX_TRADE_WEI`, `MIN_PROFIT_WEI` — sizing and profitability thresholds. Profit must exceed `gas_price * (GAS_BASE_UNITS + GAS_PER_LEG_UNITS * legs) + MIN_PROFIT_WEI`; the submitted tx gas limit is twice that estimate.
- `UNI_V2_FACTORY`, `SUSHI_FACTORY`, `UNI_V3_FACTORY`, `AERODROME_FACTORY` (Base only), `WETH_ADDRESS` — preset defaults per chain, overridable for forks/testnets.
- On OP-stack chains (Base) the gas threshold excludes the L1 data fee; it is currently small enough to be absorbed by `MIN_PROFIT_WEI`, but keep that in mind before lowering the floor.

## Commands

- Build: `cargo build`
- Test: `cargo test` (pure-math tests; no node needed). Single test: `cargo test finds_near_optimal_triangular_input`
- Run: `cargo run` (defaults to dry-run; logs at info level by default, `RUST_LOG=debug` for per-cycle and per-transaction detail)
- Lint: `cargo clippy`
- Format: `cargo fmt`

There is no CI configuration in this repository.

## Conventions

- Keep `optimal_cycle`/`swap_out` pure and side-effect free — they are the only tested surface; anything touching the chain stays in `dex.rs`/`executor.rs`/`mempool.rs`.
- Amounts are wei-denominated `U256` end to end; f64 is allowed only for locating the optimum and for log formatting, never for profit decisions.
- All cycles start and end in WETH so profit is always gas-comparable and the executor only ever risks WETH inventory (a honeypot/fee-on-transfer token makes the contract revert, not lose funds).
- On-chain addresses hardcoded as defaults must be verified against Etherscan/BaseScan before changing (the mainnet SushiSwap factory is `0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac`, easily confused with similar-looking addresses).
- The Rust-side `EXECUTOR_ABI` in `executor.rs` must stay in sync with `contracts/ArbExecutor.sol`.
