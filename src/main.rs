use log::{error, warn};
use log::info;


use web3::futures::{TryStreamExt};
use web3::transports::{WebSocket};
use web3::types::{TransactionId};


#[tokio::main]
async fn main() -> web3::Result {
    print!("done");
    env_logger::init();
    let sub_transport = WebSocket::new("ws://localhost:3334").await?;
    let web3 = web3::Web3::new(sub_transport);

    let mut pending_transactions = web3.eth_subscribe().subscribe_new_pending_transactions().await?;

    while let Some(pending_transaction_hash) = pending_transactions.try_next().await? {
        let pth = TransactionId::from(pending_transaction_hash);
        println!("Pending transaction hash: {:?}", pth);

        let res = web3.eth().transaction(pth).await;
        // println!("Transaction retrieval result: {:?}", res);

        match res {
            Ok(opt_txn) => {
                match opt_txn {
                    None => { warn!("could not find transaction for now") },
                    Some(txn) => {
                        // Log "to" and "value" fields of the transaction
                        if let Some(to) = txn.to {
                            info!("To: {:?}", to);
                        } else {
                            warn!("Transaction does not have a 'to' address");
                        }
                        info!("From: {:?}", txn.from );
                        info!("Value: {:?}", txn.value);
                        if txn.to = "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D"{
                            println!("To: {:?}", to);
                            println!("From: {:?}", txn.from );
                            println!("Value: {:?}", txn.value);
                        }
                    }
                }
            }
            Err(e) => error!("{:?}", e)
        }
    }

    Ok(())
}