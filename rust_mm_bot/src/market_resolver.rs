use anyhow::Result;
use reqwest::Client;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use tokio::sync::RwLock;

#[derive(Debug, Default)]
pub struct MarketResolver {
    by_token: RwLock<HashMap<String, (String, String)>>,
    by_condition: RwLock<HashMap<String, String>>,
    miss_token: RwLock<HashSet<String>>,
}

impl MarketResolver {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn lookup(
        &self,
        http: &Client,
        gamma_host: &str,
        condition_id: &str,
        token_id: &str,
    ) -> Result<(String, String)> {
        if let Some(v) = self.by_token.read().await.get(token_id).cloned() {
            return Ok(v);
        }

        if !self.miss_token.read().await.contains(token_id) {
            let url = format!("{}/markets?clob_token_ids={}", gamma_host.trim_end_matches('/'), token_id);
            if let Ok((q, o)) = Self::fetch_market(http, &url, token_id).await {
                self.by_token
                    .write()
                    .await
                    .insert(token_id.to_string(), (q.clone(), o.clone()));
                if !q.is_empty() && !condition_id.is_empty() {
                    self.by_condition
                        .write()
                        .await
                        .insert(condition_id.to_string(), q.clone());
                }
                return Ok((q, o));
            }
            self.miss_token.write().await.insert(token_id.to_string());
        }

        if let Some(q) = self.by_condition.read().await.get(condition_id).cloned() {
            return Ok((q, String::new()));
        }

        if !condition_id.is_empty() {
            let url = format!(
                "{}/markets?condition_ids={}",
                gamma_host.trim_end_matches('/'),
                condition_id
            );
            if let Ok((q, o)) = Self::fetch_market(http, &url, token_id).await {
                if !q.is_empty() {
                    self.by_condition
                        .write()
                        .await
                        .insert(condition_id.to_string(), q.clone());
                }
                if !q.is_empty() {
                    self.by_token
                        .write()
                        .await
                        .insert(token_id.to_string(), (q.clone(), o.clone()));
                }
                return Ok((q, o));
            }
        }

        Ok((String::new(), String::new()))
    }

    async fn fetch_market(http: &Client, url: &str, token_id: &str) -> Result<(String, String)> {
        let resp = http.get(url).send().await?;
        let json = resp.json::<Value>().await?;
        let rows = json.as_array().cloned().unwrap_or_default();
        let Some(m) = rows.first().and_then(Value::as_object) else {
            anyhow::bail!("empty gamma market");
        };
        let question = m
            .get("question")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let outcomes = m
            .get("outcomes")
            .and_then(Value::as_str)
            .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
            .unwrap_or_default();
        let token_ids = m
            .get("clobTokenIds")
            .and_then(Value::as_str)
            .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
            .unwrap_or_default();
        let mut outcome = String::new();
        if outcomes.len() == token_ids.len() {
            if let Some((idx, _)) = token_ids.iter().enumerate().find(|(_, t)| t.as_str() == token_id) {
                outcome = outcomes.get(idx).cloned().unwrap_or_default();
            }
        }
        Ok((question, outcome))
    }
}
