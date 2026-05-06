use crate::orderbook::{best_ask, best_bid, resolve_effective_tick_size};
use crate::models::{BookLevel, EngineEvent, Order, OrderBookSnapshot, Side};
use anyhow::Result;
use chrono::Utc;
use futures::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::mpsc::Sender;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, warn};

fn as_f64_loose(v: Option<&Value>) -> Option<f64> {
    let val = v?;
    if let Some(n) = val.as_f64() {
        return Some(n);
    }
    val.as_str().and_then(|s| s.parse::<f64>().ok())
}

fn parse_side(s: &str) -> Option<Side> {
    match s.to_ascii_uppercase().as_str() {
        "BUY" => Some(Side::Buy),
        "SELL" => Some(Side::Sell),
        _ => None,
    }
}

pub async fn run_market_ws(url: &str, token_ids: Vec<String>, tx: Sender<EngineEvent>) -> Result<()> {
    let (ws, _) = connect_async(url).await?;
    let (mut write, mut read) = ws.split();

    write
        .send(Message::Text(
            serde_json::json!({
                "type": "market",
                "assets_ids": token_ids,
                "custom_feature_enabled": true
            })
            .to_string(),
        ))
        .await?;
    info!("market websocket subscribed");

    while let Some(msg) = read.next().await {
        let msg = msg?;
        if msg.is_ping() {
            let _ = write.send(Message::Pong(vec![].into())).await;
            continue;
        }
        if !msg.is_text() {
            continue;
        }
        let text = msg.into_text()?;
        if text.trim().eq_ignore_ascii_case("PING") {
            let _ = write.send(Message::Text("PONG".to_string())).await;
            continue;
        }
        if text.trim().eq_ignore_ascii_case("PONG") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if v.get("event_type").and_then(Value::as_str) == Some("book") {
            let token_id = v.get("asset_id").and_then(Value::as_str).unwrap_or_default().to_string();
            if token_id.is_empty() {
                continue;
            }
            let bids = v
                .get("bids")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|x| {
                    Some(BookLevel {
                        price: as_f64_loose(x.get("price"))?,
                        size: as_f64_loose(x.get("size")).unwrap_or(0.0),
                    })
                })
                .collect::<Vec<_>>();
            let asks = v
                .get("asks")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|x| {
                    Some(BookLevel {
                        price: as_f64_loose(x.get("price"))?,
                        size: as_f64_loose(x.get("size")).unwrap_or(0.0),
                    })
                })
                .collect::<Vec<_>>();

            let best_bid = best_bid(&bids);
            let best_ask = best_ask(&asks);
            let api_tick = as_f64_loose(v.get("tick_size")).unwrap_or(0.01);
            let tick = resolve_effective_tick_size(api_tick, &bids, &asks);
            let event = EngineEvent::MarketBook(OrderBookSnapshot {
                token_id,
                best_bid,
                best_ask,
                tick_size: tick,
                bids,
                asks,
                source: "ws_market".into(),
                updated_at: Utc::now(),
            });
            if tx.send(event).await.is_err() {
                break;
            }
        }
    }
    Ok(())
}

pub async fn run_user_ws(
    url: &str,
    api_key: &str,
    api_secret: &str,
    api_passphrase: &str,
    markets: Vec<String>,
    tx: Sender<EngineEvent>,
) -> Result<()> {
    let (ws, _) = connect_async(url).await?;
    let (mut write, mut read) = ws.split();
    write
        .send(Message::Text(
            serde_json::json!({
                "type": "user",
                "auth": {
                    "apiKey": api_key,
                    "secret": api_secret,
                    "passphrase": api_passphrase
                },
                "markets": markets
            })
            .to_string(),
        ))
        .await?;
    info!("user websocket subscribed");

    while let Some(msg) = read.next().await {
        let msg = msg?;
        if msg.is_ping() {
            let _ = write.send(Message::Pong(vec![].into())).await;
            continue;
        }
        if !msg.is_text() {
            continue;
        }
        let text = msg.into_text()?;
        if text.trim().eq_ignore_ascii_case("PING") {
            let _ = write.send(Message::Text("PONG".to_string())).await;
            continue;
        }
        if text.trim().eq_ignore_ascii_case("PONG") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            continue;
        };

        match v.get("event_type").and_then(Value::as_str) {
            Some("order") => {
                let side = v
                    .get("side")
                    .and_then(Value::as_str)
                    .and_then(parse_side);
                if let Some(side) = side {
                    let order = Order {
                        id: v.get("id").and_then(Value::as_str).unwrap_or_default().to_string(),
                        token_id: v.get("asset_id").and_then(Value::as_str).unwrap_or_default().to_string(),
                        condition_id: v.get("market").and_then(Value::as_str).unwrap_or_default().to_string(),
                        side,
                        price: v.get("price").and_then(Value::as_f64).unwrap_or(0.0),
                        size: v.get("size").and_then(Value::as_f64).unwrap_or(0.0),
                        original_size: v.get("original_size").and_then(Value::as_f64).unwrap_or(0.0),
                        size_matched: v.get("size_matched").and_then(Value::as_f64).unwrap_or(0.0),
                        market_title: None,
                        outcome: None,
                        updated_at: Utc::now(),
                    };
                    let _ = tx.send(EngineEvent::UserOrderUpdate(order)).await;
                }
            }
            Some("trade") => {
                let order_id = v
                    .get("maker_orders")
                    .and_then(Value::as_array)
                    .and_then(|rows| rows.first())
                    .and_then(|x| x.get("order_id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if !order_id.is_empty() {
                    let side = v
                        .get("side")
                        .and_then(Value::as_str)
                        .and_then(parse_side)
                        .unwrap_or(Side::Buy);
                    let _ = tx
                        .send(EngineEvent::Fill {
                            token_id: v.get("asset_id").and_then(Value::as_str).unwrap_or_default().to_string(),
                            side,
                            order_id,
                            fill_price: v.get("price").and_then(Value::as_f64).unwrap_or(0.0),
                            fill_size: v.get("size").and_then(Value::as_f64).unwrap_or(0.0),
                            ts: Utc::now(),
                        })
                        .await;
                }
            }
            _ => {
                warn!("unhandled user ws event");
            }
        }
    }
    Ok(())
}
