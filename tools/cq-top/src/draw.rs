//! Rendering. Every colour, border and glyph comes from a `Theme`, so a theme can differ in
//! approach (palette, borders, bar glyphs, how state is shown), not only in colour.

use super::*;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{Block, BorderType, Cell, Paragraph, Row, Table},
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
    let mut spans = vec![
        Span::styled(" cq-top ", t.head),
        Span::styled(format!(" {}  ", root.display()), t.text),
        Span::styled(state, style),
        Span::raw("  "),
    ];
    // One tab per screen, in tab order; the number is its key.
    spans.extend(Screen::ALL.iter().enumerate().map(|(i, &tab)| {
        Span::styled(
            format!(" {} {} ", i + 1, tab.title()),
            if tab == screen { t.head } else { t.dim },
        )
    }));
    spans.push(Span::styled(
        format!(
            "  bodies:{} [b]  theme:{} [t]  [q] quit",
            if bodies { "on" } else { "off" },
            t.name
        ),
        t.dim,
    ));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The Flow screen, laid out like the Sessions screen: a box with figures on the left and a
/// graph on the right, then tables with a header row. Only names, counts and times are shown.
fn draw_flow(f: &mut Frame, root: &Path, s: &Snapshot, bodies: bool, t: &Theme) {
    let rows = workers(s);
    let [head, sent, talk, load] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(MACHINE_ROWS),
        Constraint::Min(5),
        // Borders, caption and header take four rows.
        Constraint::Length(rows.len().max(1) as u16 + 4),
    ])
    .areas(f.area());
    header(f, head, root, s, bodies, Screen::Flow, t);
    draw_sent(f, sent, s, t);
    draw_pairs(f, talk, s, t);
    draw_workers(f, load, s, &rows, t);
}

/// Minutes in the "Messages sent" graph: one point per minute of the last hour.
const SENT_MINUTES: usize = 60;

/// Messages sent in the last hour: figures on the left, messages per minute on the right.
fn draw_sent(f: &mut Frame, area: Rect, s: &Snapshot, t: &Theme) {
    use ratatui::widgets::{Axis, Chart, Dataset, GraphType};
    let per_min = slices(s, SENT_MINUTES);
    let peak = per_min.iter().copied().max().unwrap_or(0);
    let total = s.window.len();
    let last5: u64 = per_min[SENT_MINUTES - 5..].iter().sum();
    let figures = vec![
        ("Total", total.to_string(), Some(t.info)),
        ("Last 5 min", last5.to_string(), None),
        ("Peak", format!("{peak}/min"), None),
        ("Average", format!("{:.1}/min", total as f64 / 60.0), None),
    ];
    let block = t
        .block(" Messages sent, last hour ")
        .title_bottom(Span::styled(
            " graph: messages per minute, oldest left, now right ",
            t.dim,
        ));
    let Some(graph) = figures_box(f, area, block, figures, t) else {
        return;
    };
    // An empty minute draws nothing, so the graph has no baseline of dots.
    let points: Vec<(f64, f64)> = per_min
        .iter()
        .enumerate()
        .filter(|(_, n)| **n > 0)
        .map(|(i, n)| (i as f64, *n as f64))
        .collect();
    let top = peak.max(1);
    f.render_widget(
        Chart::new(vec![
            Dataset::default()
                .marker(symbols::Marker::Braille)
                .graph_type(GraphType::Bar)
                .style(t.info)
                .data(&points),
        ])
        .x_axis(Axis::default().bounds([0.0, (SENT_MINUTES - 1) as f64]))
        .y_axis(
            Axis::default()
                .bounds([0.0, top as f64])
                .style(t.dim)
                .labels([
                    Span::styled("0", t.dim),
                    Span::styled(format!("{top}/min"), t.dim),
                ]),
        ),
        graph,
    );
}

/// The narrowest and widest a bar column may be.
const BAR_MIN: u16 = 6;
const BAR_MAX: u16 = 30;

/// Who talks to whom: one row per sender and receiver, with the count and the newest age.
fn draw_pairs(f: &mut Frame, area: Rect, s: &Snapshot, t: &Theme) {
    let pairs = pairs(s);
    // Border and header take three rows; list the busiest pairs that fit.
    let shown = &pairs[..pairs.len().min(usize::from(area.height.saturating_sub(3)))];
    let most = pairs.first().map_or(1, |p| p.count.max(1));
    let cols: [(&str, u16, u8); 5] = [
        (
            "from",
            name_width("from", shown.iter().map(|p| p.from.as_str())),
            0,
        ),
        (
            "to",
            name_width("to", shown.iter().map(|p| p.to.as_str())),
            0,
        ),
        ("messages", 8, 1),
        ("last", 6, 2),
        // A bar scaled to the busiest pair; it takes what is left, up to `BAR_MAX`.
        ("", BAR_MIN, 3),
    ];
    let room = area.width.saturating_sub(2);
    let keep = fit_columns(&cols, room);
    let bar_w = (room.saturating_sub(used_width(&cols, &keep)) + BAR_MIN).min(BAR_MAX);
    let rows: Vec<Row> = shown
        .iter()
        .map(|p| {
            Row::new(kept(
                [
                    Cell::from(p.from.clone()).style(t.accent),
                    Cell::from(p.to.clone()).style(t.accent),
                    Cell::from(p.count.to_string()),
                    Cell::from(age(s.now - p.last)),
                    Cell::from(bar(p.count as f64 / most as f64, bar_w, t.info, t)),
                ],
                &keep,
            ))
        })
        .collect();
    let title = if shown.len() < pairs.len() {
        format!(
            " Who talks to whom, last hour: {} of {} pairs ",
            shown.len(),
            pairs.len()
        )
    } else {
        format!(" Who talks to whom, last hour: {} pairs ", pairs.len())
    };
    let widths = kept(cols.map(|c| c.1), &keep);
    let widths = widths
        .iter()
        .zip(kept(0..cols.len(), &keep))
        .map(|(w, i)| Constraint::Length(if i == 4 { bar_w } else { *w }));
    let empty = rows.is_empty();
    f.render_widget(
        Table::new(rows, widths)
            .header(Row::new(kept(cols.map(|c| c.0), &keep)).style(t.table_header()))
            .style(t.text)
            .block(t.block(title)),
        area,
    );
    if empty {
        empty_note(f, area, 2, "No messages in the last hour.", t);
    }
}

/// A dim note on the first row under a table's header, across the whole table: a row of
/// cells would cut it to the first column's width. `skip` is the rows above it in `area`.
fn empty_note(f: &mut Frame, area: Rect, skip: u16, note: &'static str, t: &Theme) {
    if area.height > skip + 1 && area.width > 2 {
        let line = Rect::new(area.x + 1, area.y + skip, area.width - 2, 1);
        f.render_widget(Paragraph::new(Line::styled(note, t.dim)), line);
    }
}

/// One row per worker: its watcher's state and load now, and its replies in the last hour.
fn draw_workers(f: &mut Frame, area: Rect, s: &Snapshot, rows: &[Worker], t: &Theme) {
    let block = t.block(" Workers: load now, replies in the last hour ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [caption, table] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);
    f.render_widget(
        Paragraph::new(Line::styled(
            format!(
                "load: queued + claimed, full at {LOAD_FULL} · median: task sent to reply sent \
                 · state: from the heartbeat"
            ),
            t.dim,
        )),
        caption,
    );

    let cols: [(&str, u16, u8); 8] = [
        (
            "worker",
            name_width("worker", rows.iter().map(|w| w.name.as_str())),
            0,
        ),
        ("state", 6, 1),
        ("mode", 5, 6),
        ("queued", 6, 2),
        ("claimed", 7, 3),
        ("load", 8, 4),
        ("median task→reply", 17, 5),
        ("replies", 7, 7),
    ];
    let keep = fit_columns(&cols, table.width);
    let dash = || Cell::from("-").style(t.dim);
    let lines: Vec<Row> = rows
        .iter()
        .map(|r| {
            let w = r.watcher;
            let (state, state_style) = match w {
                Some(w) if w.asleep => ("asleep", t.info),
                Some(w) => match w.beat_age {
                    Some(a) if a <= STALE_AFTER => ("live", t.ok),
                    Some(_) => ("STALE", t.bad),
                    None => ("off", t.dim),
                },
                // Replied in the last hour, but has no watcher.
                None => ("-", t.dim),
            };
            let load = w.map(|w| {
                let style = match w.busy() {
                    Busy::Asleep | Busy::Idle => t.dim,
                    Busy::Stale => t.bad,
                    Busy::Backlog => t.warn,
                    Busy::Working => t.info,
                };
                let ratio = (w.queued + w.claimed) as f64 / LOAD_FULL as f64;
                bar(ratio, cols[5].1, style, t)
            });
            Row::new(kept(
                [
                    Cell::from(r.name.clone()).style(t.accent),
                    Cell::from(state).style(state_style),
                    match w.and_then(|w| s.mode(w)) {
                        Some(Mode::Monitor) => Cell::from("mon"),
                        Some(m) => Cell::from(mode_label(m)).style(t.info),
                        None => dash(),
                    },
                    w.map_or_else(dash, |w| Cell::from(w.queued.to_string())),
                    w.map_or_else(dash, |w| Cell::from(w.claimed.to_string())),
                    load.map_or_else(dash, Cell::from),
                    r.median.map_or_else(dash, |m| Cell::from(age(m))),
                    Cell::from(r.replies.to_string()).style(if r.replies > 0 {
                        t.text
                    } else {
                        t.dim
                    }),
                ],
                &keep,
            ))
        })
        .collect();
    f.render_widget(
        Table::new(lines, kept(cols.map(|c| Constraint::Length(c.1)), &keep))
            .header(Row::new(kept(cols.map(|c| c.0), &keep)).style(t.table_header()))
            .style(t.text),
        table,
    );
    if rows.is_empty() {
        // `table` has no border; the note goes on its second row.
        let wide = Rect::new(
            table.x.saturating_sub(1),
            table.y,
            table.width + 2,
            table.height,
        );
        empty_note(f, wide, 1, "No watchers.", t);
    }
}

/// A bar `width` cells wide, filled to `ratio` (0 to 1) in `style`, the rest dim.
fn bar(ratio: f64, width: u16, style: Style, t: &Theme) -> Line<'static> {
    let w = usize::from(width);
    let filled = ((ratio.clamp(0.0, 1.0) * w as f64).round() as usize).min(w);
    Line::from(vec![
        Span::styled(t.gauge_filled.repeat(filled), style),
        Span::styled(t.gauge_empty.repeat(w - filled), t.dim),
    ])
}

/// Which columns fit in `room` cells. Each column is (header, width, priority), one cell
/// apart; a narrow terminal drops the highest priority numbers first.
fn fit_columns(cols: &[(&str, u16, u8)], room: u16) -> Vec<bool> {
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
    keep
}

/// The cells the kept columns take, gaps included.
fn used_width(cols: &[(&str, u16, u8)], keep: &[bool]) -> u16 {
    let widths = kept(cols.iter().map(|c| c.1), keep);
    widths.iter().sum::<u16>() + (widths.len() as u16).saturating_sub(1)
}

/// The items whose column is kept.
fn kept<T>(items: impl IntoIterator<Item = T>, keep: &[bool]) -> Vec<T> {
    items
        .into_iter()
        .zip(keep)
        .filter(|(_, k)| **k)
        .map(|(c, _)| c)
        .collect()
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
            _ if w.asleep => ("asleep".into(), t.info),
            Some(a) if a <= STALE_AFTER => (format!("live {}", age(a)), t.ok),
            Some(a) => (format!("STALE {}", age(a)), t.bad),
            None => ("none".into(), t.dim),
        };
        let mode = match s.mode(w) {
            Some(m) => {
                Cell::from(mode_label(m)).style(if m == Mode::Monitor { t.text } else { t.info })
            }
            None => Cell::from("-").style(t.dim),
        };
        Row::new(vec![
            Cell::from(w.name.clone()).style(t.accent),
            Cell::from(txt).style(style),
            mode,
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
                Constraint::Length(7),
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
                "mode",
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

/// The watcher whose process runs in a session's folder, for the cq columns. Without one, a
/// sleeping name last seen in the folder, or whose `members/` record names the folder. A
/// session with neither has none.
pub fn session_watcher<'a>(s: &'a Snapshot, row: &sessions::Row) -> Option<&'a Watcher> {
    if let Some(name) = row.cq_name.as_deref() {
        return s.watchers.iter().find(|w| w.name == name);
    }
    let asleep = || s.watchers.iter().filter(|w| w.asleep);
    asleep()
        .find(|w| row.cq_last.as_deref() == Some(w.name.as_str()))
        .or_else(|| asleep().find(|w| w.folder.as_deref() == Some(row.folder_key.as_str())))
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

/// A box with figures on the left: (label, value, graph colour). A figure with a colour gets
/// a `■` mark in it, as a legend for the graph. Returns the room for the graph on the right,
/// or `None` when the box is too narrow for one.
fn figures_box(
    f: &mut Frame,
    area: Rect,
    block: Block<'static>,
    figures: Vec<(&str, String, Option<Style>)>,
    t: &Theme,
) -> Option<Rect> {
    let inner = block.inner(area);
    f.render_widget(block, area);
    let label_w = figures.iter().map(|(l, ..)| l.len()).max().unwrap_or(0);
    let lines: Vec<Line> = figures
        .into_iter()
        .map(|(label, value, mark)| {
            let (mark, style) = match mark {
                Some(style) => ("■ ", style),
                None => ("  ", t.text),
            };
            Line::from(vec![
                Span::styled(mark, style),
                Span::styled(format!("{label:<label_w$}  "), t.dim),
                Span::styled(value, style),
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
    graph
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
    // The first figure is the whole computer's, and the one labelled Claude is Claude's.
    let figures = figures
        .iter()
        .enumerate()
        .map(|(i, (label, value))| {
            let mark = match (i, *label) {
                (0, _) => Some(whole),
                (_, "Claude") => Some(claude),
                _ => None,
            };
            (*label, value.clone(), mark)
        })
        .collect();
    let Some(graph) = figures_box(f, area, t.block(title), figures, t) else {
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
    let cols: [(&str, u16, u8); 15] = [
        ("session", name_w, 0),
        ("state", 9, 1),
        ("model", 10, 4),
        ("context", 7, 3),
        ("tokens", 7, 8),
        ("est.cost", 8, 9),
        ("cpu%", 5, 7),
        ("mem", 6, 14),
        ("sub", 3, 13),
        ("cq", 6, 5),
        // `inbox` or `mon` (monitor).
        ("mode", 5, 11),
        ("wait", 4, 6),
        ("claim", 5, 12),
        ("last change", 13, 10),
        ("age", 7, 2),
    ];
    let keep = fit_columns(&cols, table.width.saturating_sub(2));
    let pick = |cells: Vec<Cell<'static>>| kept(cells, &keep);

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
                _ if w.is_some_and(|w| w.asleep) => ("asleep".into(), t.info),
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
                match w.and_then(|w| s.mode(w)) {
                    Some(Mode::Inbox) => Cell::from("inbox").style(t.info),
                    Some(Mode::Codex) => Cell::from("codex").style(t.info),
                    Some(Mode::Monitor) => Cell::from("mon"),
                    None => dash(),
                },
                opt(w.map(|w| w.queued.to_string())).style(if warned { t.warn } else { t.text }),
                opt(w.map(|w| w.claimed.to_string())),
                opt(row
                    .last_change
                    .map(|(c, at)| format!("{} {}", c.label(), age(s.now - at)))),
                Cell::from(age(s.now - row.active)),
            ]))
        })
        .collect();
    let widths = kept(cols.map(|c| Constraint::Length(c.1)), &keep);
    let headers = kept(cols.map(|c| c.0), &keep);
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
