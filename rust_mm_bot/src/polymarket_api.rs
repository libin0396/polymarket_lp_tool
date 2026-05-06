use crate::models::{BookLevel, Order, OrderBookSnapshot, Side};
use crate::orderbook::{best_ask, best_bid, resolve_effective_tick_size};
use anyhow::{anyhow, Result};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE;
use chrono::Utc;
use hmac::{Hmac, Mac};
use k256::ecdsa::{RecoveryId, Signature, SigningKey};
use num_bigint::BigUint;
use num_traits::Num;
use reqwest::Client;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::Value;
use sha2::Sha256;
use sha3::{Digest, Keccak256};
use std::collections::HashMap;
use tracing::warn;

#[derive(Debug, Clone)]
pub struct PolymarketApi {
    pub http: Client,
    pub base_url: String,
    pub api_key: String,
    pub api_secret: String,
    pub api_passphrase: String,
    pub private_key: String,
    pub signature_type: u8,
    pub signer_address: String,
    pub funder: String,
    pub chain_id: u64,
}

#[derive(Debug, Deserialize)]
struct OpenOrderRow {
    id: String,
    market: String,
    asset_id: String,
    side: String,
    price: String,
    size: Option<String>,
    original_size: Option<String>,
    size_matched: Option<String>,
    question: Option<String>,
    outcome: Option<String>,
}

impl PolymarketApi {
    fn as_string(v: Option<&Value>) -> String {
        match v {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            Some(Value::Bool(b)) => b.to_string(),
            _ => String::new(),
        }
    }

    fn strip_0x(s: &str) -> &str {
        s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s)
    }

    fn hex_to_fixed_32(s: &str) -> Result<[u8; 32]> {
        let raw = Self::strip_0x(s);
        let mut out = [0u8; 32];
        let bytes = hex::decode(raw)?;
        if bytes.len() > 32 {
            return Err(anyhow!("hex value too long for bytes32"));
        }
        out[32 - bytes.len()..].copy_from_slice(&bytes);
        Ok(out)
    }

    fn address_to_32(addr: &str) -> Result<[u8; 32]> {
        let raw = Self::strip_0x(addr);
        let bytes = hex::decode(raw)?;
        if bytes.len() != 20 {
            return Err(anyhow!("invalid address length"));
        }
        let mut out = [0u8; 32];
        out[12..].copy_from_slice(&bytes);
        Ok(out)
    }

    fn decimal_u256_to_32(s: &str) -> Result<[u8; 32]> {
        let bn = BigUint::from_str_radix(s, 10)
            .map_err(|_| anyhow!("invalid uint256 decimal: {}", s))?;
        let bytes = bn.to_bytes_be();
        if bytes.len() > 32 {
            return Err(anyhow!("uint256 overflow: {}", s));
        }
        let mut out = [0u8; 32];
        out[32 - bytes.len()..].copy_from_slice(&bytes);
        Ok(out)
    }

    fn u64_to_32(v: u64) -> [u8; 32] {
        let mut out = [0u8; 32];
        out[24..].copy_from_slice(&v.to_be_bytes());
        out
    }

    fn keccak(data: &[u8]) -> [u8; 32] {
        let mut h = Keccak256::new();
        h.update(data);
        let digest = h.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&digest);
        out
    }

    fn signer_address_from_private_key(&self) -> Option<String> {
        let sk_hex = Self::strip_0x(&self.private_key);
        let sk_bytes = hex::decode(sk_hex).ok()?;
        let sk = SigningKey::from_slice(&sk_bytes).ok()?;
        let vk = sk.verifying_key();
        let point = vk.to_encoded_point(false);
        let pubkey = point.as_bytes();
        if pubkey.len() != 65 || pubkey[0] != 0x04 {
            return None;
        }
        let hash = Self::keccak(&pubkey[1..]);
        Some(format!("0x{}", hex::encode(&hash[12..])))
    }

    fn round_down(x: f64, digits: i32) -> f64 {
        let p = 10f64.powi(digits);
        (x * p).floor() / p
    }

    fn round_normal(x: f64, digits: i32) -> f64 {
        let p = 10f64.powi(digits);
        (x * p).round() / p
    }

    fn round_up(x: f64, digits: i32) -> f64 {
        let p = 10f64.powi(digits);
        (x * p).ceil() / p
    }

    fn decimal_places(x: f64) -> usize {
        let s = format!("{}", x);
        if let Some((_, frac)) = s.split_once('.') {
            frac.trim_end_matches('0').len()
        } else {
            0
        }
    }

    fn to_token_decimals(x: f64) -> u64 {
        let mut f = x * 1_000_000f64;
        if Self::decimal_places(f) > 0 {
            f = Self::round_normal(f, 0);
        }
        f.max(0.0) as u64
    }

    fn round_config_for_tick(tick_size: f64) -> (i32, i32, i32) {
        if (tick_size - 0.1).abs() < 1e-9 {
            (1, 2, 3)
        } else if (tick_size - 0.01).abs() < 1e-9 {
            (2, 2, 4)
        } else if (tick_size - 0.001).abs() < 1e-12 {
            (3, 2, 5)
        } else if (tick_size - 0.0001).abs() < 1e-12 {
            (4, 2, 6)
        } else {
            (2, 2, 4)
        }
    }

    fn compute_order_amounts(side: Side, size: f64, price: f64, tick_size: f64) -> (u64, u64) {
        let (price_d, size_d, amt_d) = Self::round_config_for_tick(tick_size);
        let raw_price = Self::round_normal(price, price_d);
        match side {
            Side::Buy => {
                let raw_taker = Self::round_down(size, size_d);
                let mut raw_maker = raw_taker * raw_price;
                if Self::decimal_places(raw_maker) > amt_d as usize {
                    raw_maker = Self::round_up(raw_maker, amt_d + 4);
                    if Self::decimal_places(raw_maker) > amt_d as usize {
                        raw_maker = Self::round_down(raw_maker, amt_d);
                    }
                }
                (Self::to_token_decimals(raw_maker), Self::to_token_decimals(raw_taker))
            }
            Side::Sell => {
                let raw_maker = Self::round_down(size, size_d);
                let mut raw_taker = raw_maker * raw_price;
                if Self::decimal_places(raw_taker) > amt_d as usize {
                    raw_taker = Self::round_up(raw_taker, amt_d + 4);
                    if Self::decimal_places(raw_taker) > amt_d as usize {
                        raw_taker = Self::round_down(raw_taker, amt_d);
                    }
                }
                (Self::to_token_decimals(raw_maker), Self::to_token_decimals(raw_taker))
            }
        }
    }

    fn parse_open_orders_page(json: &Value) -> (Vec<OpenOrderRow>, String) {
        let next_cursor = json
            .get("next_cursor")
            .and_then(Value::as_str)
            .unwrap_or("LTE=")
            .to_string();
        let rows = json
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|item| {
                let obj = item.as_object()?;
                Some(OpenOrderRow {
                    id: Self::as_string(obj.get("id")),
                    market: Self::as_string(obj.get("market")),
                    asset_id: Self::as_string(obj.get("asset_id")),
                    side: Self::as_string(obj.get("side")),
                    price: Self::as_string(obj.get("price")),
                    size: obj.get("size").map(|v| Self::as_string(Some(v))),
                    original_size: obj.get("original_size").map(|v| Self::as_string(Some(v))),
                    size_matched: obj.get("size_matched").map(|v| Self::as_string(Some(v))),
                    question: obj.get("question").and_then(Value::as_str).map(ToOwned::to_owned),
                    outcome: obj.get("outcome").and_then(Value::as_str).map(ToOwned::to_owned),
                })
            })
            .collect::<Vec<_>>();
        (rows, next_cursor)
    }

    fn to_header_map(headers: HashMap<String, String>) -> Result<HeaderMap> {
        let mut out = HeaderMap::new();
        for (k, v) in headers {
            let name = HeaderName::from_bytes(k.as_bytes())
                .map_err(|e| anyhow!("invalid header name {}: {}", k, e))?;
            let value = HeaderValue::from_str(&v)
                .map_err(|e| anyhow!("invalid header value for {}: {}", k, e))?;
            out.insert(name, value);
        }
        Ok(out)
    }

    pub fn new(
        http: Client,
        base_url: String,
        api_key: String,
        api_secret: String,
        api_passphrase: String,
        private_key: String,
        signature_type: u8,
        signer_address: String,
        funder: String,
        chain_id: u64,
    ) -> Self {
        Self {
            http,
            base_url,
            api_key,
            api_secret,
            api_passphrase,
            private_key,
            signature_type,
            signer_address,
            funder,
            chain_id,
        }
    }

    fn l2_headers(&self, method: &str, request_path: &str, serialized_body: Option<&str>) -> Result<HashMap<String, String>> {
        type HmacSha256 = Hmac<Sha256>;
        let ts = Utc::now().timestamp().to_string();
        let mut message = format!("{}{}{}", ts, method, request_path);
        if let Some(body) = serialized_body {
            if !body.is_empty() {
                message.push_str(body);
            }
        }
        let secret = URL_SAFE
            .decode(self.api_secret.as_bytes())
            .map_err(|e| anyhow!("invalid api secret (base64-url): {}", e))?;
        let mut mac =
            HmacSha256::new_from_slice(&secret).map_err(|e| anyhow!("hmac init failed: {}", e))?;
        mac.update(message.as_bytes());
        let sig = URL_SAFE.encode(mac.finalize().into_bytes());

        let mut h = HashMap::new();
        h.insert("POLY_ADDRESS".to_string(), self.signer_address.clone());
        h.insert("POLY_SIGNATURE".to_string(), sig);
        h.insert("POLY_TIMESTAMP".to_string(), ts);
        h.insert("POLY_API_KEY".to_string(), self.api_key.clone());
        h.insert("POLY_PASSPHRASE".to_string(), self.api_passphrase.clone());
        Ok(h)
    }

    pub async fn get_open_orders(&self) -> Result<Vec<Order>> {
        let path = "/data/orders";
        let mut cursor = "MA==".to_string();
        let mut rows = Vec::<OpenOrderRow>::new();
        loop {
            let url = format!("{}{}", self.base_url, path);
            let headers = Self::to_header_map(self.l2_headers("GET", path, None)?)?;
            let resp = self
                .http
                .get(url)
                .headers(headers)
                .query(&[("next_cursor", cursor.as_str())])
                .send()
                .await?;
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if !status.is_success() {
                warn!("get_open_orders failed status={} body={}", status, text);
                return Err(anyhow!("get_open_orders failed status={}", status));
            }
            let json: Value = serde_json::from_str(&text)
                .map_err(|e| anyhow!("get_open_orders invalid json: {} body={}", e, text))?;
            let (page_rows, next_cursor) = Self::parse_open_orders_page(&json);
            rows.extend(page_rows);
            if next_cursor == "LTE=" {
                break;
            }
            cursor = next_cursor;
        }
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let side = match r.side.to_ascii_uppercase().as_str() {
                "BUY" => Side::Buy,
                "SELL" => Side::Sell,
                _ => continue,
            };
            out.push(Order {
                id: r.id,
                token_id: r.asset_id,
                condition_id: r.market,
                side,
                price: r.price.parse().unwrap_or(0.0),
                size: r.size.as_deref().and_then(|v| v.parse().ok()).unwrap_or(0.0),
                original_size: r.original_size.as_deref().and_then(|v| v.parse().ok()).unwrap_or(0.0),
                size_matched: r.size_matched.as_deref().and_then(|v| v.parse().ok()).unwrap_or(0.0),
                market_title: r.question,
                outcome: r.outcome,
                updated_at: Utc::now(),
            });
        }
        Ok(out)
    }

    pub async fn cancel_order(&self, order_id: &str) -> Result<()> {
        let path = "/order";
        let url = format!("{}{}", self.base_url, path);
        let body = serde_json::json!({ "orderID": order_id });
        let serialized = serde_json::to_string(&body)?;
        let headers = Self::to_header_map(self.l2_headers("DELETE", path, Some(&serialized))?)?;
        let resp = self
            .http
            .delete(url)
            .headers(headers)
            .body(serialized)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(anyhow!("cancel order failed with status {}", resp.status()));
        }
        Ok(())
    }

    pub async fn get_tick_size(&self, token_id: &str) -> Result<f64> {
        let url = format!("{}/tick-size", self.base_url);
        let resp = self.http.get(url).query(&[("token_id", token_id)]).send().await?;
        if !resp.status().is_success() {
            return Ok(0.01);
        }
        let json: Value = resp.json().await.unwrap_or(Value::Null);
        if let Some(v) = json.get("minimum_tick_size") {
            if let Some(f) = v.as_f64() {
                return Ok(f);
            }
            if let Some(s) = v.as_str() {
                if let Ok(f) = s.parse::<f64>() {
                    return Ok(f);
                }
            }
        }
        Ok(0.01)
    }

    async fn get_neg_risk(&self, token_id: &str) -> Result<bool> {
        let url = format!("{}/neg-risk", self.base_url);
        let resp = self.http.get(url).query(&[("token_id", token_id)]).send().await?;
        if !resp.status().is_success() {
            return Ok(false);
        }
        let json: Value = resp.json().await.unwrap_or(Value::Null);
        Ok(json.get("neg_risk").and_then(Value::as_bool).unwrap_or(false))
    }

    fn exchange_v2_address(&self, neg_risk: bool) -> Result<&'static str> {
        match self.chain_id {
            137 | 80002 => {
                if neg_risk {
                    Ok("0xe2222d279d744050d28e00520010520000310F59")
                } else {
                    Ok("0xE111180000d2663C0091e4f400237545B87B996B")
                }
            }
            _ => Err(anyhow!("unsupported chain_id for V2 order signing: {}", self.chain_id)),
        }
    }

    fn sign_order_v2(
        &self,
        token_id: &str,
        side: Side,
        maker_amount: u64,
        taker_amount: u64,
        expiration: u64,
        timestamp_ms: u64,
        salt: u64,
        neg_risk: bool,
    ) -> Result<String> {
        let contract = self.exchange_v2_address(neg_risk)?;
        let domain_type_hash = Self::keccak(
            b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
        );
        let name_hash = Self::keccak(b"Polymarket CTF Exchange");
        let version_hash = Self::keccak(b"2");
        let chain_id = Self::u64_to_32(self.chain_id);
        let contract_addr = Self::address_to_32(contract)?;

        let mut domain_encoded = Vec::with_capacity(32 * 5);
        domain_encoded.extend_from_slice(&domain_type_hash);
        domain_encoded.extend_from_slice(&name_hash);
        domain_encoded.extend_from_slice(&version_hash);
        domain_encoded.extend_from_slice(&chain_id);
        domain_encoded.extend_from_slice(&contract_addr);
        let domain_separator = Self::keccak(&domain_encoded);

        let order_type_hash = Self::keccak(
            b"Order(uint256 salt,address maker,address signer,uint256 tokenId,uint256 makerAmount,uint256 takerAmount,uint8 side,uint8 signatureType,uint256 timestamp,bytes32 metadata,bytes32 builder)",
        );
        let signer_addr = self
            .signer_address_from_private_key()
            .unwrap_or_else(|| self.signer_address.clone());
        let salt_b = Self::u64_to_32(salt);
        let maker_b = Self::address_to_32(&self.funder)?;
        let signer_b = Self::address_to_32(&signer_addr)?;
        let token_b = Self::decimal_u256_to_32(token_id)?;
        let maker_amt_b = Self::u64_to_32(maker_amount);
        let taker_amt_b = Self::u64_to_32(taker_amount);
        let mut side_b = [0u8; 32];
        side_b[31] = match side {
            Side::Buy => 0,
            Side::Sell => 1,
        };
        let mut sig_type_b = [0u8; 32];
        sig_type_b[31] = self.signature_type;
        let ts_b = Self::u64_to_32(timestamp_ms);
        let metadata_b = Self::hex_to_fixed_32(
            "0x0000000000000000000000000000000000000000000000000000000000000000",
        )?;
        let builder_b = Self::hex_to_fixed_32(
            "0x0000000000000000000000000000000000000000000000000000000000000000",
        )?;

        let mut order_encoded = Vec::with_capacity(32 * 12);
        order_encoded.extend_from_slice(&order_type_hash);
        order_encoded.extend_from_slice(&salt_b);
        order_encoded.extend_from_slice(&maker_b);
        order_encoded.extend_from_slice(&signer_b);
        order_encoded.extend_from_slice(&token_b);
        order_encoded.extend_from_slice(&maker_amt_b);
        order_encoded.extend_from_slice(&taker_amt_b);
        order_encoded.extend_from_slice(&side_b);
        order_encoded.extend_from_slice(&sig_type_b);
        order_encoded.extend_from_slice(&ts_b);
        order_encoded.extend_from_slice(&metadata_b);
        order_encoded.extend_from_slice(&builder_b);
        let struct_hash = Self::keccak(&order_encoded);

        let mut digest_input = Vec::with_capacity(66);
        digest_input.push(0x19);
        digest_input.push(0x01);
        digest_input.extend_from_slice(&domain_separator);
        digest_input.extend_from_slice(&struct_hash);
        let digest = Self::keccak(&digest_input);

        let sk_hex = Self::strip_0x(&self.private_key);
        let sk_bytes = hex::decode(sk_hex)?;
        let sk = SigningKey::from_slice(&sk_bytes)?;
        let (sig, recid): (Signature, RecoveryId) = sk.sign_prehash_recoverable(&digest)?;
        let mut sig_bytes = [0u8; 65];
        sig_bytes[..64].copy_from_slice(&sig.to_bytes());
        sig_bytes[64] = recid.to_byte().saturating_add(27);
        let _ = expiration; // expiration is included in payload, not in V2 typed data.
        Ok(format!("0x{}", hex::encode(sig_bytes)))
    }

    pub async fn post_order(&self, token_id: &str, side: Side, price: f64, size: f64, post_only: bool) -> Result<String> {
        if self.private_key.trim().is_empty() {
            return Err(anyhow!("post order requires PRIVATE_KEY/POLYMARKET_PRIVATE_KEY for V2 signing"));
        }

        let tick_size = self.get_tick_size(token_id).await.unwrap_or(0.01);
        let neg_risk = self.get_neg_risk(token_id).await.unwrap_or(false);
        let (maker_amount, taker_amount) = Self::compute_order_amounts(side, size, price, tick_size);
        let signer_addr = self
            .signer_address_from_private_key()
            .unwrap_or_else(|| self.signer_address.clone());
        let now_ms = Utc::now().timestamp_millis().max(0) as u64;
        let salt = now_ms.saturating_mul(1_000).saturating_add((std::process::id() as u64) % 1000);
        let expiration = 0u64;
        let signature = self.sign_order_v2(
            token_id,
            side,
            maker_amount,
            taker_amount,
            expiration,
            now_ms,
            salt,
            neg_risk,
        )?;

        let side_text = match side {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        };
        let path = "/order";
        let url = format!("{}{}", self.base_url, path);
        let body = serde_json::json!({
            "order": {
                "salt": salt,
                "maker": self.funder.clone(),
                "signer": signer_addr,
                "tokenId": token_id,
                "makerAmount": maker_amount.to_string(),
                "takerAmount": taker_amount.to_string(),
                "side": side_text,
                "expiration": expiration.to_string(),
                "signatureType": self.signature_type,
                "timestamp": now_ms.to_string(),
                "metadata": "0x0000000000000000000000000000000000000000000000000000000000000000",
                "builder": "0x0000000000000000000000000000000000000000000000000000000000000000",
                "signature": signature
            },
            "owner": self.api_key.clone(),
            "orderType": "GTC",
            "deferExec": false,
            "postOnly": post_only
        });
        let serialized = serde_json::to_string(&body)?;
        let headers = Self::to_header_map(self.l2_headers("POST", path, Some(&serialized))?)?;
        let resp = self
            .http
            .post(url)
            .headers(headers)
            .body(serialized)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("post order failed status={} body={}", status, body));
        }
        Ok(resp.text().await.unwrap_or_default())
    }

    pub async fn get_midpoint(&self, token_id: &str) -> Result<Option<f64>> {
        let url = format!("{}/midpoint", self.base_url);
        let resp = self.http.get(url).query(&[("token_id", token_id)]).send().await?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        let json: Value = resp.json().await.unwrap_or(Value::Null);
        if let Some(v) = json.get("mid") {
            if let Some(f) = v.as_f64() {
                return Ok(Some(f));
            }
            if let Some(s) = v.as_str() {
                return Ok(s.parse::<f64>().ok());
            }
        }
        Ok(None)
    }

    pub async fn get_book_snapshot(&self, token_id: &str) -> Result<Option<OrderBookSnapshot>> {
        let url = format!("{}/book", self.base_url);
        let resp = self
            .http
            .get(url)
            .query(&[("token_id", token_id)])
            .send()
            .await?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        let json: Value = resp.json().await.unwrap_or(Value::Null);
        let parse_px = |v: Option<&Value>| -> Option<f64> {
            let vv = v?;
            if let Some(f) = vv.as_f64() {
                return Some(f);
            }
            vv.as_str().and_then(|s| s.parse::<f64>().ok())
        };
        let bids = json
            .get("bids")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|x| {
                Some(BookLevel {
                    price: parse_px(x.get("price"))?,
                    size: parse_px(x.get("size")).unwrap_or(0.0),
                })
            })
            .collect::<Vec<_>>();
        let asks = json
            .get("asks")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|x| {
                Some(BookLevel {
                    price: parse_px(x.get("price"))?,
                    size: parse_px(x.get("size")).unwrap_or(0.0),
                })
            })
            .collect::<Vec<_>>();
        let api_tick = json
            .get("tick_size")
            .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok())))
            .unwrap_or(0.01);
        let tick = resolve_effective_tick_size(api_tick, &bids, &asks);
        Ok(Some(OrderBookSnapshot {
            token_id: token_id.to_string(),
            best_bid: best_bid(&bids),
            best_ask: best_ask(&asks),
            tick_size: tick,
            bids,
            asks,
            source: "rest_book_fallback".to_string(),
            updated_at: Utc::now(),
        }))
    }
}
