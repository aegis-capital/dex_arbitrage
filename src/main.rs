mod arb;
mod config;
mod dex;
mod executor;
mod mempool;

use std::time::Duration;

use log::{error, info, warn};
use tokio::sync::mpsc;
use tokio::time::Instant;
use web3::futures::TryStreamExt;
use web3::transports::WebSocket;
use web3::types::U256;
use web3::Web3;

use arb::format_eth;
use config::Config;
use executor::{BoxError, Executor};

struct Market {
    symbol: &'static str,
    uni: dex::Pool,
    sushi: dex::Pool,
}

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cfg = Config::from_env();
    info!(
        "starting arbitrage bot: chain={} node={} dry_run={} max_trade={} ETH min_profit={} ETH",
        cfg.chain,
        cfg.ws_url,
        cfg.dry_run,
        format_eth(cfg.max_trade_wei),
        format_eth(cfg.min_profit_wei),
    );

    loop {
        if let Err(e) = run(&cfg).await {
            error!("connection lost: {}; reconnecting in 5s", e);
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn run(cfg: &Config) -> Result<(), BoxError> {
    let transport = WebSocket::new(&cfg.ws_url).await?;
    let web3 = Web3::new(transport);
    let chain_id = web3.eth().chain_id().await?.as_u64();
    info!("connected to chain id {}", chain_id);
    if chain_id != cfg.expected_chain_id {
        warn!(
            "node reports chain id {} but CHAIN={} expects {}; \
             the default contract addresses are wrong unless overridden via env",
            chain_id, cfg.chain, cfg.expected_chain_id
        );
    }

    let markets = resolve_markets(&web3, cfg).await?;
    if markets.is_empty() {
        return Err("no WETH pairs exist on both factories; nothing to arbitrage".into());
    }

    let exec = match (&cfg.private_key, &cfg.executor_contract, cfg.dry_run) {
        (Some(key), Some(contract), false) => {
            let exec = Executor::new(web3.clone(), *contract, key, cfg.gas_limit, chain_id)?;
            info!("executing via contract {:?}, signing as {:?}", exec.contract_address(), exec.signer_address());
            let inventory = dex::erc20_balance(&web3, cfg.weth, *contract).await?;
            if inventory < cfg.max_trade_wei {
                warn!(
                    "executor contract WETH balance ({} ETH) is below MAX_TRADE_WEI ({} ETH); \
                     trades above the balance will revert",
                    format_eth(inventory),
                    format_eth(cfg.max_trade_wei)
                );
            }
            Some(exec)
        }
        _ => {
            info!("dry-run mode: opportunities will be logged but not executed");
            None
        }
    };

    let (trigger_tx, mut trigger_rx) = mpsc::channel::<&'static str>(8);
    let mempool_task = tokio::spawn(mempool::watch(web3.clone(), cfg.routers.clone(), trigger_tx));

    let mut heads = web3.eth_subscribe().subscribe_new_heads().await?;
    info!("watching {} market(s) across Uniswap V2 / SushiSwap", markets.len());

    // Rescans are triggered by new blocks and by pending router transactions,
    // debounced so a burst of mempool hits causes one scan, not dozens.
    let mut last_scan = Instant::now() - Duration::from_secs(60);
    let result = loop {
        let trigger = tokio::select! {
            head = heads.try_next() => match head {
                Ok(Some(_)) => "block",
                Ok(None) => break Err("new-heads subscription ended".into()),
                Err(e) => break Err(e.into()),
            },
            t = trigger_rx.recv() => match t {
                Some(t) => t,
                None => break Err("mempool watcher stopped unexpectedly".into()),
            },
        };
        if last_scan.elapsed() < Duration::from_millis(200) {
            continue;
        }
        last_scan = Instant::now();
        if let Err(e) = scan(&web3, &markets, cfg, exec.as_ref(), trigger).await {
            warn!("scan failed: {}", e);
        }
    };

    mempool_task.abort();
    result
}

async fn resolve_markets(web3: &Web3<WebSocket>, cfg: &Config) -> Result<Vec<Market>, BoxError> {
    let mut markets = Vec::new();
    for (symbol, quote) in &cfg.quote_tokens {
        let uni = dex::resolve_pool(web3, cfg.uni_factory, cfg.weth, *quote, "uniswap").await?;
        let sushi = dex::resolve_pool(web3, cfg.sushi_factory, cfg.weth, *quote, "sushiswap").await?;
        match (uni, sushi) {
            (Some(uni), Some(sushi)) => {
                info!("WETH/{}: uniswap pair {:?}, sushiswap pair {:?}", symbol, uni.address, sushi.address);
                markets.push(Market { symbol, uni, sushi });
            }
            _ => warn!("WETH/{} does not exist on both factories; skipping", symbol),
        }
    }
    Ok(markets)
}

async fn scan(
    web3: &Web3<WebSocket>,
    markets: &[Market],
    cfg: &Config,
    exec: Option<&Executor>,
    trigger: &str,
) -> Result<(), BoxError> {
    let gas_price = web3.eth().gas_price().await?;
    let gas_cost = gas_price * U256::from(cfg.gas_limit);

    for market in markets {
        let (uni_res, sushi_res) = tokio::join!(market.uni.reserves(), market.sushi.reserves());
        let (uni_res, sushi_res) = match (uni_res, sushi_res) {
            (Ok(u), Ok(s)) => (u, s),
            (u, s) => {
                warn!("failed to fetch WETH/{} reserves: {:?} / {:?}", market.symbol, u.err(), s.err());
                continue;
            }
        };

        let directions = [
            (&market.uni, uni_res, &market.sushi, sushi_res),
            (&market.sushi, sushi_res, &market.uni, uni_res),
        ];
        for (pool_buy, res_buy, pool_sell, res_sell) in directions {
            let opp = match arb::optimal_arb(&res_buy, &res_sell, cfg.max_trade_wei) {
                Some(opp) => opp,
                None => continue,
            };
            let threshold = gas_cost + cfg.min_profit_wei;
            if opp.profit <= threshold {
                info!(
                    "[{}] WETH/{} {}->{}: raw profit {} ETH below threshold {} ETH (gas {} ETH)",
                    trigger, market.symbol, pool_buy.dex, pool_sell.dex,
                    format_eth(opp.profit), format_eth(threshold), format_eth(gas_cost),
                );
                continue;
            }

            info!(
                "[{}] PROFITABLE WETH/{} {}->{}: in {} ETH, out {} ETH, profit {} ETH (gas {} ETH)",
                trigger, market.symbol, pool_buy.dex, pool_sell.dex,
                format_eth(opp.amount_in), format_eth(opp.amount_out),
                format_eth(opp.profit), format_eth(gas_cost),
            );

            if let Some(exec) = exec {
                // On-chain floor: if reserves move before inclusion and profit
                // would drop below gas cost, the contract reverts instead.
                match exec.send(pool_buy.address, pool_sell.address, opp.amount_in, gas_cost, gas_price).await {
                    Ok(hash) => {
                        info!("submitted arbitrage txn {:?}; pausing one block for nonce settlement", hash);
                        tokio::time::sleep(Duration::from_secs(cfg.block_time_secs + 1)).await;
                    }
                    Err(e) => error!("failed to submit arbitrage txn: {}", e),
                }
            }
        }
    }
    Ok(())
}
