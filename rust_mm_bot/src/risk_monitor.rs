use crate::models::{RiskSnapshot, Side};
use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct FillRow {
    pub token_id: String,
    pub fill_size: f64,
    pub fill_price: f64,
    pub ts: DateTime<Utc>,
}

#[derive(Debug, Default)]
pub struct RiskMonitor {
    fills_by_token: HashMap<String, Vec<FillRow>>,
}

impl RiskMonitor {
    pub fn on_fill(&mut self, row: FillRow) {
        self.fills_by_token
            .entry(row.token_id.clone())
            .or_default()
            .push(row);
    }

    pub fn snapshot(&self, token_id: &str, side: Side, scoring: bool, depth_total: f64, depth_closer: f64) -> RiskSnapshot {
        let now = Utc::now();
        let short_cutoff = now - Duration::seconds(180);
        let long_cutoff = now - Duration::seconds(3600);
        let fills = self.fills_by_token.get(token_id).cloned().unwrap_or_default();

        let mut short_count = 0usize;
        let mut long_count = 0usize;
        let mut notional_short = 0.0;
        let mut notional_long = 0.0;
        for f in fills {
            if f.ts >= long_cutoff {
                long_count += 1;
                notional_long += f.fill_size * f.fill_price;
                if f.ts >= short_cutoff {
                    short_count += 1;
                    notional_short += f.fill_size * f.fill_price;
                }
            }
        }
        let fill_rate = (long_count as f64 / 25.0).min(1.0);
        // Reserve directional slot for later parity refinements.
        let directional_factor = match side {
            Side::Buy => 0.98,
            Side::Sell => 0.98,
        };
        let fill_risk_score = ((0.55 * (short_count as f64 / 8.0).min(1.0))
            + (0.45 * (notional_long / 400.0).min(1.0))
            + (0.25 * (notional_short / 120.0).min(1.0)))
            * directional_factor;

        let inner_band_depth_ratio = if depth_total > 1e-9 {
            (depth_closer / depth_total).clamp(0.0, 1.0)
        } else {
            0.0
        };

        RiskSnapshot {
            fill_rate,
            short_window_fills: short_count,
            long_window_fills: long_count,
            fill_risk_score: fill_risk_score.clamp(0.0, 1.0),
            depth_total_in_band: depth_total,
            depth_closer_than_order: depth_closer,
            inner_band_depth_ratio,
            scoring,
        }
    }
}
