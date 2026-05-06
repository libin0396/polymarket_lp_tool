use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use tokio::sync::{mpsc::Sender, RwLock};
use tracing::{info, warn};
use crate::models::{CustomPricingSettings, EngineEvent, Side, TickRegime};

#[derive(Debug, Clone, Serialize)]
pub struct OrderDashboardRow {
    pub order_id: String,
    pub token_id: String,
    pub market_title: String,
    pub outcome_label: String,
    pub side: String,
    pub order_price: f64,
    pub size: f64,
    pub mid_price: Option<f64>,
    pub reward_range_lo: Option<f64>,
    pub reward_range_hi: Option<f64>,
    pub reward_tick_size: Option<f64>,
    pub pricing_mode: String,
    pub pricing_rule: String,
    pub tick_regime: String,
    pub last_decision_reason: String,
    pub last_check_at: Option<DateTime<Utc>>,
    pub last_candidate_levels: Vec<f64>,
}

#[derive(Debug, Clone, Deserialize)]
struct SetRuleRequest {
    order_id: String,
    coarse_tick_offset_from_mid: Option<usize>,
    coarse_allow_top_of_book: Option<bool>,
    coarse_min_candidate_levels: Option<usize>,
    fine_safe_band_min: Option<f64>,
    fine_safe_band_max: Option<f64>,
    fine_target_band_ratio: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
struct SetRuleResponse {
    ok: bool,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardSnapshot {
    pub updated_at: DateTime<Utc>,
    pub process_started_at: DateTime<Utc>,
    pub server_memory_used_mb: f64,
    pub server_memory_total_mb: f64,
    pub server_memory_usage_pct: f64,
    pub clob_latency_ms: Option<u128>,
    pub clob_latency_last_checked_at: Option<DateTime<Utc>>,
    pub order_poll_last_count: Option<usize>,
    pub order_poll_last_ok_at: Option<DateTime<Utc>>,
    pub order_poll_last_error: Option<String>,
    pub order_poll_last_error_at: Option<DateTime<Utc>>,
    pub open_orders_count: usize,
    pub rows: Vec<OrderDashboardRow>,
}

#[derive(Debug, Default)]
struct DashboardStateInner {
    process_started_at: Option<DateTime<Utc>>,
    server_memory_used_mb: f64,
    server_memory_total_mb: f64,
    server_memory_usage_pct: f64,
    clob_latency_ms: Option<u128>,
    clob_latency_last_checked_at: Option<DateTime<Utc>>,
    order_poll_last_count: Option<usize>,
    order_poll_last_ok_at: Option<DateTime<Utc>>,
    order_poll_last_error: Option<String>,
    order_poll_last_error_at: Option<DateTime<Utc>>,
    rows: BTreeMap<String, OrderDashboardRow>,
    updated_at: Option<DateTime<Utc>>,
}

#[derive(Clone)]
pub struct DashboardStateHandle {
    inner: Arc<RwLock<DashboardStateInner>>,
}

impl DashboardStateHandle {
    pub fn new() -> Self {
        let mut inner = DashboardStateInner::default();
        inner.process_started_at = Some(Utc::now());
        inner.updated_at = Some(Utc::now());
        Self {
            inner: Arc::new(RwLock::new(inner)),
        }
    }

    pub async fn set_server_memory(&self, used_mb: f64, total_mb: f64) {
        let mut w = self.inner.write().await;
        w.server_memory_used_mb = used_mb.max(0.0);
        w.server_memory_total_mb = total_mb.max(0.0);
        w.server_memory_usage_pct = if total_mb > 1e-9 {
            (used_mb / total_mb * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        };
        w.updated_at = Some(Utc::now());
    }

    pub async fn set_clob_latency(&self, latency_ms: Option<u128>) {
        let mut w = self.inner.write().await;
        w.clob_latency_ms = latency_ms;
        w.clob_latency_last_checked_at = Some(Utc::now());
        w.updated_at = Some(Utc::now());
    }

    pub async fn set_order_poll_ok(&self, count: usize) {
        let mut w = self.inner.write().await;
        w.order_poll_last_count = Some(count);
        w.order_poll_last_ok_at = Some(Utc::now());
        w.order_poll_last_error = None;
        w.order_poll_last_error_at = None;
        w.updated_at = Some(Utc::now());
    }

    pub async fn set_order_poll_error(&self, error: String) {
        let mut w = self.inner.write().await;
        w.order_poll_last_error = Some(error);
        w.order_poll_last_error_at = Some(Utc::now());
        w.updated_at = Some(Utc::now());
    }

    pub async fn upsert_order_row(&self, row: OrderDashboardRow) {
        let mut w = self.inner.write().await;
        w.rows.insert(row.order_id.clone(), row);
        w.updated_at = Some(Utc::now());
    }

    pub async fn retain_orders(&self, alive_order_ids: &HashSet<String>) {
        let mut w = self.inner.write().await;
        w.rows.retain(|oid, _| alive_order_ids.contains(oid));
        w.updated_at = Some(Utc::now());
    }

    pub async fn snapshot(&self) -> DashboardSnapshot {
        let r = self.inner.read().await;
        DashboardSnapshot {
            updated_at: r.updated_at.unwrap_or_else(Utc::now),
            process_started_at: r.process_started_at.unwrap_or_else(Utc::now),
            server_memory_used_mb: r.server_memory_used_mb,
            server_memory_total_mb: r.server_memory_total_mb,
            server_memory_usage_pct: r.server_memory_usage_pct,
            clob_latency_ms: r.clob_latency_ms,
            clob_latency_last_checked_at: r.clob_latency_last_checked_at,
            order_poll_last_count: r.order_poll_last_count,
            order_poll_last_ok_at: r.order_poll_last_ok_at,
            order_poll_last_error: r.order_poll_last_error.clone(),
            order_poll_last_error_at: r.order_poll_last_error_at,
            open_orders_count: r.rows.len(),
            rows: r.rows.values().cloned().collect(),
        }
    }

    async fn get_order_row(&self, order_id: &str) -> Option<OrderDashboardRow> {
        let r = self.inner.read().await;
        r.rows.get(order_id).cloned()
    }
}

#[derive(Clone)]
struct DashboardAppState {
    dashboard: DashboardStateHandle,
    engine_tx: Sender<EngineEvent>,
}

async fn api_state(State(state): State<DashboardAppState>) -> impl IntoResponse {
    Json(state.dashboard.snapshot().await)
}

async fn api_set_rule(
    State(state): State<DashboardAppState>,
    Json(req): Json<SetRuleRequest>,
) -> impl IntoResponse {
    let Some(row) = state.dashboard.get_order_row(&req.order_id).await else {
        return Json(SetRuleResponse {
            ok: false,
            message: "order not found on dashboard".to_string(),
        });
    };

    let side = match row.side.as_str() {
        "BUY" => Side::Buy,
        "SELL" => Side::Sell,
        _ => {
            return Json(SetRuleResponse {
                ok: false,
                message: "invalid side".to_string(),
            })
        }
    };
    let tick_regime = match row.tick_regime.as_str() {
        "coarse" => TickRegime::Coarse,
        "fine" => TickRegime::Fine,
        _ => TickRegime::Unsupported,
    };
    if tick_regime == TickRegime::Unsupported {
        return Json(SetRuleResponse {
            ok: false,
            message: "unsupported tick regime for custom popup".to_string(),
        });
    }

    let settings = CustomPricingSettings {
        coarse_tick_offset_from_mid: req.coarse_tick_offset_from_mid.unwrap_or(1).max(1),
        coarse_allow_top_of_book: req.coarse_allow_top_of_book.unwrap_or(true),
        coarse_min_candidate_levels: req.coarse_min_candidate_levels.unwrap_or(1).max(1),
        fine_safe_band_min: req.fine_safe_band_min.unwrap_or(0.4),
        fine_safe_band_max: req.fine_safe_band_max.unwrap_or(0.6),
        fine_target_band_ratio: req.fine_target_band_ratio.unwrap_or(0.5),
    };

    let evt = EngineEvent::UpsertCustomRule {
        token_id: row.token_id,
        side,
        tick_regime,
        settings,
    };
    if state.engine_tx.send(evt).await.is_err() {
        return Json(SetRuleResponse {
            ok: false,
            message: "engine channel closed".to_string(),
        });
    }
    Json(SetRuleResponse {
        ok: true,
        message: "rule saved".to_string(),
    })
}

fn render_html(data: &DashboardSnapshot) -> String {
    let mut rows_html = String::new();
    for row in &data.rows {
        let check_time = row
            .last_check_at
            .map(|t| t.to_rfc3339())
            .unwrap_or_else(|| "-".to_string());
        let mid = row
            .mid_price
            .map(|v| format!("{:.4}", v))
            .unwrap_or_else(|| "-".to_string());
        let reward_range = match (row.reward_range_lo, row.reward_range_hi) {
            (Some(lo), Some(hi)) if row.tick_regime == "coarse" => {
                let tick = row.reward_tick_size.unwrap_or(0.01).max(1e-9);
                let k_lo = ((lo / tick) - 1e-9).ceil() as i64;
                let k_hi = ((hi / tick) + 1e-9).floor() as i64;
                if k_hi < k_lo {
                    "-".to_string()
                } else {
                    let mut vals = Vec::new();
                    for k in k_lo..=k_hi {
                        vals.push(format!("{:.4}", (k as f64) * tick));
                        if vals.len() >= 50 {
                            break;
                        }
                    }
                    format!("[{}]", vals.join(","))
                }
            }
            (Some(lo), Some(hi)) => format!("[{:.4},{:.4}]", lo, hi),
            _ => "-".to_string(),
        };
        let candidates = if row.last_candidate_levels.is_empty() {
            "-".to_string()
        } else {
            row.last_candidate_levels
                .iter()
                .take(12)
                .map(|v| format!("{:.4}", v))
                .collect::<Vec<_>>()
                .join(", ")
        };
        rows_html.push_str(&format!(
            "<tr>\
                <td>{}</td><td>{}</td>\
                <td>{:.4}</td><td>{:.4}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td>\
                <td>{}</td><td>{}</td><td>{}</td><td><button class=\"rule-btn\" data-order-id=\"{}\" data-regime=\"{}\" data-i18n=\"btnSetRule\">调价规则</button></td>\
            </tr>",
            row.market_title,
            row.outcome_label,
            row.order_price,
            row.size,
            mid,
            reward_range,
            row.pricing_mode,
            row.pricing_rule,
            row.tick_regime,
            check_time,
            candidates,
            row.order_id,
            row.tick_regime
        ));
    }
    let gate_status = if data.rows.is_empty() {
        "-".to_string()
    } else if data.rows.len() == 1 {
        let r = &data.rows[0];
        format!("{} / {}: {}", r.market_title, r.outcome_label, r.last_decision_reason)
    } else {
        let mut seg = Vec::new();
        for r in data.rows.iter().take(3) {
            seg.push(format!("{}: {}", r.market_title, r.last_decision_reason));
        }
        if data.rows.len() > 3 {
            seg.push(format!("...+{} more", data.rows.len() - 3));
        }
        seg.join(" | ")
    };

    format!(
        r##"<!doctype html>
<html>
<head>
  <meta charset="utf-8"/>
  <meta name="viewport" content="width=device-width, initial-scale=1"/>
  <title>Polymarket Rust Bot Dashboard</title>
  <style>
    body {{ font-family: Arial, sans-serif; margin: 16px; background: #0f1115; color: #e6e6e6; }}
    .card {{ padding: 12px; border: 1px solid #2c2f36; border-radius: 8px; margin-bottom: 12px; background: #171a21; }}
    .topbar {{ display: flex; justify-content: space-between; align-items: center; margin-bottom: 10px; }}
    .lang {{ display: inline-flex; gap: 8px; align-items: center; }}
    .nav {{ display: flex; gap: 12px; align-items: center; margin-bottom: 12px; }}
    .nav a {{ color: #93c5fd; text-decoration: none; }}
    .nav a:hover {{ text-decoration: underline; }}
    select {{ background: #171a21; color: #e6e6e6; border: 1px solid #2c2f36; border-radius: 6px; padding: 4px 8px; }}
    table {{ width: 100%; border-collapse: collapse; font-size: 12px; }}
    th, td {{ border: 1px solid #2c2f36; padding: 6px; text-align: left; }}
    th {{ background: #20242d; position: sticky; top: 0; }}
    .muted {{ color: #9aa3b2; }}
    .usage-text {{ white-space: pre-line; line-height: 1.6; }}
  </style>
</head>
<body>
  <div class="topbar">
    <h2 id="title" style="margin:0">臭臭panda Polymarket LP Tool 2.0(beta)</h2>
    <div class="lang">
      <label id="langLabel" for="langSelect">语言/Language</label>
      <select id="langSelect">
        <option value="zh-CN">简体中文</option>
        <option value="en">English</option>
      </select>
    </div>
  </div>
  <div class="card nav">
    <a href="#orders" data-i18n="navOrders">挂单栏</a>
    <span class="muted">|</span>
    <a href="#guide" data-i18n="navGuide">使用说明</a>
    <span class="muted">|</span>
    <a href="#support" data-i18n="navSupport">支持作者</a>
  </div>
  <div class="card" id="orders">
    <div><span data-i18n="processStarted">Process started</span>: <span class="muted">{}</span></div>
    <div><span data-i18n="updatedAt">Updated at</span>: <span class="muted">{}</span></div>
    <div><span data-i18n="serverMemory">Server memory</span>: <b>{:.2} MB</b> / {:.2} MB ({:.2}%)</div>
    <div><span data-i18n="serverLatency">Server latency (CLOB /time)</span>: <b>{}</b> ms</div>
    <div><span data-i18n="openOrdersTracked">Open orders tracked</span>: <b>{}</b></div>
    <div><span data-i18n="orderPoll">Order poll</span>: <b>{}</b></div>
    <div><span data-i18n="gateStatus">Decision gate status</span>: <b>{}</b></div>
    <div class="muted"><span data-i18n="jsonApi">JSON API</span>: /api/state</div>
  </div>
  <div class="card">
    <table>
      <thead>
        <tr>
          <th data-i18n="thMarket">Market</th><th data-i18n="thOutcome">Outcome</th>
          <th data-i18n="thPrice">Price</th><th data-i18n="thSize">Size</th><th data-i18n="thMid">Mid</th><th data-i18n="thRewardRange">Reward Range</th><th data-i18n="thMode">Mode</th><th data-i18n="thRule">Rule</th>
          <th data-i18n="thRegime">Regime</th><th data-i18n="thLastCheck">Last Level Check</th><th data-i18n="thCandidates">Candidate Levels</th><th data-i18n="thActions">Actions</th>
        </tr>
      </thead>
      <tbody>{}</tbody>
    </table>
  </div>
  <div class="card" id="rulePanel" style="display:none">
    <h3 style="margin-top:0" data-i18n="rulePanelTitle">设置挂单调价规则</h3>
    <input id="ruleOrderId" type="hidden" />
    <input id="ruleRegime" type="hidden" />
    <div id="coarseFields">
      <div>coarse_n: <input id="coarseN" type="number" min="1" value="1"/></div>
      <div>allow_top: <input id="coarseAllowTop" type="checkbox" checked/></div>
      <div>min_cands: <input id="coarseMinCands" type="number" min="1" value="1"/></div>
    </div>
    <div id="fineFields">
      <div>safe_min: <input id="fineSafeMin" type="number" step="0.001" value="0.4"/></div>
      <div>safe_max: <input id="fineSafeMax" type="number" step="0.001" value="0.6"/></div>
      <div>target_ratio: <input id="fineTargetRatio" type="number" step="0.001" value="0.5"/></div>
    </div>
    <div style="margin-top:8px; display:flex; gap:8px;">
      <button id="saveRuleBtn" data-i18n="saveRuleBtn">保存</button>
      <button id="cancelRuleBtn" data-i18n="cancelRuleBtn">取消</button>
    </div>
  </div>
  <div class="card" id="guide">
    <h3 style="margin-top:0" data-i18n="usageTitle">使用说明</h3>
    <div class="muted usage-text" data-i18n="usageBody">本页面用于观察机器人状态、挂单变化与调价规则。优先关注拉单状态与上次档位检查时间。</div>
  </div>
  <div class="card" id="support">
    <h3 style="margin-top:0" data-i18n="supportTitle">支持作者</h3>
    <div class="muted usage-text" data-i18n="supportBody">感谢使用臭臭panda Polymarket LP Tool 2.0(beta)。如需支持作者，可通过你们约定的渠道联系。</div>
  </div>
  <script>
    const I18N = {{
      "en": {{
        title: "臭臭panda Polymarket LP Tool 2.0(beta)",
        langLabel: "语言/Language",
        navOrders: "Orders",
        navGuide: "Guide",
        navSupport: "Support Author",
        processStarted: "Process started",
        updatedAt: "Updated at",
        serverMemory: "Server memory",
        serverLatency: "Server latency (CLOB /time)",
        openOrdersTracked: "Open orders tracked",
        orderPoll: "Order poll",
        gateStatus: "Decision gate status",
        jsonApi: "JSON API",
        thMarket: "Market",
        thOutcome: "Outcome",
        thPrice: "Price",
        thSize: "Size",
        thMid: "Mid",
        thRewardRange: "Reward Range",
        thMode: "Mode",
        thRule: "Rule",
        thRegime: "Regime",
        thLastCheck: "Last Level Check",
        thCandidates: "Candidate Levels",
        thActions: "Actions",
        btnSetRule: "Set Rule",
        rulePanelTitle: "Set Repricing Rule",
        saveRuleBtn: "Save",
        cancelRuleBtn: "Cancel",
        usageTitle: "Guide",
        usageBody: "Markets are grouped into coarse-tick and fine-tick. The repricing rule (mid-following logic) uses different strategies by tick granularity.\n\nCoarse tick:\n- Larger price step (usually 1 cent)\n- Fewer discrete levels in reward band\n- Strategy is level selection (for example coarse_n)\n- Focus on whether candidate level count is sufficient (min_cands)\n\nFine tick:\n- Smaller price step (usually 0.1 cent)\n- More continuous and finer levels\n- Strategy is distance control versus mid within a target ratio band\n- Focus on whether distance ratio to mid is inside target band",
        supportTitle: "Support Author",
        supportBody: "Follow author on X: https://x.com/Chosmos110\nUse author Polymarket invite link: https://polymarket.com/?r=xiaochouchou\nFor strategy customization:\nWeChat: Licc594\nTelegram: https://t.me/Chosmos2025"
      }},
      "zh-CN": {{
        title: "臭臭panda Polymarket LP Tool 2.0(beta)",
        langLabel: "语言/Language",
        navOrders: "挂单栏",
        navGuide: "使用说明",
        navSupport: "支持作者",
        processStarted: "进程启动时间",
        updatedAt: "更新时间",
        serverMemory: "服务器内存",
        serverLatency: "服务器延迟（CLOB /time）",
        openOrdersTracked: "当前跟踪挂单数",
        orderPoll: "拉单状态",
        gateStatus: "当前调价门槛状态",
        jsonApi: "JSON 接口",
        thMarket: "盘口名称",
        thOutcome: "订单方向(Yes/No)",
        thPrice: "价格",
        thSize: "数量",
        thMid: "中间价",
        thRewardRange: "奖励范围",
        thMode: "模式",
        thRule: "规则",
        thRegime: "Tick 类型",
        thLastCheck: "上次档位检查",
        thCandidates: "候选档位",
        thActions: "操作",
        btnSetRule: "调价规则",
        rulePanelTitle: "设置挂单调价规则",
        saveRuleBtn: "保存",
        cancelRuleBtn: "取消",
        usageTitle: "使用说明",
        usageBody: "盘口分为：粗tick和细tick两类。本系统的调价规则（跟随中间价规则）以tick的粗细两类分为不同策略。\n\n粗 tick（coarse tick）\n- 价格步长较大（通常 1 美分）\n- 奖励带内可选价位是离散的、数量较少，比如 0.81, 0.82, 0.83...\n- 策略更像“选档位”：从奖励带内订单簿已有档位里按规则选第 N 档（如 coarse_n=3）\n- 更关注“候选档位数量够不够”（min_cands）\n\n细 tick（fine tick）\n- 价格步长更小（通常 0.1 美分）\n- 价格更连续、可微调，候选位更多\n- 策略更像“控距离”：让挂单保持在中间价到奖励带之间的某个比例区间（如 safe band / target ratio）\n- 更关注“当前价距离 mid 的比例”是否落在目标带内",
        supportTitle: "支持作者",
        supportBody: "请关注作者推特：https://x.com/Chosmos110\n使用作者 Polymarket 邀请链接：https://polymarket.com/?r=xiaochouchou\n策略定制联系：\n微信：Licc594\nTelegram：https://t.me/Chosmos2025"
      }}
    }};

    function applyLang(lang) {{
      const dict = I18N[lang] || I18N["en"];
      document.getElementById("title").textContent = dict.title;
      document.getElementById("langLabel").textContent = dict.langLabel;
      document.querySelectorAll("[data-i18n]").forEach(el => {{
        const key = el.getAttribute("data-i18n");
        if (dict[key]) {{
          if (key === "supportBody") {{
            const lines = String(dict[key]).split("\n");
            const html = lines
              .map(line => line.trim())
              .map(line => {{
                if (line.startsWith("http://") || line.startsWith("https://")) {{
                  return '<a href="' + line + '" target="_blank" rel="noopener noreferrer">' + line + '</a>';
                }}
                if (line.includes("https://")) {{
                  const idx = line.indexOf("https://");
                  const prefix = line.slice(0, idx);
                  const url = line.slice(idx);
                  return prefix + '<a href="' + url + '" target="_blank" rel="noopener noreferrer">' + url + '</a>';
                }}
                return line;
              }})
              .join("<br/>");
            el.innerHTML = html;
          }} else {{
            el.textContent = dict[key];
          }}
        }}
      }});
      localStorage.setItem("poly_dashboard_lang", lang);
    }}

    const sel = document.getElementById("langSelect");
    const saved = localStorage.getItem("poly_dashboard_lang");
    const initial = saved || (navigator.language || "en");
    const chosen = (initial.startsWith("zh")) ? "zh-CN" : "en";
    sel.value = chosen;
    applyLang(chosen);
    let pauseRefreshUntilMs = 0;
    function pauseAutoRefresh(ms) {{
      pauseRefreshUntilMs = Date.now() + ms;
    }}
    sel.addEventListener("mousedown", () => pauseAutoRefresh(8000));
    sel.addEventListener("focus", () => pauseAutoRefresh(8000));
    sel.addEventListener("touchstart", () => pauseAutoRefresh(8000));
    sel.addEventListener("change", (e) => {{
      applyLang(e.target.value);
      pauseAutoRefresh(5000);
    }});
    let rulePanelOpen = false;
    function bindRuleButtons() {{
      document.querySelectorAll(".rule-btn").forEach(btn => {{
        btn.addEventListener("click", () => {{
          const orderId = btn.getAttribute("data-order-id") || "";
          const regime = btn.getAttribute("data-regime") || "";
          document.getElementById("ruleOrderId").value = orderId;
          document.getElementById("ruleRegime").value = regime;
          document.getElementById("rulePanel").style.display = "block";
          document.getElementById("coarseFields").style.display = regime === "coarse" ? "block" : "none";
          document.getElementById("fineFields").style.display = regime === "fine" ? "block" : "none";
          rulePanelOpen = true;
        }});
      }});
    }}
    document.getElementById("cancelRuleBtn").addEventListener("click", () => {{
      document.getElementById("rulePanel").style.display = "none";
      rulePanelOpen = false;
    }});
    document.getElementById("saveRuleBtn").addEventListener("click", async () => {{
      const payload = {{
        order_id: document.getElementById("ruleOrderId").value,
        coarse_tick_offset_from_mid: parseInt(document.getElementById("coarseN").value || "1", 10),
        coarse_allow_top_of_book: document.getElementById("coarseAllowTop").checked,
        coarse_min_candidate_levels: parseInt(document.getElementById("coarseMinCands").value || "1", 10),
        fine_safe_band_min: parseFloat(document.getElementById("fineSafeMin").value || "0.4"),
        fine_safe_band_max: parseFloat(document.getElementById("fineSafeMax").value || "0.6"),
        fine_target_band_ratio: parseFloat(document.getElementById("fineTargetRatio").value || "0.5")
      }};
      const res = await fetch("/api/set_rule", {{
        method: "POST",
        headers: {{ "Content-Type": "application/json" }},
        body: JSON.stringify(payload)
      }});
      const out = await res.json();
      if (!out.ok) {{
        alert("save failed: " + (out.message || "unknown"));
        return;
      }}
      document.getElementById("rulePanel").style.display = "none";
      rulePanelOpen = false;
      window.location.reload();
    }});
    bindRuleButtons();
    setInterval(() => {{
      if (Date.now() < pauseRefreshUntilMs) return;
      if (document.hidden) return;
      if (!rulePanelOpen) window.location.reload();
    }}, 1000);
  </script>
</body>
</html>"##,
        data.process_started_at.to_rfc3339(),
        data.updated_at.to_rfc3339(),
        data.server_memory_used_mb,
        data.server_memory_total_mb,
        data.server_memory_usage_pct,
        data.clob_latency_ms
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".to_string()),
        data.open_orders_count,
        if let Some(err) = &data.order_poll_last_error {
            format!(
                "error at {}: {}",
                data.order_poll_last_error_at
                    .map(|t| t.to_rfc3339())
                    .unwrap_or_else(|| "-".to_string()),
                err
            )
        } else {
            format!(
                "ok count={} at {}",
                data.order_poll_last_count
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                data.order_poll_last_ok_at
                    .map(|t| t.to_rfc3339())
                    .unwrap_or_else(|| "-".to_string())
            )
        },
        gate_status,
        rows_html
    )
}

async fn page(State(state): State<DashboardAppState>) -> impl IntoResponse {
    let snapshot = state.dashboard.snapshot().await;
    Html(render_html(&snapshot))
}

pub async fn run_dashboard_server(bind_addr: String, dashboard: DashboardStateHandle, engine_tx: Sender<EngineEvent>) {
    let state = DashboardAppState { dashboard, engine_tx };
    let app = Router::new()
        .route("/", get(page))
        .route("/api/state", get(api_state))
        .route("/api/set_rule", post(api_set_rule))
        .with_state(state);

    let listener = match tokio::net::TcpListener::bind(&bind_addr).await {
        Ok(v) => v,
        Err(err) => {
            warn!("dashboard bind failed addr={} err={}", bind_addr, err);
            return;
        }
    };
    info!("dashboard listening on http://{}", bind_addr);
    if let Err(err) = axum::serve(listener, app).await {
        warn!("dashboard server stopped err={}", err);
    }
}

pub fn read_memory_mb_from_proc() -> Option<(f64, f64)> {
    let raw = std::fs::read_to_string("/proc/meminfo").ok()?;
    let mut total_kb = None::<f64>;
    let mut avail_kb = None::<f64>;
    for line in raw.lines() {
        if line.starts_with("MemTotal:") {
            let v = line.split_whitespace().nth(1)?.parse::<f64>().ok()?;
            total_kb = Some(v);
        } else if line.starts_with("MemAvailable:") {
            let v = line.split_whitespace().nth(1)?.parse::<f64>().ok()?;
            avail_kb = Some(v);
        }
    }
    let total = total_kb?;
    let avail = avail_kb?;
    let used = (total - avail).max(0.0);
    Some((used / 1024.0, total / 1024.0))
}
