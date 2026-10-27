//! Rendering. Every colour, border and glyph comes from a `Theme`, so a theme can differ in
//! approach (palette, borders, bar glyphs, how state is shown), not only in colour.

use super::*;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{
        Bar, BarChart, BarGroup, Block, BorderType, Cell, LineGauge, Paragraph, Row, Sparkline,
        Table,
    },
};

pub struct Theme {
    pub name: &'static str,
    border: BorderType,
    /// Painted over the whole screen when set; `None` keeps the terminal's own background.
    bg: Option<Color>,
    text: Style,
    dim: Style,
    accent: Style,
    ok: Style,
    warn: Style,
    bad: Style,
    info: Style,
    alt: Style,
    /// Header badge and the active tab.
    head: Style,
    bars: symbols::bar::Set<'static>,
    gauge_filled: &'static str,
    gauge_empty: &'static str,
}

const AMBER: Color = Color::Rgb(255, 176, 0);
const AMBER_DIM: Color = Color::Rgb(160, 104, 0);
const AMBER_BG: Color = Color::Rgb(18, 11, 0);
const NEON_BG: Color = Color::Rgb(13, 17, 33);

pub static THEMES: [Theme; 3] = [
    // Classic: terminal background, one colour per meaning, plain single borders.
    Theme {
        name: "classic",
        border: BorderType::Plain,
        bg: None,
        text: Style::new(),
        dim: Style::new().fg(Color::DarkGray),
        accent: Style::new().add_modifier(Modifier::BOLD),
        ok: Style::new().fg(Color::Green),
        warn: Style::new().fg(Color::Yellow),
        bad: Style::new().fg(Color::Red),
        info: Style::new().fg(Color::Cyan),
        alt: Style::new().fg(Color::Magenta),
        head: Style::new().add_modifier(Modifier::REVERSED),
        bars: symbols::bar::NINE_LEVELS,
        gauge_filled: "─",
        gauge_empty: "─",
    },
    // Amber: a single-phosphor terminal. One hue only, so meaning comes from weight,
    // underline and inverse video; double borders; coarse three-level bars.
    Theme {
        name: "amber",
        border: BorderType::Double,
        bg: Some(AMBER_BG),
        text: Style::new().fg(AMBER),
        dim: Style::new().fg(AMBER_DIM),
        accent: Style::new().fg(AMBER).add_modifier(Modifier::BOLD),
        ok: Style::new().fg(AMBER),
        warn: Style::new().fg(AMBER).add_modifier(Modifier::BOLD),
        bad: Style::new()
            .fg(AMBER_BG)
            .bg(AMBER)
            .add_modifier(Modifier::BOLD),
        info: Style::new().fg(AMBER).add_modifier(Modifier::UNDERLINED),
        alt: Style::new().fg(AMBER_DIM).add_modifier(Modifier::ITALIC),
        head: Style::new()
            .fg(AMBER_BG)
            .bg(AMBER)
            .add_modifier(Modifier::BOLD),
        bars: symbols::bar::THREE_LEVELS,
        gauge_filled: "█",
        gauge_empty: "░",
    },
    // Neon: truecolor on deep navy, rounded borders, fine eighth-block bars, glowing accents.
    Theme {
        name: "neon",
        border: BorderType::Rounded,
        bg: Some(NEON_BG),
        text: Style::new().fg(Color::Rgb(200, 210, 255)),
        dim: Style::new().fg(Color::Rgb(86, 98, 140)),
        accent: Style::new()
            .fg(Color::Rgb(255, 64, 200))
            .add_modifier(Modifier::BOLD),
        ok: Style::new().fg(Color::Rgb(57, 255, 160)),
        warn: Style::new().fg(Color::Rgb(255, 222, 64)),
        bad: Style::new()
            .fg(Color::Rgb(255, 72, 96))
            .add_modifier(Modifier::BOLD),
        info: Style::new().fg(Color::Rgb(0, 224, 255)),
        alt: Style::new().fg(Color::Rgb(184, 124, 255)),
        head: Style::new()
            .fg(NEON_BG)
            .bg(Color::Rgb(255, 64, 200))
            .add_modifier(Modifier::BOLD),
        bars: symbols::bar::NINE_LEVELS,
        gauge_filled: "━",
        gauge_empty: "╌",
    },
];

impl Theme {
    fn block(&self, title: impl Into<String>) -> Block<'static> {
        Block::bordered()
            .border_type(self.border)
            .border_style(self.dim)
            .title(Span::styled(title.into(), self.accent))
            .style(self.text)
    }

    /// Style and glyph for where a message is in its life.
    fn bucket(&self, b: &str) -> (Style, &'static str) {
        match b {
            "queued" => (self.warn, "◌"),
            "claimed" => (self.info, "◐"),
            "done" => (self.ok, "●"),
            "rejected" => (self.alt, "✕"),
            _ => (self.bad, "!"),
        }
    }

    fn table_header(&self) -> Style {
        self.dim.add_modifier(Modifier::BOLD)
    }
}

fn age(secs: i64) -> String {
    let s = secs.max(0);
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m{:02}s", s / 60, s % 60),
        _ => format!("{}h{:02}m", s / 3600, s % 3600 / 60),
    }
}

fn median(v: &mut [i64]) -> i64 {
    v.sort_unstable();
    v[v.len() / 2]
}

pub fn draw(f: &mut Frame, root: &Path, s: &Snapshot, bodies: bool, flow: bool, t: &Theme) {
    if let Some(bg) = t.bg {
        f.render_widget(Block::new().style(t.text.bg(bg)), f.area());
    }
    if flow {
        draw_flow(f, root, s, bodies, t);
    } else {
        draw_overview(f, root, s, bodies, t);
    }
}

fn header(
    f: &mut Frame,
    area: Rect,
    root: &Path,
    s: &Snapshot,
    bodies: bool,
    flow: bool,
    t: &Theme,
) {
    let (state, style) = if s.paused {
        ("PAUSED", t.bad)
    } else {
        ("running", t.ok)
    };
    let tab = |label: &'static str, on: bool| Span::styled(label, if on { t.head } else { t.dim });
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" cq-top ", t.head),
            Span::styled(format!(" {}  ", root.display()), t.text),
            Span::styled(state, style),
            Span::raw("  "),
            tab(" 1 Overview ", !flow),
            tab(" 2 Flow ", flow),
            Span::styled(
                format!(
                    "  bodies:{} [b]  theme:{} [t]  [tab] switch  [q] quit",
                    if bodies { "on" } else { "off" },
                    t.name
                ),
                t.dim,
            ),
        ])),
        area,
    );
}

fn draw_flow(f: &mut Frame, root: &Path, s: &Snapshot, bodies: bool, t: &Theme) {
    let [head, spark, mid, busy] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(8),
        Constraint::Min(8),
        Constraint::Length(s.watchers.len() as u16 + 2),
    ])
    .areas(f.area());
    header(f, head, root, s, bodies, true, t);

    // Messages sent per time slice over the last hour; one slice per column.
    let n = spark.width.saturating_sub(2).max(1) as usize;
    let mut slices = vec![0u64; n];
    for m in &s.window {
        let ago = (s.now - m.ts).clamp(0, WINDOW_SECS - 1);
        slices[n - 1 - (ago as usize * n / WINDOW_SECS as usize)] += 1;
    }
    let peak = slices.iter().copied().max().unwrap_or(0);
    f.render_widget(
        Sparkline::default()
            .block(t.block(format!(
                " Messages sent, last hour: {} total, peak {} per {}s slice (oldest left, now right) ",
                s.window.len(),
                peak,
                WINDOW_SECS as usize / n
            )))
            .data(&slices)
            .bar_set(t.bars.clone())
            .style(t.info),
        spark,
    );

    let [pairs_area, turn_area] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(mid);

    // Who talks to whom.
    let mut pairs: BTreeMap<(String, String), u64> = BTreeMap::new();
    for m in &s.window {
        *pairs.entry((m.from.clone(), m.to.clone())).or_default() += 1;
    }
    let mut pairs: Vec<_> = pairs.into_iter().collect();
    pairs.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
    let bars: Vec<Bar> = pairs
        .iter()
        .take(12)
        .map(|((a, b), c)| {
            Bar::default()
                .label(format!("{a}>{b}"))
                .value(*c)
                .style(t.warn)
                .value_style(t.text)
        })
        .collect();
    f.render_widget(
        BarChart::default()
            .block(t.block(" Who talks to whom (messages, last hour) "))
            .direction(Direction::Horizontal)
            .bar_set(t.bars.clone())
            .bar_width(1)
            .bar_gap(0)
            .label_style(t.text)
            .data(BarGroup::default().bars(&bars)),
        pairs_area,
    );

    // Task -> reply turnaround per worker: reply send time minus task send time.
    let mut turn: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    for m in s.window.iter().filter(|m| m.kind == "result") {
        if let Some(task_ts) = parse_ts(&m.reply_to) {
            turn.entry(m.from.clone())
                .or_default()
                .push((m.ts - task_ts).max(0));
        }
    }
    let bars: Vec<Bar> = turn
        .iter_mut()
        .map(|(w, v)| {
            let med = median(v);
            Bar::default()
                .label(format!("{w} ({})", v.len()))
                .value(med as u64)
                .text_value(age(med))
                .style(t.ok)
                .value_style(t.text.add_modifier(Modifier::REVERSED))
        })
        .collect();
    f.render_widget(
        BarChart::default()
            .block(t.block(" Turnaround task to reply, median (replies) "))
            .bar_set(t.bars.clone())
            .bar_width(10)
            .bar_gap(2)
            .label_style(t.text)
            .data(BarGroup::default().bars(&bars)),
        turn_area,
    );

    // Backlog per worker right now: claimed + queued, full bar at 4 or more.
    let block = t.block(" Busy now (claimed + queued, full at 4) ");
    let inner = block.inner(busy);
    f.render_widget(block, busy);
    let rows = Layout::vertical(vec![Constraint::Length(1); s.watchers.len()]).split(inner);
    for (w, area) in s.watchers.iter().zip(rows.iter()) {
        let backlog = w.claimed + w.queued;
        let stale = w.beat_age.is_none_or(|a| a > STALE_AFTER);
        let (state, style) = match (stale, backlog) {
            (true, _) => ("STALE", t.bad),
            (false, 0) => ("idle", t.dim),
            _ if w.queued > 0 => ("backlog", t.warn),
            _ => ("working", t.info),
        };
        f.render_widget(
            LineGauge::default()
                .label(Span::styled(
                    format!(
                        "{:<8} {:<8} claimed {} queued {}",
                        w.name, state, w.claimed, w.queued
                    ),
                    style,
                ))
                .ratio((backlog as f64 / 4.0).min(1.0))
                .filled_symbol(t.gauge_filled)
                .unfilled_symbol(t.gauge_empty)
                .filled_style(style)
                .unfilled_style(t.dim),
            *area,
        );
    }
}

fn draw_overview(f: &mut Frame, root: &Path, s: &Snapshot, bodies: bool, t: &Theme) {
    let [head, watchers, flight, recent] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(s.watchers.len() as u16 + 3),
        Constraint::Percentage(35),
        Constraint::Min(5),
    ])
    .areas(f.area());

    header(f, head, root, s, bodies, false, t);

    let rows = s.watchers.iter().map(|w| {
        let (txt, style) = match w.beat_age {
            Some(a) if a <= STALE_AFTER => (format!("live {}", age(a)), t.ok),
            Some(a) => (format!("STALE {}", age(a)), t.bad),
            None => ("none".into(), t.dim),
        };
        Row::new(vec![
            Cell::from(w.name.clone()).style(t.accent),
            Cell::from(txt).style(style),
            Cell::from(w.pid.clone()),
            Cell::from(w.session.clone()),
            Cell::from(w.queued.to_string()),
            Cell::from(w.claimed.to_string()),
            Cell::from(w.done.to_string()),
            Cell::from(w.rejected.to_string()),
            Cell::from(w.dead.to_string()).style(if w.dead > 0 { t.bad } else { t.text }),
        ])
    });
    f.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(14),
                Constraint::Length(14),
                Constraint::Length(8),
                Constraint::Length(10),
                Constraint::Length(7),
                Constraint::Length(8),
                Constraint::Length(6),
                Constraint::Length(9),
                Constraint::Length(5),
            ],
        )
        .header(
            Row::new([
                "watcher",
                "heartbeat",
                "pid",
                "session",
                "queued",
                "claimed",
                "done",
                "rejected",
                "dead",
            ])
            .style(t.table_header()),
        )
        .style(t.text)
        .block(t.block(" Watchers ")),
        watchers,
    );

    f.render_widget(
        msg_table(
            t,
            &s.in_flight,
            s.now,
            bodies,
            " In flight (oldest first) ",
            true,
        ),
        flight,
    );
    f.render_widget(
        msg_table(t, &s.recent, s.now, bodies, " Recent ", false),
        recent,
    );
}

fn msg_table(
    t: &Theme,
    msgs: &[Msg],
    now: i64,
    bodies: bool,
    title: &str,
    state_col: bool,
) -> Table<'static> {
    let rows = msgs.iter().map(|m| {
        let text = if bodies {
            format!("{}  {}", m.subject, m.body.replace('\n', " "))
        } else {
            m.subject.clone()
        };
        let (style, glyph) = t.bucket(m.bucket);
        let state = if state_col || m.status.is_empty() {
            format!("{glyph} {}", m.bucket)
        } else {
            format!("{glyph} {} {}", m.bucket, m.status)
        };
        Row::new(vec![
            Cell::from(age(now - m.ts)),
            Cell::from(format!("{} -> {}", m.from, m.to)),
            Cell::from(m.kind.clone()),
            Cell::from(state).style(style),
            Cell::from(text),
        ])
    });
    Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Length(20),
            Constraint::Length(7),
            Constraint::Length(16),
            Constraint::Min(20),
        ],
    )
    .header(Row::new(["age", "from -> to", "type", "state", "subject"]).style(t.table_header()))
    .style(t.text)
    .block(t.block(title))
}
