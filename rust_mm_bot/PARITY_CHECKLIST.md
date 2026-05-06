# Python ↔ Rust Parity Checklist

This checklist tracks alignment against the current Python production path (`py_clob_client_v2` + `passive_liquidity`).

## 1) Auth & Transport

- [x] REST L2 header names aligned:
  - `POLY_ADDRESS`
  - `POLY_SIGNATURE`
  - `POLY_TIMESTAMP`
  - `POLY_API_KEY`
  - `POLY_PASSPHRASE`
- [x] REST L2 HMAC message format aligned: `timestamp + method + request_path + serialized_body`
- [x] URL-safe base64 decode/encode signing path implemented
- [x] `/data/orders` uses signed L2 GET and paginated cursor walk (`MA==` -> `LTE=`)
- [x] WebSocket market subscribe payload aligned:
  - `{"type":"market","assets_ids":[...],"custom_feature_enabled":true}`
- [x] WebSocket user subscribe payload aligned:
  - `{"type":"user","auth":{"apiKey","secret","passphrase"},"markets":[...]}`
- [x] WS ping/pong compatibility handling (`PING`/`PONG` text + frame ping)

## 2) Pricing / Strategy Behavior

- [x] Coarse tick default decision flow present
- [x] Fine tick default decision flow present
- [x] Custom coarse/fine rule branches present
- [x] Replace threshold via min ticks present
- [x] Anti-sniping protections present:
  - midpoint jump pause
  - stable midpoint confirmation
  - filtered midpoint (EMA + rolling median)
  - fill cooldown
  - max repricing distance per update

## 3) Execution Engine Safety

- [x] pending replace guard
- [x] in-flight cancel guard
- [x] retry loop for replace post
- [x] post-only flag wiring
- [x] cooldown state persisted

## 4) Telegram Surface

- [x] `/status`
- [x] `/orders`
- [x] `/pnl`
- [x] `/set_rule` FSM style
- [x] `/input`
- [x] `/get_rule`
- [x] `/clear_rule`
- [x] `/list_rules`

## 5) Remaining Gaps To Reach Full Trading Parity

- [ ] **EIP712 signed order construction parity**
  - Python path: `create_order(...)` -> `order_to_json_v2(...)` -> `POST /order`
  - Rust currently uses a simplified post payload fallback and may be rejected by exchange.
- [ ] V2 exchange contract/order builder parity (maker/taker amounts, fee metadata, builder code, timestamp salt semantics)
- [ ] Full scoring/reward/depth metric parity against Python monitor outputs
- [ ] Web panel parity (optional but currently Python-only)
- [ ] Fixture-based regression test suite comparing Python vs Rust decisions on same market snapshots

## 6) Recommended Next Delivery Chunk

1. Implement native Rust V2 order signing compatible with `order_to_json_v2`.
2. Replace simplified `post_order` fallback with signed payload-only path.
3. Add replay test harness to diff decisions against Python for a fixed dataset.
