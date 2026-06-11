use web3::contract::tokens::Detokenize;
use web3::contract::{Contract, Options};
use web3::ethabi::Token;
use web3::transports::WebSocket;
use web3::types::{H160, U256};
use web3::Web3;

use crate::arb;

/// How a factory is queried for pools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactoryKind {
    /// Uniswap V2 style: getPair(tokenA, tokenB), fixed 0.3% fee.
    UniV2,
    /// Solidly style (Aerodrome/Velodrome): getPool(tokenA, tokenB, stable)
    /// with the fee read per pool from the factory. Only volatile (x*y=k)
    /// pools are used; stable-curve pools have different math.
    Solidly,
    /// Uniswap V3: getPool(tokenA, tokenB, fee) per fee tier. Pools expose
    /// slot0/liquidity instead of reserves; within the current tick range
    /// they are constant-product over virtual reserves (see arb.rs).
    UniV3,
}

/// V3 fee tiers in 1/1_000_000 units; nonexistent tiers resolve to zero.
const V3_FEE_TIERS: [u32; 4] = [100, 500, 3000, 10_000];

#[derive(Debug, Clone)]
pub struct FactorySpec {
    pub kind: FactoryKind,
    pub name: &'static str,
    pub address: H160,
}

/// On-chain swap mechanics of a pool; the discriminant is what the executor
/// contract receives in its poolKinds array.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolKind {
    /// transfer-in then pair.swap(out0, out1, to, data); fee math client-side.
    V2Style = 0,
    /// pool.swap with exact input and payment via uniswapV3SwapCallback.
    V3 = 1,
}

const FACTORY_V2_ABI: &[u8] = br#"[
  {"name":"getPair","type":"function","stateMutability":"view",
   "inputs":[{"name":"tokenA","type":"address"},{"name":"tokenB","type":"address"}],
   "outputs":[{"name":"pair","type":"address"}]}
]"#;

const FACTORY_SOLIDLY_ABI: &[u8] = br#"[
  {"name":"getPool","type":"function","stateMutability":"view",
   "inputs":[{"name":"tokenA","type":"address"},{"name":"tokenB","type":"address"},{"name":"stable","type":"bool"}],
   "outputs":[{"name":"pool","type":"address"}]},
  {"name":"getFee","type":"function","stateMutability":"view",
   "inputs":[{"name":"pool","type":"address"},{"name":"stable","type":"bool"}],
   "outputs":[{"name":"","type":"uint256"}]}
]"#;

const FACTORY_V3_ABI: &[u8] = br#"[
  {"name":"getPool","type":"function","stateMutability":"view",
   "inputs":[{"name":"tokenA","type":"address"},{"name":"tokenB","type":"address"},{"name":"fee","type":"uint24"}],
   "outputs":[{"name":"pool","type":"address"}]}
]"#;

// Reserves declared as uint256 so the same minimal ABI decodes both Uniswap V2
// pairs (uint112) and Solidly pools (uint256); each is one 32-byte word.
const PAIR_ABI: &[u8] = br#"[
  {"name":"getReserves","type":"function","stateMutability":"view","inputs":[],
   "outputs":[{"name":"reserve0","type":"uint256"},{"name":"reserve1","type":"uint256"},
              {"name":"blockTimestampLast","type":"uint256"}]},
  {"name":"token0","type":"function","stateMutability":"view","inputs":[],
   "outputs":[{"name":"","type":"address"}]}
]"#;

const POOL_V3_ABI: &[u8] = br#"[
  {"name":"slot0","type":"function","stateMutability":"view","inputs":[],
   "outputs":[{"name":"sqrtPriceX96","type":"uint160"},{"name":"tick","type":"int24"},
              {"name":"observationIndex","type":"uint16"},{"name":"observationCardinality","type":"uint16"},
              {"name":"observationCardinalityNext","type":"uint16"},{"name":"feeProtocol","type":"uint8"},
              {"name":"unlocked","type":"bool"}]},
  {"name":"liquidity","type":"function","stateMutability":"view","inputs":[],
   "outputs":[{"name":"","type":"uint128"}]},
  {"name":"token0","type":"function","stateMutability":"view","inputs":[],
   "outputs":[{"name":"","type":"address"}]}
]"#;

const ERC20_ABI: &[u8] = br#"[
  {"name":"balanceOf","type":"function","stateMutability":"view",
   "inputs":[{"name":"owner","type":"address"}],
   "outputs":[{"name":"","type":"uint256"}]},
  {"name":"symbol","type":"function","stateMutability":"view","inputs":[],
   "outputs":[{"name":"","type":"string"}]}
]"#;

/// Detokenizes the first returned word as an unsigned integer, ignoring any
/// further outputs (slot0 returns seven values; only sqrtPriceX96 is needed).
struct FirstUint(U256);

impl Detokenize for FirstUint {
    fn from_tokens(tokens: Vec<Token>) -> Result<Self, web3::contract::Error> {
        match tokens.first() {
            Some(Token::Uint(v)) | Some(Token::Int(v)) => Ok(FirstUint(*v)),
            other => Err(web3::contract::Error::InvalidOutputType(format!(
                "expected uint, got {:?}",
                other
            ))),
        }
    }
}

pub struct Pool {
    /// Display name, e.g. "uniswap", "aerodrome", "uniswap-v3@5bps".
    pub dex: String,
    pub kind: PoolKind,
    pub address: H160,
    pub token0: H160,
    pub token1: H160,
    /// Fee in 1/10000 units (30 = 0.3%).
    pub fee_bps: u32,
    contract: Contract<WebSocket>,
}

impl Pool {
    /// (token0, token1) reserves; for V3, virtual reserves at the current
    /// price, which are exact until the swap crosses an initialized tick
    /// (the executor's on-chain minProfit floor covers that deviation).
    pub async fn reserves(&self) -> Result<(U256, U256), web3::contract::Error> {
        match self.kind {
            PoolKind::V2Style => {
                let (r0, r1, _ts): (U256, U256, U256) =
                    self.contract.query("getReserves", (), None, Options::default(), None).await?;
                Ok((r0, r1))
            }
            PoolKind::V3 => {
                let FirstUint(sqrt_price) =
                    self.contract.query("slot0", (), None, Options::default(), None).await?;
                let FirstUint(liquidity) =
                    self.contract.query("liquidity", (), None, Options::default(), None).await?;
                Ok(arb::v3_virtual_reserves(sqrt_price, liquidity))
            }
        }
    }

    pub fn other_token(&self, token: H160) -> H160 {
        if token == self.token0 {
            self.token1
        } else {
            self.token0
        }
    }
}

/// Looks up all pools for (a, b) on the given factory: zero or one for
/// V2/Solidly factories, one per existing fee tier for V3.
pub async fn resolve_pools(
    web3: &Web3<WebSocket>,
    spec: &FactorySpec,
    a: H160,
    b: H160,
) -> Result<Vec<Pool>, web3::contract::Error> {
    let mut found = Vec::new();
    match spec.kind {
        FactoryKind::UniV2 => {
            let factory = Contract::from_json(web3.eth(), spec.address, FACTORY_V2_ABI)?;
            let pair: H160 =
                factory.query("getPair", (a, b), None, Options::default(), None).await?;
            if pair != H160::zero() {
                found.push(make_pool(web3, spec.name.to_string(), PoolKind::V2Style, pair, a, b, 30).await?);
            }
        }
        FactoryKind::Solidly => {
            let factory = Contract::from_json(web3.eth(), spec.address, FACTORY_SOLIDLY_ABI)?;
            let pool: H160 =
                factory.query("getPool", (a, b, false), None, Options::default(), None).await?;
            if pool != H160::zero() {
                let fee: U256 =
                    factory.query("getFee", (pool, false), None, Options::default(), None).await?;
                found.push(
                    make_pool(web3, spec.name.to_string(), PoolKind::V2Style, pool, a, b, fee.low_u32())
                        .await?,
                );
            }
        }
        FactoryKind::UniV3 => {
            let factory = Contract::from_json(web3.eth(), spec.address, FACTORY_V3_ABI)?;
            for tier in V3_FEE_TIERS {
                let pool: H160 =
                    factory.query("getPool", (a, b, tier), None, Options::default(), None).await?;
                if pool != H160::zero() {
                    let name = format!("{}@{}bps", spec.name, tier / 100);
                    found.push(make_pool(web3, name, PoolKind::V3, pool, a, b, tier / 100).await?);
                }
            }
        }
    }
    Ok(found)
}

async fn make_pool(
    web3: &Web3<WebSocket>,
    dex: String,
    kind: PoolKind,
    address: H160,
    a: H160,
    b: H160,
    fee_bps: u32,
) -> Result<Pool, web3::contract::Error> {
    let abi = match kind {
        PoolKind::V2Style => PAIR_ABI,
        PoolKind::V3 => POOL_V3_ABI,
    };
    let contract = Contract::from_json(web3.eth(), address, abi)?;
    let token0: H160 = contract.query("token0", (), None, Options::default(), None).await?;
    let token1 = if token0 == a { b } else { a };
    Ok(Pool { dex, kind, address, token0, token1, fee_bps, contract })
}

pub async fn erc20_balance(
    web3: &Web3<WebSocket>,
    token: H160,
    owner: H160,
) -> Result<U256, web3::contract::Error> {
    let token = Contract::from_json(web3.eth(), token, ERC20_ABI)?;
    token.query("balanceOf", (owner,), None, Options::default(), None).await
}

/// Best-effort symbol lookup; tokens with non-standard (bytes32) symbols fall
/// back to a shortened address.
pub async fn token_symbol(web3: &Web3<WebSocket>, token: H160) -> String {
    let fallback = format!("{:?}", token)[..10].to_string();
    match Contract::from_json(web3.eth(), token, ERC20_ABI) {
        Ok(c) => {
            let sym: Result<String, _> = c.query("symbol", (), None, Options::default(), None).await;
            sym.unwrap_or(fallback)
        }
        Err(_) => fallback,
    }
}
