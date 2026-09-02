use crate::models::Side;

#[derive(Debug, Clone)]
pub struct QuoteInput {
    pub condition_id: String,
    pub token_id: String,
    pub midpoint: f64,
    pub tick_size: f64,
    pub rewards_max_spread_cents: f64,
    pub rewards_min_size: f64,
    pub daily_rate: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuotePlan {
    pub condition_id: String,
    pub token_id: String,
    pub side: Side,
    pub price: f64,
    pub size: f64,
    pub notional: f64,
    pub daily_rate: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct QuotePlanner {
    max_capital_usdc: f64,
    max_order_usdc: f64,
    max_markets: usize,
    min_daily_rate: f64,
}

impl QuotePlanner {
    pub fn new(
        max_capital_usdc: f64,
        max_order_usdc: f64,
        max_markets: usize,
        min_daily_rate: f64,
    ) -> Self {
        Self {
            max_capital_usdc,
            max_order_usdc,
            max_markets,
            min_daily_rate,
        }
    }

    pub fn plan(&self, inputs: &[QuoteInput], committed_usdc: f64) -> Vec<QuotePlan> {
        if self.max_markets == 0
            || !self.max_capital_usdc.is_finite()
            || !self.max_order_usdc.is_finite()
            || self.max_capital_usdc <= 0.0
            || self.max_order_usdc <= 0.0
        {
            return vec![];
        }

        let committed = if committed_usdc.is_finite() {
            committed_usdc.max(0.0)
        } else {
            return vec![];
        };
        let mut remaining = (self.max_capital_usdc - committed).max(0.0);
        let mut candidates = inputs.iter().collect::<Vec<_>>();
        candidates.sort_by(|a, b| b.daily_rate.total_cmp(&a.daily_rate));

        candidates
            .into_iter()
            .take(self.max_markets)
            .filter_map(|input| {
                if input.condition_id.is_empty()
                    || input.token_id.is_empty()
                    || !input.midpoint.is_finite()
                    || !input.tick_size.is_finite()
                    || !input.rewards_max_spread_cents.is_finite()
                    || !input.rewards_min_size.is_finite()
                    || !input.daily_rate.is_finite()
                    || input.midpoint <= 0.0
                    || input.midpoint >= 1.0
                    || input.tick_size <= 0.0
                    || input.rewards_max_spread_cents <= 0.0
                    || input.rewards_min_size <= 0.0
                    || input.daily_rate < self.min_daily_rate
                {
                    return None;
                }

                let target = input.midpoint - input.rewards_max_spread_cents * 0.01 * 0.5;
                let price = (target / input.tick_size).round() * input.tick_size;
                let notional = price * input.rewards_min_size;
                if !price.is_finite()
                    || !notional.is_finite()
                    || price <= 0.0
                    || price >= 1.0
                    || notional <= 0.0
                    || notional > self.max_order_usdc + 1e-9
                    || notional > remaining + 1e-9
                {
                    return None;
                }

                remaining -= notional;
                Some(QuotePlan {
                    condition_id: input.condition_id.clone(),
                    token_id: input.token_id.clone(),
                    side: Side::Buy,
                    price,
                    size: input.rewards_min_size,
                    notional,
                    daily_rate: input.daily_rate,
                })
            })
            .collect()
    }
}

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

    #[test]
    fn does_not_use_already_committed_capital() {
        let input = QuoteInput {
            condition_id: "condition".into(),
            token_id: "token".into(),
            midpoint: 0.50,
            tick_size: 0.01,
            rewards_max_spread_cents: 5.0,
            rewards_min_size: 20.0,
            daily_rate: 5.0,
        };

        assert!(planner().plan(&[input], 95.0).is_empty());
    }
}
