#[cfg(test)]
mod tests {
    use super::{QuoteInput, QuotePlanner};
    use crate::models::Side;

    fn planner() -> QuotePlanner {
        QuotePlanner::new(100.0, 25.0, 2, 1.0)
    }

    #[test]
    fn skips_market_without_a_positive_reward_spread() {
        let input = QuoteInput {
            condition_id: "condition".into(),
            token_id: "token".into(),
            midpoint: 0.50,
            tick_size: 0.01,
            rewards_max_spread_cents: 0.0,
            rewards_min_size: 20.0,
            daily_rate: 5.0,
        };

        assert!(planner().plan(&[input], 0.0).is_empty());
    }

    #[test]
    fn plans_a_rounded_buy_without_exceeding_capital() {
        let input = QuoteInput {
            condition_id: "condition".into(),
            token_id: "token".into(),
            midpoint: 0.503,
            tick_size: 0.01,
            rewards_max_spread_cents: 5.0,
            rewards_min_size: 20.0,
            daily_rate: 5.0,
        };

        let plans = planner().plan(&[input], 0.0);
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].side, Side::Buy);
        assert_eq!(plans[0].price, 0.48);
        assert_eq!(plans[0].size, 20.0);
        assert!(plans[0].notional <= 25.0);
    }
}
