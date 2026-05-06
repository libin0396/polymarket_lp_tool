mod config;
mod custom_rules;
mod dashboard;
mod execution_engine;
mod market_resolver;
mod models;
mod orderbook;
mod persistence;
mod polymarket_api;
mod pricing_engine;
mod rewards;
mod risk_monitor;
mod telegram;
mod websocket;
mod tui;

use anyhow::Result;
use chrono::Utc;
use config::load_config;
use custom_rules::CustomRulesStore;
use dashboard::{read_memory_mb_from_proc, run_dashboard_server, DashboardStateHandle, OrderDashboardRow};
use execution_engine::ExecutionEngine;
use market_resolver::MarketResolver;
use models::{CustomPricingSettings, EngineEvent, ExecutionRequest, Order, PricingMode};
use persistence::{load_state, save_state, PersistedState};
use polymarket_api::PolymarketApi;
use pricing_engine::PricingEngine;
use futures::stream::{FuturesUnordered, StreamExt};
use reqwest::Client;
use rewards::RewardsClient;
use risk_monitor::{FillRow, RiskMonitor};
use std::collections::BTreeMap;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};
use tokio::sync::{mpsc, RwLock};
use tui::run_tui;
use tracing::{error, info, warn};

fn classify_tick_regime_label(tick: f64) -> &'static str {
    if (tick - 0.01).abs() < 1e-9 || (tick - 1.0).abs() < 1e-6 {
        "coarse"
    } else if (tick - 0.001).abs() < 1e-12 || (tick - 0.1).abs() < 1e-6 {
        "fine"
    } else {
        "unsupported"
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,reqwest=warn,tungstenite=warn".into()),
        )
        .json()
        .init();

    let cfg = load_config()?;
    info!(
        "startup config loaded host={} chain_id={} ws_market={} ws_user={} telegram_enabled={} post_only={} loop_interval_ms={}",
        cfg.clob_http_url,
        cfg.chain_id,
        cfg.ws_market_url,
        cfg.ws_user_url,
        cfg.telegram_enabled,
        cfg.post_only,
        cfg.loop_interval_ms
    );
    info!(
        "dashboard enabled={} bind={}",
        cfg.dashboard_enabled,
        cfg.dashboard_bind
    );
    info!(
        "ui_mode={} dashboard_auto_open={}",
        cfg.ui_mode, cfg.dashboard_auto_open
    );
    let http = Client::new();
    let api = PolymarketApi::new(
        http.clone(),
        cfg.clob_http_url.clone(),
        cfg.api_key.clone(),
        cfg.api_secret.clone(),
        cfg.api_passphrase.clone(),
        cfg.private_key.clone(),
        cfg.signature_type,
        cfg.signer_address.clone(),
        cfg.funder.clone(),
        cfg.chain_id,
    );
    let rewards = RewardsClient::new(http.clone(), cfg.clob_http_url.clone());
    let market_resolver = Arc::new(MarketResolver::new());

    let persisted = load_state(&cfg.state_path).await.unwrap_or_default();
    let rules_store = Arc::new(RwLock::new(CustomRulesStore::from_rules(
        persisted.custom_rules.clone(),
    )));

    let default_custom = CustomPricingSettings {
        coarse_tick_offset_from_mid: cfg.custom_coarse_tick_offset,
        coarse_allow_top_of_book: cfg.custom_coarse_allow_top_of_book,
        coarse_min_candidate_levels: cfg.custom_coarse_min_candidates,
        fine_safe_band_min: cfg.custom_fine_safe_min,
        fine_safe_band_max: cfg.custom_fine_safe_max,
        fine_target_band_ratio: cfg.custom_fine_target_ratio,
    };

    let open_orders = Arc::new(RwLock::new(Vec::<Order>::new()));
    let dashboard = DashboardStateHandle::new();
    let (tx, mut rx) = mpsc::channel::<EngineEvent>(10_000);

    let ui_mode = cfg.ui_mode.to_ascii_lowercase();
    if ui_mode == "web" && cfg.dashboard_enabled {
        let dash_bind = cfg.dashboard_bind.clone();
        let dash_bind_for_open = cfg.dashboard_bind.clone();
        let dash_auto_open = cfg.dashboard_auto_open;
        let dash_state = dashboard.clone();
        let dash_tx = tx.clone();
        tokio::spawn(async move {
            run_dashboard_server(dash_bind, dash_state, dash_tx).await;
        });
        if dash_auto_open {
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                let url = format!("http://{}", dash_bind_for_open);
                let mut opened = false;
                if std::env::var("DISPLAY").is_ok() || std::env::var("WAYLAND_DISPLAY").is_ok() {
                    if std::process::Command::new("xdg-open")
                        .arg(&url)
                        .spawn()
                        .is_ok()
                    {
                        opened = true;
                    }
                }
                if !opened {
                    info!("dashboard url: {}", url);
                }
            });
        } else {
            info!("dashboard url: http://{}", cfg.dashboard_bind);
        }
    } else if ui_mode == "tui" {
        let dash_state = dashboard.clone();
        tokio::spawn(async move {
            if let Err(err) = run_tui(dash_state).await {
                warn!("tui stopped err={}", err);
            }
        });
    }

    let dash_state_mem = dashboard.clone();
    tokio::spawn(async move {
        loop {
            if let Some((used_mb, total_mb)) = read_memory_mb_from_proc() {
                dash_state_mem.set_server_memory(used_mb, total_mb).await;
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });

    let dash_state_latency = dashboard.clone();
    let latency_host = cfg.clob_http_url.clone();
    let latency_http = http.clone();
    tokio::spawn(async move {
        loop {
            let start = std::time::Instant::now();
            let result = latency_http
                .get(format!("{}/time", latency_host))
                .timeout(std::time::Duration::from_secs(3))
                .send()
                .await;
            let latency = if result.is_ok() {
                Some(start.elapsed().as_millis())
            } else {
                None
            };
            dash_state_latency.set_clob_latency(latency).await;
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
    });

    let tg = telegram::Telegram {
        enabled: cfg.telegram_enabled,
        bot_token: cfg.telegram_bot_token.clone(),
        chat_id: cfg.telegram_chat_id.clone(),
        http: http.clone(),
    };

    let market_tx = tx.clone();
    let user_tx = tx.clone();

    let ws_tokens = Arc::new(RwLock::new(Vec::<String>::new()));
    let heartbeat_orders = open_orders.clone();
    let heartbeat_tokens = ws_tokens.clone();
    let heartbeat_telegram_enabled = cfg.telegram_enabled;
    tokio::spawn(async move {
        loop {
            let orders_n = heartbeat_orders.read().await.len();
            let tokens_n = heartbeat_tokens.read().await.len();
            info!(
                "heartbeat running=1 open_orders={} subscribed_tokens={} telegram_enabled={}",
                orders_n,
                tokens_n,
                heartbeat_telegram_enabled
            );
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    });

    let ws_tokens_for_market = ws_tokens.clone();
    let ws_market_url = cfg.ws_market_url.clone();
    tokio::spawn(async move {
        loop {
            let tokens = ws_tokens_for_market.read().await.clone();
            if tokens.is_empty() {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
            if let Err(err) = websocket::run_market_ws(&ws_market_url, tokens, market_tx.clone()).await {
                warn!("market websocket reconnect after error: {}", err);
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    });

    let ws_user_url = cfg.ws_user_url.clone();
    let api_key = cfg.api_key.clone();
    let api_secret = cfg.api_secret.clone();
    let api_passphrase = cfg.api_passphrase.clone();
    let user_markets_ref = open_orders.clone();
    tokio::spawn(async move {
        loop {
            let markets = user_markets_ref
                .read()
                .await
                .iter()
                .map(|o| o.condition_id.clone())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            if markets.is_empty() {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
            if let Err(err) =
                websocket::run_user_ws(&ws_user_url, &api_key, &api_secret, &api_passphrase, markets, user_tx.clone()).await
            {
                warn!("user websocket reconnect after error: {}", err);
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    });

    let order_poll_api = api.clone();
    let orders_ref = open_orders.clone();
    let ws_tokens_ref = ws_tokens.clone();
    let dashboard_poll = dashboard.clone();
    let poll_tx = tx.clone();
    let poll_ms = cfg.loop_interval_ms;
    tokio::spawn(async move {
        let mut rest_bootstrapped_tokens = HashSet::<String>::new();
        loop {
            match order_poll_api.get_open_orders().await {
                Ok(orders) => {
                    dashboard_poll.set_order_poll_ok(orders.len()).await;
                    {
                        let mut w = orders_ref.write().await;
                        *w = orders.clone();
                    }
                    let mut by_token = HashMap::<String, Order>::new();
                    for o in &orders {
                        by_token.entry(o.token_id.clone()).or_insert_with(|| o.clone());
                        let _ = poll_tx.send(EngineEvent::UserOrderUpdate(o.clone())).await;
                    }
                    let mut tokens = by_token.keys().cloned().collect::<Vec<_>>();
                    tokens.sort();
                    *ws_tokens_ref.write().await = tokens;

                    // REST /book bootstrap fallback:
                    // when a token is newly subscribed, fetch one snapshot immediately
                    // so strategy/dashboard don't wait too long for first WS book.
                    rest_bootstrapped_tokens.retain(|t| by_token.contains_key(t));
                    for token_id in by_token.keys() {
                        if rest_bootstrapped_tokens.contains(token_id) {
                            continue;
                        }
                        match order_poll_api.get_book_snapshot(token_id).await {
                            Ok(Some(book)) => {
                                let _ = poll_tx.send(EngineEvent::MarketBook(book)).await;
                            }
                            Ok(None) => {}
                            Err(err) => {
                                warn!("rest /book fallback failed token={} err={}", token_id, err);
                            }
                        }
                        rest_bootstrapped_tokens.insert(token_id.clone());
                    }
                }
                Err(err) => {
                    dashboard_poll
                        .set_order_poll_error(err.to_string())
                        .await;
                    warn!("open order polling failed: {}", err)
                }
            }
            let _ = poll_tx.send(EngineEvent::Tick).await;
            tokio::time::sleep(std::time::Duration::from_millis(poll_ms)).await;
        }
    });

    if tg.enabled {
        let _ = tg
            .send(&telegram::startup_summary("polymarket-rust-bot", open_orders.read().await.len()))
            .await;
        tokio::spawn(telegram::run_command_poller(
            tg.clone(),
            rules_store.clone(),
            open_orders.clone(),
            default_custom.clone(),
        ));
    }

    let mut pricing_engine = PricingEngine::new();
    let mut execution_engine = ExecutionEngine::default();
    for (token_id, until) in persisted.cooldown_state.dangerous_until {
        execution_engine.set_cooldown_until(token_id, until);
    }
    let mut risk_monitor = RiskMonitor::default();
    let mut order_books = HashMap::<String, models::OrderBookSnapshot>::new();
    let mut scoring_map = HashMap::<String, bool>::new();
    let mut midpoint_cache = HashMap::<String, f64>::new();
    let mut midpoint_cache_at = HashMap::<String, Instant>::new();
    let mut tick_cache = HashMap::<String, f64>::new();
    let mut tick_cache_at = HashMap::<String, Instant>::new();
    let mut order_next_due_at = HashMap::<String, Instant>::new();

    loop {
        let Some(event) = rx.recv().await else {
            break;
        };
        match event {
            EngineEvent::MarketBook(book) => {
                order_books.insert(book.token_id.clone(), book);
            }
            EngineEvent::UserOrderUpdate(order) => {
                scoring_map.entry(order.id.clone()).or_insert(false);
            }
            EngineEvent::Fill {
                token_id,
                side: _,
                order_id: _,
                fill_price,
                fill_size,
                ts,
            } => {
                execution_engine.mark_recent_fill(token_id.clone(), cfg.anti_sniping_fill_cooldown_ms);
                risk_monitor.on_fill(FillRow {
                    token_id,
                    fill_size,
                    fill_price,
                    ts,
                });
            }
            EngineEvent::ScoringUpdate { order_id, scoring, .. } => {
                scoring_map.insert(order_id, scoring);
            }
            EngineEvent::UpsertCustomRule {
                token_id,
                side,
                tick_regime,
                settings,
            } => {
                let mut rules = rules_store.write().await;
                rules.upsert(token_id, side, tick_regime, settings);
            }
            EngineEvent::Tick => {
                let orders = open_orders.read().await.clone();
                let alive: HashSet<String> = orders.iter().map(|o| o.id.clone()).collect();
                dashboard.retain_orders(&alive).await;
                order_next_due_at.retain(|oid, _| alive.contains(oid));

                // Parallel IO prefetch + cache:
                // midpoint (short TTL) and tick-size (long TTL) by unique token.
                let now = Instant::now();
                let unique_tokens = orders
                    .iter()
                    .map(|o| o.token_id.clone())
                    .collect::<HashSet<_>>();

                let mut mid_fetches = FuturesUnordered::new();
                for token in &unique_tokens {
                    let stale = midpoint_cache_at
                        .get(token)
                        .map(|ts| now.duration_since(*ts) > StdDuration::from_secs(2))
                        .unwrap_or(true);
                    if stale {
                        let api_cloned = api.clone();
                        let token_cloned = token.clone();
                        mid_fetches.push(async move {
                            (
                                token_cloned.clone(),
                                api_cloned.get_midpoint(&token_cloned).await.ok().flatten(),
                            )
                        });
                    }
                }
                while let Some((token, mid)) = mid_fetches.next().await {
                    if let Some(v) = mid {
                        midpoint_cache.insert(token.clone(), v);
                        midpoint_cache_at.insert(token, now);
                    }
                }

                let mut tick_fetches = FuturesUnordered::new();
                for token in &unique_tokens {
                    let stale = tick_cache_at
                        .get(token)
                        .map(|ts| now.duration_since(*ts) > StdDuration::from_secs(30))
                        .unwrap_or(true);
                    if stale {
                        let api_cloned = api.clone();
                        let token_cloned = token.clone();
                        tick_fetches.push(async move {
                            (
                                token_cloned.clone(),
                                api_cloned.get_tick_size(&token_cloned).await.ok(),
                            )
                        });
                    }
                }
                while let Some((token, tick)) = tick_fetches.next().await {
                    if let Some(v) = tick {
                        tick_cache.insert(token.clone(), v);
                        tick_cache_at.insert(token, now);
                    }
                }

                // Rewards: one fetch per unique condition id (RewardsClient has its own cache).
                let mut rewards_spread_map = HashMap::<String, f64>::new();
                let unique_conditions = orders
                    .iter()
                    .map(|o| o.condition_id.clone())
                    .collect::<HashSet<_>>();
                for condition_id in unique_conditions {
                    let spread = rewards
                        .rewards_max_spread_for_market(&condition_id)
                        .await
                        .unwrap_or(0.0);
                    rewards_spread_map.insert(condition_id, spread);
                }

                for order in &orders {
                    // Layered frequency:
                    // - with orderbook: check at main poll interval
                    // - without orderbook: slower checks to avoid IO storm
                    let has_book = order_books.contains_key(&order.token_id);
                    let due_interval = if has_book {
                        StdDuration::from_millis(poll_ms)
                    } else {
                        StdDuration::from_secs(4)
                    };
                    if let Some(next_due) = order_next_due_at.get(&order.id).copied() {
                        if now < next_due {
                            continue;
                        }
                    }
                    order_next_due_at.insert(order.id.clone(), now + due_interval);

                    let mut market_title = order
                        .market_title
                        .clone()
                        .unwrap_or_else(|| "".to_string());
                    let mut outcome_label = order.outcome.clone().unwrap_or_else(|| "".to_string());
                    if market_title.trim().is_empty() || market_title == "Unknown Market" {
                        if let Ok((q, o)) = market_resolver
                            .lookup(
                                &http,
                                &cfg.gamma_api_host,
                                &order.condition_id,
                                &order.token_id,
                            )
                            .await
                        {
                            if !q.trim().is_empty() {
                                market_title = q;
                            }
                            if !o.trim().is_empty() {
                                outcome_label = o;
                            }
                        }
                    }

                    let rewards_spread = *rewards_spread_map.get(&order.condition_id).unwrap_or(&0.0);
                    let provisional_mid = if let Some(m) = midpoint_cache.get(&order.token_id).copied() {
                        Some(m)
                    } else {
                        None
                    };
                    let provisional_tick = if let Some(book) = order_books.get(&order.token_id) {
                        Some(book.tick_size)
                    } else {
                        tick_cache.get(&order.token_id).copied().or(Some(0.01))
                    };
                    let (provisional_lo, provisional_hi) = if let Some(mid) = provisional_mid {
                        let delta = rewards::RewardsClient::reward_range(mid, rewards_spread)
                            .delta
                            .max(1e-9);
                        match order.side {
                            models::Side::Buy => (Some(mid - delta), Some(mid)),
                            models::Side::Sell => (Some(mid), Some(mid + delta)),
                        }
                    } else {
                        (None, None)
                    };

                    // Always show polled orders in UI first. If book is missing, use
                    // midpoint/tick fallback and mark fields as provisional.
                    dashboard
                        .upsert_order_row(OrderDashboardRow {
                            order_id: order.id.clone(),
                            token_id: order.token_id.clone(),
                            market_title: if market_title.trim().is_empty() {
                                "Unknown Market".to_string()
                            } else {
                                market_title.clone()
                            },
                            outcome_label: if outcome_label.trim().is_empty() {
                                "-".to_string()
                            } else {
                                outcome_label.clone()
                            },
                            side: order.side.as_str().to_string(),
                            order_price: order.price,
                            size: order.remaining_size(),
                            mid_price: provisional_mid,
                            reward_range_lo: provisional_lo,
                            reward_range_hi: provisional_hi,
                            reward_tick_size: provisional_tick,
                            pricing_mode: "provisional".to_string(),
                            pricing_rule: "provisional(midpoint+tick fallback)".to_string(),
                            tick_regime: provisional_tick
                                .map(classify_tick_regime_label)
                                .unwrap_or("pending")
                                .to_string(),
                            last_decision_reason: "waiting_for_orderbook".to_string(),
                            last_check_at: Some(Utc::now()),
                            last_candidate_levels: vec![],
                        })
                        .await;

                    let Some(book) = order_books.get(&order.token_id) else {
                        continue;
                    };
                    let mid_for_reward = if let Some(m) = book.mid() {
                        midpoint_cache.insert(order.token_id.clone(), m);
                        m
                    } else if let Ok(Some(m)) = api.get_midpoint(&order.token_id).await {
                        midpoint_cache.insert(order.token_id.clone(), m);
                        m
                    } else {
                        midpoint_cache
                            .get(&order.token_id)
                            .copied()
                            .unwrap_or(order.price)
                    };
                    let reward = rewards::RewardsClient::reward_range(mid_for_reward, rewards_spread);
                    let delta = reward.delta.max(1e-9);
                    let (raw_lo, raw_hi) = match order.side {
                        models::Side::Buy => (mid_for_reward - delta, mid_for_reward),
                        models::Side::Sell => (mid_for_reward, mid_for_reward + delta),
                    };
                    let side_levels = match order.side {
                        models::Side::Buy => &book.bids,
                        models::Side::Sell => &book.asks,
                    };
                    let clipped_prices = side_levels
                        .iter()
                        .filter(|lv| lv.size > 0.0)
                        .map(|lv| lv.price)
                        .filter(|p| *p >= raw_lo - 1e-12 && *p <= raw_hi + 1e-12)
                        .collect::<Vec<_>>();
                    let (reward_lo, reward_hi) = if clipped_prices.is_empty() {
                        (None, None)
                    } else {
                        let lo = clipped_prices
                            .iter()
                            .copied()
                            .min_by(f64::total_cmp)
                            .unwrap_or(raw_lo);
                        let hi = clipped_prices
                            .iter()
                            .copied()
                            .max_by(f64::total_cmp)
                            .unwrap_or(raw_hi);
                        (Some(lo), Some(hi))
                    };

                    let use_custom = {
                        let rules = rules_store.read().await;
                        rules.get(&order.token_id, order.side).is_some()
                            || cfg.default_custom_pricing
                            || cfg.custom_order_ids.iter().any(|id| id == &order.id)
                    };
                    let custom_settings = {
                        let rules = rules_store.read().await;
                        rules
                            .get(&order.token_id, order.side)
                            .map(|r| r.settings.clone())
                            .unwrap_or(default_custom.clone())
                    };
                    let scoring = *scoring_map.get(&order.id).unwrap_or(&false);
                    let risk = risk_monitor.snapshot(&order.token_id, order.side, scoring, 0.0, 0.0);
                    let mut decision = pricing_engine.decide(
                        order,
                        book,
                        Some(mid_for_reward),
                        delta,
                        use_custom,
                        Some(&custom_settings),
                        Some(risk.clone()),
                        &cfg,
                    );

                    if execution_engine.recently_filled(&order.token_id, cfg.anti_sniping_fill_cooldown_ms) {
                        if matches!(decision.action, models::DecisionAction::Replace { .. }) {
                            decision.action = models::DecisionAction::Keep;
                            decision.reason = format!("{}_recent_fill_cooldown", decision.reason);
                        }
                    }
                    if execution_engine.in_version_mismatch_cooldown(&order.token_id) {
                        if matches!(decision.action, models::DecisionAction::Replace { .. }) {
                            decision.action = models::DecisionAction::Keep;
                            decision.reason = "order_version_mismatch_cooldown".to_string();
                        }
                    }

                    if let models::DecisionAction::Replace { new_price } = decision.action {
                        info!(
                            "action=replace token={} side={} order_id={} old_price={} new_price={} reason={} mode={:?} risk={:?}",
                            order.token_id,
                            order.side.as_str(),
                            order.id,
                            order.price,
                            new_price,
                            decision.reason,
                            match decision.mode { PricingMode::Default => "default", PricingMode::Custom => "custom" },
                            decision.risk_overlay
                        );
                    }

                    let rule_summary = if use_custom {
                        format!(
                            "custom coarse_n={} allow_top={} min_cands={} safe=[{:.3},{:.3}] target={:.3}",
                            custom_settings.coarse_tick_offset_from_mid,
                            custom_settings.coarse_allow_top_of_book,
                            custom_settings.coarse_min_candidate_levels,
                            custom_settings.fine_safe_band_min,
                            custom_settings.fine_safe_band_max,
                            custom_settings.fine_target_band_ratio
                        )
                    } else {
                        "default(simple coarse/fine)".to_string()
                    };
                    let regime = match decision.regime {
                        models::TickRegime::Coarse => "coarse",
                        models::TickRegime::Fine => "fine",
                        models::TickRegime::Unsupported => "unsupported",
                    }
                    .to_string();
                    let mode = match decision.mode {
                        PricingMode::Default => "default",
                        PricingMode::Custom => "custom",
                    }
                    .to_string();
                    dashboard
                        .upsert_order_row(OrderDashboardRow {
                            order_id: order.id.clone(),
                            token_id: order.token_id.clone(),
                            market_title: if market_title.trim().is_empty() {
                                "Unknown Market".to_string()
                            } else {
                                market_title
                            },
                            outcome_label: if outcome_label.trim().is_empty() {
                                "-".to_string()
                            } else {
                                outcome_label
                            },
                            side: order.side.as_str().to_string(),
                            order_price: order.price,
                            size: order.remaining_size(),
                            mid_price: Some(mid_for_reward),
                            reward_range_lo: reward_lo,
                            reward_range_hi: reward_hi,
                            reward_tick_size: Some(book.tick_size),
                            pricing_mode: mode,
                            pricing_rule: rule_summary,
                            tick_regime: regime,
                            last_decision_reason: decision.reason.clone(),
                            last_check_at: Some(Utc::now()),
                            last_candidate_levels: decision.candidate_prices.clone(),
                        })
                        .await;

                    let req = ExecutionRequest {
                        order_id: order.id.clone(),
                        token_id: order.token_id.clone(),
                        side: order.side,
                        old_price: order.price,
                        size: order.remaining_size(),
                        decision,
                        ts: Utc::now(),
                    };
                    if let Err(err) = execution_engine.apply(&api, req, cfg.post_only).await {
                        error!("execution error token={} side={} order_id={} err={}", order.token_id, order.side.as_str(), order.id, err);
                    }
                }

                let persist = PersistedState {
                    custom_rules: rules_store.read().await.list(),
                    cooldown_state: models::CooldownState {
                        dangerous_until: execution_engine.cooldown_snapshot().into_iter().collect(),
                        last_replace_mid: BTreeMap::new(),
                    },
                    strategy_states: vec![],
                    account_snapshots: HashMap::new(),
                };
                if let Err(err) = save_state(&cfg.state_path, &persist).await {
                    warn!("state persistence failed: {}", err);
                }
            }
        }
    }
    Ok(())
}
