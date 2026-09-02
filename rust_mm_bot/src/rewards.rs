use crate::models::RewardRange;
use anyhow::{anyhow, Result};
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub struct RewardMarket {
    pub condition_id: String,
    pub rewards_max_spread_cents: f64,
    pub rewards_min_size: f64,
    pub daily_rate: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RewardToken {
    pub token_id: String,
    pub outcome: String,
    #[serde(default)]
    pub price: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct RewardMarketDetail {
    pub condition_id: String,
    pub question: String,
    pub tokens: Vec<RewardToken>,
    pub rewards_max_spread_cents: f64,
    pub rewards_min_size: f64,
}

#[derive(Debug, Deserialize)]
struct RewardsRow {
    condition_id: Option<String>,
    rewards_max_spread: Option<f64>,
    rewards_min_size: Option<f64>,
    native_daily_rate: Option<f64>,
    total_daily_rate: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct RewardsResponse {
    data: Option<Vec<RewardsRow>>,
}

#[derive(Debug, Deserialize)]
struct MarketDetailRow {
    condition_id: String,
    question: Option<String>,
    tokens: Option<Vec<RewardToken>>,
    rewards_max_spread: Option<f64>,
    rewards_min_size: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct MarketDetailResponse {
    data: Option<Vec<MarketDetailRow>>,
}

pub fn parse_current_markets(body: &str) -> Result<Vec<RewardMarket>> {
    let payload = serde_json::from_str::<RewardsResponse>(body)?;
    let mut markets = payload
        .data
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| {
            let condition_id = row.condition_id?.trim().to_string();
            let spread = row.rewards_max_spread?;
            let min_size = row.rewards_min_size?;
            let daily_rate = row.total_daily_rate.or(row.native_daily_rate)?;
            if condition_id.is_empty()
                || !spread.is_finite()
                || !min_size.is_finite()
                || !daily_rate.is_finite()
                || spread <= 0.0
                || min_size <= 0.0
                || daily_rate <= 0.0
            {
                return None;
            }
            Some(RewardMarket {
                condition_id,
                rewards_max_spread_cents: spread,
                rewards_min_size: min_size,
                daily_rate,
            })
        })
        .collect::<Vec<_>>();
    markets.sort_by(|a, b| b.daily_rate.total_cmp(&a.daily_rate));
    Ok(markets)
}

pub fn parse_market_detail(body: &str) -> Result<RewardMarketDetail> {
    let payload = serde_json::from_str::<MarketDetailResponse>(body)?;
    let row = payload
        .data
        .unwrap_or_default()
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("empty reward market detail"))?;
    let spread = row
        .rewards_max_spread
        .filter(|v| v.is_finite() && *v > 0.0)
        .ok_or_else(|| anyhow!("reward market detail has no positive spread"))?;
    let min_size = row
        .rewards_min_size
        .filter(|v| v.is_finite() && *v > 0.0)
        .ok_or_else(|| anyhow!("reward market detail has no positive minimum size"))?;
    let tokens = row
        .tokens
        .filter(|tokens| !tokens.is_empty())
        .ok_or_else(|| anyhow!("reward market detail has no tokens"))?;
    Ok(RewardMarketDetail {
        condition_id: row.condition_id,
        question: row.question.unwrap_or_default(),
        tokens,
        rewards_max_spread_cents: spread,
        rewards_min_size: min_size,
    })
}

#[derive(Debug)]
pub struct RewardsClient {
    http: Client,
    base_url: String,
    cache: RwLock<HashMap<String, Option<f64>>>,
}

impl RewardsClient {
    pub fn new(http: Client, base_url: String) -> Self {
        Self {
            http,
            base_url,
            cache: RwLock::new(HashMap::new()),
        }
    }

    pub fn reward_range(mid: f64, rewards_max_spread: f64) -> RewardRange {
        RewardRange {
            mid,
            delta: rewards_max_spread.max(0.0) * 0.01,
        }
    }

    pub async fn current_markets(&self) -> Result<Vec<RewardMarket>> {
        let url = format!("{}/rewards/markets/current", self.base_url);
        let resp = self.http.get(url).send().await?;
        if !resp.status().is_success() {
            return Err(anyhow!("current reward markets failed status={}", resp.status()));
        }
        let body = resp.text().await?;
        parse_current_markets(&body)
    }

    pub async fn market_detail(&self, condition_id: &str) -> Result<RewardMarketDetail> {
        let url = format!("{}/rewards/markets/{}", self.base_url, condition_id);
        let resp = self.http.get(url).send().await?;
        if !resp.status().is_success() {
            return Err(anyhow!("reward market detail failed status={}", resp.status()));
        }
        let body = resp.text().await?;
        parse_market_detail(&body)
    }

    pub async fn rewards_max_spread_for_market(&self, condition_id: &str) -> Result<Option<f64>> {
        if let Some(v) = self.cache.read().await.get(condition_id).copied() {
            return Ok(v);
        }

        let url = format!("{}/rewards/markets/{}", self.base_url, condition_id);
        let resp = self.http.get(url).send().await?;
        if !resp.status().is_success() {
            return Err(anyhow!("reward market lookup failed status={}", resp.status()));
        }
        let body = resp.text().await?;
        let spread = parse_market_detail(&body)
            .ok()
            .map(|market| market.rewards_max_spread_cents);
        self.cache
            .write()
            .await
            .insert(condition_id.to_string(), spread);
        Ok(spread)
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_current_markets, parse_market_detail};

    #[test]
    fn parses_current_reward_market_for_ranking() {
        let markets = parse_current_markets(
            r#"{"data":[{"condition_id":"condition","rewards_max_spread":5.5,"rewards_min_size":20,"total_daily_rate":12}]}"#,
        )
        .unwrap();

        assert_eq!(markets.len(), 1);
        assert_eq!(markets[0].condition_id, "condition");
        assert_eq!(markets[0].rewards_max_spread_cents, 5.5);
        assert_eq!(markets[0].rewards_min_size, 20.0);
        assert_eq!(markets[0].daily_rate, 12.0);
    }

    #[test]
    fn parses_market_detail_tokens_for_discovery() {
        let market = parse_market_detail(
            r#"{"data":[{"condition_id":"condition","question":"Q","tokens":[{"token_id":"yes","outcome":"Yes"},{"token_id":"no","outcome":"No"}],"rewards_max_spread":5.5,"rewards_min_size":20}]}"#,
        )
        .unwrap();

        assert_eq!(market.condition_id, "condition");
        assert_eq!(market.tokens.len(), 2);
        assert_eq!(market.tokens[0].token_id, "yes");
        assert_eq!(market.tokens[1].outcome, "No");
    }
}
