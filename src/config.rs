use std::env;

use web3::types::{H160, U256};

use crate::dex::{FactoryKind, FactorySpec};

pub struct Config {
    pub chain: String,
    /// Chain id the configured addresses belong to; mismatches are warned at startup.
    pub expected_chain_id: u64,
    /// Used to pause one block after submitting a transaction.
    pub block_time_secs: u64,
    pub ws_url: String,
    /// When true (the default), opportunities are only logged, never executed.
    pub dry_run: bool,
    pub private_key: Option<String>,
    pub executor_contract: Option<H160>,
    /// Inventory cap per trade, in base-token wei.
    pub max_trade_wei: U256,
    /// Required profit on top of the estimated gas cost, in wei.
    pub min_profit_wei: U256,
    /// Gas estimate model: base + per_leg * number_of_swaps.
    pub gas_base_units: u64,
    pub gas_per_leg_units: u64,
    /// Maximum swaps per arbitrage cycle (2 = direct cross-DEX, 3 = triangular, ...).
    pub max_legs: usize,
    pub factories: Vec<FactorySpec>,
    pub weth: H160,
    /// (symbol, address) of quote tokens traded against WETH.
    pub quote_tokens: Vec<(&'static str, H160)>,
    /// Additional tokens (e.g. from TOKENS env) whose pools are discovered and
    /// included in cycle search; symbols are resolved on-chain at startup.
    pub extra_tokens: Vec<H160>,
    /// Router addresses whose pending transactions trigger an immediate rescan.
    pub routers: Vec<H160>,
}

/// Per-chain defaults; addresses are individually overridable via env.
struct ChainPreset {
    chain_id: u64,
    block_time_secs: u64,
    factories: &'static [(FactoryKind, &'static str, &'static str, Option<&'static str>)],
    weth: &'static str,
    quote_tokens: &'static [(&'static str, &'static str)],
    routers: &'static [&'static str],
}

const MAINNET: ChainPreset = ChainPreset {
    chain_id: 1,
    block_time_secs: 12,
    factories: &[
        (FactoryKind::UniV2, "uniswap", "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f", Some("UNI_V2_FACTORY")),
        (FactoryKind::UniV2, "sushiswap", "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac", Some("SUSHI_FACTORY")),
    ],
    weth: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
    quote_tokens: &[
        ("USDC", "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
        ("DAI", "0x6B175474E89094C44Da98b954EedeAC495271d0F"),
        ("USDT", "0xdAC17F958D2ee523a2206206994597C13D831ec7"),
    ],
    routers: &[
        // Uniswap V2 Router 02
        "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D",
        // Uniswap Universal Router
        "0x3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD",
        // SushiSwap Router
        "0xd9e1cE17f2641f24aE83637ab66a2cca9C378B9F",
    ],
};

/// Base (OP-stack L2). Note: Base has no public mempool, so mempool triggers
/// never fire there; scanning is driven by the 2s block cadence instead.
const BASE: ChainPreset = ChainPreset {
    chain_id: 8453,
    block_time_secs: 2,
    factories: &[
        (FactoryKind::UniV2, "uniswap", "0x8909Dc15e40173Ff4699343b6eB8132c65e18eC6", Some("UNI_V2_FACTORY")),
        (FactoryKind::UniV2, "sushiswap", "0x71524B4f93c58fcbF659783284E38825f0622859", Some("SUSHI_FACTORY")),
        // Volatile (x*y=k) pools only; per-pool fee read from the factory.
        (FactoryKind::Solidly, "aerodrome", "0x420DD381b31aEf6683db6B902084cB0FFECe40Da", Some("AERODROME_FACTORY")),
    ],
    weth: "0x4200000000000000000000000000000000000006",
    quote_tokens: &[
        // Native (Circle) USDC, not bridged USDbC.
        ("USDC", "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"),
        ("DAI", "0x50c5725949A6F0c72E6C4a641F24049A917DB0Cb"),
        ("USDT", "0xfde4C96c8593536E31F229EA8f37b2ADa2699bb2"),
    ],
    routers: &[
        // Uniswap V2 Router 02
        "0x4752ba5DBc23f44D87826276BF6Fd6b1C372aD24",
        // Uniswap Universal Router (v1.2 and v2.0 deployments)
        "0x3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD",
        "0x6fF5693b99212Da76ad316178A184AB56D299b43",
    ],
};

fn addr(s: &str) -> H160 {
    s.parse().expect("invalid address constant")
}

fn env_addr(key: &str, default: &str) -> H160 {
    match env::var(key) {
        Ok(v) => v.parse().unwrap_or_else(|_| panic!("{} is not a valid address: {}", key, v)),
        Err(_) => addr(default),
    }
}

fn env_u256(key: &str, default: &str) -> U256 {
    match env::var(key) {
        Ok(v) => U256::from_dec_str(&v).unwrap_or_else(|_| panic!("{} is not a valid integer: {}", key, v)),
        Err(_) => U256::from_dec_str(default).unwrap(),
    }
}

fn env_u64(key: &str, default: u64) -> u64 {
    env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

impl Config {
    pub fn from_env() -> Config {
        let chain = env::var("CHAIN").unwrap_or_else(|_| "mainnet".to_string()).to_lowercase();
        let preset = match chain.as_str() {
            "mainnet" | "ethereum" => &MAINNET,
            "base" => &BASE,
            other => panic!("unsupported CHAIN '{}'; expected 'mainnet' or 'base'", other),
        };

        let dry_run = env::var("DRY_RUN").map(|v| v != "false" && v != "0").unwrap_or(true);
        let private_key = env::var("PRIVATE_KEY").ok();
        let executor_contract = env::var("EXECUTOR_CONTRACT").ok().map(|v| {
            v.parse().unwrap_or_else(|_| panic!("EXECUTOR_CONTRACT is not a valid address: {}", v))
        });

        if !dry_run {
            assert!(
                private_key.is_some() && executor_contract.is_some(),
                "DRY_RUN=false requires both PRIVATE_KEY and EXECUTOR_CONTRACT to be set"
            );
        }

        let extra_tokens = env::var("TOKENS")
            .or_else(|_| env::var("TOKEN_ADDRESS"))
            .map(|csv| {
                csv.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.parse().unwrap_or_else(|_| panic!("TOKENS contains invalid address: {}", s)))
                    .collect()
            })
            .unwrap_or_default();

        let factories = preset
            .factories
            .iter()
            .map(|(kind, name, default, env_key)| FactorySpec {
                kind: *kind,
                name,
                address: match env_key {
                    Some(key) => env_addr(key, default),
                    None => addr(default),
                },
            })
            .collect();

        Config {
            expected_chain_id: preset.chain_id,
            block_time_secs: preset.block_time_secs,
            chain,
            ws_url: env::var("ETH_WS_URL").unwrap_or_else(|_| "ws://localhost:3334".to_string()),
            dry_run,
            private_key,
            executor_contract,
            // 0.5 ETH
            max_trade_wei: env_u256("MAX_TRADE_WEI", "500000000000000000"),
            // 0.001 ETH
            min_profit_wei: env_u256("MIN_PROFIT_WEI", "1000000000000000"),
            gas_base_units: env_u64("GAS_BASE_UNITS", 120_000),
            gas_per_leg_units: env_u64("GAS_PER_LEG_UNITS", 120_000),
            max_legs: env_u64("MAX_LEGS", 3).clamp(2, 4) as usize,
            factories,
            weth: env_addr("WETH_ADDRESS", preset.weth),
            quote_tokens: preset.quote_tokens.iter().map(|(sym, a)| (*sym, addr(a))).collect(),
            extra_tokens,
            routers: preset.routers.iter().map(|a| addr(a)).collect(),
        }
    }
}
