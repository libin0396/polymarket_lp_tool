use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "UPPERCASE")]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn as_str(self) -> &'static str {
        match self {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: String,
    pub token_id: String,
    pub condition_id: String,
    pub side: Side,
    pub price: f64,
    pub size: f64,
    pub original_size: f64,
    pub size_matched: f64,
    pub market_title: Option<String>,
    pub outcome: Option<String>,
    pub updated_at: DateTime<Utc>,
}

impl Order {
    pub fn remaining_size(&self) -> f64 {
        if self.size > 0.0 {
            self.size
        } else {
            (self.original_size - self.size_matched).max(0.0)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookLevel {
    pub price: f64,
    pub size: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBookSnapshot {
    pub token_id: String,
    pub best_bid: Option<f64>,
    pub best_ask: Option<f64>,
    pub tick_size: f64,
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
    pub source: String,
    pub updated_at: DateTime<Utc>,
}

impl OrderBookSnapshot {
    pub fn mid(&self) -> Option<f64> {
        match (self.best_bid, self.best_ask) {
            (Some(b), Some(a)) => Some((a + b) * 0.5),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum TickRegime {
    Coarse,
    Fine,
    Unsupported,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomPricingSettings {
    pub coarse_tick_offset_from_mid: usize,
    pub coarse_allow_top_of_book: bool,
    pub coarse_min_candidate_levels: usize,
    pub fine_safe_band_min: f64,
    pub fine_safe_band_max: f64,
    pub fine_target_band_ratio: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PricingMode {
    Default,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DecisionAction {
    Keep,
    Cancel,
    Replace { new_price: f64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingDecision {
    pub action: DecisionAction,
    pub reason: String,
    pub mode: PricingMode,
    pub regime: TickRegime,
    pub candidate_prices: Vec<f64>,
    pub chosen_target: Option<f64>,
    pub risk_overlay: Option<RiskSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RiskSnapshot {
    pub fill_rate: f64,
    pub short_window_fills: usize,
    pub long_window_fills: usize,
    pub fill_risk_score: f64,
    pub depth_total_in_band: f64,
    pub depth_closer_than_order: f64,
    pub inner_band_depth_ratio: f64,
    pub scoring: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewardRange {
    pub mid: f64,
    pub delta: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomRule {
    pub token_id: String,
    pub side: Side,
    pub tick_regime: TickRegime,
    pub settings: CustomPricingSettings,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CooldownState {
    pub dangerous_until: BTreeMap<String, DateTime<Utc>>,
    pub last_replace_mid: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyState {
    pub token_id: String,
    pub side: Side,
    pub filtered_mid: f64,
    pub raw_mid: f64,
    pub last_reprice_at: Option<DateTime<Utc>>,
    pub recent_mids: VecDeque<f64>,
    pub midpoint_pause_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EngineEvent {
    MarketBook(OrderBookSnapshot),
    UserOrderUpdate(Order),
    Fill {
        token_id: String,
        side: Side,
        order_id: String,
        fill_price: f64,
        fill_size: f64,
        ts: DateTime<Utc>,
    },
    ScoringUpdate {
        order_id: String,
        scoring: bool,
        ts: DateTime<Utc>,
    },
    UpsertCustomRule {
        token_id: String,
        side: Side,
        tick_regime: TickRegime,
        settings: CustomPricingSettings,
    },
    Tick,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRequest {
    pub order_id: String,
    pub token_id: String,
    pub side: Side,
    pub old_price: f64,
    pub size: f64,
    pub decision: PricingDecision,
    pub ts: DateTime<Utc>,
}
