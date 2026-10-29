//! The HTML dashboard: one snapshot as a single self-contained page. No scripts, no
//! external files; light and dark follow the viewer's system setting. Every string read
//! from the queue is escaped, and message bodies are never written.

use super::*;
use std::fmt::Write;

/// Slices in the "last hour" chart: 12 of 5 minutes.
const CHART_SLICES: usize = 12;
const PAIRS_SHOWN: usize = 12;
const PROBLEMS_SHOWN: usize = 20;
/// Reasons are free text from the session that declined the message; long ones are cut.
const REASON_MAX: usize = 200;

/// Escapes text for use in HTML text and in quoted attribute values.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

// ---------- UK local time ----------

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian.
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// 0 is Sunday.
fn weekday(epoch: i64) -> i64 {
    (epoch.div_euclid(86_400) + 4).rem_euclid(7)
}

/// 01:00 UTC on the last Sunday of a month that has 31 days.
fn last_sunday_1am(year: i64, month: i64) -> i64 {
    let t = parse_ts(&format!("{year:04}-{month:02}-31T01:00:00Z")).unwrap_or(0);
    t - weekday(t) * 86_400
}

/// UK clock time: BST (UTC+1) from 01:00 UTC on the last Sunday of March to 01:00 UTC on
/// the last Sunday of October, GMT otherwise. Returns (local epoch, zone name).
fn uk_local(epoch: i64) -> (i64, &'static str) {
    let (y, _, _) = civil(epoch.div_euclid(86_400));
    if (last_sunday_1am(y, 3)..last_sunday_1am(y, 10)).contains(&epoch) {
        (epoch + 3600, "BST")
    } else {
        (epoch, "GMT")
    }
}

/// `Tue 27 Oct 2026, 11:42:10 GMT`.
fn uk_datetime(epoch: i64) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (t, zone) = uk_local(epoch);
    let (y, m, d) = civil(t.div_euclid(86_400));
    let secs = t.rem_euclid(86_400);
    format!(
        "{} {d} {} {y}, {:02}:{:02}:{:02} {zone}",
        DAYS[weekday(t) as usize],
        MONTHS[(m - 1) as usize],
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// `11:42`, UK clock time.
fn uk_clock(epoch: i64) -> String {
    let secs = uk_local(epoch).0.rem_euclid(86_400);
    format!("{:02}:{:02}", secs / 3600, secs % 3600 / 60)
}

// ---------- status chips ----------

/// A state is shown by a coloured glyph and a word, never by colour alone.
#[derive(Clone, Copy)]
enum Tone {
    Good,
    Warning,
    Serious,
    Critical,
    Accent,
    Muted,
}

impl Tone {
    fn class(self) -> &'static str {
        match self {
            Tone::Good => "good",
            Tone::Warning => "warning",
            Tone::Serious => "serious",
            Tone::Critical => "critical",
            Tone::Accent => "accent",
            Tone::Muted => "muted",
        }
    }
}

fn chip(tone: Tone, glyph: &str, label: &str) -> String {
    format!(
        "<span class=\"chip\"><i class=\"{}\" aria-hidden=\"true\">{glyph}</i>{}</span>",
        tone.class(),
        esc(label)
    )
}

fn bucket_chip(bucket: &str) -> String {
    match bucket {
        "queued" => chip(Tone::Warning, "◌", "queued"),
        "claimed" => chip(Tone::Accent, "◐", "claimed"),
        "done" => chip(Tone::Good, "●", "done"),
        "rejected" => chip(Tone::Serious, "✕", "rejected"),
        _ => chip(Tone::Critical, "!", "dead-lettered"),
    }
}

fn heartbeat_chip(w: &Watcher) -> String {
    match w.beat_age {
        _ if w.asleep => chip(Tone::Muted, "z", "asleep"),
        Some(a) if a <= STALE_AFTER => chip(Tone::Good, "●", &format!("live, {}", age(a))),
        Some(a) => chip(Tone::Critical, "▲", &format!("STALE, {}", age(a))),
        None => chip(Tone::Muted, "○", "no heartbeat"),
    }
}

/// The watcher's mode, or a dim dash when neither its process nor its record shows one.
fn mode_cell(s: &Snapshot, w: &Watcher) -> String {
    match s.mode(w) {
        Some(m) => mode_label(m).to_string(),
        None => "<span class=\"dim\">-</span>".to_string(),
    }
}

/// The colour of a watcher's load bar.
fn busy_tone(b: Busy) -> Tone {
    match b {
        Busy::Asleep | Busy::Idle => Tone::Muted,
        Busy::Stale => Tone::Critical,
        Busy::Backlog => Tone::Warning,
        Busy::Working => Tone::Accent,
    }
}

/// A bar filled to `ratio` (0 to 1), with `label` for screen readers.
fn meter(tone: Tone, ratio: f64, label: &str) -> String {
    let pct = (ratio.clamp(0.0, 1.0) * 100.0).round();
    format!(
        "<div class=\"meter {}\" role=\"img\" aria-label=\"{}\">\
         <span style=\"width:{pct}%\"></span></div>",
        tone.class(),
        esc(label)
    )
}

// ---------- the page ----------

pub fn dashboard(root: &Path, s: &Snapshot) -> String {
    let mut body = String::new();
    let b = &mut body;
    let total = |f: fn(&Watcher) -> usize| s.watchers.iter().map(f).sum::<usize>();
    let (queued, claimed, done, rejected, dead) = (
        total(|w| w.queued),
        total(|w| w.claimed),
        total(|w| w.done),
        total(|w| w.rejected),
        total(|w| w.dead),
    );

    // Header.
    let state = if s.paused {
        chip(Tone::Critical, "■", "PAUSED")
    } else {
        chip(Tone::Good, "●", "running")
    };
    let _ = write!(
        b,
        "<header><div class=\"title\"><h1>cq queue</h1>{state}</div>\
         <p class=\"meta\">Snapshot taken {} · queue root <code>{}</code></p></header>",
        uk_datetime(s.now),
        esc(&root.display().to_string())
    );

    // What needs a look, first.
    let stale: Vec<&str> = s
        .watchers
        .iter()
        .filter(|w| !w.asleep && w.beat_age.is_some_and(|a| a > STALE_AFTER))
        .map(|w| w.name.as_str())
        .collect();
    let mut notes = Vec::new();
    if s.paused {
        notes.push(chip(
            Tone::Critical,
            "■",
            "The queue is paused: watchers deliver nothing until PAUSE is removed.",
        ));
    }
    if !stale.is_empty() {
        notes.push(chip(
            Tone::Critical,
            "▲",
            &format!(
                "{} watcher{} stale (heartbeat older than {STALE_AFTER} s): {}",
                stale.len(),
                if stale.len() == 1 { "" } else { "s" },
                stale.join(", ")
            ),
        ));
    }
    let overdue = s.overdue();
    if overdue > 0 {
        notes.push(chip(
            Tone::Warning,
            "▲",
            &format!(
                "{overdue} task{} overdue (unclaimed for {} min or claimed for {} min); \
                 a coordinator follows up",
                if overdue == 1 { "" } else { "s" },
                s.limits.unclaimed.as_secs() / 60,
                s.limits.claimed.as_secs() / 60
            ),
        ));
    }
    if dead > 0 {
        notes.push(chip(
            Tone::Critical,
            "!",
            &format!(
                "{dead} message{} dead-lettered",
                if dead == 1 { "" } else { "s" }
            ),
        ));
    }
    if notes.is_empty() {
        notes.push(chip(Tone::Good, "✓", "Nothing needs attention."));
    }
    let _ = write!(
        b,
        "<section class=\"notes\" aria-label=\"Needs attention\"><ul>{}</ul></section>",
        notes
            .iter()
            .map(|n| format!("<li>{n}</li>"))
            .collect::<String>()
    );

    // Counts.
    let tile = |label: &str, value: usize, note: &str| {
        format!(
            "<div class=\"tile\"><div class=\"label\">{label}</div>\
             <div class=\"value\">{value}</div><div class=\"note\">{note}</div></div>"
        )
    };
    let _ = write!(
        b,
        "<section class=\"tiles\" aria-label=\"Message counts\">{}{}{}{}{}{}</section>",
        tile("Queued", queued, "waiting to be claimed"),
        tile("Claimed", claimed, "being worked on"),
        tile("Done", done, "kept 14 days"),
        tile("Rejected", rejected, "kept 14 days"),
        tile(
            "Dead-lettered",
            dead,
            if dead > 0 {
                "<span class=\"critical\">! </span>needs a look"
            } else {
                "none"
            }
        ),
        tile("Sent, last hour", s.window.len(), "all senders"),
    );

    // Watchers.
    let _ = write!(
        b,
        "<section class=\"card\"><h2>Watchers</h2><div class=\"scroll\"><table>\
         <thead><tr><th>Name</th><th>Heartbeat</th><th>Mode</th><th class=\"n\">Queued</th>\
         <th class=\"n\">Claimed</th><th class=\"n\">Done</th><th class=\"n\">Rejected</th>\
         <th class=\"n\">Dead</th><th>PID</th><th>Session</th></tr></thead><tbody>"
    );
    for w in &s.watchers {
        let _ = write!(
            b,
            "<tr><td class=\"nowrap\"><strong>{}</strong></td><td class=\"nowrap\">{}</td><td>{}</td>\
             <td class=\"n\">{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td>\
             <td class=\"n\">{}</td><td class=\"dim\">{}</td><td class=\"dim\">{}</td></tr>",
            esc(&w.name),
            heartbeat_chip(w),
            mode_cell(s, w),
            w.queued,
            w.claimed,
            w.done,
            w.rejected,
            if w.dead > 0 {
                format!("<span class=\"critical\">! </span>{}", w.dead)
            } else {
                "0".into()
            },
            esc(&w.pid),
            esc(&w.session)
        );
    }
    if s.watchers.is_empty() {
        b.push_str("<tr><td colspan=\"10\" class=\"dim\">No watchers.</td></tr>");
    }
    b.push_str("</tbody></table></div></section>");

    sessions_table(b, s);

    // Traffic over the last hour, then who talks to whom, as on the Flow screen.
    b.push_str("<div class=\"cols\">");
    slice_chart(b, s);
    let pairs = pairs(s);
    let _ = write!(
        b,
        "<section class=\"card\"><h2>Who talks to whom <span class=\"count\">{}</span></h2>\
         <p class=\"sub\">Messages per sender and receiver, last hour. Most first, then newest.</p>\
         <div class=\"scroll\"><table><thead><tr><th>From</th><th>To</th>\
         <th class=\"n\">Messages</th><th class=\"n\">Last</th><th></th></tr></thead><tbody>",
        pairs.len()
    );
    let most = pairs.first().map_or(1, |p| p.count.max(1));
    for p in pairs.iter().take(PAIRS_SHOWN) {
        let _ = write!(
            b,
            "<tr><td class=\"nowrap\"><strong>{}</strong></td><td class=\"nowrap\"><strong>{}</strong></td>\
             <td class=\"n\">{}</td><td class=\"n\">{}</td><td class=\"meter-cell\">{}</td></tr>",
            esc(&p.from),
            esc(&p.to),
            p.count,
            age(s.now - p.last),
            meter(
                Tone::Accent,
                p.count as f64 / most as f64,
                &format!("{} of {most}", p.count)
            )
        );
    }
    if pairs.is_empty() {
        b.push_str("<tr><td colspan=\"5\" class=\"dim\">No messages in the last hour.</td></tr>");
    }
    b.push_str("</tbody></table></div>");
    if pairs.len() > PAIRS_SHOWN {
        let _ = write!(
            b,
            "<p class=\"dim\">{} more pairs not shown.</p>",
            pairs.len() - PAIRS_SHOWN
        );
    }
    b.push_str("</section></div>");

    // One row per worker: load now and replies in the last hour.
    let _ = write!(
        b,
        "<section class=\"card\"><h2>Workers <span class=\"count\">load now, replies in the last hour</span></h2>\
         <p class=\"sub\">Load is queued + claimed messages; the bar is full at {LOAD_FULL}. \
         Median task → reply is the time from a task being sent to its reply being sent. \
         State comes from the heartbeat.</p>\
         <div class=\"scroll\"><table><thead><tr><th>Worker</th><th>State</th><th>Mode</th>\
         <th class=\"n\">Queued</th><th class=\"n\">Claimed</th><th>Load</th>\
         <th class=\"n\">Median task → reply</th><th class=\"n\">Replies</th></tr></thead><tbody>"
    );
    let rows = workers(s);
    for r in &rows {
        let dash = "<span class=\"dim\">-</span>";
        let (state, mode, queued, claimed, load) = match r.watcher {
            Some(w) => {
                let backlog = w.queued + w.claimed;
                (
                    heartbeat_chip(w),
                    mode_cell(s, w),
                    w.queued.to_string(),
                    w.claimed.to_string(),
                    meter(
                        busy_tone(w.busy()),
                        backlog as f64 / LOAD_FULL as f64,
                        &format!("{backlog} of {LOAD_FULL}"),
                    ),
                )
            }
            // Replied in the last hour, but has no watcher.
            None => (
                dash.to_string(),
                dash.to_string(),
                dash.to_string(),
                dash.to_string(),
                dash.to_string(),
            ),
        };
        let _ = write!(
            b,
            "<tr><td class=\"nowrap\"><strong>{}</strong></td><td class=\"nowrap\">{state}</td>\
             <td>{mode}</td><td class=\"n\">{queued}</td><td class=\"n\">{claimed}</td>\
             <td class=\"meter-cell\">{load}</td><td class=\"n\">{}</td><td class=\"n\">{}</td></tr>",
            esc(&r.name),
            r.median.map_or(dash.to_string(), age),
            r.replies
        );
    }
    if rows.is_empty() {
        b.push_str("<tr><td colspan=\"8\" class=\"dim\">No watchers.</td></tr>");
    }
    b.push_str("</tbody></table></div></section>");

    msg_table(
        b,
        "In flight",
        "Queued, claimed and dead-lettered messages, oldest first. Bodies are not shown.",
        &s.in_flight,
        s.now,
    );
    problem_table(b, s);
    msg_table(
        b,
        "Recent",
        &format!(
            "The {RECENT_LIMIT} newest finished, rejected and dead-lettered messages, newest first."
        ),
        &s.recent,
        s.now,
    );

    format!(
        "<!doctype html>\n<html lang=\"en-GB\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta name=\"color-scheme\" content=\"light dark\">\
         <title>cq queue dashboard</title>\n<style>{CSS}</style></head>\n\
         <body><main>{body}<footer>Written by cq-top --html. A snapshot, not live: \
         run it again to refresh.</footer></main></body></html>\n"
    )
}

/// The smallest of 1, 2, 5, 10, 20, 50 ... that splits `peak` into at most 4 steps.
fn tick_step(peak: u64) -> u64 {
    let mut base = 1;
    loop {
        for m in [1, 2, 5] {
            if peak <= base * m * 4 {
                return base * m;
            }
        }
        base *= 10;
    }
}

/// Messages sent per 5-minute slice over the last hour, as an inline SVG column chart.
fn slice_chart(b: &mut String, s: &Snapshot) {
    let counts = slices(s, CHART_SLICES);
    let peak = counts.iter().copied().max().unwrap_or(0);
    let step = tick_step(peak.max(1));
    let top = step * peak.max(1).div_ceil(step);
    let slice_secs = WINDOW_SECS / CHART_SLICES as i64;
    let start = |i: usize| s.now - WINDOW_SECS + i as i64 * slice_secs;

    // Plot geometry in SVG user units.
    let (w, h) = (640.0, 220.0);
    let (left, right, top_pad, bottom) = (36.0, 8.0, 20.0, 26.0);
    let plot_w = w - left - right;
    let plot_h = h - top_pad - bottom;
    let band = plot_w / CHART_SLICES as f64;
    let bar_w = 24.0_f64.min(band - 4.0);
    let y = |v: u64| top_pad + plot_h * (1.0 - v as f64 / top as f64);

    let _ = write!(
        b,
        "<section class=\"card\"><h2>Messages sent, last hour</h2>\
         <p class=\"sub\">{} in total, per 5-minute slice. Oldest on the left. \
         Hover a column for its count.</p>\
         <svg class=\"chart\" viewBox=\"0 0 {w} {h}\" role=\"img\" \
         aria-label=\"Messages sent per 5-minute slice, last hour; peak {peak}\">",
        s.window.len()
    );
    // Gridlines and y ticks.
    let mut v = 0;
    while v <= top {
        let gy = y(v);
        let _ = write!(
            b,
            "<line class=\"{}\" x1=\"{left}\" x2=\"{}\" y1=\"{gy:.1}\" y2=\"{gy:.1}\"/>\
             <text class=\"tick\" x=\"{}\" y=\"{:.1}\" text-anchor=\"end\">{v}</text>",
            if v == 0 { "base" } else { "grid" },
            w - right,
            left - 6.0,
            gy + 4.0
        );
        v += step;
    }
    // Columns: 4px rounded data end, square at the baseline. The hit target is the whole band.
    let mut peak_done = false;
    for (i, &n) in counts.iter().enumerate() {
        let x0 = left + band * i as f64;
        let bx = x0 + (band - bar_w) / 2.0;
        let label = format!(
            "{}–{}: {n} message{}",
            uk_clock(start(i)),
            uk_clock(start(i + 1)),
            if n == 1 { "" } else { "s" }
        );
        let _ = write!(
            b,
            "<g class=\"col\"><title>{label}</title>\
             <rect class=\"hit\" x=\"{x0:.1}\" y=\"{top_pad}\" width=\"{band:.1}\" height=\"{plot_h}\"/>"
        );
        if n > 0 {
            let by = y(n);
            let base = y(0);
            let r = 4.0_f64.min(base - by);
            let _ = write!(
                b,
                "<path class=\"bar\" d=\"M{bx:.1},{base:.1}V{:.1}Q{bx:.1},{by:.1} {:.1},{by:.1}\
                 H{:.1}Q{:.1},{by:.1} {:.1},{:.1}V{base:.1}Z\"/>",
                by + r,
                bx + r,
                bx + bar_w - r,
                bx + bar_w,
                bx + bar_w,
                by + r
            );
            // Label the peak only; the table below has every value.
            if n == peak && !peak_done {
                peak_done = true;
                let _ = write!(
                    b,
                    "<text class=\"peak\" x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"middle\">{n}</text>",
                    bx + bar_w / 2.0,
                    by - 6.0
                );
            }
        }
        b.push_str("</g>");
        if i % 3 == 0 {
            let _ = write!(
                b,
                "<text class=\"tick\" x=\"{x0:.1}\" y=\"{}\" text-anchor=\"start\">{}</text>",
                h - 8.0,
                uk_clock(start(i))
            );
        }
    }
    let _ = write!(
        b,
        "<text class=\"tick\" x=\"{}\" y=\"{}\" text-anchor=\"end\">now {}</text></svg>",
        w - right,
        h - 8.0,
        uk_clock(s.now)
    );
    // The same numbers as a table.
    b.push_str(
        "<details><summary>Show as a table</summary><table><thead><tr><th>Slice (UK time)</th>\
         <th class=\"n\">Messages</th></tr></thead><tbody>",
    );
    for (i, n) in counts.iter().enumerate() {
        let _ = write!(
            b,
            "<tr><td>{}–{}</td><td class=\"n\">{n}</td></tr>",
            uk_clock(start(i)),
            uk_clock(start(i + 1))
        );
    }
    b.push_str("</tbody></table></details></section>");
}

/// Cuts text to `max` characters, ending in an ellipsis when cut, on one line.
fn clip(text: &str, max: usize) -> String {
    let one_line = text.replace(['\n', '\r'], " ");
    if one_line.chars().count() <= max {
        return one_line;
    }
    let mut cut: String = one_line.chars().take(max - 1).collect();
    cut.push('…');
    cut
}

fn problem_table(b: &mut String, s: &Snapshot) {
    let shown = &s.problems[..s.problems.len().min(PROBLEMS_SHOWN)];
    let _ = write!(
        b,
        "<section class=\"card\"><h2>Rejected and dead-lettered \
         <span class=\"count\">{} of {}</span></h2>\
         <p class=\"sub\">Newest first. Rejected messages were declined by the receiver; \
         dead-lettered ones could not be processed. The reason comes from the reason file. \
         Bodies are not shown.</p><div class=\"scroll\"><table class=\"msgs\"><thead><tr>\
         <th class=\"n\">Age</th><th>State</th><th>From</th><th>To</th><th>Type</th>\
         <th>Subject</th><th>Reason</th></tr></thead><tbody>",
        shown.len(),
        s.problems_total
    );
    for m in shown {
        let reason = if m.reason.is_empty() {
            "<span class=\"dim\">no reason file</span>".to_string()
        } else {
            esc(&clip(&m.reason, REASON_MAX))
        };
        let _ = write!(
            b,
            "<tr><td class=\"n\">{}</td><td class=\"nowrap\">{}</td>\
             <td class=\"nowrap\">{}</td><td class=\"nowrap\">{}</td><td class=\"dim\">{}</td>\
             <td class=\"subject\">{}</td><td class=\"subject\">{reason}</td></tr>",
            age(s.now - m.ts),
            bucket_chip(m.bucket),
            esc(&m.from),
            esc(&m.to),
            esc(&m.kind),
            esc(&m.subject)
        );
    }
    if shown.is_empty() {
        b.push_str("<tr><td colspan=\"7\" class=\"dim\">None.</td></tr>");
    }
    b.push_str("</tbody></table></div></section>");
}

/// The context figure, orange above `CONTEXT_WARN` tokens and red above `CONTEXT_BAD`.
fn context_cell(tokens: u64) -> String {
    let n = sessions::count(tokens);
    let (class, over) = match sessions::context(tokens) {
        sessions::Context::Normal => return n,
        sessions::Context::Warn => (Tone::Serious, sessions::CONTEXT_WARN),
        sessions::Context::Bad => (Tone::Critical, sessions::CONTEXT_BAD),
    };
    format!(
        "<span class=\"{}\" title=\"over {} tokens\">{n}</span>",
        class.class(),
        sessions::count(over)
    )
}

/// Claude Code sessions, as on the Sessions screen. Titles are escaped; no message text is
/// ever read into the report.
fn sessions_table(b: &mut String, s: &Snapshot) {
    use sessions::{State, bytes, count, usd};
    let r = &s.sessions;
    let list = |figs: &[(&str, String)]| {
        figs.iter()
            .map(|(l, v)| format!("{l} {}", esc(v)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let machine = match &r.machine {
        Some(m) => format!(
            "This computer. CPU: {}. Memory: {}. Claude is {} processes and Codex is {}, each \
             with the processes it started, and their CPU here is a share of the whole \
             computer. A busy core is one at {}% or more.",
            list(&m.cpu_figures()),
            list(&m.mem_figures()),
            m.claude_procs,
            m.codex_procs,
            sessions::BUSY_CORE
        ),
        None => "Process data unavailable: CPU and memory show -, and the state uses file \
                 activity only."
            .into(),
    };
    let _ = write!(
        b,
        "<section class=\"card\"><h2>Sessions <span class=\"count\">{}</span></h2>\
         <p class=\"sub\">{machine} Tokens and cost count from local midnight, the session \
         and its subagents together. The cost is an estimate at API list prices, not the \
         plan's bill. In the table, CPU 100% is one core.</p><div class=\"scroll\"><table><thead><tr>\
         <th>Session</th><th>State</th><th>Model</th><th class=\"n\">Context</th>\
         <th class=\"n\">Tokens today</th><th class=\"n\">Est. cost today</th>\
         <th class=\"n\">CPU %</th><th class=\"n\">Memory</th><th class=\"n\">Sub</th>\
         <th>cq</th><th>Mode</th><th class=\"n\">Waiting</th><th class=\"n\">Claimed</th>\
         <th>Last commit or edit</th><th class=\"n\">Age</th></tr></thead><tbody>",
        r.rows.len()
    );
    let dash = || "<span class=\"dim\">-</span>".to_string();
    for row in &r.rows {
        let (state, warned) = draw::session_state(s, row);
        let state_chip = match state {
            _ if warned => chip(Tone::Warning, "▲", "idle, messages waiting"),
            State::Working => chip(Tone::Good, "●", "working"),
            State::Recent => chip(Tone::Accent, "◐", "recent"),
            State::Idle => chip(Tone::Muted, "○", "idle"),
        };
        let w = draw::session_watcher(s, row);
        let beat = match w {
            Some(w) => heartbeat_chip(w),
            None => dash(),
        };
        let opt = |v: Option<String>| v.map_or_else(dash, |v| esc(&v));
        let _ = write!(
            b,
            "<tr><td class=\"nowrap\"><strong>{}</strong></td><td class=\"nowrap\">{state_chip}</td>\
             <td class=\"nowrap\">{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td>\
             <td class=\"n\">{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td>\
             <td class=\"n\">{}</td><td class=\"nowrap\">{beat}</td><td>{}</td><td class=\"n\">{}</td>\
             <td class=\"n\">{}</td><td class=\"nowrap\">{}</td><td class=\"n\">{}</td></tr>",
            esc(&row.name),
            opt(Some(row.model.clone()).filter(|m| !m.is_empty())),
            row.context.map_or_else(dash, context_cell),
            count(row.tokens_today),
            esc(&usd(row.cost_today)),
            opt(row.cpu.map(|c| format!("{c:.1}"))),
            opt(row.mem.map(bytes)),
            row.subagents,
            w.map_or_else(dash, |w| mode_cell(s, w)),
            opt(w.map(|w| w.queued.to_string())),
            opt(w.map(|w| w.claimed.to_string())),
            opt(row
                .last_change
                .map(|(c, at)| format!("{} {}", c.label(), age(s.now - at)))),
            age(s.now - row.active)
        );
    }
    if r.rows.is_empty() {
        b.push_str(
            "<tr><td colspan=\"15\" class=\"dim\">No sessions in the last 24 hours.</td></tr>",
        );
    }
    b.push_str("</tbody></table></div></section>");
}

fn msg_table(b: &mut String, title: &str, sub: &str, msgs: &[Msg], now: i64) {
    let _ = write!(
        b,
        "<section class=\"card\"><h2>{title} <span class=\"count\">{}</span></h2>\
         <p class=\"sub\">{sub}</p><div class=\"scroll\"><table class=\"msgs\"><thead><tr>\
         <th>State</th><th class=\"n\">Age</th><th>From</th><th>To</th><th>Type</th>\
         <th>Subject</th></tr></thead><tbody>",
        msgs.len()
    );
    for m in msgs {
        let status = match m.status.as_str() {
            "" => String::new(),
            "failed" => format!(" {}", chip(Tone::Serious, "✕", "failed")),
            st => format!(" <span class=\"dim\">{}</span>", esc(st)),
        };
        let status = if m.overdue {
            format!("{status} {}", chip(Tone::Warning, "▲", "overdue"))
        } else {
            status
        };
        let _ = write!(
            b,
            "<tr><td class=\"nowrap\">{}{status}</td><td class=\"n\">{}</td><td class=\"nowrap\">{}</td>\
             <td class=\"nowrap\">{}</td><td class=\"dim\">{}</td><td class=\"subject\">{}</td></tr>",
            bucket_chip(m.bucket),
            age(now - m.ts),
            esc(&m.from),
            esc(&m.to),
            esc(&m.kind),
            esc(&m.subject)
        );
    }
    if msgs.is_empty() {
        b.push_str("<tr><td colspan=\"6\" class=\"dim\">None.</td></tr>");
    }
    b.push_str("</tbody></table></div></section>");
}

const CSS: &str = r#"
:root{color-scheme:light;
 --page:#f9f9f7;--surface:#fcfcfb;--ink:#0b0b0b;--ink-2:#52514e;--muted:#6b6a65;
 --grid:#e1e0d9;--axis:#c3c2b7;--ring:rgba(11,11,11,.10);
 --series:#2a78d6;--good:#0ca30c;--warning:#fab219;--serious:#ec835a;--critical:#d03b3b}
@media (prefers-color-scheme: dark){:root{color-scheme:dark;
 --page:#0d0d0d;--surface:#1a1a19;--ink:#f0efec;--ink-2:#c3c2b7;--muted:#a3a29b;
 --grid:#2c2c2a;--axis:#383835;--ring:rgba(255,255,255,.10);
 --series:#3987e5}}
*{box-sizing:border-box}
html{-webkit-text-size-adjust:100%}
body{margin:0;background:var(--page);color:var(--ink);
 font:15px/1.45 system-ui,-apple-system,"Segoe UI",sans-serif}
main{max-width:1200px;margin:0 auto;padding:20px 16px 32px}
header{margin-bottom:12px}
.title{display:flex;flex-wrap:wrap;align-items:center;gap:8px 16px}
h1{font-size:26px;margin:0;font-weight:650}
h2{font-size:17px;margin:0 0 2px;font-weight:650}
.meta{margin:4px 0 0;color:var(--ink-2)}
code{font-family:"Cascadia Mono",Consolas,monospace;font-size:13px;overflow-wrap:anywhere}
.sub{margin:0 0 12px;color:var(--ink-2);font-size:13.5px}
.dim{color:var(--ink-2)}
.count{font-weight:400;color:var(--ink-2)}
.card{background:var(--surface);border:1px solid var(--ring);border-radius:10px;
 padding:16px;margin-bottom:16px;min-width:0}
.cols{display:grid;grid-template-columns:repeat(auto-fit,minmax(min(100%,440px),1fr));
 gap:0 16px;align-items:start}
.notes ul{list-style:none;margin:0 0 16px;padding:12px 16px;background:var(--surface);
 border:1px solid var(--ring);border-radius:10px;display:flex;flex-direction:column;gap:6px;
 font-weight:600}
.tiles{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:12px;
 margin-bottom:16px}
.tile{background:var(--surface);border:1px solid var(--ring);border-radius:10px;padding:12px 14px}
.tile .label{color:var(--ink-2);font-size:13.5px}
.tile .value{font-size:30px;font-weight:650;line-height:1.2}
.tile .note{color:var(--ink-2);font-size:12.5px}
.chip{display:inline-flex;align-items:baseline;gap:6px;white-space:nowrap}
.notes .chip{white-space:normal}
.chip i{font-style:normal;font-weight:700;width:1em;text-align:center}
.good{color:var(--good)}.warning{color:var(--warning)}.serious{color:var(--serious)}
.critical{color:var(--critical);font-weight:700}.accent{color:var(--series)}.muted{color:var(--muted)}
.chip i.critical{font-weight:700}
.scroll{overflow-x:auto}
table{border-collapse:collapse;width:100%;font-size:14px}
th{text-align:left;font-weight:600;color:var(--ink-2);font-size:12.5px;
 border-bottom:1px solid var(--axis);padding:6px 16px 6px 0;white-space:nowrap}
td{border-bottom:1px solid var(--grid);padding:7px 16px 7px 0;vertical-align:top}
th:last-child,td:last-child{padding-right:0}
tbody tr:last-child td{border-bottom:0}
.n{text-align:right;font-variant-numeric:tabular-nums;white-space:nowrap}
.nowrap{white-space:nowrap}
.msgs td.subject{min-width:220px;overflow-wrap:anywhere}
.meter-cell{min-width:90px;width:22%}
.meter-cell .meter{margin-top:5px}
.meter{height:10px;border-radius:5px;overflow:hidden;
 background:color-mix(in srgb,currentColor 16%,var(--surface))}
.meter span{display:block;height:100%;background:currentColor;border-radius:0 4px 4px 0}
.meter.muted{background:var(--grid)}
.chart{width:100%;height:auto;display:block}
.chart .grid{stroke:var(--grid);stroke-width:1}
.chart .base{stroke:var(--axis);stroke-width:1}
.chart .tick{fill:var(--muted);font-size:13px;font-variant-numeric:tabular-nums}
.chart .peak{fill:var(--ink);font-size:12px;font-weight:600}
.chart .bar{fill:var(--series)}
.chart .hit{fill:transparent}
.chart .col:hover .bar{opacity:.75}
.chart .col:hover .hit{fill:var(--grid);opacity:.5}
details{margin-top:8px;font-size:14px}
summary{cursor:pointer;color:var(--ink-2)}
footer{color:var(--ink-2);font-size:12.5px;margin-top:8px}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uk_time_follows_bst() {
        let t = |s: &str| uk_datetime(parse_ts(s).unwrap());
        assert_eq!(t("2026-10-27T11:42:10Z"), "Tue 27 Oct 2026, 11:42:10 GMT");
        assert_eq!(t("2026-07-01T12:00:00Z"), "Wed 1 Jul 2026, 13:00:00 BST");
        // 2026: BST from Sun 29 March to Sun 25 October, changing at 01:00 UTC.
        assert_eq!(t("2026-03-29T00:59:59Z"), "Sun 29 Mar 2026, 00:59:59 GMT");
        assert_eq!(t("2026-03-29T01:00:00Z"), "Sun 29 Mar 2026, 02:00:00 BST");
        assert_eq!(t("2026-10-25T00:59:59Z"), "Sun 25 Oct 2026, 01:59:59 BST");
        assert_eq!(t("2026-10-25T01:00:00Z"), "Sun 25 Oct 2026, 01:00:00 GMT");
        assert_eq!(t("2024-02-29T23:59:59Z"), "Thu 29 Feb 2024, 23:59:59 GMT");
    }

    #[test]
    fn tick_steps_are_round() {
        let steps: Vec<u64> = [1, 4, 5, 8, 9, 20, 21, 40, 41, 199].map(tick_step).to_vec();
        assert_eq!(steps, [1, 1, 2, 2, 5, 5, 10, 10, 20, 50]);
    }

    #[test]
    fn esc_covers_every_special_character() {
        assert_eq!(
            esc(r#"<a href="x" title='y'>&</a>"#),
            "&lt;a href=&quot;x&quot; title=&#39;y&#39;&gt;&amp;&lt;/a&gt;"
        );
    }

    #[test]
    fn clip_cuts_long_reasons_on_one_line() {
        assert_eq!(clip("short\nreason", 20), "short reason");
        assert_eq!(clip("abcdefghij", 5), "abcd…");
        assert_eq!(clip("abcde", 5), "abcde");
    }
}
