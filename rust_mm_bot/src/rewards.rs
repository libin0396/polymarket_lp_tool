use crate::models::RewardRange;
use anyhow::Result;
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;
use tokio::sync::RwLock;

#[derive(Debug, Deserialize)]
struct RewardsRow {
    rewards_max_spread: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct RewardsResponse {
    data: Option<Vec<RewardsRow>>,
}

#[derive(Debug)]
pub struct RewardsClient {
    http: Client,
    base_url: String,
    cache: RwLock<HashMap<String, f64>>,
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

    pub async fn rewards_max_spread_for_market(&self, condition_id: &str) -> Result<f64> {
        if let Some(v) = self.cache.read().await.get(condition_id).copied() {
            return Ok(v);
        }

        let url = format!("{}/rewards/markets/{}", self.base_url, condition_id);
        let resp = self.http.get(url).send().await?;
        let payload = resp.json::<RewardsResponse>().await.unwrap_or(RewardsResponse { data: None });
        let v = payload
            .data
            .and_then(|rows| rows.into_iter().next())
            .and_then(|row| row.rewards_max_spread)
            .unwrap_or(0.0);
        self.cache.write().await.insert(condition_id.to_string(), v);
        Ok(v)
    }
}
