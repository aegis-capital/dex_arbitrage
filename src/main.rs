use log::{error, warn};
use log::info;


use web3::futures::{TryStreamExt};
use web3::transports::{WebSocket};
use web3::types::{TransactionId};


#[tokio::main]
async fn main() -> web3::Result {
    print!("done");
    env_logger::init();
    let sub_transport = WebSocket::new("wss://eth-mainnet.g.alchemy.com/v2/U7qmz5USjazOpq8YRDV9vmGtZwbVwLr2").await?;
    let web3 = web3::Web3::new(sub_transport);

    let mut pending_transactions = web3.eth_subscribe().subscribe_new_pending_transactions().await?;

    while let Some(pending_transaction_hash) = pending_transactions.try_next().await? {
        let pth = TransactionId::from(pending_transaction_hash);
        println!("Pending transaction hash: {:?}", pth); // Logging the pending transaction hash
        let res = web3.eth().transaction(pth).await;
        match res {
            Ok(opt_txn) => {
                match opt_txn {
                    None => { warn!("could not find transaction for now") },
                    Some(txn) => info!("{:?}", txn)
                }
            }
            Err(e) => error!("{:?}", e)
        }
    }

    Ok(())
}