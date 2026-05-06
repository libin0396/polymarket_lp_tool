use anyhow::{Context, Result};
use dotenvy::dotenv;
use k256::ecdsa::SigningKey;
use serde::{Deserialize, Serialize};
use sha3::{Digest, Keccak256};
use std::env;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub clob_http_url: String,
    pub gamma_api_host: String,
    pub ws_market_url: String,
    pub ws_user_url: String,
    pub ws_rtds_url: Option<String>,
    pub api_key: String,
    pub api_secret: String,
    pub api_passphrase: String,
    pub private_key: String,
    pub signature_type: u8,
    pub signer_address: String,
    pub funder: String,
    pub chain_id: u64,
    pub post_only: bool,
    pub loop_interval_ms: u64,
    pub min_replace_ticks: u32,
    pub custom_rules_path: PathBuf,
    pub state_path: PathBuf,
    pub account_snapshot_path: PathBuf,
    pub telegram_enabled: bool,
    pub telegram_bot_token: String,
    pub telegram_chat_id: String,
    pub default_custom_pricing: bool,
    pub custom_order_ids: Vec<String>,
    pub custom_coarse_tick_offset: usize,
    pub custom_coarse_allow_top_of_book: bool,
    pub custom_coarse_min_candidates: usize,
    pub custom_fine_safe_min: f64,
    pub custom_fine_safe_max: f64,
    pub custom_fine_target_ratio: f64,
    pub anti_sniping_mid_jump_threshold: f64,
    pub anti_sniping_pause_ms: u64,
    pub anti_sniping_stable_confirm_ms: u64,
    pub anti_sniping_ema_alpha: f64,
    pub anti_sniping_rolling_median_window: usize,
    pub anti_sniping_max_reprice_ticks_per_update: u32,
    pub anti_sniping_fill_cooldown_ms: u64,
    pub dashboard_enabled: bool,
    pub dashboard_bind: String,
    pub ui_mode: String,
    pub dashboard_auto_open: bool,
}

fn parse_bool(key: &str, default: bool) -> bool {
    env::var(key)
        .ok()
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(default)
}

fn parse_u64(key: &str, default: u64) -> u64 {
    env::var(key).ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(default)
}

fn parse_u32(key: &str, default: u32) -> u32 {
    env::var(key).ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or(default)
}

fn parse_usize(key: &str, default: usize) -> usize {
    env::var(key)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(default)
}

fn parse_f64(key: &str, default: f64) -> f64 {
    env::var(key).ok().and_then(|v| v.parse::<f64>().ok()).unwrap_or(default)
}

fn derive_eth_address_from_private_key_hex(private_key_hex: &str) -> Option<String> {
    let raw = private_key_hex.trim().trim_start_matches("0x");
    let bytes = hex::decode(raw).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let sk = SigningKey::from_slice(&bytes).ok()?;
    let vk = sk.verifying_key();
    let point = vk.to_encoded_point(false);
    let pubkey = point.as_bytes();
    if pubkey.len() != 65 || pubkey[0] != 0x04 {
        return None;
    }
    let hash = Keccak256::digest(&pubkey[1..]);
    let addr = &hash[12..];
    Some(format!(
        "0x{}",
        addr.iter().map(|b| format!("{:02x}", b)).collect::<String>()
    ))
}

pub fn load_config() -> Result<Config> {
    let _ = dotenv();
    let custom_rules_path = env::var("PASSIVE_CUSTOM_RULES_PATH")
        .unwrap_or_else(|_| "custom_pricing_rules.json".to_string());
    let state_path = env::var("PASSIVE_STRATEGY_STATE_PATH")
        .unwrap_or_else(|_| "strategy_state.json".to_string());
    let account_snapshot_path = env::var("PASSIVE_ACCOUNT_SNAPSHOT_PATH")
        .unwrap_or_else(|_| "account_snapshot.json".to_string());

    let custom_order_ids = env::var("PASSIVE_CUSTOM_ORDER_IDS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    let dashboard_enabled = parse_bool("PASSIVE_DASHBOARD_ENABLED", true);
    let ui_mode = env::var("PASSIVE_UI_MODE").unwrap_or_else(|_| {
        if dashboard_enabled {
            "web".to_string()
        } else {
            "off".to_string()
        }
    });

    let signer_address = env::var("POLYMARKET_SIGNER_ADDRESS")
        .or_else(|_| env::var("POLYMARKET_ADDRESS"))
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            env::var("POLYMARKET_PRIVATE_KEY")
                .ok()
                .or_else(|| env::var("PRIVATE_KEY").ok())
                .and_then(|pk| derive_eth_address_from_private_key_hex(&pk))
        })
        .unwrap_or_else(|| env::var("POLYMARKET_FUNDER").unwrap_or_default());

    Ok(Config {
        clob_http_url: env::var("POLYMARKET_HOST")
            .unwrap_or_else(|_| "https://clob.polymarket.com".to_string()),
        gamma_api_host: env::var("POLYMARKET_GAMMA_API")
            .unwrap_or_else(|_| "https://gamma-api.polymarket.com".to_string()),
        ws_market_url: env::var("PASSIVE_WS_MARKET_URL")
            .unwrap_or_else(|_| "wss://ws-subscriptions-clob.polymarket.com/ws/market".to_string()),
        ws_user_url: env::var("PASSIVE_WS_USER_URL")
            .unwrap_or_else(|_| "wss://ws-subscriptions-clob.polymarket.com/ws/user".to_string()),
        ws_rtds_url: env::var("PASSIVE_WS_RTDS_URL").ok(),
        api_key: env::var("POLYMARKET_API_KEY").unwrap_or_default(),
        api_secret: env::var("POLYMARKET_API_SECRET").unwrap_or_default(),
        api_passphrase: env::var("POLYMARKET_API_PASSPHRASE").unwrap_or_default(),
        private_key: env::var("POLYMARKET_PRIVATE_KEY")
            .or_else(|_| env::var("PRIVATE_KEY"))
            .unwrap_or_default(),
        signature_type: env::var("POLYMARKET_SIGNATURE_TYPE")
            .ok()
            .and_then(|v| v.parse::<u8>().ok())
            .unwrap_or(0),
        signer_address,
        funder: env::var("POLYMARKET_FUNDER").context("POLYMARKET_FUNDER is required")?,
        chain_id: parse_u64("POLYMARKET_CHAIN_ID", 137),
        post_only: parse_bool("PASSIVE_MONITORING_POST_ONLY", true),
        loop_interval_ms: parse_u64("PASSIVE_LOOP_INTERVAL_MS", 1500),
        min_replace_ticks: parse_u32("PASSIVE_ADJ_MIN_REPLACE_TICKS", 1),
        custom_rules_path: PathBuf::from(custom_rules_path),
        state_path: PathBuf::from(state_path),
        account_snapshot_path: PathBuf::from(account_snapshot_path),
        telegram_enabled: parse_bool("TELEGRAM_ENABLED", false),
        telegram_bot_token: env::var("TELEGRAM_BOT_TOKEN").unwrap_or_default(),
        telegram_chat_id: env::var("TELEGRAM_CHAT_ID").unwrap_or_default(),
        default_custom_pricing: parse_bool("PASSIVE_DEFAULT_CUSTOM_PRICING", false),
        custom_order_ids,
        custom_coarse_tick_offset: parse_usize("PASSIVE_CUSTOM_COARSE_TICK_OFFSET", 1),
        custom_coarse_allow_top_of_book: parse_bool("PASSIVE_CUSTOM_COARSE_ALLOW_TOP_OF_BOOK", true),
        custom_coarse_min_candidates: parse_usize("PASSIVE_CUSTOM_COARSE_MIN_CANDIDATES", 1),
        custom_fine_safe_min: parse_f64("PASSIVE_CUSTOM_FINE_SAFE_MIN", 0.4),
        custom_fine_safe_max: parse_f64("PASSIVE_CUSTOM_FINE_SAFE_MAX", 0.6),
        custom_fine_target_ratio: parse_f64("PASSIVE_CUSTOM_FINE_TARGET_RATIO", 0.5),
        anti_sniping_mid_jump_threshold: parse_f64("PASSIVE_MID_JUMP_THRESHOLD", 0.03),
        anti_sniping_pause_ms: parse_u64("PASSIVE_MID_JUMP_PAUSE_MS", 5000),
        anti_sniping_stable_confirm_ms: parse_u64("PASSIVE_MID_STABLE_CONFIRM_MS", 2000),
        anti_sniping_ema_alpha: parse_f64("PASSIVE_MID_EMA_ALPHA", 0.2),
        anti_sniping_rolling_median_window: parse_usize("PASSIVE_MID_MEDIAN_WINDOW", 9),
        anti_sniping_max_reprice_ticks_per_update: parse_u32("PASSIVE_MAX_REPRICE_TICKS_PER_UPDATE", 2),
        anti_sniping_fill_cooldown_ms: parse_u64("PASSIVE_FILL_COOLDOWN_MS", 15_000),
        dashboard_enabled,
        dashboard_bind: env::var("PASSIVE_DASHBOARD_BIND")
            .unwrap_or_else(|_| "127.0.0.1:8787".to_string()),
        ui_mode,
        dashboard_auto_open: parse_bool("PASSIVE_DASHBOARD_AUTO_OPEN", false),
    })
}
