use crate::models::{CooldownState, CustomRule, StrategyState};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use tokio::fs;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PersistedState {
    pub custom_rules: Vec<CustomRule>,
    pub cooldown_state: CooldownState,
    pub strategy_states: Vec<StrategyState>,
    pub account_snapshots: HashMap<String, serde_json::Value>,
}

pub async fn load_state(path: &Path) -> Result<PersistedState> {
    if !path.exists() {
        return Ok(PersistedState::default());
    }
    let raw = fs::read_to_string(path).await?;
    let state = serde_json::from_str::<PersistedState>(&raw)?;
    Ok(state)
}

pub async fn save_state(path: &Path, state: &PersistedState) -> Result<()> {
    let payload = serde_json::to_string_pretty(state)?;
    fs::write(path, payload).await?;
    Ok(())
}
