use crate::models::{DecisionAction, ExecutionRequest};
use crate::polymarket_api::PolymarketApi;
use anyhow::Result;
use chrono::{Duration, Utc};
use std::collections::{HashMap, HashSet};
use tracing::{info, warn};

#[derive(Debug, Default)]
pub struct ExecutionEngine {
    pending_replace: HashSet<String>,
    in_flight_cancels: HashSet<String>,
    recent_fills: HashMap<String, chrono::DateTime<Utc>>,
    dangerous_until: HashMap<String, chrono::DateTime<Utc>>,
    version_mismatch_cooldown_until: HashMap<String, chrono::DateTime<Utc>>,
}

impl ExecutionEngine {
    pub fn in_version_mismatch_cooldown(&self, token_id: &str) -> bool {
        self.version_mismatch_cooldown_until
            .get(token_id)
            .map(|until| Utc::now() < *until)
            .unwrap_or(false)
    }

    pub fn mark_recent_fill(&mut self, token_id: String, cooldown_ms: u64) {
        let now = Utc::now();
        self.recent_fills.insert(token_id.clone(), now);
        self.dangerous_until
            .insert(token_id, now + Duration::milliseconds(cooldown_ms as i64));
    }

    pub fn recently_filled(&self, token_id: &str, cooldown_ms: u64) -> bool {
        self.recent_fills
            .get(token_id)
            .map(|ts| Utc::now().signed_duration_since(*ts) < Duration::milliseconds(cooldown_ms as i64))
            .unwrap_or(false)
    }

    pub fn set_cooldown_until(&mut self, token_id: String, until: chrono::DateTime<Utc>) {
        self.dangerous_until.insert(token_id, until);
    }

    pub fn cooldown_snapshot(&self) -> HashMap<String, chrono::DateTime<Utc>> {
        self.dangerous_until.clone()
    }

    pub async fn apply(&mut self, api: &PolymarketApi, req: ExecutionRequest, post_only: bool) -> Result<()> {
        if self.pending_replace.contains(&req.order_id) {
            info!("skip duplicate replace order_id={}", req.order_id);
            return Ok(());
        }

        match req.decision.action {
            DecisionAction::Keep => {
                info!("keep token={} side={} order_id={} reason={}", req.token_id, req.side.as_str(), req.order_id, req.decision.reason);
            }
            DecisionAction::Cancel => {
                if self.in_flight_cancels.insert(req.order_id.clone()) {
                    let cancel_result = api.cancel_order(&req.order_id).await;
                    self.in_flight_cancels.remove(&req.order_id);
                    if let Err(err) = cancel_result {
                        warn!("cancel failed order_id={} err={}", req.order_id, err);
                    } else {
                        info!("cancel ok token={} side={} order_id={} reason={}", req.token_id, req.side.as_str(), req.order_id, req.decision.reason);
                    }
                }
            }
            DecisionAction::Replace { new_price } => {
                if let Some(until) = self
                    .version_mismatch_cooldown_until
                    .get(&req.token_id)
                    .copied()
                {
                    if Utc::now() < until {
                        info!(
                            "skip replace token={} side={} order_id={} reason=order_version_mismatch_cooldown until={}",
                            req.token_id,
                            req.side.as_str(),
                            req.order_id,
                            until.to_rfc3339()
                        );
                        return Ok(());
                    }
                }

                self.pending_replace.insert(req.order_id.clone());
                let mut cancel_retries = 0u8;
                let mut canceled_old = false;
                while cancel_retries < 5 {
                    cancel_retries += 1;
                    match api.cancel_order(&req.order_id).await {
                        Ok(_) => {
                            canceled_old = true;
                            break;
                        }
                        Err(err) => {
                            warn!(
                                "replace cancel-old failed token={} side={} order_id={} retry={} err={}",
                                req.token_id,
                                req.side.as_str(),
                                req.order_id,
                                cancel_retries,
                                err
                            );
                        }
                    }
                }

                if !canceled_old {
                    warn!(
                        "replace aborted cancel_old_failed token={} side={} old_order_id={} old_price={} new_price={}",
                        req.token_id,
                        req.side.as_str(),
                        req.order_id,
                        req.old_price,
                        new_price
                    );
                    self.pending_replace.remove(&req.order_id);
                    return Ok(());
                }

                let mut post_retries = 0u8;
                let mut posted_new: Option<String> = None;
                while post_retries < 5 {
                    post_retries += 1;
                    match api
                        .post_order(&req.token_id, req.side, new_price, req.size, post_only)
                        .await
                    {
                        Ok(posted_id) => {
                            self.version_mismatch_cooldown_until.remove(&req.token_id);
                            posted_new = Some(posted_id);
                            break;
                        }
                        Err(err) => {
                            let err_text = err.to_string();
                            if err_text.contains("order_version_mismatch") {
                                let until = Utc::now() + Duration::seconds(30);
                                self.version_mismatch_cooldown_until
                                    .insert(req.token_id.clone(), until);
                                warn!(
                                    "replace post rejected token={} side={} order_id={} err=order_version_mismatch set_cooldown_until={}",
                                    req.token_id,
                                    req.side.as_str(),
                                    req.order_id,
                                    until.to_rfc3339()
                                );
                                break;
                            }
                            warn!(
                                "replace post failed token={} side={} order_id={} retry={} err={}",
                                req.token_id,
                                req.side.as_str(),
                                req.order_id,
                                post_retries,
                                err_text
                            );
                        }
                    }
                }

                if let Some(posted_id) = posted_new {
                    info!(
                        "replace ok token={} side={} old_order_id={} old_price={} new_price={} reason={} posted={} path=cancel_first",
                        req.token_id,
                        req.side.as_str(),
                        req.order_id,
                        req.old_price,
                        new_price,
                        req.decision.reason,
                        posted_id
                    );
                } else {
                    warn!(
                        "replace partial token={} side={} old_order_id={} old_price={} new_price={} reason=post_failed_after_cancel",
                        req.token_id,
                        req.side.as_str(),
                        req.order_id,
                        req.old_price,
                        new_price
                    );
                }
                self.pending_replace.remove(&req.order_id);
            }
        }
        Ok(())
    }
}
