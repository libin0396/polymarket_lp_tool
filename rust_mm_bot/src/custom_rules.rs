use crate::models::{CustomPricingSettings, CustomRule, Side, TickRegime};
use chrono::Utc;
use std::collections::HashMap;

#[derive(Debug, Default, Clone)]
pub struct CustomRulesStore {
    inner: HashMap<(String, Side), CustomRule>,
}

impl CustomRulesStore {
    pub fn from_rules(rules: Vec<CustomRule>) -> Self {
        let mut inner = HashMap::new();
        for rule in rules {
            inner.insert((rule.token_id.clone(), rule.side), rule);
        }
        Self { inner }
    }

    pub fn get(&self, token_id: &str, side: Side) -> Option<&CustomRule> {
        self.inner.get(&(token_id.to_string(), side))
    }

    pub fn upsert(
        &mut self,
        token_id: String,
        side: Side,
        tick_regime: TickRegime,
        settings: CustomPricingSettings,
    ) {
        self.inner.insert(
            (token_id.clone(), side),
            CustomRule {
                token_id,
                side,
                tick_regime,
                settings,
                updated_at: Utc::now(),
            },
        );
    }

    pub fn clear(&mut self, token_id: &str, side: Side) -> bool {
        self.inner.remove(&(token_id.to_string(), side)).is_some()
    }

    pub fn list(&self) -> Vec<CustomRule> {
        self.inner.values().cloned().collect()
    }
}
