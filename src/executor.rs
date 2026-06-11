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
   "inputs":[{"name":"pairBuy","type":"address"},{"name":"pairSell","type":"address"},
             {"name":"amountIn","type":"uint256"},{"name":"minProfit","type":"uint256"}],
   "outputs":[]}
]"#;

pub type BoxError = Box<dyn Error + Send + Sync>;

pub struct Executor {
    web3: Web3<WebSocket>,
    contract: H160,
    key: SecretKey,
    abi: Abi,
    gas_limit: u64,
    chain_id: u64,
}

impl Executor {
    pub fn new(
        web3: Web3<WebSocket>,
        contract: H160,
        private_key_hex: &str,
        gas_limit: u64,
        chain_id: u64,
    ) -> Result<Executor, BoxError> {
        let key = SecretKey::from_str(private_key_hex.trim_start_matches("0x"))
            .map_err(|e| format!("invalid PRIVATE_KEY: {}", e))?;
        let abi = Abi::load(EXECUTOR_ABI.as_bytes())?;
        Ok(Executor { web3, contract, key, abi, gas_limit, chain_id })
    }

    pub fn signer_address(&self) -> H160 {
        SecretKeyRef::new(&self.key).address()
    }

    pub fn contract_address(&self) -> H160 {
        self.contract
    }

    /// Signs and submits ArbExecutor.execute(pairBuy, pairSell, amountIn, minProfit).
    /// The contract reverts unless the round trip nets at least `min_profit`,
    /// so a stale opportunity costs only gas, never inventory.
    pub async fn send(
        &self,
        pair_buy: H160,
        pair_sell: H160,
        amount_in: U256,
        min_profit: U256,
        gas_price: U256,
    ) -> Result<H256, BoxError> {
        let data = self.abi.function("execute")?.encode_input(&[
            Token::Address(pair_buy),
            Token::Address(pair_sell),
            Token::Uint(amount_in),
            Token::Uint(min_profit),
        ])?;
        let tx = TransactionParameters {
            to: Some(self.contract),
            gas: self.gas_limit.into(),
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
                Token::Address(H160::repeat_byte(1)),
                Token::Address(H160::repeat_byte(2)),
                Token::Uint(U256::from(3u64)),
                Token::Uint(U256::from(4u64)),
            ])
            .unwrap();
        // 4-byte selector + 4 static words.
        assert_eq!(data.len(), 4 + 4 * 32);
    }
}
