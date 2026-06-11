use web3::contract::{Contract, Options};
use web3::transports::WebSocket;
use web3::types::{H160, U256};
use web3::Web3;

/// How a factory is queried for pools. Both kinds produce pools with the
/// UniswapV2 pair interface (getReserves/token0/swap).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactoryKind {
    /// Uniswap V2 style: getPair(tokenA, tokenB), fixed 0.3% fee.
    UniV2,
    /// Solidly style (Aerodrome/Velodrome): getPool(tokenA, tokenB, stable)
    /// with the fee read per pool from the factory. Only volatile (x*y=k)
    /// pools are used; stable-curve pools have different math.
    Solidly,
}

#[derive(Debug, Clone)]
pub struct FactorySpec {
    pub kind: FactoryKind,
    pub name: &'static str,
    pub address: H160,
}

const FACTORY_ABI: &[u8] = br#"[
  {"name":"getPair","type":"function","stateMutability":"view",
   "inputs":[{"name":"tokenA","type":"address"},{"name":"tokenB","type":"address"}],
   "outputs":[{"name":"pair","type":"address"}]},
  {"name":"getPool","type":"function","stateMutability":"view",
   "inputs":[{"name":"tokenA","type":"address"},{"name":"tokenB","type":"address"},{"name":"stable","type":"bool"}],
   "outputs":[{"name":"pool","type":"address"}]},
  {"name":"getFee","type":"function","stateMutability":"view",
   "inputs":[{"name":"pool","type":"address"},{"name":"stable","type":"bool"}],
   "outputs":[{"name":"","type":"uint256"}]}
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

const ERC20_ABI: &[u8] = br#"[
  {"name":"balanceOf","type":"function","stateMutability":"view",
   "inputs":[{"name":"owner","type":"address"}],
   "outputs":[{"name":"","type":"uint256"}]},
  {"name":"symbol","type":"function","stateMutability":"view","inputs":[],
   "outputs":[{"name":"","type":"string"}]}
]"#;

pub struct Pool {
    pub dex: &'static str,
    pub address: H160,
    pub token0: H160,
    pub token1: H160,
    /// Fee in 1/10000 units (30 = 0.3%).
    pub fee_bps: u32,
    contract: Contract<WebSocket>,
}

impl Pool {
    pub async fn reserves(&self) -> Result<(U256, U256), web3::contract::Error> {
        let (r0, r1, _ts): (U256, U256, U256) =
            self.contract.query("getReserves", (), None, Options::default(), None).await?;
        Ok((r0, r1))
    }

    pub fn other_token(&self, token: H160) -> H160 {
        if token == self.token0 {
            self.token1
        } else {
            self.token0
        }
    }
}

/// Looks up the pool for (a, b) on the given factory. Returns None if the
/// pool doesn't exist there.
pub async fn resolve_pool(
    web3: &Web3<WebSocket>,
    spec: &FactorySpec,
    a: H160,
    b: H160,
) -> Result<Option<Pool>, web3::contract::Error> {
    let factory = Contract::from_json(web3.eth(), spec.address, FACTORY_ABI)?;
    let (address, fee_bps): (H160, u32) = match spec.kind {
        FactoryKind::UniV2 => {
            let pair: H160 = factory.query("getPair", (a, b), None, Options::default(), None).await?;
            (pair, 30)
        }
        FactoryKind::Solidly => {
            let pool: H160 =
                factory.query("getPool", (a, b, false), None, Options::default(), None).await?;
            if pool == H160::zero() {
                return Ok(None);
            }
            let fee: U256 =
                factory.query("getFee", (pool, false), None, Options::default(), None).await?;
            (pool, fee.low_u32())
        }
    };
    if address == H160::zero() {
        return Ok(None);
    }
    let contract = Contract::from_json(web3.eth(), address, PAIR_ABI)?;
    let token0: H160 = contract.query("token0", (), None, Options::default(), None).await?;
    let token1 = if token0 == a { b } else { a };
    Ok(Some(Pool { dex: spec.name, address, token0, token1, fee_bps, contract }))
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
