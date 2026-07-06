//! `sluss dash` — read-only terminal dashboard over the audit store.
//! Re-queries every 2s; `q` or Esc quits. Charts are plain ratatui widgets
//! over SQL aggregates — no analytics engine, the data is small.

mod value;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Bar, BarChart, BarGroup, Block, Paragraph, Row, Sparkline, Table};
use ratatui::Frame;
use sluss_audit::{AuditStore, DecisionRow};

use value::ValueStats;

struct Data {
    events_hourly: Vec<u64>,
    per_repo: Vec<(String, u64)>,
    verdicts: Vec<(String, u64)>,
    stats: ValueStats,
    recent: Vec<DecisionRow>,
    total_events: u64,
}

pub fn run() -> Result<()> {
    let store = AuditStore::open(crate::db_path()?)?;
    let mut terminal = ratatui::init();
    let mut data = load(&store)?;
    let mut last_refresh = Instant::now();

    let result = loop {
        if let Err(err) = terminal.draw(|frame| draw(frame, &data)) {
            break Err(err.into());
        }
        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                    break Ok(());
                }
            }
        }
        if last_refresh.elapsed() > Duration::from_secs(2) {
            data = load(&store)?;
            last_refresh = Instant::now();
        }
    };
    ratatui::restore();
    result
}

fn load(store: &AuditStore) -> Result<Data> {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before 1970")
        .as_millis() as i64;
    Ok(Data {
        events_hourly: store.events_per_hour(48, now_ms)?,
        per_repo: store.decisions_per_repo()?,
        verdicts: store.verdict_breakdown()?,
        stats: ValueStats::compute(&store.decision_outcomes(500)?, now_ms),
        recent: store.recent_decisions(50)?,
        total_events: store.event_count()?,
    })
}

fn draw(frame: &mut Frame, data: &Data) {
    let [header, mid, lower, table] = Layout::vertical([
        Constraint::Length(4),
        Constraint::Length(9),
        Constraint::Length(8),
        Constraint::Min(5),
    ])
    .areas(frame.area());

    draw_header(frame, header, data);
    let [repos, verdicts] = Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(mid);
    draw_bars(frame, repos, "decisions per repo", &data.per_repo, Color::Cyan);
    draw_bars(frame, verdicts, "verdicts", &data.verdicts, Color::Magenta);
    let [latency, velocity] = Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(lower);
    draw_bars(frame, latency, "pipeline time", &data.stats.latency_buckets, Color::Yellow);
    draw_value(frame, velocity, &data.stats);
    draw_recent(frame, table, &data.recent);
}

fn draw_header(frame: &mut Frame, area: Rect, data: &Data) {
    let block = Block::bordered().title(format!(
        " sluss — {} events · {} decisions ",
        data.total_events, data.stats.decisions
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [line, spark] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);
    frame.render_widget(
        Paragraph::new("event volume, last 48h (1 bar = 1h) · q to quit"),
        line,
    );
    frame.render_widget(
        Sparkline::default()
            .data(&data.events_hourly)
            .style(Style::default().fg(Color::Green)),
        spark,
    );
}

fn draw_bars(frame: &mut Frame, area: Rect, title: &str, items: &[(String, u64)], color: Color) {
    let bars: Vec<Bar> = items
        .iter()
        .take(8)
        .map(|(label, n)| {
            Bar::default()
                .label(shorten(label, (area.width as usize / items.len().clamp(1, 8)).saturating_sub(2)))
                .value(*n)
        })
        .collect();
    let chart = BarChart::default()
        .block(Block::bordered().title(format!(" {title} ")))
        .bar_width(((area.width.saturating_sub(4)) / bars.len().max(1) as u16).clamp(3, 16))
        .bar_style(Style::default().fg(color))
        .value_style(Style::default().add_modifier(Modifier::BOLD))
        .data(BarGroup::default().bars(&bars));
    frame.render_widget(chart, area);
}

fn draw_value(frame: &mut Frame, area: Rect, stats: &ValueStats) {
    let text = format!(
        "velocity     {:.1} decisions/day (7d)\n\
         avg conf     {:.2}\n\
         p50 / p95    {} / {}\n\
         value        {:.1} pts ({:.1} pts/h of bot time)\n\
         \n\
         value = Σ decisiveness × confidence\n\
         (approve/req-changes 1.0 · comment 0.3)",
        stats.per_day_7d,
        stats.avg_confidence,
        fmt_ms(stats.p50_ms),
        fmt_ms(stats.p95_ms),
        stats.total_value,
        stats.value_per_hour,
    );
    frame.render_widget(
        Paragraph::new(text).block(Block::bordered().title(" velocity & value ")),
        area,
    );
}

fn draw_recent(frame: &mut Frame, area: Rect, recent: &[DecisionRow]) {
    let rows: Vec<Row> = recent
        .iter()
        .map(|d| {
            let color = match d.verdict.as_str() {
                "approve" => Color::Green,
                "request_changes" => Color::Red,
                _ => Color::Yellow,
            };
            Row::new(vec![
                value::fmt_time(d.at_unix_ms),
                format!("{}#{}", d.repo, d.number),
                d.verdict.clone(),
                format!("{:.2}", d.confidence),
                shorten(&d.summary, 80),
            ])
            .style(Style::default().fg(color))
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(16),
            Constraint::Length(24),
            Constraint::Length(15),
            Constraint::Length(5),
            Constraint::Min(20),
        ],
    )
    .header(Row::new(vec!["when", "change", "verdict", "conf", "summary"]).style(Style::default().add_modifier(Modifier::BOLD)))
    .block(Block::bordered().title(" recent decisions "));
    frame.render_widget(table, area);
}

fn shorten(s: &str, max: usize) -> String {
    if max < 2 || s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

fn fmt_ms(ms: i64) -> String {
    if ms >= 60_000 {
        format!("{:.1}m", ms as f64 / 60_000.0)
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}
