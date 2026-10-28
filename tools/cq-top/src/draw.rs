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
    pub bg: Option<Color>,
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

/// The widest name in a name column, so names are not cut: at least the header, at most 24.
fn name_width<'a>(header: &str, names: impl Iterator<Item = &'a str>) -> u16 {
    names
        .map(|n| n.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(header.len(), 24) as u16
}

pub fn draw(f: &mut Frame, root: &Path, s: &Snapshot, bodies: bool, screen: Screen, t: &Theme) {
    if let Some(bg) = t.bg {
        f.render_widget(Block::new().style(t.text.bg(bg)), f.area());
    }
    match screen {
        Screen::Overview => draw_overview(f, root, s, bodies, t),
        Screen::Flow => draw_flow(f, root, s, bodies, t),
        Screen::Rejected => draw_rejected(f, root, s, bodies, t),
        Screen::Sessions => draw_sessions(f, root, s, bodies, t),
    }
}

fn header(
    f: &mut Frame,
    area: Rect,
    root: &Path,
    s: &Snapshot,
    bodies: bool,
    screen: Screen,
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
            tab(" 1 Overview ", screen == Screen::Overview),
            tab(" 2 Flow ", screen == Screen::Flow),
            tab(" 3 Rejected ", screen == Screen::Rejected),
            tab(" 4 Sessions ", screen == Screen::Sessions),
            Span::styled(
                format!(
                    "  bodies:{} [b]  theme:{} [t]  [q] quit",
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
    header(f, head, root, s, bodies, Screen::Flow, t);

    // Messages sent per time slice over the last hour; one slice per column.
    let n = spark.width.saturating_sub(2).max(1) as usize;
    let slices = slices(s, n);
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
    let bars: Vec<Bar> = pairs(s)
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

    // Task -> reply turnaround per worker.
    let bars: Vec<Bar> = turnaround(s)
        .into_iter()
        .map(|(w, replies, med)| {
            Bar::default()
                .label(format!("{w} ({replies})"))
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
        let busy = w.busy();
        let style = match busy {
            Busy::Stale => t.bad,
            Busy::Idle => t.dim,
            Busy::Backlog => t.warn,
            Busy::Working => t.info,
        };
        f.render_widget(
            LineGauge::default()
                .label(Span::styled(
                    format!(
                        "{:<8} {:<8} claimed {} queued {}",
                        w.name,
                        busy.label(),
                        w.claimed,
                        w.queued
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

    header(f, head, root, s, bodies, Screen::Overview, t);

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
    let name_w = name_width("watcher", s.watchers.iter().map(|w| w.name.as_str()));
    f.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(name_w),
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
            Cell::from(m.from.clone()),
            Cell::from(m.to.clone()),
            Cell::from(m.kind.clone()),
            Cell::from(state).style(style),
            Cell::from(text),
        ])
    });
    // The subject has the lowest priority: in a narrow terminal it shrinks first.
    Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Length(name_width("from", msgs.iter().map(|m| m.from.as_str()))),
            Constraint::Length(name_width("to", msgs.iter().map(|m| m.to.as_str()))),
            Constraint::Length(7),
            Constraint::Length(if state_col { 10 } else { 14 }),
            Constraint::Fill(1),
        ],
    )
    .header(Row::new(["age", "from", "to", "type", "state", "subject"]).style(t.table_header()))
    .style(t.text)
    .block(t.block(title))
}

/// Rejected and dead-lettered messages, newest first, with the reason from each sidecar.
/// Bodies are never shown here, whatever `b` says.
fn draw_rejected(f: &mut Frame, root: &Path, s: &Snapshot, bodies: bool, t: &Theme) {
    let [head, table] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(4)]).areas(f.area());
    header(f, head, root, s, bodies, Screen::Rejected, t);

    let from_w = name_width("from", s.problems.iter().map(|m| m.from.as_str()));
    let to_w = name_width("to", s.problems.iter().map(|m| m.to.as_str()));
    // Subject and reason share what is left after the borders, the fixed columns and the
    // six one-cell gaps; each wraps onto up to three lines.
    let fixed = 2 + 8 + 10 + from_w + to_w + 7 + 6;
    let text_w = (table.width.saturating_sub(fixed) / 2).max(8) as usize;

    let widths = [
        Constraint::Length(8),
        Constraint::Length(10),
        Constraint::Length(from_w),
        Constraint::Length(to_w),
        Constraint::Length(7),
        Constraint::Length(text_w as u16),
        Constraint::Fill(1),
    ];

    // Border and header take three rows; list rows while they fit.
    let mut room = table.height.saturating_sub(3);
    let mut rows = Vec::new();
    for m in &s.problems {
        let (style, glyph) = t.bucket(m.bucket);
        let subject = wrap(&m.subject, text_w, 3);
        let (reason, reason_style) = if m.reason.is_empty() {
            (vec!["(no reason file)".to_string()], t.dim)
        } else {
            (wrap(&m.reason, text_w, 3), t.text)
        };
        let height = subject.len().max(reason.len()) as u16;
        if height > room {
            break;
        }
        room -= height;
        rows.push(
            Row::new(vec![
                Cell::from(age(s.now - m.ts)),
                Cell::from(format!("{glyph} {}", m.bucket)).style(style),
                Cell::from(m.from.clone()),
                Cell::from(m.to.clone()),
                Cell::from(m.kind.clone()),
                Cell::from(subject.join("\n")),
                Cell::from(reason.join("\n")).style(reason_style),
            ])
            .height(height),
        );
    }
    let title = format!(
        " Rejected and dead-lettered, newest first: {} of {} ",
        rows.len(),
        s.problems_total
    );
    f.render_widget(
        Table::new(rows, widths)
            .header(
                Row::new(["age", "state", "from", "to", "type", "subject", "reason"])
                    .style(t.table_header()),
            )
            .style(t.text)
            .block(t.block(title)),
        table,
    );
}

/// The watcher whose process runs in a session's folder, for the cq columns. A session with
/// no watcher in its folder has none.
pub fn session_watcher<'a>(s: &'a Snapshot, row: &sessions::Row) -> Option<&'a Watcher> {
    let name = row.cq_name.as_deref()?;
    s.watchers.iter().find(|w| w.name == name)
}

/// The state shown for a session, and whether it is a warning (idle with cq messages).
pub fn session_state(s: &Snapshot, row: &sessions::Row) -> (sessions::State, bool) {
    let (waiting, claimed) = session_watcher(s, row).map_or((0, 0), |w| (w.queued, w.claimed));
    (row.state, sessions::warn(row.state, waiting, claimed))
}

/// Height of the CPU and Memory boxes, borders included.
const MACHINE_ROWS: u16 = 9;
/// Narrower than this, a box shows its figures without the graph.
const GRAPH_MIN: u16 = 12;

/// The CPU and Memory boxes side by side: figures on the left, and on the right a graph of
/// the whole computer with Claude's share drawn over it in a second colour.
fn draw_machine(f: &mut Frame, area: Rect, r: &sessions::Report, t: &Theme) {
    let m = match (&r.machine, r.ready) {
        (Some(m), true) => m,
        (_, ready) => {
            let line = if ready {
                Line::styled(
                    "Process data unavailable: CPU and memory show -, and the state uses file activity only.",
                    t.warn,
                )
            } else {
                Line::styled("Collecting sessions…", t.dim)
            };
            f.render_widget(Paragraph::new(line).block(t.block(" This computer ")), area);
            return;
        }
    };
    let [cpu, mem] = Layout::horizontal([Constraint::Ratio(1, 2); 2]).areas(area);
    let now = m.sample();
    let series = |get: fn(&sessions::Sample) -> f32| r.history.iter().map(get).collect::<Vec<_>>();
    machine_box(
        f,
        cpu,
        " CPU ",
        &m.cpu_figures(),
        (now.cpu, now.claude_cpu),
        (series(|s| s.cpu), series(|s| s.claude_cpu)),
        t,
    );
    machine_box(
        f,
        mem,
        " Memory ",
        &m.mem_figures(),
        (now.mem, now.claude_mem),
        (series(|s| s.mem), series(|s| s.claude_mem)),
        t,
    );
}

/// One box. `now` and `history` are (whole computer, Claude), 0 to 100.
fn machine_box(
    f: &mut Frame,
    area: Rect,
    title: &str,
    figures: &[(&str, String)],
    now: (f32, f32),
    history: (Vec<f32>, Vec<f32>),
    t: &Theme,
) {
    use ratatui::widgets::{Axis, Chart, Dataset, GraphType};
    // The whole computer, and Claude's share of it.
    let (whole, claude) = (t.ok, t.alt);
    let block = t.block(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    // The first figure is the whole computer's, and the one labelled Claude is Claude's.
    let label_w = figures.iter().map(|(l, _)| l.len()).max().unwrap_or(0);
    let lines: Vec<Line> = figures
        .iter()
        .enumerate()
        .map(|(i, (label, value))| {
            let (mark, style) = match (i, *label) {
                (0, _) => ("■ ", whole),
                (_, "Claude") => ("■ ", claude),
                _ => ("  ", t.text),
            };
            Line::from(vec![
                Span::styled(mark, style),
                Span::styled(format!("{label:<label_w$}  "), t.dim),
                Span::styled(value.clone(), style),
            ])
        })
        .collect();
    let fig_w = lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 1;
    let top = inner.height.saturating_sub(lines.len() as u16) / 2;
    let (figs, graph) = if inner.width >= fig_w + 1 + GRAPH_MIN {
        let [a, _, b] = Layout::horizontal([
            Constraint::Length(fig_w),
            Constraint::Length(1),
            Constraint::Min(GRAPH_MIN),
        ])
        .areas(inner);
        (a, Some(b))
    } else {
        (inner, None)
    };
    let mut padded = vec![Line::default(); top as usize];
    padded.extend(lines);
    f.render_widget(Paragraph::new(padded), figs);
    let Some(graph) = graph else {
        return;
    };

    // With history, a scrolling graph that fills from the right.
    if history.0.len() >= 2 {
        let cap = sessions::HISTORY;
        let points = |v: &[f32]| {
            let start = cap.saturating_sub(v.len());
            v.iter()
                .enumerate()
                .map(|(i, y)| ((start + i) as f64, f64::from(*y)))
                .collect::<Vec<_>>()
        };
        let (a, b) = (points(&history.0), points(&history.1));
        let set = |data, style| {
            Dataset::default()
                .marker(symbols::Marker::Braille)
                .graph_type(GraphType::Bar)
                .style(style)
                .data(data)
        };
        f.render_widget(
            Chart::new(vec![set(&a, whole), set(&b, claude)])
                .x_axis(Axis::default().bounds([0.0, (cap - 1) as f64]))
                .y_axis(
                    Axis::default()
                        .bounds([0.0, 100.0])
                        .style(t.dim)
                        .labels([Span::styled("0%", t.dim), Span::styled("100%", t.dim)]),
                ),
            graph,
        );
        return;
    }

    // A one-shot export has no history: one bar for now, Claude's share first.
    let w = usize::from(graph.width);
    let cells = |v: f32| ((f64::from(v) / 100.0 * w as f64).round() as usize).min(w);
    let (all, mine) = (cells(now.0), cells(now.1).min(cells(now.0)));
    let full = t.bars.full;
    let bar = Line::from(vec![
        Span::styled(full.repeat(mine), claude),
        Span::styled(full.repeat(all - mine), whole),
        Span::styled("░".repeat(w - all), t.dim),
    ]);
    let mut rows = vec![Line::default(); usize::from(graph.height.saturating_sub(5) / 2)];
    rows.extend([bar.clone(), bar.clone(), bar]);
    rows.push(Line::default());
    rows.push(Line::styled(
        "now · the graph builds in the live view",
        t.dim,
    ));
    f.render_widget(Paragraph::new(rows), graph);
}

/// The Sessions screen: CPU and Memory boxes for the whole computer, then one row per Claude
/// Code session. Only counts, times, model names and titles are shown, never message text.
fn draw_sessions(f: &mut Frame, root: &Path, s: &Snapshot, bodies: bool, t: &Theme) {
    use sessions::{State, bytes, count, usd};
    let [head, strip, table] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(MACHINE_ROWS),
        Constraint::Min(4),
    ])
    .areas(f.area());
    header(f, head, root, s, bodies, Screen::Sessions, t);
    let r = &s.sessions;
    draw_machine(f, strip, r, t);

    // Columns: header, width, priority. A narrow terminal drops the highest numbers first.
    let name_w = name_width("session", r.rows.iter().map(|w| w.name.as_str()));
    let cols: [(&str, u16, u8); 14] = [
        ("session", name_w, 0),
        ("state", 9, 1),
        ("model", 10, 4),
        ("context", 7, 3),
        ("tokens", 7, 8),
        ("est.cost", 8, 9),
        ("cpu%", 5, 7),
        ("mem", 6, 13),
        ("sub", 3, 12),
        ("cq", 6, 5),
        ("wait", 4, 6),
        ("claim", 5, 11),
        ("last change", 13, 10),
        ("age", 7, 2),
    ];
    let room = table.width.saturating_sub(2);
    let mut by_prio: Vec<usize> = (0..cols.len()).collect();
    by_prio.sort_by_key(|&i| cols[i].2);
    let mut keep = vec![false; cols.len()];
    let mut used = 0u16;
    for i in by_prio {
        let need = cols[i].1 + u16::from(used > 0);
        if used + need <= room {
            keep[i] = true;
            used += need;
        }
    }
    let pick = |cells: Vec<Cell<'static>>| -> Vec<Cell<'static>> {
        cells
            .into_iter()
            .zip(&keep)
            .filter(|(_, k)| **k)
            .map(|(c, _)| c)
            .collect()
    };

    let dash = || Cell::from("-").style(t.dim);
    let rows: Vec<Row> = r
        .rows
        .iter()
        .map(|row| {
            let (state, warned) = session_state(s, row);
            let (glyph, style) = match state {
                _ if warned => ("▲", t.warn),
                State::Working => ("●", t.ok),
                State::Recent => ("◐", t.info),
                State::Idle => ("○", t.dim),
            };
            let w = session_watcher(s, row);
            let (beat, beat_style) = match w.map(|w| w.beat_age) {
                Some(Some(a)) if a <= STALE_AFTER => (age(a), t.ok),
                Some(Some(_)) => ("STALE".into(), t.bad),
                Some(None) => ("off".into(), t.dim),
                // No watcher runs in this folder.
                None => ("-".into(), t.dim),
            };
            let opt = |v: Option<String>| v.map_or_else(dash, Cell::from);
            Row::new(pick(vec![
                Cell::from(row.name.clone()).style(if warned { t.warn } else { t.accent }),
                Cell::from(format!("{glyph} {}", state.label())).style(style),
                if row.model.is_empty() {
                    dash()
                } else {
                    Cell::from(row.model.clone())
                },
                opt(row.context.map(count)),
                Cell::from(count(row.tokens_today)),
                Cell::from(usd(row.cost_today)),
                opt(row.cpu.map(|c| format!("{c:.1}"))),
                opt(row.mem.map(bytes)),
                Cell::from(row.subagents.to_string()).style(if row.subagents > 0 {
                    t.info
                } else {
                    t.dim
                }),
                Cell::from(beat).style(beat_style),
                opt(w.map(|w| w.queued.to_string())).style(if warned { t.warn } else { t.text }),
                opt(w.map(|w| w.claimed.to_string())),
                opt(row
                    .last_change
                    .map(|(c, at)| format!("{} {}", c.label(), age(s.now - at)))),
                Cell::from(age(s.now - row.active)),
            ]))
        })
        .collect();
    let widths: Vec<Constraint> = pick_widths(&cols.map(|c| c.1), &keep);
    let headers: Vec<&str> = cols
        .iter()
        .zip(&keep)
        .filter(|(_, k)| **k)
        .map(|(c, _)| c.0)
        .collect();
    let n = |st: State| r.rows.iter().filter(|row| row.state == st).count();
    let title = if r.ready {
        format!(
            " Sessions: {} ({} working, {} recent, {} idle). Tokens and cost since local midnight ",
            r.rows.len(),
            n(State::Working),
            n(State::Recent),
            n(State::Idle)
        )
    } else {
        " Sessions ".to_string()
    };
    let mut rows = rows;
    if r.ready && rows.is_empty() {
        rows.push(Row::new(vec![
            Cell::from(format!(
                "No sessions in the last 24 hours in {}",
                r.projects.display()
            ))
            .style(t.dim),
        ]));
    }
    f.render_widget(
        Table::new(rows, widths)
            .header(Row::new(headers).style(t.table_header()))
            .style(t.text)
            .block(t.block(title).title_bottom(Span::styled(
                " est.cost: API list prices, not your plan's bill · cpu% 100 = one core ",
                t.dim,
            ))),
        table,
    );
}

fn pick_widths(widths: &[u16], keep: &[bool]) -> Vec<Constraint> {
    widths
        .iter()
        .zip(keep)
        .filter(|(_, k)| **k)
        .map(|(w, _)| Constraint::Length(*w))
        .collect()
}

/// Word-wraps text to `width` characters and at most `max_lines`
/// lines. A word longer than a line is split; text that does not fit ends in an ellipsis.
fn wrap(text: &str, width: usize, max_lines: usize) -> Vec<String> {
    let width = width.max(2);
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut words: Vec<String> = text.split_whitespace().map(str::to_owned).collect();
    words.reverse();
    while let Some(word) = words.pop() {
        let (used, len) = (line.chars().count(), word.chars().count());
        if len > width {
            // Split a long word at the end of this line (or of a fresh one).
            let room = if used == 0 {
                width
            } else {
                width.saturating_sub(used + 1)
            };
            if room == 0 {
                lines.push(std::mem::take(&mut line));
                words.push(word);
                continue;
            }
            let head: String = word.chars().take(room).collect();
            let rest: String = word.chars().skip(room).collect();
            if used > 0 {
                line.push(' ');
            }
            line.push_str(&head);
            lines.push(std::mem::take(&mut line));
            words.push(rest);
        } else if used == 0 {
            line = word;
        } else if used + 1 + len <= width {
            line.push(' ');
            line.push_str(&word);
        } else {
            lines.push(std::mem::replace(&mut line, word));
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        let last = &mut lines[max_lines - 1];
        let mut cut: String = last.chars().take(width - 1).collect();
        cut.push('…');
        *last = cut;
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::wrap;

    #[test]
    fn wrap_fits_words_splits_long_ones_and_marks_a_cut() {
        assert_eq!(
            wrap("sender not in scope", 10, 3),
            ["sender not", "in scope"]
        );
        assert_eq!(wrap("", 10, 3), [""]);
        assert_eq!(wrap("ab abcdefghijkl", 6, 3), ["ab abc", "defghi", "jkl"]);
        assert_eq!(wrap("one two three four", 5, 2), ["one", "two…"]);
        for line in wrap("x".repeat(100).as_str(), 7, 3) {
            assert!(line.chars().count() <= 7);
        }
    }
}
