use crate::models::BookLevel;

pub fn best_bid(levels: &[BookLevel]) -> Option<f64> {
    levels
        .iter()
        .map(|l| l.price)
        .filter(|p| p.is_finite())
        .max_by(f64::total_cmp)
}

pub fn best_ask(levels: &[BookLevel]) -> Option<f64> {
    levels
        .iter()
        .map(|l| l.price)
        .filter(|p| p.is_finite())
        .min_by(f64::total_cmp)
}

pub fn resolve_effective_tick_size(api_tick: f64, bids: &[BookLevel], asks: &[BookLevel]) -> f64 {
    let inferred_sub_cent = bids
        .iter()
        .chain(asks.iter())
        .any(|l| ((l.price * 100.0) - (l.price * 100.0).round()).abs() > 1e-7);

    if (api_tick - 0.01).abs() < 1e-9 && inferred_sub_cent {
        0.001
    } else if api_tick > 0.0 {
        api_tick
    } else if inferred_sub_cent {
        0.001
    } else {
        0.01
    }
}

