use web3::contract::{Contract, Options};
use web3::transports::WebSocket;
use web3::types::{H160, U256};
use web3::Web3;

use crate::arb::PoolReserves;

const FACTORY_ABI: &[u8] = br#"[
  {"name":"getPair","type":"function","stateMutability":"view",
   "inputs":[{"name":"tokenA","type":"address"},{"name":"tokenB","type":"address"}],
   "outputs":[{"name":"pair","type":"address"}]}
]"#;

const PAIR_ABI: &[u8] = br#"[
  {"name":"getReserves","type":"function","stateMutability":"view","inputs":[],
   "outputs":[{"name":"reserve0","type":"uint112"},{"name":"reserve1","type":"uint112"},
              {"name":"blockTimestampLast","type":"uint32"}]},
  {"name":"token0","type":"function","stateMutability":"view","inputs":[],
   "outputs":[{"name":"","type":"address"}]}
]"#;

const ERC20_ABI: &[u8] = br#"[
  {"name":"balanceOf","type":"function","stateMutability":"view",
   "inputs":[{"name":"owner","type":"address"}],
   "outputs":[{"name":"","type":"uint256"}]}
]"#;

pub struct Pool {
    pub dex: &'static str,
    pub address: H160,
    pub base_is_token0: bool,
    contract: Contract<WebSocket>,
}

impl Pool {
    pub async fn reserves(&self) -> Result<PoolReserves, web3::contract::Error> {
        let (r0, r1, _ts): (U256, U256, U256) =
            self.contract.query("getReserves", (), None, Options::default(), None).await?;
        Ok(if self.base_is_token0 {
            PoolReserves { base: r0, quote: r1 }
        } else {
            PoolReserves { base: r1, quote: r0 }
        })
    }
}

/// Looks up the pair for (base, quote) on the given V2-style factory and
/// orients it around the base token. Returns None if the pair doesn't exist.
pub async fn resolve_pool(
    web3: &Web3<WebSocket>,
    factory: H160,
    base: H160,
    quote: H160,
    dex: &'static str,
) -> Result<Option<Pool>, web3::contract::Error> {
    let factory = Contract::from_json(web3.eth(), factory, FACTORY_ABI)?;
    let pair: H160 = factory.query("getPair", (base, quote), None, Options::default(), None).await?;
    if pair == H160::zero() {
        return Ok(None);
    }
    let contract = Contract::from_json(web3.eth(), pair, PAIR_ABI)?;
    let token0: H160 = contract.query("token0", (), None, Options::default(), None).await?;
    Ok(Some(Pool { dex, address: pair, base_is_token0: token0 == base, contract }))
}

pub async fn erc20_balance(
    web3: &Web3<WebSocket>,
    token: H160,
    owner: H160,
) -> Result<U256, web3::contract::Error> {
    let token = Contract::from_json(web3.eth(), token, ERC20_ABI)?;
    token.query("balanceOf", (owner,), None, Options::default(), None).await
}
