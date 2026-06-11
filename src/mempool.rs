use log::{debug, warn};
use tokio::sync::mpsc;
use web3::futures::TryStreamExt;
use web3::transports::WebSocket;
use web3::types::{TransactionId, H160};
use web3::Web3;

/// Watches pending transactions and fires a trigger whenever one targets a
/// known DEX router, so the main loop can rescan reserves ahead of the next
/// block. Loops forever, resubscribing on errors, so the trigger channel
/// stays open for the lifetime of the connection.
pub async fn watch(web3: Web3<WebSocket>, routers: Vec<H160>, trigger: mpsc::Sender<&'static str>) {
    loop {
        if let Err(e) = watch_once(&web3, &routers, &trigger).await {
            warn!("mempool subscription error: {:?}; resubscribing in 5s", e);
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

async fn watch_once(
    web3: &Web3<WebSocket>,
    routers: &[H160],
    trigger: &mpsc::Sender<&'static str>,
) -> web3::Result<()> {
    let mut pending = web3.eth_subscribe().subscribe_new_pending_transactions().await?;

    while let Some(hash) = pending.try_next().await? {
        let web3 = web3.clone();
        let routers = routers.to_vec();
        let trigger = trigger.clone();
        // The subscription only delivers hashes; fetch bodies concurrently so
        // one slow lookup doesn't stall the stream.
        tokio::spawn(async move {
            match web3.eth().transaction(TransactionId::from(hash)).await {
                Ok(Some(txn)) => {
                    if let Some(to) = txn.to {
                        if routers.contains(&to) {
                            debug!("pending router txn {:?} (to {:?})", txn.hash, to);
                            // try_send: if a scan is already queued, dropping
                            // extra triggers loses nothing.
                            let _ = trigger.try_send("mempool");
                        }
                    }
                }
                // Pending bodies often aren't retrievable yet; the periodic
                // block scan covers anything missed here.
                Ok(None) => debug!("pending txn {:?} not yet retrievable", hash),
                Err(e) => debug!("failed to fetch pending txn {:?}: {:?}", hash, e),
            }
        });
    }
    Ok(())
}
