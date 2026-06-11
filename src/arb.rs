use web3::types::U256;

/// One swap hop in an arbitrage cycle: reserves oriented in the direction of
/// travel, plus the pool's fee in 1/10000 units (30 = 0.3%).
#[derive(Debug, Clone, Copy)]
pub struct Leg {
    pub reserve_in: U256,
    pub reserve_out: U256,
    pub fee_bps: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct Opportunity {
    /// Base-token amount to send into the first leg.
    pub amount_in: U256,
    /// Base-token amount received from the last leg.
    pub amount_out: U256,
    /// amount_out - amount_in.
    pub profit: U256,
}

const BPS: u64 = 10_000;

/// Constant-product swap output with fee on input:
/// out = (10000-fee)*x*rOut / (10000*rIn + (10000-fee)*x).
pub fn swap_out(amount_in: U256, leg: &Leg) -> U256 {
    if amount_in.is_zero() || leg.reserve_in.is_zero() || leg.reserve_out.is_zero() {
        return U256::zero();
    }
    let with_fee = amount_in * U256::from(BPS - leg.fee_bps as u64);
    (with_fee * leg.reserve_out) / (leg.reserve_in * U256::from(BPS) + with_fee)
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

/// Finds the profit-maximizing input for a cycle of swaps that starts and ends
/// in the same token, capped at `max_in`.
///
/// Each leg is a fractional-linear map f(x) = E*x/(F + G*x) with
/// E = k*rOut, F = rIn, G = k (k = 1 - fee), and that family is closed under
/// composition, so any cycle reduces to a single (E, F, G):
/// profitable iff E > F, maximized at x* = (sqrt(E*F) - F) / G.
/// The optimum is located in f64 (relative error ~1e-15, irrelevant for
/// sizing); the resulting profit is then verified with exact integer math.
pub fn optimal_cycle(legs: &[Leg], max_in: U256) -> Option<Opportunity> {
    if legs.len() < 2 {
        return None;
    }
    let (mut e, mut f, mut g) = (1f64, 1f64, 0f64);
    for leg in legs {
        let r_in = u256_to_f64(leg.reserve_in);
        let r_out = u256_to_f64(leg.reserve_out);
        if r_in == 0.0 || r_out == 0.0 {
            return None;
        }
        let k = (BPS - leg.fee_bps as u64) as f64 / BPS as f64;
        // f_leg(prev(x)): E' = E_leg*E, F' = F_leg*F, G' = F_leg*G + G_leg*E
        let (e_leg, f_leg, g_leg) = (k * r_out, r_in, k);
        g = f_leg * g + g_leg * e;
        e *= e_leg;
        f *= f_leg;
    }
    if e <= f {
        return None;
    }
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

    let mut amount = amount_in;
    for leg in legs {
        amount = swap_out(amount, leg);
    }
    let amount_out = amount;
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

    fn leg(r_in: u64, r_out: u64, fee: u32) -> Leg {
        Leg { reserve_in: U256::from(r_in), reserve_out: U256::from(r_out), fee_bps: fee }
    }

    fn round_trip(x: U256, legs: &[Leg]) -> U256 {
        legs.iter().fold(x, |amt, l| swap_out(amt, l))
    }

    fn brute_force_best(legs: &[Leg], up_to: u64) -> U256 {
        let mut best = U256::zero();
        for x in 1..up_to {
            let x = U256::from(x);
            if let Some(p) = round_trip(x, legs).checked_sub(x) {
                if p > best {
                    best = p;
                }
            }
        }
        best
    }

    #[test]
    fn swap_out_matches_uniswap_formula() {
        let out = swap_out(U256::from(1_000u64), &leg(1_000_000, 1_000_000, 30));
        // 9970*1000*1e6 / (1e10 + 9.97e6) = 996.006...
        assert_eq!(out, U256::from(996u64));
    }

    #[test]
    fn lower_fee_gives_more_output() {
        let base = swap_out(U256::from(1_000_000u64), &leg(100_000_000, 100_000_000, 30));
        let aero = swap_out(U256::from(1_000_000u64), &leg(100_000_000, 100_000_000, 25));
        assert!(aero > base);
    }

    #[test]
    fn no_arb_when_pools_identical() {
        let legs = [leg(1_000_000, 2_000_000, 30), leg(2_000_000, 1_000_000, 30)];
        assert!(optimal_cycle(&legs, U256::max_value()).is_none());
    }

    #[test]
    fn finds_near_optimal_input() {
        // Quote 5.3% cheaper on the first pool than the second.
        let legs = [leg(1_000_000, 2_000_000, 30), leg(1_900_000, 1_000_000, 30)];
        let opp = optimal_cycle(&legs, U256::max_value()).expect("should be profitable");
        let best = brute_force_best(&legs, 50_000);
        assert!(!best.is_zero());
        assert!(opp.profit >= best - U256::from(1u64), "analytic {} vs brute {}", opp.profit, best);
        assert_eq!(round_trip(opp.amount_in, &legs), opp.amount_out);
        assert_eq!(opp.amount_out - opp.amount_in, opp.profit);
    }

    #[test]
    fn finds_near_optimal_triangular_input() {
        // WETH -> USDC -> TOK -> WETH with mixed fees and a mispriced TOK/WETH pool.
        let legs = [
            leg(1_000_000, 3_000_000, 30),  // WETH/USDC
            leg(3_000_000, 9_000_000, 25),  // USDC/TOK (Aerodrome-style fee)
            leg(8_600_000, 1_000_000, 30),  // TOK/WETH, TOK overpriced here
        ];
        let opp = optimal_cycle(&legs, U256::max_value()).expect("should be profitable");
        let best = brute_force_best(&legs, 50_000);
        assert!(!best.is_zero());
        assert!(opp.profit >= best - U256::from(1u64), "analytic {} vs brute {}", opp.profit, best);
        assert_eq!(round_trip(opp.amount_in, &legs), opp.amount_out);
    }

    #[test]
    fn caps_trade_at_max_in() {
        let legs = [leg(1_000_000, 2_000_000, 30), leg(1_900_000, 1_000_000, 30)];
        let opp = optimal_cycle(&legs, U256::from(100u64)).expect("capped trade still profitable");
        assert_eq!(opp.amount_in, U256::from(100u64));
    }

    #[test]
    fn rejects_degenerate_cycles() {
        assert!(optimal_cycle(&[], U256::max_value()).is_none());
        assert!(optimal_cycle(&[leg(1_000_000, 2_000_000, 30)], U256::max_value()).is_none());
        let dead = [leg(0, 2_000_000, 30), leg(1_900_000, 1_000_000, 30)];
        assert!(optimal_cycle(&dead, U256::max_value()).is_none());
    }

    #[test]
    fn works_at_mainnet_scale() {
        // ~3000 WETH / ~9.1M USDC-style pools with a 1% price gap.
        let p1_base = U256::from_dec_str("3000000000000000000000").unwrap();
        let p1_quote = U256::from_dec_str("9100000000000").unwrap();
        let p2_base = U256::from_dec_str("3000000000000000000000").unwrap();
        let p2_quote = U256::from_dec_str("9009000000000").unwrap();
        let legs = [
            Leg { reserve_in: p1_base, reserve_out: p1_quote, fee_bps: 30 },
            Leg { reserve_in: p2_quote, reserve_out: p2_base, fee_bps: 30 },
        ];
        let opp = optimal_cycle(&legs, U256::max_value()).expect("1% gap is profitable");
        assert!(opp.profit > U256::zero());
        assert_eq!(round_trip(opp.amount_in, &legs), opp.amount_out);
        // Perturbing the input in either direction must not do better.
        let bump = opp.amount_in / 100;
        for x in [opp.amount_in - bump, opp.amount_in + bump] {
            let out = round_trip(x, &legs);
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
