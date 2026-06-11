use std::error::Error;
use std::str::FromStr;

use secp256k1::SecretKey;
use web3::ethabi::{Contract as Abi, Token};
use web3::signing::{Key, SecretKeyRef};
use web3::transports::WebSocket;
use web3::types::{Bytes, H160, H256, TransactionParameters, U256};
use web3::Web3;

/// ABI of contracts/ArbExecutor.sol.
const EXECUTOR_ABI: &str = r#"[
  {"name":"execute","type":"function","stateMutability":"nonpayable",
   "inputs":[{"name":"pairs","type":"address[]"},{"name":"path","type":"address[]"},
             {"name":"feesBps","type":"uint256[]"},{"name":"poolKinds","type":"uint8[]"},
             {"name":"amountIn","type":"uint256"},{"name":"minProfit","type":"uint256"}],
   "outputs":[]}
]"#;

pub type BoxError = Box<dyn Error + Send + Sync>;

pub struct Executor {
    web3: Web3<WebSocket>,
    contract: H160,
    key: SecretKey,
    abi: Abi,
    chain_id: u64,
}

impl Executor {
    pub fn new(
        web3: Web3<WebSocket>,
        contract: H160,
        private_key_hex: &str,
        chain_id: u64,
    ) -> Result<Executor, BoxError> {
        let key = SecretKey::from_str(private_key_hex.trim_start_matches("0x"))
            .map_err(|e| format!("invalid PRIVATE_KEY: {}", e))?;
        let abi = Abi::load(EXECUTOR_ABI.as_bytes())?;
        Ok(Executor { web3, contract, key, abi, chain_id })
    }

    pub fn signer_address(&self) -> H160 {
        SecretKeyRef::new(&self.key).address()
    }

    pub fn contract_address(&self) -> H160 {
        self.contract
    }

    /// Signs and submits ArbExecutor.execute(pairs, path, feesBps, amountIn, minProfit).
    /// The contract reverts unless the cycle nets at least `min_profit`,
    /// so a stale opportunity costs only gas, never inventory.
    #[allow(clippy::too_many_arguments)]
    pub async fn send(
        &self,
        pairs: &[H160],
        path: &[H160],
        fees_bps: &[u32],
        pool_kinds: &[u8],
        amount_in: U256,
        min_profit: U256,
        gas_price: U256,
        gas_limit: u64,
    ) -> Result<H256, BoxError> {
        let data = self.abi.function("execute")?.encode_input(&[
            Token::Array(pairs.iter().map(|p| Token::Address(*p)).collect()),
            Token::Array(path.iter().map(|t| Token::Address(*t)).collect()),
            Token::Array(fees_bps.iter().map(|f| Token::Uint((*f).into())).collect()),
            Token::Array(pool_kinds.iter().map(|k| Token::Uint((*k).into())).collect()),
            Token::Uint(amount_in),
            Token::Uint(min_profit),
        ])?;
        let tx = TransactionParameters {
            to: Some(self.contract),
            gas: gas_limit.into(),
            gas_price: Some(gas_price),
            value: U256::zero(),
            data: Bytes(data),
            chain_id: Some(self.chain_id),
            ..Default::default()
        };
        let signed = self.web3.accounts().sign_transaction(tx, SecretKeyRef::new(&self.key)).await?;
        Ok(self.web3.eth().send_raw_transaction(signed.raw_transaction).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_calldata_encodes() {
        let abi = Abi::load(EXECUTOR_ABI.as_bytes()).unwrap();
        let data = abi
            .function("execute")
            .unwrap()
            .encode_input(&[
                Token::Array(vec![Token::Address(H160::repeat_byte(1)), Token::Address(H160::repeat_byte(2))]),
                Token::Array(vec![
                    Token::Address(H160::repeat_byte(3)),
                    Token::Address(H160::repeat_byte(4)),
                    Token::Address(H160::repeat_byte(3)),
                ]),
                Token::Array(vec![Token::Uint(30u32.into()), Token::Uint(25u32.into())]),
                Token::Array(vec![Token::Uint(0u8.into()), Token::Uint(1u8.into())]),
                Token::Uint(U256::from(3u64)),
                Token::Uint(U256::from(4u64)),
            ])
            .unwrap();
        // selector + 6 head words + 4 dynamic arrays (len + elements).
        assert_eq!(data.len(), 4 + 6 * 32 + (1 + 2) * 32 + (1 + 3) * 32 + (1 + 2) * 32 + (1 + 2) * 32);
    }
}
