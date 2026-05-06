use crate::custom_rules::CustomRulesStore;
use crate::models::{CustomPricingSettings, CustomRule, Order, Side, TickRegime};
use anyhow::Result;
use chrono::Utc;
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::warn;

#[derive(Clone)]
pub struct Telegram {
    pub enabled: bool,
    pub bot_token: String,
    pub chat_id: String,
    pub http: Client,
}

impl Telegram {
    pub async fn send(&self, text: &str) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.bot_token);
        self.http
            .post(url)
            .json(&serde_json::json!({
                "chat_id": self.chat_id,
                "text": text,
                "parse_mode": "Markdown",
            }))
            .send()
            .await?;
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct TgUpdatesResponse {
    ok: bool,
    result: Vec<TgUpdate>,
}

#[derive(Debug, Deserialize)]
struct TgUpdate {
    update_id: i64,
    message: Option<TgMessage>,
}

#[derive(Debug, Deserialize)]
struct TgMessage {
    text: Option<String>,
    chat: TgChat,
}

#[derive(Debug, Deserialize)]
struct TgChat {
    id: i64,
}

fn parse_side(v: &str) -> Option<Side> {
    match v.to_ascii_uppercase().as_str() {
        "BUY" => Some(Side::Buy),
        "SELL" => Some(Side::Sell),
        _ => None,
    }
}

pub async fn run_command_poller(
    tg: Telegram,
    rules: Arc<RwLock<CustomRulesStore>>,
    open_orders: Arc<RwLock<Vec<Order>>>,
    default_settings: CustomPricingSettings,
) -> Result<()> {
    if !tg.enabled {
        return Ok(());
    }
    let mut offset: i64 = 0;
    let mut sessions: HashMap<i64, RuleSession> = HashMap::new();
    loop {
        let url = format!(
            "https://api.telegram.org/bot{}/getUpdates?timeout=25&offset={}",
            tg.bot_token, offset
        );
        let resp = tg.http.get(url).send().await?;
        let payload = resp
            .json::<TgUpdatesResponse>()
            .await
            .unwrap_or(TgUpdatesResponse {
                ok: false,
                result: vec![],
            });
        if !payload.ok {
            continue;
        }

        for u in payload.result {
            offset = offset.max(u.update_id + 1);
            let Some(msg) = u.message else {
                continue;
            };
            if msg.chat.id.to_string() != tg.chat_id {
                continue;
            }
            let Some(text) = msg.text else {
                continue;
            };
            let cmd = text.trim();

            if cmd.starts_with("/input ") {
                let input = cmd.trim_start_matches("/input ").trim();
                let reply = if let Some(sess) = sessions.get_mut(&msg.chat.id) {
                    apply_session_input(sess, input)
                } else {
                    "No active setup. Use /set_rule <order_id>.".to_string()
                };
                if reply == "CANCELLED" {
                    sessions.remove(&msg.chat.id);
                    let _ = tg.send("Rule setup cancelled.").await;
                    continue;
                }
                if reply == "SAVE_COARSE" || reply == "SAVE_FINE" {
                    if let Some(sess) = sessions.remove(&msg.chat.id) {
                        let settings = CustomPricingSettings {
                            coarse_tick_offset_from_mid: sess.coarse_offset,
                            coarse_allow_top_of_book: sess.coarse_allow_top,
                            coarse_min_candidate_levels: sess.coarse_min_candidates,
                            fine_safe_band_min: sess.fine_safe_min,
                            fine_safe_band_max: sess.fine_safe_max,
                            fine_target_band_ratio: sess.fine_target_ratio,
                        };
                        rules
                            .write()
                            .await
                            .upsert(sess.token_id.clone(), sess.side, sess.regime, settings);
                        let _ = tg
                            .send(&format!(
                                "Saved rule token={} side={} regime={:?}",
                                sess.token_id,
                                sess.side.as_str(),
                                sess.regime
                            ))
                            .await;
                    }
                    continue;
                }
                let _ = tg.send(&reply).await;
                continue;
            }

            if cmd == "/status" {
                let orders = open_orders.read().await;
                let _ = tg
                    .send(&format!("Running. open_orders={}", orders.len()))
                    .await;
            } else if cmd == "/orders" {
                let orders = open_orders.read().await;
                let body = orders
                    .iter()
                    .take(20)
                    .map(|o| format!("{} {} {} @ {}", o.id, o.token_id, o.side.as_str(), o.price))
                    .collect::<Vec<_>>()
                    .join("\n");
                let _ = tg.send(&format!("Open orders:\n{}", body)).await;
            } else if cmd == "/pnl" {
                let _ = tg.send("PNL reporting wired; populate account snapshot feed.").await;
            } else if cmd.starts_with("/set_rule ") {
                let parts = cmd.split_whitespace().collect::<Vec<_>>();
                if parts.len() == 2 {
                    let oid = parts[1];
                    let orders = open_orders.read().await;
                    let Some(order) = orders.iter().find(|o| o.id == oid).cloned() else {
                        let _ = tg.send("Order id not found in current open orders.").await;
                        continue;
                    };
                    let regime = if (order.price * 100.0 - (order.price * 100.0).round()).abs() > 1e-7 {
                        TickRegime::Fine
                    } else {
                        TickRegime::Coarse
                    };
                    sessions.insert(
                        msg.chat.id,
                        RuleSession::new(order.token_id.clone(), order.side, regime, &default_settings),
                    );
                    let prompt = if regime == TickRegime::Coarse {
                        format!(
                            "Coarse setup for order={} token={} side={}\nStep 1/4: `/input <N>` tick_offset_from_mid (>=1)",
                            order.id, order.token_id, order.side.as_str()
                        )
                    } else {
                        format!(
                            "Fine setup for order={} token={} side={}\nStep 1/4: `/input <safe_min>` in [0,1]",
                            order.id, order.token_id, order.side.as_str()
                        )
                    };
                    let _ = tg.send(&prompt).await;
                } else {
                    let _ = tg.send("Usage: /set_rule <order_id>").await;
                }
            } else if cmd.starts_with("/get_rule ") {
                let parts = cmd.split_whitespace().collect::<Vec<_>>();
                if parts.len() != 3 {
                    let _ = tg.send("Usage: /get_rule <token_id> <BUY|SELL>").await;
                    continue;
                }
                let token = parts[1];
                let side = parse_side(parts[2]).unwrap_or(Side::Buy);
                let rule = rules.read().await.get(token, side).cloned();
                if let Some(CustomRule { tick_regime, settings, .. }) = rule {
                    let _ = tg
                        .send(&format!(
                            "rule {} {} regime={:?} coarse_n={} safe=[{},{}] target={}",
                            token,
                            side.as_str(),
                            tick_regime,
                            settings.coarse_tick_offset_from_mid,
                            settings.fine_safe_band_min,
                            settings.fine_safe_band_max,
                            settings.fine_target_band_ratio
                        ))
                        .await;
                } else {
                    let _ = tg.send("no rule").await;
                }
            } else if cmd.starts_with("/clear_rule ") {
                let parts = cmd.split_whitespace().collect::<Vec<_>>();
                if parts.len() != 3 {
                    let _ = tg.send("Usage: /clear_rule <token_id> <BUY|SELL>").await;
                    continue;
                }
                let token = parts[1];
                let side = parse_side(parts[2]).unwrap_or(Side::Buy);
                let removed = rules.write().await.clear(token, side);
                let _ = tg.send(if removed { "rule cleared" } else { "rule missing" }).await;
            } else if cmd == "/list_rules" {
                let rules_now = rules.read().await.list();
                let lines = rules_now
                    .iter()
                    .map(|r| format!("{} {} {:?}", r.token_id, r.side.as_str(), r.tick_regime))
                    .collect::<Vec<_>>()
                    .join("\n");
                let _ = tg.send(if lines.is_empty() { "no rules" } else { &lines }).await;
            } else if cmd == "/help" || cmd == "/start" {
                let _ = tg
                    .send(
                        "/status\n/orders\n/pnl\n/set_rule <order_id>\n/input <value>\n/get_rule\n/clear_rule\n/list_rules\n",
                    )
                    .await;
            } else {
                warn!("unknown telegram command: {}", cmd);
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
}

pub fn startup_summary(label: &str, open_orders: usize) -> String {
    format!(
        "*{}* startup summary\nopen_orders={}\nts={}",
        label,
        open_orders,
        Utc::now()
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionStep {
    CoarseOffset,
    CoarseAllowTop,
    CoarseMinCandidates,
    CoarseConfirm,
    FineSafeMin,
    FineSafeMax,
    FineTarget,
    FineConfirm,
}

#[derive(Debug, Clone)]
struct RuleSession {
    token_id: String,
    side: Side,
    regime: TickRegime,
    step: SessionStep,
    coarse_offset: usize,
    coarse_allow_top: bool,
    coarse_min_candidates: usize,
    fine_safe_min: f64,
    fine_safe_max: f64,
    fine_target_ratio: f64,
}

impl RuleSession {
    fn new(token_id: String, side: Side, regime: TickRegime, defaults: &CustomPricingSettings) -> Self {
        let step = if regime == TickRegime::Coarse {
            SessionStep::CoarseOffset
        } else {
            SessionStep::FineSafeMin
        };
        Self {
            token_id,
            side,
            regime,
            step,
            coarse_offset: defaults.coarse_tick_offset_from_mid,
            coarse_allow_top: defaults.coarse_allow_top_of_book,
            coarse_min_candidates: defaults.coarse_min_candidate_levels,
            fine_safe_min: defaults.fine_safe_band_min,
            fine_safe_max: defaults.fine_safe_band_max,
            fine_target_ratio: defaults.fine_target_band_ratio,
        }
    }
}

fn apply_session_input(sess: &mut RuleSession, input: &str) -> String {
    let low = input.trim().to_ascii_lowercase();
    if low == "cancel" {
        return "CANCELLED".to_string();
    }
    match sess.step {
        SessionStep::CoarseOffset => {
            let Ok(n) = input.parse::<usize>() else {
                return "Step 1/4 invalid. `/input <N>` where N >= 1".into();
            };
            if n < 1 {
                return "N must be >= 1".into();
            }
            sess.coarse_offset = n;
            sess.step = SessionStep::CoarseAllowTop;
            "Step 2/4: `/input yes` or `/input no` for allow_top_of_book".into()
        }
        SessionStep::CoarseAllowTop => {
            match low.as_str() {
                "yes" | "y" | "true" | "1" => sess.coarse_allow_top = true,
                "no" | "n" | "false" | "0" => sess.coarse_allow_top = false,
                _ => return "Step 2/4 invalid. use yes/no".into(),
            }
            sess.step = SessionStep::CoarseMinCandidates;
            "Step 3/4: `/input <min_candidate_levels>` integer >= 1".into()
        }
        SessionStep::CoarseMinCandidates => {
            let Ok(v) = input.parse::<usize>() else {
                return "Step 3/4 invalid integer".into();
            };
            if v < 1 {
                return "min_candidate_levels must be >= 1".into();
            }
            sess.coarse_min_candidates = v;
            sess.step = SessionStep::CoarseConfirm;
            format!(
                "Step 4/4 confirm with `/input confirm`\nN={}\nallow_top={}\nmin_candidates={}",
                sess.coarse_offset, sess.coarse_allow_top, sess.coarse_min_candidates
            )
        }
        SessionStep::CoarseConfirm => {
            if low == "confirm" {
                "SAVE_COARSE".into()
            } else {
                "Use `/input confirm` or `/input cancel`".into()
            }
        }
        SessionStep::FineSafeMin => {
            let Ok(v) = input.parse::<f64>() else {
                return "Step 1/4 invalid. use number in [0,1]".into();
            };
            if !(0.0..=1.0).contains(&v) {
                return "safe_min out of range [0,1]".into();
            }
            sess.fine_safe_min = v;
            sess.step = SessionStep::FineSafeMax;
            "Step 2/4: `/input <safe_max>` in [0,1] and > safe_min".into()
        }
        SessionStep::FineSafeMax => {
            let Ok(v) = input.parse::<f64>() else {
                return "Step 2/4 invalid number".into();
            };
            if !(0.0..=1.0).contains(&v) || v <= sess.fine_safe_min {
                return "safe_max must be > safe_min and <= 1".into();
            }
            sess.fine_safe_max = v;
            sess.step = SessionStep::FineTarget;
            "Step 3/4: `/input <target_ratio>` in [0,1]".into()
        }
        SessionStep::FineTarget => {
            let Ok(v) = input.parse::<f64>() else {
                return "Step 3/4 invalid number".into();
            };
            if !(0.0..=1.0).contains(&v) {
                return "target ratio out of range [0,1]".into();
            }
            sess.fine_target_ratio = v;
            sess.step = SessionStep::FineConfirm;
            format!(
                "Step 4/4 confirm with `/input confirm`\nsafe=[{},{}] target={}",
                sess.fine_safe_min, sess.fine_safe_max, sess.fine_target_ratio
            )
        }
        SessionStep::FineConfirm => {
            if low == "confirm" {
                "SAVE_FINE".into()
            } else {
                "Use `/input confirm` or `/input cancel`".into()
            }
        }
    }
}
