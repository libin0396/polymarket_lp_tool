use crate::config::Config;
use crate::models::{
    BookLevel, CustomPricingSettings, DecisionAction, Order, OrderBookSnapshot, PricingDecision, PricingMode, RiskSnapshot,
    Side, TickRegime,
};
use chrono::{DateTime, Duration, Utc};
use std::collections::{HashMap, VecDeque};

#[derive(Debug, Clone)]
struct MidpointGuard {
    ema_mid: Option<f64>,
    recent: VecDeque<f64>,
    pause_until: Option<DateTime<Utc>>,
    stable_since: Option<DateTime<Utc>>,
}

impl Default for MidpointGuard {
    fn default() -> Self {
        Self {
            ema_mid: None,
            recent: VecDeque::new(),
            pause_until: None,
            stable_since: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct PricingEngine {
    guards: HashMap<String, MidpointGuard>,
}

fn classify_tick_regime(tick: f64) -> TickRegime {
    if (tick - 0.01).abs() < 1e-9 || (tick - 1.0).abs() < 1e-6 {
        TickRegime::Coarse
    } else if (tick - 0.001).abs() < 1e-12 || (tick - 0.1).abs() < 1e-6 {
        TickRegime::Fine
    } else {
        TickRegime::Unsupported
    }
}

fn round_tick(price: f64, tick: f64) -> f64 {
    let t = tick.max(1e-12);
    let steps = (price / t).round();
    (steps * t).clamp(t, 1.0 - t)
}

fn level_prices_in_range(levels: &[BookLevel], lo: f64, hi: f64, tick: f64) -> Vec<f64> {
    let mut v = levels
        .iter()
        .filter(|lv| lv.size > 0.0)
        .map(|lv| round_tick(lv.price, tick))
        .filter(|p| *p >= lo - 1e-12 && *p <= hi + 1e-12)
        .collect::<Vec<_>>();
    v.sort_by(f64::total_cmp);
    v.dedup_by(|a, b| (*a - *b).abs() < 1e-12);
    v
}

impl PricingEngine {
    pub fn new() -> Self {
        Self::default()
    }

    fn filtered_midpoint(&mut self, token_id: &str, raw_mid: f64, cfg: &Config) -> (f64, bool) {
        let now = Utc::now();
        let g = self.guards.entry(token_id.to_string()).or_default();
        let prev_ema = g.ema_mid.unwrap_or(raw_mid);
        let ema = cfg.anti_sniping_ema_alpha * raw_mid + (1.0 - cfg.anti_sniping_ema_alpha) * prev_ema;
        g.ema_mid = Some(ema);

        g.recent.push_back(raw_mid);
        while g.recent.len() > cfg.anti_sniping_rolling_median_window.max(3) {
            let _ = g.recent.pop_front();
        }
        let mut mids = g.recent.iter().copied().collect::<Vec<_>>();
        mids.sort_by(f64::total_cmp);
        let median = mids[mids.len() / 2];
        let filtered = 0.5 * ema + 0.5 * median;

        let jump = (raw_mid - prev_ema).abs();
        if jump > cfg.anti_sniping_mid_jump_threshold {
            g.pause_until = Some(now + Duration::milliseconds(cfg.anti_sniping_pause_ms as i64));
            g.stable_since = None;
        } else if g.stable_since.is_none() {
            g.stable_since = Some(now);
        }

        let paused = g.pause_until.map(|t| t > now).unwrap_or(false);
        (filtered, paused)
    }

    fn coarse_default(&self, order: &Order, mid: f64, tick: f64, delta: f64, book: &OrderBookSnapshot, min_replace_ticks: u32) -> PricingDecision {
        let band_ticks = ((delta / tick).floor() as i64).max(1);
        let band = (band_ticks as f64) * tick;
        let (lo, hi) = match order.side {
            Side::Buy => (mid - band, mid),
            Side::Sell => (mid, mid + band),
        };
        let side_levels = match order.side {
            Side::Buy => &book.bids,
            Side::Sell => &book.asks,
        };
        let candidates = level_prices_in_range(side_levels, lo.max(1e-12), hi.min(1.0 - 1e-12), tick);

        if candidates.len() <= 2 {
            return PricingDecision {
                action: DecisionAction::Cancel,
                reason: "coarse_tick_abandon_due_to_too_few_levels".into(),
                mode: PricingMode::Default,
                regime: TickRegime::Coarse,
                candidate_prices: candidates,
                chosen_target: None,
                risk_overlay: None,
            };
        }

        let mut by_dist = candidates.clone();
        by_dist.sort_by(|a, b| (a - mid).abs().total_cmp(&(b - mid).abs()));
        let chosen = if by_dist.len() == 3 {
            by_dist[1]
        } else {
            let mut far = candidates.clone();
            far.sort_by(|a, b| (b - mid).abs().total_cmp(&(a - mid).abs()));
            far[1]
        };
        let min_delta = (min_replace_ticks as f64).max(1.0) * tick;
        let action = if (chosen - round_tick(order.price, tick)).abs() < min_delta - 1e-12 {
            DecisionAction::Keep
        } else {
            DecisionAction::Replace { new_price: chosen }
        };

        PricingDecision {
            action,
            reason: "coarse_tick_default_target".into(),
            mode: PricingMode::Default,
            regime: TickRegime::Coarse,
            candidate_prices: candidates,
            chosen_target: Some(chosen),
            risk_overlay: None,
        }
    }

    fn fine_default(&self, order: &Order, mid: f64, tick: f64, delta: f64, min_replace_ticks: u32) -> PricingDecision {
        let band = delta.max(1e-12);
        let dr = (order.price - mid).abs() / band;
        if (0.4 - 1e-12..=0.6 + 1e-12).contains(&dr) {
            return PricingDecision {
                action: DecisionAction::Keep,
                reason: "fine_tick_keep_in_target_band".into(),
                mode: PricingMode::Default,
                regime: TickRegime::Fine,
                candidate_prices: vec![],
                chosen_target: Some(order.price),
                risk_overlay: None,
            };
        }
        let target = match order.side {
            Side::Buy => round_tick(mid - 0.5 * band, tick),
            Side::Sell => round_tick(mid + 0.5 * band, tick),
        };
        let min_delta = (min_replace_ticks as f64).max(1.0) * tick;
        let action = if (target - order.price).abs() < min_delta {
            DecisionAction::Keep
        } else {
            DecisionAction::Replace { new_price: target }
        };
        PricingDecision {
            action,
            reason: if dr < 0.4 {
                "fine_tick_move_outward_to_half_band".into()
            } else {
                "fine_tick_move_inward_to_half_band".into()
            },
            mode: PricingMode::Default,
            regime: TickRegime::Fine,
            candidate_prices: vec![],
            chosen_target: Some(target),
            risk_overlay: None,
        }
    }

    fn coarse_custom(
        &self,
        order: &Order,
        mid: f64,
        tick: f64,
        delta: f64,
        book: &OrderBookSnapshot,
        settings: &CustomPricingSettings,
        min_replace_ticks: u32,
    ) -> PricingDecision {
        let band_ticks = ((delta / tick).floor() as i64).max(1);
        let band = (band_ticks as f64) * tick;
        let (lo, hi) = match order.side {
            Side::Buy => (mid - band, mid),
            Side::Sell => (mid, mid + band),
        };
        let side_levels = match order.side {
            Side::Buy => &book.bids,
            Side::Sell => &book.asks,
        };
        let mut candidates = level_prices_in_range(side_levels, lo.max(1e-12), hi.min(1.0 - 1e-12), tick);
        candidates.sort_by(|a, b| (a - mid).abs().total_cmp(&(b - mid).abs()));

        if candidates.len() < settings.coarse_min_candidate_levels.max(1) {
            return PricingDecision {
                action: DecisionAction::Keep,
                reason: "custom_coarse_keep_insufficient_candidates".into(),
                mode: PricingMode::Custom,
                regime: TickRegime::Coarse,
                candidate_prices: candidates,
                chosen_target: None,
                risk_overlay: None,
            };
        }

        let n = settings.coarse_tick_offset_from_mid.max(1) - 1;
        let Some(chosen) = candidates.get(n).copied() else {
            return PricingDecision {
                action: DecisionAction::Keep,
                reason: "custom_coarse_keep_rank_outside_band_levels".into(),
                mode: PricingMode::Custom,
                regime: TickRegime::Coarse,
                candidate_prices: candidates,
                chosen_target: None,
                risk_overlay: None,
            };
        };

        if !settings.coarse_allow_top_of_book {
            match (order.side, book.best_bid, book.best_ask) {
                (Side::Buy, Some(bb), _) if (bb - chosen).abs() <= 1e-9 => {
                    return PricingDecision {
                        action: DecisionAction::Keep,
                        reason: "custom_coarse_keep_target_is_top_of_book".into(),
                        mode: PricingMode::Custom,
                        regime: TickRegime::Coarse,
                        candidate_prices: candidates,
                        chosen_target: Some(chosen),
                        risk_overlay: None,
                    };
                }
                (Side::Sell, _, Some(ba)) if (ba - chosen).abs() <= 1e-9 => {
                    return PricingDecision {
                        action: DecisionAction::Keep,
                        reason: "custom_coarse_keep_target_is_top_of_book".into(),
                        mode: PricingMode::Custom,
                        regime: TickRegime::Coarse,
                        candidate_prices: candidates,
                        chosen_target: Some(chosen),
                        risk_overlay: None,
                    };
                }
                _ => {}
            }
        }

        let min_delta = (min_replace_ticks as f64).max(1.0) * tick;
        let action = if (chosen - round_tick(order.price, tick)).abs() < min_delta - 1e-12 {
            DecisionAction::Keep
        } else {
            DecisionAction::Replace { new_price: chosen }
        };
        PricingDecision {
            action,
            reason: "custom_coarse_replace_exact_offset_from_mid".into(),
            mode: PricingMode::Custom,
            regime: TickRegime::Coarse,
            candidate_prices: candidates,
            chosen_target: Some(chosen),
            risk_overlay: None,
        }
    }

    fn fine_custom(&self, order: &Order, mid: f64, tick: f64, delta: f64, settings: &CustomPricingSettings, min_replace_ticks: u32) -> PricingDecision {
        let dr = (order.price - mid).abs() / delta.max(1e-12);
        let mut smin = settings.fine_safe_band_min;
        let mut smax = settings.fine_safe_band_max;
        if smin > smax {
            std::mem::swap(&mut smin, &mut smax);
        }
        if (smin - 1e-12..=smax + 1e-12).contains(&dr) {
            return PricingDecision {
                action: DecisionAction::Keep,
                reason: "custom_fine_keep_in_safe_band".into(),
                mode: PricingMode::Custom,
                regime: TickRegime::Fine,
                candidate_prices: vec![],
                chosen_target: Some(order.price),
                risk_overlay: None,
            };
        }
        let tr = settings.fine_target_band_ratio.clamp(0.0, 1.0);
        let raw = match order.side {
            Side::Buy => mid - tr * delta,
            Side::Sell => mid + tr * delta,
        };
        let target = round_tick(raw, tick);
        let min_delta = (min_replace_ticks as f64).max(1.0) * tick;
        let action = if (target - order.price).abs() < min_delta {
            DecisionAction::Keep
        } else {
            DecisionAction::Replace { new_price: target }
        };
        PricingDecision {
            action,
            reason: "custom_fine_move_toward_target_ratio".into(),
            mode: PricingMode::Custom,
            regime: TickRegime::Fine,
            candidate_prices: vec![],
            chosen_target: Some(target),
            risk_overlay: None,
        }
    }

    fn apply_reprice_speed_limit(&self, order: &Order, tick: f64, decision: PricingDecision, cfg: &Config) -> PricingDecision {
        let DecisionAction::Replace { new_price } = decision.action else {
            return decision;
        };
        let max_ticks = cfg.anti_sniping_max_reprice_ticks_per_update.max(1) as f64;
        let max_abs_delta = max_ticks * tick.max(1e-12);
        let diff = new_price - order.price;
        if diff.abs() <= max_abs_delta + 1e-12 {
            return PricingDecision {
                action: DecisionAction::Replace { new_price },
                ..decision
            };
        }
        let limited = if diff.is_sign_positive() {
            order.price + max_abs_delta
        } else {
            order.price - max_abs_delta
        };
        PricingDecision {
            action: DecisionAction::Replace {
                new_price: round_tick(limited, tick),
            },
            reason: format!("{}_rate_limited", decision.reason),
            ..decision
        }
    }

    pub fn decide(
        &mut self,
        order: &Order,
        book: &OrderBookSnapshot,
        midpoint_fallback: Option<f64>,
        rewards_delta: f64,
        use_custom: bool,
        custom_settings: Option<&CustomPricingSettings>,
        risk: Option<RiskSnapshot>,
        cfg: &Config,
    ) -> PricingDecision {
        let tick = book.tick_size.max(1e-12);
        let Some(raw_mid) = book.mid().or(midpoint_fallback) else {
            return PricingDecision {
                action: DecisionAction::Keep,
                reason: "missing_midpoint".into(),
                mode: if use_custom { PricingMode::Custom } else { PricingMode::Default },
                regime: classify_tick_regime(tick),
                candidate_prices: vec![],
                chosen_target: None,
                risk_overlay: risk,
            };
        };
        let (mid, _paused) = self.filtered_midpoint(&order.token_id, raw_mid, cfg);

        let regime = classify_tick_regime(tick);
        let mut decision = if use_custom {
            match regime {
                TickRegime::Coarse => self.coarse_custom(
                    order,
                    mid,
                    tick,
                    rewards_delta,
                    book,
                    custom_settings.unwrap_or(&CustomPricingSettings {
                        coarse_tick_offset_from_mid: 1,
                        coarse_allow_top_of_book: true,
                        coarse_min_candidate_levels: 1,
                        fine_safe_band_min: 0.4,
                        fine_safe_band_max: 0.6,
                        fine_target_band_ratio: 0.5,
                    }),
                    cfg.min_replace_ticks,
                ),
                TickRegime::Fine => self.fine_custom(
                    order,
                    mid,
                    tick,
                    rewards_delta,
                    custom_settings.unwrap_or(&CustomPricingSettings {
                        coarse_tick_offset_from_mid: 1,
                        coarse_allow_top_of_book: true,
                        coarse_min_candidate_levels: 1,
                        fine_safe_band_min: 0.4,
                        fine_safe_band_max: 0.6,
                        fine_target_band_ratio: 0.5,
                    }),
                    cfg.min_replace_ticks,
                ),
                TickRegime::Unsupported => PricingDecision {
                    action: DecisionAction::Keep,
                    reason: "unsupported_tick_keep".into(),
                    mode: PricingMode::Custom,
                    regime,
                    candidate_prices: vec![],
                    chosen_target: None,
                    risk_overlay: None,
                },
            }
        } else {
            match regime {
                TickRegime::Coarse => self.coarse_default(order, mid, tick, rewards_delta, book, cfg.min_replace_ticks),
                TickRegime::Fine => self.fine_default(order, mid, tick, rewards_delta, cfg.min_replace_ticks),
                TickRegime::Unsupported => PricingDecision {
                    action: DecisionAction::Keep,
                    reason: "unsupported_tick_keep".into(),
                    mode: PricingMode::Default,
                    regime,
                    candidate_prices: vec![],
                    chosen_target: None,
                    risk_overlay: None,
                },
            }
        };

        decision = self.apply_reprice_speed_limit(order, tick, decision, cfg);
        decision.risk_overlay = risk;
        decision
    }
}
