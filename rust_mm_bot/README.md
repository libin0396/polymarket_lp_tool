# Polymarket LP Rust Bot

Rust rewrite of the existing passive LP market-making bot, with behavior aligned to the Python implementation:

- coarse/fine tick default pricing
- custom rules per `token_id + side`
- websocket-first event ingestion
- conservative replace/cancel execution
- Telegram command and rule flow
- long-running persistence for rules and cooldown state

## Modules

- `config`: `.env` parsing and runtime config
- `polymarket_api`: REST adapter for open orders/cancel/post
- `websocket`: market/user websocket listeners
- `orderbook`: best levels and tick resolution helpers
- `rewards`: reward spread lookup and delta conversion
- `pricing_engine`: default + custom pricing and anti-sniping filters
- `custom_rules`: in-memory keyed rule store
- `telegram`: notifier + command poller + finite-state `/set_rule` flow
- `risk_monitor`: fill/depth risk metrics for logging and alert context
- `execution_engine`: idempotent cancel/replace with retries
- `persistence`: JSON storage for rules/cooldowns/strategy snapshots
- `main`: task orchestration and event-driven runtime

## Anti-sniping controls

- midpoint jump pause (`PASSIVE_MID_JUMP_THRESHOLD`, `PASSIVE_MID_JUMP_PAUSE_MS`)
- stable-mid confirmation gate (`PASSIVE_MID_STABLE_CONFIRM_MS`)
- filtered midpoint (EMA + rolling median)
- cooldown after fills (`PASSIVE_FILL_COOLDOWN_MS`)
- per-update max repricing distance (`PASSIVE_MAX_REPRICE_TICKS_PER_UPDATE`)

## Run

```bash
cd rust_mm_bot
cargo run
```

## Dashboard UI

The bot includes a built-in web dashboard:

- Default URL: `http://127.0.0.1:8787`
- JSON API: `http://127.0.0.1:8787/api/state`

It shows:

- server memory usage
- CLOB latency (`/time` probe)
- current open orders
- pricing mode/rule per order
- last level-check timestamp per order

Env switches:

- `PASSIVE_DASHBOARD_ENABLED=true|false`
- `PASSIVE_DASHBOARD_BIND=127.0.0.1:8787`
- `PASSIVE_DASHBOARD_AUTO_OPEN=true|false` (try opening browser automatically)

## Low-Latency TUI

For lower-overhead monitoring than browser UI, use terminal TUI mode:

```bash
PASSIVE_UI_MODE=tui RUST_LOG=info cargo run
```

Modes:

- `PASSIVE_UI_MODE=tui` -> terminal dashboard (low latency)
- `PASSIVE_UI_MODE=web` -> web dashboard
- `PASSIVE_UI_MODE=off` -> no UI, logs only

For a popup-like experience, prefer:

```bash
PASSIVE_UI_MODE=web PASSIVE_DASHBOARD_AUTO_OPEN=true RUST_LOG=info cargo run
```

## Current Scope

Implemented:

- WebSocket-first ingestion (market/user) with REST reconciliation
- Coarse/fine deterministic pricing and custom per-token-side rules
- Anti-sniping protections (jump pause, stable confirm, filtered mid, cooldown, chase limit)
- Execution safety path (cancel/replace, retries, duplicate guard)
- Telegram commands: `/status`, `/orders`, `/pnl`, `/set_rule`, `/input`, `/get_rule`, `/clear_rule`, `/list_rules`
- JSON persistence for custom rules and cooldown state

Not yet fully parity-complete with Python:

- Web panel migration
- Full exchange-specific signing/headers/order payload parity hardening
- Deep risk signal parity (all fill/depth/scoring nuances)
- End-to-end fixture tests against production-like streams

## Migration Recommendation

1. Run Rust in shadow mode (same account read path, no critical capital).
2. Compare decision logs between Python and Rust for the same markets.
3. Start with low-exposure live mode.
4. Promote gradually after multi-session stability checks.

Detailed parity tracking is maintained in `PARITY_CHECKLIST.md`.

## Notes

- This version is production-oriented Rust architecture and compiles cleanly.
- Exchange-specific signing/headers and exact endpoint payloads may need to be adjusted to your currently active Polymarket API credentials model before live trading.
