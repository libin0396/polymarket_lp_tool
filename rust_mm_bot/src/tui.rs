use crate::dashboard::DashboardStateHandle;
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use ratatui::Terminal;
use std::io;
use std::time::Duration;
use tracing::{info, warn};

pub async fn run_tui(state: DashboardStateHandle) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    info!("tui started (press q to quit ui)");

    loop {
        let snap = state.snapshot().await;
        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(6), Constraint::Min(10)])
                .split(f.area());

            let header = vec![
                Line::from(format!(
                    "Open Orders: {} | Mem: {:.1}/{:.1} MB ({:.1}%) | CLOB Latency: {} ms",
                    snap.open_orders_count,
                    snap.server_memory_used_mb,
                    snap.server_memory_total_mb,
                    snap.server_memory_usage_pct,
                    snap.clob_latency_ms
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "-".to_string())
                )),
                Line::from(format!(
                    "Order Poll: {}",
                    if let Some(err) = &snap.order_poll_last_error {
                        format!(
                            "error at {}: {}",
                            snap.order_poll_last_error_at
                                .map(|t| t.format("%H:%M:%S").to_string())
                                .unwrap_or_else(|| "-".to_string()),
                            err
                        )
                    } else {
                        format!(
                            "ok count={} at {}",
                            snap.order_poll_last_count
                                .map(|v| v.to_string())
                                .unwrap_or_else(|| "-".to_string()),
                            snap.order_poll_last_ok_at
                                .map(|t| t.format("%H:%M:%S").to_string())
                                .unwrap_or_else(|| "-".to_string())
                        )
                    }
                )),
                Line::from(format!(
                    "Updated: {} | Started: {}",
                    snap.updated_at, snap.process_started_at
                )),
                Line::from("Columns: market outcome price size mode rule regime last_check"),
                Line::from("Press q to exit TUI (bot keeps running if not interrupted)."),
            ];
            let p = Paragraph::new(header).block(
                Block::default()
                    .title("Polymarket Rust Bot TUI")
                    .borders(Borders::ALL),
            );
            f.render_widget(p, chunks[0]);

            let rows = snap.rows.iter().take(40).map(|r| {
                let check = r
                    .last_check_at
                    .map(|t| t.format("%H:%M:%S").to_string())
                    .unwrap_or_else(|| "-".to_string());
                let rule = if r.pricing_rule.len() > 40 {
                    format!("{}...", &r.pricing_rule[..40])
                } else {
                    r.pricing_rule.clone()
                };
                Row::new(vec![
                    Cell::from(r.market_title.clone()),
                    Cell::from(r.outcome_label.clone()),
                    Cell::from(format!("{:.4}", r.order_price)),
                    Cell::from(format!("{:.2}", r.size)),
                    Cell::from(r.pricing_mode.clone()),
                    Cell::from(rule),
                    Cell::from(r.tick_regime.clone()),
                    Cell::from(check),
                ])
            });

            let t = Table::new(
                rows,
                [
                    Constraint::Percentage(25),
                    Constraint::Length(8),
                    Constraint::Length(8),
                    Constraint::Length(8),
                    Constraint::Length(8),
                    Constraint::Percentage(35),
                    Constraint::Length(9),
                    Constraint::Length(10),
                ],
            )
            .header(
                Row::new([
                    "market",
                    "outcome",
                    "price",
                    "size",
                    "mode",
                    "rule",
                    "regime",
                    "checked",
                ])
                .style(Style::default().fg(Color::Yellow)),
            )
            .block(Block::default().title("Orders").borders(Borders::ALL));
            f.render_widget(t, chunks[1]);
        })?;

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(k) = event::read()? {
                if k.code == KeyCode::Char('q') {
                    break;
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    if let Err(err) = disable_raw_mode() {
        warn!("disable_raw_mode failed: {}", err);
    }
    if let Err(err) = execute!(terminal.backend_mut(), LeaveAlternateScreen) {
        warn!("leave alt screen failed: {}", err);
    }
    if let Err(err) = terminal.show_cursor() {
        warn!("show cursor failed: {}", err);
    }
    Ok(())
}
