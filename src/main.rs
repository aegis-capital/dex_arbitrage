mod arb;
mod config;
mod dex;
mod executor;
mod mempool;

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use log::{debug, error, info, warn};
use tokio::sync::mpsc;
use tokio::time::Instant;
use web3::futures::future::join_all;
use web3::futures::TryStreamExt;
use web3::transports::WebSocket;
use web3::types::{H160, U256};
use web3::Web3;

use arb::format_eth;
use config::Config;
use executor::{BoxError, Executor};

/// One hop of an arbitrage cycle: which pool, entering with which token.
struct CycleLeg {
    pool: usize,
    token_in: H160,
}

struct Cycle {
    name: String,
    legs: Vec<CycleLeg>,
}

struct Universe {
    pools: Vec<dex::Pool>,
    cycles: Vec<Cycle>,
}

const MAX_CYCLES: usize = 1_000;

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cfg = Config::from_env();
    info!(
        "starting arbitrage bot: chain={} node={} dry_run={} max_trade={} ETH min_profit={} ETH max_legs={}",
        cfg.chain,
        cfg.ws_url,
        cfg.dry_run,
        format_eth(cfg.max_trade_wei),
        format_eth(cfg.min_profit_wei),
        cfg.max_legs,
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

    let universe = build_universe(&web3, cfg).await?;
    if universe.cycles.is_empty() {
        return Err("no arbitrage cycles found (no token has pools on two DEXes)".into());
    }

    let exec = match (&cfg.private_key, &cfg.executor_contract, cfg.dry_run) {
        (Some(key), Some(contract), false) => {
            let exec = Executor::new(web3.clone(), *contract, key, chain_id)?;
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
    info!(
        "watching {} pool(s), {} arbitrage cycle(s)",
        universe.pools.len(),
        universe.cycles.len()
    );

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
        if let Err(e) = scan(&web3, &universe, cfg, exec.as_ref(), trigger).await {
            warn!("scan failed: {}", e);
        }
    };

    mempool_task.abort();
    result
}

/// Discovers every pool among the candidate tokens (WETH, the quote set, and
/// any TOKENS extras) on every configured factory, then enumerates arbitrage
/// cycles: WETH -> ... -> WETH paths of up to max_legs swaps over distinct
/// pools with distinct intermediate tokens.
async fn build_universe(web3: &Web3<WebSocket>, cfg: &Config) -> Result<Universe, BoxError> {
    let mut tokens: Vec<(String, H160)> = vec![("WETH".to_string(), cfg.weth)];
    for (sym, addr) in &cfg.quote_tokens {
        tokens.push((sym.to_string(), *addr));
    }
    for addr in &cfg.extra_tokens {
        if tokens.iter().any(|(_, a)| a == addr) {
            continue;
        }
        let sym = dex::token_symbol(web3, *addr).await;
        info!("extra token {:?} resolved as {}", addr, sym);
        tokens.push((sym, *addr));
    }
    let symbols: HashMap<H160, String> = tokens.iter().map(|(s, a)| (*a, s.clone())).collect();

    let mut pools = Vec::new();
    for i in 0..tokens.len() {
        for j in (i + 1)..tokens.len() {
            let (a, b) = (tokens[i].1, tokens[j].1);
            for factory in &cfg.factories {
                match dex::resolve_pools(web3, factory, a, b).await {
                    Ok(found) => {
                        for pool in found {
                            info!(
                                "{}/{} on {}: pool {:?} (fee {} bps)",
                                tokens[i].0, tokens[j].0, pool.dex, pool.address, pool.fee_bps
                            );
                            pools.push(pool);
                        }
                    }
                    Err(e) => warn!(
                        "failed to resolve {}/{} on {}: {:?}",
                        tokens[i].0, tokens[j].0, factory.name, e
                    ),
                }
            }
        }
    }

    let mut by_token: HashMap<H160, Vec<usize>> = HashMap::new();
    for (idx, pool) in pools.iter().enumerate() {
        by_token.entry(pool.token0).or_default().push(idx);
        by_token.entry(pool.token1).or_default().push(idx);
    }

    let mut cycles = Vec::new();
    let mut legs = Vec::new();
    let mut used_pools = HashSet::new();
    let mut visited = HashSet::new();
    dfs_cycles(
        cfg.weth,
        cfg.weth,
        cfg.max_legs,
        &pools,
        &by_token,
        &mut legs,
        &mut used_pools,
        &mut visited,
        &mut cycles,
    );
    if cycles.len() >= MAX_CYCLES {
        warn!("cycle enumeration truncated at {}; reduce MAX_LEGS or TOKENS", MAX_CYCLES);
    }
    for cycle in &mut cycles {
        cycle.name = cycle
            .legs
            .iter()
            .map(|leg| {
                let pool = &pools[leg.pool];
                let unknown = || format!("{:?}", pool.other_token(leg.token_in))[..10].to_string();
                format!(
                    "{}:{}>{}",
                    pool.dex,
                    symbols.get(&leg.token_in).cloned().unwrap_or_else(unknown),
                    symbols.get(&pool.other_token(leg.token_in)).cloned().unwrap_or_else(unknown),
                )
            })
            .collect::<Vec<_>>()
            .join(" | ");
        debug!("cycle: {}", cycle.name);
    }
    Ok(Universe { pools, cycles })
}

#[allow(clippy::too_many_arguments)]
fn dfs_cycles(
    current: H160,
    weth: H160,
    max_legs: usize,
    pools: &[dex::Pool],
    by_token: &HashMap<H160, Vec<usize>>,
    legs: &mut Vec<CycleLeg>,
    used_pools: &mut HashSet<usize>,
    visited: &mut HashSet<H160>,
    cycles: &mut Vec<Cycle>,
) {
    if cycles.len() >= MAX_CYCLES {
        return;
    }
    for &idx in by_token.get(&current).into_iter().flatten() {
        if used_pools.contains(&idx) {
            continue;
        }
        let next = pools[idx].other_token(current);
        legs.push(CycleLeg { pool: idx, token_in: current });
        if next == weth {
            if legs.len() >= 2 {
                cycles.push(Cycle {
                    name: String::new(),
                    legs: legs.iter().map(|l| CycleLeg { pool: l.pool, token_in: l.token_in }).collect(),
                });
            }
        } else if legs.len() < max_legs && !visited.contains(&next) {
            used_pools.insert(idx);
            visited.insert(next);
            dfs_cycles(next, weth, max_legs, pools, by_token, legs, used_pools, visited, cycles);
            visited.remove(&next);
            used_pools.remove(&idx);
        }
        legs.pop();
    }
}

async fn scan(
    web3: &Web3<WebSocket>,
    universe: &Universe,
    cfg: &Config,
    exec: Option<&Executor>,
    trigger: &str,
) -> Result<(), BoxError> {
    let gas_price = web3.eth().gas_price().await?;

    // One reserves call per pool per scan; every cycle reads from this map.
    let results = join_all(universe.pools.iter().map(|p| p.reserves())).await;
    let mut reserves: HashMap<usize, (U256, U256)> = HashMap::new();
    for (idx, res) in results.into_iter().enumerate() {
        match res {
            Ok(r) => {
                reserves.insert(idx, r);
            }
            Err(e) => warn!("failed to fetch reserves of pool {:?}: {:?}", universe.pools[idx].address, e),
        }
    }

    for cycle in &universe.cycles {
        let legs: Option<Vec<arb::Leg>> = cycle
            .legs
            .iter()
            .map(|leg| {
                let pool = &universe.pools[leg.pool];
                reserves.get(&leg.pool).map(|&(r0, r1)| {
                    let (reserve_in, reserve_out) =
                        if leg.token_in == pool.token0 { (r0, r1) } else { (r1, r0) };
                    arb::Leg { reserve_in, reserve_out, fee_bps: pool.fee_bps }
                })
            })
            .collect();
        let legs = match legs {
            Some(l) => l,
            None => continue, // a pool fetch failed this scan
        };

        let opp = match arb::optimal_cycle(&legs, cfg.max_trade_wei) {
            Some(opp) => opp,
            None => continue,
        };
        let gas_units = cfg.gas_base_units + cfg.gas_per_leg_units * legs.len() as u64;
        let gas_cost = gas_price * U256::from(gas_units);
        let threshold = gas_cost + cfg.min_profit_wei;
        if opp.profit <= threshold {
            debug!(
                "[{}] {}: raw profit {} ETH below threshold {} ETH",
                trigger, cycle.name, format_eth(opp.profit), format_eth(threshold),
            );
            continue;
        }

        info!(
            "[{}] PROFITABLE {}: in {} ETH, out {} ETH, profit {} ETH (gas {} ETH)",
            trigger, cycle.name,
            format_eth(opp.amount_in), format_eth(opp.amount_out),
            format_eth(opp.profit), format_eth(gas_cost),
        );

        if let Some(exec) = exec {
            let pairs: Vec<H160> = cycle.legs.iter().map(|l| universe.pools[l.pool].address).collect();
            let mut path: Vec<H160> = cycle.legs.iter().map(|l| l.token_in).collect();
            path.push(cfg.weth);
            let fees: Vec<u32> = cycle.legs.iter().map(|l| universe.pools[l.pool].fee_bps).collect();
            let kinds: Vec<u8> = cycle.legs.iter().map(|l| universe.pools[l.pool].kind as u8).collect();
            // On-chain floor: if reserves move before inclusion and profit
            // would drop below gas cost, the contract reverts instead.
            match exec.send(&pairs, &path, &fees, &kinds, opp.amount_in, gas_cost, gas_price, gas_units * 2).await {
                Ok(hash) => {
                    info!("submitted arbitrage txn {:?}; pausing one block for nonce settlement", hash);
                    tokio::time::sleep(Duration::from_secs(cfg.block_time_secs + 1)).await;
                }
                Err(e) => error!("failed to submit arbitrage txn: {}", e),
            }
        }
    }
    Ok(())
}
