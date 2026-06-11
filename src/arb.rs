use web3::types::U256;

/// Reserves of a constant-product pool, oriented around the bot's base token
/// (the token profit is measured in, e.g. WETH).
#[derive(Debug, Clone, Copy)]
pub struct PoolReserves {
    pub base: U256,
    pub quote: U256,
}

#[derive(Debug, Clone, Copy)]
pub struct Opportunity {
    /// Base-token amount to send into the first pool.
    pub amount_in: U256,
    /// Base-token amount received from the second pool.
    pub amount_out: U256,
    /// amount_out - amount_in.
    pub profit: U256,
}

/// Uniswap V2 swap output with the 0.3% fee: out = 997*x*rOut / (1000*rIn + 997*x).
pub fn swap_out(amount_in: U256, reserve_in: U256, reserve_out: U256) -> U256 {
    if amount_in.is_zero() || reserve_in.is_zero() || reserve_out.is_zero() {
        return U256::zero();
    }
    let with_fee = amount_in * U256::from(997u64);
    (with_fee * reserve_out) / (reserve_in * U256::from(1000u64) + with_fee)
}

pub fn u256_to_f64(v: U256) -> f64 {
    let mut acc = 0f64;
    for i in (0..4).rev() {
        acc = acc * 2f64.powi(64) + v.0[i] as f64;
    }
    acc
}

pub fn f64_to_u256(v: f64) -> U256 {
    if !v.is_finite() || v < 1.0 {
        return U256::zero();
    }
    // f64 has 52 mantissa bits; going through the decimal representation keeps
    // every bit the float actually carries, which is plenty for trade sizing.
    U256::from_dec_str(&format!("{:.0}", v)).unwrap_or_else(|_| U256::zero())
}

/// Finds the profit-maximizing amount of base token to swap base->quote in
/// `pool_buy` and quote->base in `pool_sell`, capped at `max_in`.
///
/// With fee factor k = 0.997 the round trip output is
///   out(x) = E*x / (F + G*x),  E = k^2*q1*b2,  F = b1*q2,  G = k*q2 + k^2*q1
/// which is profitable iff E > F, and maximized at x* = (sqrt(E*F) - F) / G.
/// The optimum is located in f64 (relative error ~1e-15, irrelevant for
/// sizing); the resulting profit is then verified with exact integer math.
pub fn optimal_arb(pool_buy: &PoolReserves, pool_sell: &PoolReserves, max_in: U256) -> Option<Opportunity> {
    const K: f64 = 0.997;
    let b1 = u256_to_f64(pool_buy.base);
    let q1 = u256_to_f64(pool_buy.quote);
    let b2 = u256_to_f64(pool_sell.base);
    let q2 = u256_to_f64(pool_sell.quote);
    if b1 == 0.0 || q1 == 0.0 || b2 == 0.0 || q2 == 0.0 {
        return None;
    }

    let e = K * K * q1 * b2;
    let f = b1 * q2;
    if e <= f {
        return None;
    }
    let g = K * q2 + K * K * q1;
    let x_star = ((e * f).sqrt() - f) / g;
    if x_star < 1.0 {
        return None;
    }

    // out(x) is concave and increasing on [0, x*], so when the optimum exceeds
    // our inventory cap the best feasible trade is the cap itself.
    let mut amount_in = f64_to_u256(x_star);
    if amount_in > max_in {
        amount_in = max_in;
    }
    if amount_in.is_zero() {
        return None;
    }

    let amount_mid = swap_out(amount_in, pool_buy.base, pool_buy.quote);
    let amount_out = swap_out(amount_mid, pool_sell.quote, pool_sell.base);
    let profit = amount_out.checked_sub(amount_in)?;
    if profit.is_zero() {
        return None;
    }
    Some(Opportunity { amount_in, amount_out, profit })
}

pub fn format_eth(wei: U256) -> String {
    format!("{:.6}", u256_to_f64(wei) / 1e18)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(x: U256, p1: &PoolReserves, p2: &PoolReserves) -> U256 {
        swap_out(swap_out(x, p1.base, p1.quote), p2.quote, p2.base)
    }

    #[test]
    fn swap_out_matches_uniswap_formula() {
        let out = swap_out(U256::from(1_000u64), U256::from(1_000_000u64), U256::from(1_000_000u64));
        // 997*1000*1e6 / (1e9 + 997e3) = 996.006...
        assert_eq!(out, U256::from(996u64));
    }

    #[test]
    fn no_arb_when_pools_identical() {
        let p = PoolReserves { base: U256::from(1_000_000u64), quote: U256::from(2_000_000u64) };
        assert!(optimal_arb(&p, &p, U256::max_value()).is_none());
    }

    #[test]
    fn finds_near_optimal_input() {
        let p1 = PoolReserves { base: U256::from(1_000_000u64), quote: U256::from(2_000_000u64) };
        let p2 = PoolReserves { base: U256::from(1_000_000u64), quote: U256::from(1_900_000u64) };
        let opp = optimal_arb(&p1, &p2, U256::max_value()).expect("should be profitable");

        // Brute-force the best integer input and compare.
        let mut best_profit = U256::zero();
        for x in (1u64..50_000).step_by(1) {
            let x = U256::from(x);
            let out = round_trip(x, &p1, &p2);
            if let Some(profit) = out.checked_sub(x) {
                if profit > best_profit {
                    best_profit = profit;
                }
            }
        }
        assert!(!best_profit.is_zero());
        assert!(opp.profit >= best_profit - U256::from(1u64), "analytic {} vs brute {}", opp.profit, best_profit);
        assert_eq!(opp.amount_out - opp.amount_in, opp.profit);
        assert_eq!(round_trip(opp.amount_in, &p1, &p2), opp.amount_out);
    }

    #[test]
    fn caps_trade_at_max_in() {
        let p1 = PoolReserves { base: U256::from(1_000_000u64), quote: U256::from(2_000_000u64) };
        let p2 = PoolReserves { base: U256::from(1_000_000u64), quote: U256::from(1_900_000u64) };
        let opp = optimal_arb(&p1, &p2, U256::from(100u64)).expect("capped trade still profitable");
        assert_eq!(opp.amount_in, U256::from(100u64));
    }

    #[test]
    fn works_at_mainnet_scale() {
        // ~3000 WETH / ~9.1M USDC-style pool (18 vs 6 decimals don't matter,
        // both pools quote the same pair) with a 1% price gap.
        let p1 = PoolReserves {
            base: U256::from_dec_str("3000000000000000000000").unwrap(),
            quote: U256::from_dec_str("9100000000000").unwrap(),
        };
        let p2 = PoolReserves {
            base: U256::from_dec_str("3000000000000000000000").unwrap(),
            quote: U256::from_dec_str("9009000000000").unwrap(),
        };
        let opp = optimal_arb(&p1, &p2, U256::max_value()).expect("1% gap is profitable");
        assert!(opp.profit > U256::zero());
        assert_eq!(round_trip(opp.amount_in, &p1, &p2), opp.amount_out);
        // Perturbing the input in either direction must not do better.
        let bump = opp.amount_in / 100;
        for x in [opp.amount_in - bump, opp.amount_in + bump] {
            let out = round_trip(x, &p1, &p2);
            assert!(out - x <= opp.profit);
        }
    }

    #[test]
    fn f64_conversion_roundtrip() {
        for s in ["1", "1000000000000000000", "340282366920938463463374607431768211455"] {
            let v = U256::from_dec_str(s).unwrap();
            let back = f64_to_u256(u256_to_f64(v));
            let diff = if back > v { back - v } else { v - back };
            // Within f64 precision (~1e-15 relative).
            assert!(diff <= v / U256::from(1_000_000_000_000u64));
        }
        assert_eq!(f64_to_u256(-5.0), U256::zero());
        assert_eq!(f64_to_u256(f64::NAN), U256::zero());
    }
}
