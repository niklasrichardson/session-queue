//! `--report`: one self-contained HTML page that analyses the queue's history, beside the
//! `--html` dashboard, which shows the state now.
//!
//! It reads `inbox/` (with `.processing/`), `done/`, `rejected/`, `dlq/`, the watcher exit
//! log, heartbeats and member records. It ignores `archive/`, which cq never writes. It
//! writes nothing to the queue. Metadata and sizes only: a message's body and subject are
//! measured in bytes and never kept, and a reject reason is kept as its first few words.
//!
//! Times come from the files. `cq claim` sets a message file's modified time to the claim
//! time and the move to `done/` keeps it, so modified time minus `created` is the wait in
//! the inbox. `done/` and `rejected/` are pruned after 14 days, so the report covers at most
//! 14 days of finished messages.
//!
//! About 20 lines of script give tooltips and the chord's hover. The page reads fully
//! without them.

use super::*;
use html::{DAYS, MONTHS, civil, esc, tick_step, uk_datetime, uk_local, weekday};
use std::collections::HashMap;
use std::fmt::Write;

const TYPES: [&str; 3] = ["task", "info", "result"];
/// Where a message file is: its folder, or `claimed` for `inbox/<name>/.processing/`.
const STATES: [&str; 5] = ["inbox", "claimed", "done", "rejected", "dlq"];
const BIGGEST_SHOWN: usize = 15;
const THREADS_SHOWN: usize = 6;
const ORPHANS_SHOWN: usize = 40;
/// Watcher exit reasons with their own colour; the rest are "other".
const EXIT_REASONS_SHOWN: usize = 5;
/// The heatmap and sparklines cover at most this many days, the newest.
const MAX_DAYS: i64 = 31;
/// A reject reason is cut to this many words.
const REASON_WORDS: usize = 4;
/// A folded watcher exit reason is cut to this many characters.
const EXIT_REASON_MAX: usize = 48;

// ---------- reading the queue ----------

/// One message file, light: no body or subject text, only their lengths.
struct Rec {
    id: String,
    from: String,
    to: String,
    kind: &'static str,
    parent: Option<String>,
    reply_to: Option<String>,
    status: String,
    hops: u64,
    /// `created`, else the time in the ID, else the file's modified time.
    created: i64,
    mtime: i64,
    size: u64,
    state: &'static str,
    /// The name whose folder holds the file.
    folder: String,
    body_bytes: u64,
    subject_bytes: u64,
    /// `from_session_id`: counted per name, never shown.
    session_id: Option<String>,
}

struct ExitLine {
    t: i64,
    name: String,
    mode: String,
    /// Folded to a short category (see `exit_reason`).
    reason: String,
}

/// Everything the page is drawn from.
struct Data {
    root: String,
    now: i64,
    recs: Vec<Rec>,
    /// Message ID -> its reject or dead-letter reason, cut to its first words.
    reasons: HashMap<String, String>,
    /// Bytes of the other files and folders in the root, by top-level entry.
    other: BTreeMap<String, u64>,
    exits: Vec<ExitLine>,
    /// Name -> last heartbeat.
    beats: BTreeMap<String, i64>,
    /// Name -> the watcher mode from its member record.
    modes: BTreeMap<String, &'static str>,
}

/// The page for the queue at `root`, at time `now`.
pub fn page(root: &cq::Root, now: i64) -> String {
    render(&scan(root, now))
}

struct File {
    path: PathBuf,
    name: String,
    mtime: i64,
    size: u64,
}

/// The plain files in a folder, not following links.
fn files(dir: &Path) -> Vec<File> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let meta = e.metadata().ok().filter(|m| m.is_file())?;
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs() as i64);
            Some(File {
                path: e.path(),
                name: e.file_name().to_string_lossy().into_owned(),
                mtime,
                size: meta.len(),
            })
        })
        .collect()
}

/// Bytes under `path`, not following links.
fn tree_size(path: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.is_file() {
        return meta.len();
    }
    if !meta.is_dir() {
        return 0;
    }
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| tree_size(&e.path()))
        .sum()
}

fn read_rec(f: &File, state: &'static str, folder: &str) -> Option<Rec> {
    let v: Value = serde_json::from_str(&fs::read_to_string(&f.path).ok()?).ok()?;
    let s = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
    };
    // Lengths only: the text itself goes out of scope with `v`.
    let len = |k: &str| match v.get(k) {
        None | Some(Value::Null) => 0,
        Some(Value::String(t)) => t.len() as u64,
        Some(other) => other.to_string().len() as u64,
    };
    let id = s("id")?;
    let from = s("from")?;
    let created = s("created")
        .and_then(|c| parse_ts(&c))
        .or_else(|| parse_ts(&id))
        .unwrap_or(f.mtime);
    Some(Rec {
        to: s("to").unwrap_or_else(|| folder.to_string()),
        kind: match v.get("type").and_then(Value::as_str) {
            Some("task") => "task",
            Some("result") => "result",
            _ => "info",
        },
        parent: s("parent"),
        reply_to: s("reply_to"),
        status: s("status").unwrap_or_default(),
        hops: v.get("hops").and_then(Value::as_u64).unwrap_or(0),
        created,
        mtime: f.mtime,
        size: f.size,
        state,
        folder: folder.to_string(),
        body_bytes: len("body"),
        subject_bytes: len("subject"),
        session_id: s("from_session_id"),
        id,
        from,
    })
}

fn scan(root: &cq::Root, now: i64) -> Data {
    let r = root.path();
    let mut recs = Vec::new();
    let mut reasons = HashMap::new();
    let mut msg_bytes: BTreeMap<&str, u64> = BTreeMap::new();
    for top in ["inbox", "done", "rejected", "dlq"] {
        for name in subdirs(&r.join(top)) {
            let mut dirs = vec![(r.join(top).join(&name), top)];
            if top == "inbox" {
                dirs.push((root.processing(&name), "claimed"));
            }
            for (dir, state) in dirs {
                for f in files(&dir) {
                    if let Some(id) = f.name.strip_suffix(".reason.json") {
                        if let Some(reason) = fs::read_to_string(&f.path)
                            .ok()
                            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                            .and_then(|v| v.get("reason").and_then(Value::as_str).map(short_reason))
                        {
                            reasons.insert(id.to_string(), reason);
                        }
                        continue;
                    }
                    if !f.name.ends_with(".json") {
                        continue;
                    }
                    if let Some(rec) = read_rec(&f, state, &name) {
                        *msg_bytes.entry(top).or_default() += rec.size;
                        recs.push(rec);
                    }
                }
            }
        }
    }
    // Every other byte in the root, by top-level entry; message files are counted above.
    let mut other = BTreeMap::new();
    for e in fs::read_dir(r).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name == "archive" {
            continue;
        }
        let bytes =
            tree_size(&e.path()).saturating_sub(msg_bytes.get(name.as_str()).copied().unwrap_or(0));
        if bytes > 0 {
            other.insert(name, bytes);
        }
    }
    let log = root.watch_exits();
    let mut exits: Vec<ExitLine> = [cq::exitlog::old_path(&log), log]
        .iter()
        .filter_map(|p| fs::read_to_string(p).ok())
        .flat_map(|t| t.lines().filter_map(parse_exit).collect::<Vec<_>>())
        .collect();
    exits.sort_by_key(|e| e.t);
    let mut beats = BTreeMap::new();
    for f in files(&r.join("heartbeat")) {
        if !f.name.contains('.')
            && let Some(t) = fs::read_to_string(&f.path)
                .ok()
                .and_then(|t| t.lines().next().and_then(|l| parse_ts(l.trim())))
        {
            beats.insert(f.name, t);
        }
    }
    let mut modes = BTreeMap::new();
    for f in files(&root.members()) {
        if let Ok(t) = fs::read_to_string(&f.path)
            && let Some(name) = t.lines().next()
        {
            modes.insert(
                name.trim().to_string(),
                Mode::parse(t.lines().nth(3)).as_str(),
            );
        }
    }
    Data {
        root: r.display().to_string(),
        now,
        recs,
        reasons,
        other,
        exits,
        beats,
        modes,
    }
}

/// One line of `watch-exits.log` (see `cq::exitlog`).
fn parse_exit(line: &str) -> Option<ExitLine> {
    let (ts, rest) = line.split_once(' ')?;
    let t = parse_ts(ts)?;
    let (fields, reason) = rest.split_once("reason=\"")?;
    let field = |k: &str| {
        fields
            .split_whitespace()
            .find_map(|w| w.strip_prefix(k))
            .unwrap_or("unknown")
            .to_string()
    };
    Some(ExitLine {
        t,
        name: field("name="),
        mode: field("mode="),
        reason: exit_reason(reason.strip_suffix('"').unwrap_or(reason)),
    })
}

/// A watcher exit reason folded to a short category: parenthesised parts, PIDs, retry
/// counts and anything after `;` are removed.
fn exit_reason(r: &str) -> String {
    let mut plain = String::new();
    let mut depth = 0;
    for c in r.chars() {
        match c {
            '(' => depth += 1,
            ')' if depth > 0 => depth -= 1,
            c if depth == 0 => plain.push(c),
            _ => {}
        }
    }
    let first = plain.split(';').next().unwrap_or("");
    let words: Vec<&str> = first.split_whitespace().collect();
    let number = |w: Option<&&str>| {
        w.is_some_and(|w| {
            let w = w.trim_end_matches(['.', ',']);
            !w.is_empty() && w.chars().all(|c| c.is_ascii_digit())
        })
    };
    let mut out: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < words.len() {
        match words[i] {
            "from" | "by" if words.get(i + 1) == Some(&"PID") && number(words.get(i + 2)) => {
                i += 3;
            }
            "after"
                if number(words.get(i + 1))
                    && words.get(i + 2).map(|w| w.trim_end_matches('.')) == Some("tries") =>
            {
                out.extend(["after", "retries"]);
                i += 3;
            }
            "console" if words.get(i + 1) == Some(&"event") => i += 2,
            w => {
                out.push(w);
                i += 1;
            }
        }
    }
    let folded = out.join(" ");
    folded
        .trim_end_matches('.')
        .chars()
        .take(EXIT_REASON_MAX)
        .collect()
}

/// A free-text reject reason cut to a short label: the first clause, lower case, at most
/// `REASON_WORDS` words, else one fewer and `...`.
fn short_reason(text: &str) -> String {
    let first = text.trim().split([':', '.', ';']).next().unwrap_or("");
    let words: Vec<String> = first.split_whitespace().map(str::to_lowercase).collect();
    if words.len() <= REASON_WORDS {
        words.join(" ")
    } else {
        format!("{} ...", words[..REASON_WORDS - 1].join(" "))
    }
}

// ---------- figures ----------

/// Threads, depths and replies, from the `parent` and `reply_to` links.
struct Links {
    /// Indexes into `recs`, largest first, then oldest first.
    threads: Vec<Vec<usize>>,
    depth: Vec<usize>,
    /// (message, link field, missing ID).
    orphans: Vec<(usize, &'static str, String)>,
    /// Task ID -> the result that replies to it.
    replies: HashMap<String, usize>,
}

fn links(recs: &[Rec]) -> Links {
    let by_id: HashMap<&str, usize> = recs
        .iter()
        .enumerate()
        .map(|(i, r)| (r.id.as_str(), i))
        .collect();
    fn find(up: &mut [usize], mut a: usize) -> usize {
        while up[a] != a {
            up[a] = up[up[a]];
            a = up[a];
        }
        a
    }
    let mut up: Vec<usize> = (0..recs.len()).collect();
    let mut orphans = Vec::new();
    for (i, r) in recs.iter().enumerate() {
        for (field, link) in [("parent", &r.parent), ("reply_to", &r.reply_to)] {
            let Some(link) = link else { continue };
            match by_id.get(link.as_str()) {
                Some(&j) => {
                    let (a, b) = (find(&mut up, i), find(&mut up, j));
                    up[a] = b;
                }
                None => orphans.push((i, field, link.clone())),
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..recs.len() {
        groups.entry(find(&mut up, i)).or_default().push(i);
    }
    let mut threads: Vec<Vec<usize>> = groups.into_values().collect();
    let first = |g: &Vec<usize>| g.iter().map(|&i| recs[i].created).min().unwrap_or(0);
    threads.sort_by_key(|g| (std::cmp::Reverse(g.len()), first(g)));

    // Depth: steps up the parent (else reply_to) chain to a message with no link on disk.
    let up_of = |i: usize| {
        recs[i]
            .parent
            .as_deref()
            .or(recs[i].reply_to.as_deref())
            .and_then(|l| by_id.get(l).copied())
    };
    let mut depth: Vec<Option<usize>> = vec![None; recs.len()];
    for start in 0..recs.len() {
        let mut path = Vec::new();
        let mut cur = Some(start);
        let mut base = 0;
        while let Some(i) = cur {
            if let Some(d) = depth[i] {
                base = d + 1;
                break;
            }
            // A loop of links counts as its own top.
            if path.contains(&i) {
                break;
            }
            path.push(i);
            cur = up_of(i);
        }
        for (k, &i) in path.iter().rev().enumerate() {
            depth[i] = Some(base + k);
        }
    }
    let replies = recs
        .iter()
        .enumerate()
        .filter(|(_, r)| r.kind == "result")
        .filter_map(|(i, r)| {
            let t = r.reply_to.as_ref()?;
            by_id.contains_key(t.as_str()).then(|| (t.clone(), i))
        })
        .collect();
    Links {
        threads,
        depth: depth.into_iter().map(Option::unwrap_or_default).collect(),
        orphans,
        replies,
    }
}

/// Created to claimed, for a message that was claimed. Not floored: the log axis clamps
/// only where it draws a dot.
fn claim_wait(r: &Rec) -> Option<f64> {
    matches!(r.state, "done" | "rejected" | "claimed")
        .then(|| ((r.mtime - r.created) as f64).max(0.0))
}

/// The middle value, or the mean of the two middle values for an even count.
fn median_f(v: &[f64]) -> Option<f64> {
    let mut v = v.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    match n {
        0 => None,
        _ if n.is_multiple_of(2) => Some((v[n / 2 - 1] + v[n / 2]) / 2.0),
        _ => Some(v[n / 2]),
    }
}

/// `45s`, `3m05s`, `2h10m`, `3d04h`.
fn dur(secs: f64) -> String {
    let s = secs.round().max(0.0) as i64;
    if s < 86_400 {
        age(s)
    } else {
        format!("{}d{:02}h", s / 86_400, s % 86_400 / 3600)
    }
}

fn size_str(b: u64) -> String {
    match b {
        0..1024 => format!("{b} B"),
        1024..1_048_576 => format!("{:.1} KB", b as f64 / 1024.0),
        _ => format!("{:.2} MB", b as f64 / 1_048_576.0),
    }
}

/// `2,128`.
fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The UK calendar day, as days since 1970-01-01.
fn uk_day(epoch: i64) -> i64 {
    uk_local(epoch).0.div_euclid(86_400)
}

/// `Tue 27 Oct`.
fn day_label(day: i64) -> String {
    let (_, m, d) = civil(day);
    format!(
        "{} {d} {}",
        DAYS[weekday(day * 86_400) as usize],
        MONTHS[(m - 1) as usize]
    )
}

/// `27 Oct`.
fn short_day(day: i64) -> String {
    let (_, m, d) = civil(day);
    format!("{d} {}", MONTHS[(m - 1) as usize])
}

/// `27 Oct 11:42`, UK time.
fn uk_short(epoch: i64) -> String {
    format!("{} {}", short_day(uk_day(epoch)), html::uk_clock(epoch))
}

/// A value in -0.5 to 0.5 from a hash of `id` (FNV-1a), so dots keep their place.
fn jitter(id: &str) -> f64 {
    let mut h: u32 = 0x811c_9dc5;
    for b in id.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    f64::from(h % 1000) / 1000.0 - 0.5
}

// ---------- SVG helpers ----------

fn type_var(kind: &str) -> &'static str {
    match kind {
        "task" => "var(--s1)",
        "info" => "var(--s2)",
        _ => "var(--s3)",
    }
}

fn status_var(status: &str) -> &'static str {
    match status {
        "ok" => "var(--good)",
        "failed" => "var(--critical)",
        "rejected" => "var(--serious)",
        _ => "var(--muted)",
    }
}

/// Seven steps of one hue, light to dark.
fn seq_var(step: usize) -> String {
    format!("var(--q{})", step.min(6) + 1)
}

fn tip(text: &str) -> String {
    format!(" data-tip=\"{}\"", esc(text))
}

/// Never drawn larger than its own size, so text keeps its set size on wide screens.
fn svg(w: f64, h: f64, body: &str, class: &str, label: &str) -> String {
    format!(
        "<svg class=\"{class}\" viewBox=\"0 0 {w:.0} {h:.0}\" style=\"max-width:{w:.0}px\" \
         role=\"img\" aria-label=\"{}\" preserveAspectRatio=\"xMinYMin meet\">{body}</svg>",
        esc(label)
    )
}

fn legend(items: &[(&str, &str)]) -> String {
    let mut out = String::from("<div class=\"legend\">");
    for (name, colour) in items {
        let _ = write!(
            out,
            "<span><i style=\"background:{colour}\"></i>{}</span>",
            esc(name)
        );
    }
    out.push_str("</div>");
    out
}

fn type_legend() -> String {
    legend(&TYPES.map(|t| (t, type_var(t))))
}

/// A vertical bar with a rounded top, on the baseline.
fn bar_path(x: f64, y: f64, w: f64, h: f64, round: bool) -> String {
    if h <= 0.0 || w <= 0.0 {
        return String::new();
    }
    if !round {
        return format!("M{x:.1},{y:.1}h{w:.1}v{h:.1}h{:.1}Z", -w);
    }
    let r = 3.0_f64.min(w / 2.0).min(h);
    format!(
        "M{x:.1},{:.1}V{:.1}Q{x:.1},{y:.1} {:.1},{y:.1}H{:.1}Q{:.1},{y:.1} {:.1},{:.1}V{:.1}Z",
        y + h,
        y + r,
        x + r,
        x + w - r,
        x + w,
        x + w,
        y + r,
        y + h
    )
}

/// A horizontal bar with a rounded end, from the left.
fn hbar_path(x: f64, y: f64, w: f64, h: f64, round: bool) -> String {
    if h <= 0.0 || w <= 0.0 {
        return String::new();
    }
    if !round {
        return format!("M{x:.1},{y:.1}h{w:.1}v{h:.1}h{:.1}Z", -w);
    }
    let r = 3.0_f64.min(h / 2.0).min(w);
    format!(
        "M{x:.1},{y:.1}H{:.1}Q{:.1},{y:.1} {:.1},{:.1}V{:.1}Q{:.1},{:.1} {:.1},{:.1}H{x:.1}Z",
        x + w - r,
        x + w,
        x + w,
        y + r,
        y + h - r,
        x + w,
        y + h,
        x + w - r,
        y + h
    )
}

/// Stacked columns, one per category. Each series is (name, value per category, colour).
fn stacked_columns(
    labels: &[String],
    series: &[(String, Vec<u64>, &str)],
    tips: &[String],
    label: &str,
) -> String {
    let (w, h) = (1110.0, 220.0);
    let (left, right, top, bottom) = (44.0, 8.0, 12.0, 34.0);
    let (pw, ph) = (w - left - right, h - top - bottom);
    let totals: Vec<u64> = (0..labels.len())
        .map(|i| series.iter().map(|s| s.1[i]).sum())
        .collect();
    let peak = totals.iter().copied().max().unwrap_or(0).max(1);
    let step = tick_step(peak);
    let ymax = (step * peak.div_ceil(step)) as f64;
    let bw = pw / labels.len().max(1) as f64;
    let gap = 4.0_f64.min(bw * 0.25);
    let every = labels.len().div_ceil(12).max(1);
    let mut out = String::new();
    let mut v = 0;
    while v as f64 <= ymax {
        let y = top + ph - v as f64 / ymax * ph;
        let _ = write!(
            out,
            "<line class=\"grid\" x1=\"{left}\" x2=\"{}\" y1=\"{y:.1}\" y2=\"{y:.1}\"/>\
             <text class=\"tick\" x=\"{}\" y=\"{:.1}\" text-anchor=\"end\">{v}</text>",
            w - right,
            left - 6.0,
            y + 4.0
        );
        v += step;
    }
    for (i, lab) in labels.iter().enumerate() {
        let x = left + i as f64 * bw + gap / 2.0;
        let mut base = top + ph;
        let shown: Vec<&(String, Vec<u64>, &str)> = series.iter().filter(|s| s.1[i] > 0).collect();
        for (k, s) in shown.iter().enumerate() {
            let bh = s.1[i] as f64 / ymax * ph;
            let last = k == shown.len() - 1;
            let seg = bh - if !last && bh > 3.0 { 2.0 } else { 0.0 };
            let _ = write!(
                out,
                "<path class=\"mark\" fill=\"{}\" d=\"{}\"/>",
                s.2,
                bar_path(x, base - bh, bw - gap, seg, last)
            );
            base -= bh;
        }
        let _ = write!(
            out,
            "<rect class=\"hit\" x=\"{:.1}\" y=\"{top}\" width=\"{bw:.1}\" height=\"{ph}\"{}/>",
            left + i as f64 * bw,
            tip(&tips[i])
        );
        if i % every == 0 {
            let _ = write!(
                out,
                "<text class=\"tick\" x=\"{:.1}\" y=\"{}\" text-anchor=\"middle\">{}</text>",
                left + i as f64 * bw + bw / 2.0,
                h - bottom + 16.0,
                esc(lab)
            );
        }
    }
    let _ = write!(
        out,
        "<line class=\"axis\" x1=\"{left}\" x2=\"{}\" y1=\"{:.1}\" y2=\"{:.1}\"/>",
        w - right,
        top + ph,
        top + ph
    );
    svg(w, h, &out, "chart", label)
}

/// Horizontal bars, one per (label, value, tooltip).
fn hbars(rows: &[(String, u64, String)], colour: &str) -> String {
    if rows.is_empty() {
        return "<p class=\"sub\">None.</p>".into();
    }
    let (w, row_h, label_w) = (540.0, 22.0, 200.0);
    let peak = rows.iter().map(|r| r.1).max().unwrap_or(1).max(1) as f64;
    let pw = w - label_w - 60.0;
    let mut out = String::new();
    for (i, (lab, v, t)) in rows.iter().enumerate() {
        let y = 3.0 + i as f64 * row_h;
        let bw = (*v as f64 / peak * pw).max(2.0);
        let _ = write!(
            out,
            "<text class=\"lab\" x=\"{:.0}\" y=\"{:.1}\" text-anchor=\"end\">{}</text>\
             <path class=\"mark\" fill=\"{colour}\" d=\"{}\"/>\
             <text class=\"val\" x=\"{:.1}\" y=\"{:.1}\">{v}</text>\
             <rect class=\"hit\" x=\"0\" y=\"{y:.1}\" width=\"{w}\" height=\"{row_h}\"{}/>",
            label_w - 8.0,
            y + row_h / 2.0 + 4.0,
            esc(lab),
            hbar_path(label_w, y + 4.0, bw, row_h - 8.0, true),
            label_w + bw + 6.0,
            y + row_h / 2.0 + 4.0,
            tip(t)
        );
    }
    svg(
        w,
        row_h * rows.len() as f64 + 6.0,
        &out,
        "chart",
        "Reject reasons",
    )
}

/// One point of a strip plot: (seconds, colour, tooltip, ID for the jitter).
type Point = (f64, &'static str, String, String);

/// Dots per row on a log time axis, with a bar at each row's median.
fn strip_plot(rows: &[(String, Vec<Point>)], label: &str) -> String {
    let rows: Vec<&(String, Vec<Point>)> = rows.iter().filter(|r| !r.1.is_empty()).collect();
    if rows.is_empty() {
        return "<p class=\"sub\">No data.</p>".into();
    }
    let w = 1110.0;
    let (left, right, top, row_h) = (150.0, 16.0, 22.0, 26.0);
    let pw = w - left - right;
    let lo = 0.5_f64;
    let hi = rows
        .iter()
        .flat_map(|r| r.1.iter().map(|p| p.0))
        .fold(lo, f64::max)
        * 1.2;
    let x = |v: f64| left + (v.clamp(lo, hi).log10() - lo.log10()) / (hi.log10() - lo.log10()) * pw;
    let h = top + row_h * rows.len() as f64 + 30.0;
    let mut out = String::new();
    for (t, lab) in [
        (1.0, "1s"),
        (10.0, "10s"),
        (60.0, "1m"),
        (600.0, "10m"),
        (3600.0, "1h"),
        (21_600.0, "6h"),
        (86_400.0, "1d"),
        (604_800.0, "7d"),
    ] {
        if t > hi {
            break;
        }
        let _ = write!(
            out,
            "<line class=\"grid\" x1=\"{0:.1}\" x2=\"{0:.1}\" y1=\"{1}\" y2=\"{2}\"/>\
             <text class=\"tick\" x=\"{0:.1}\" y=\"{3}\" text-anchor=\"middle\">{lab}</text>",
            x(t),
            top - 6.0,
            h - 28.0,
            h - 12.0
        );
    }
    for (i, (lab, pts)) in rows.iter().enumerate() {
        let yc = top + i as f64 * row_h + row_h / 2.0;
        let _ = write!(
            out,
            "<text class=\"lab\" x=\"{:.0}\" y=\"{:.1}\" text-anchor=\"end\">{} \
             <tspan class=\"tick\">{}</tspan></text>",
            left - 8.0,
            yc + 4.0,
            esc(lab),
            pts.len()
        );
        for (v, colour, t, id) in pts {
            let _ = write!(
                out,
                "<circle class=\"sdot\" cx=\"{:.1}\" cy=\"{:.1}\" r=\"3\" fill=\"{colour}\"{}/>",
                x(*v),
                yc + jitter(id) * (row_h - 8.0),
                tip(t)
            );
        }
        let vals: Vec<f64> = pts.iter().map(|p| p.0).collect();
        let med = median_f(&vals).unwrap_or(lo);
        let _ = write!(
            out,
            "<line class=\"med\" x1=\"{0:.1}\" x2=\"{0:.1}\" y1=\"{1:.1}\" y2=\"{2:.1}\"{3}/>",
            x(med),
            yc - row_h / 2.0 + 2.0,
            yc + row_h / 2.0 - 2.0,
            tip(&format!("{lab}: median {}", dur(med)))
        );
    }
    svg(w, h, &out, "chart", label)
}

/// Squarified treemap layout: one rectangle (x, y, w, h) per value, in order. Values must
/// be above zero.
fn squarify(vals: &[f64], x: f64, y: f64, w: f64, h: f64) -> Vec<(f64, f64, f64, f64)> {
    let total: f64 = vals.iter().sum();
    if total <= 0.0 || w <= 0.0 || h <= 0.0 {
        return vec![(x, y, 0.0, 0.0); vals.len()];
    }
    let (mut x, mut y, mut w, mut h) = (x, y, w, h);
    let areas: Vec<f64> = vals.iter().map(|v| v * w * h / total).collect();
    let mut rects = Vec::with_capacity(vals.len());
    let mut i = 0;
    while i < areas.len() {
        let short = w.min(h);
        let worst = |row: &[f64]| {
            let s: f64 = row.iter().sum();
            row.iter()
                .map(|&r| (short * short * r / (s * s)).max(s * s / (short * short * r)))
                .fold(0.0, f64::max)
        };
        let mut j = i + 1;
        while j < areas.len() && worst(&areas[i..=j]) <= worst(&areas[i..j]) {
            j += 1;
        }
        let s: f64 = areas[i..j].iter().sum();
        if w >= h {
            let cw = s / h;
            let mut cy = y;
            for &a in &areas[i..j] {
                rects.push((x, cy, cw, a / cw));
                cy += a / cw;
            }
            x += cw;
            w -= cw;
        } else {
            let rh = s / w;
            let mut cx = x;
            for &a in &areas[i..j] {
                rects.push((cx, y, a / rh, rh));
                cx += a / rh;
            }
            y += rh;
            h -= rh;
        }
        i = j;
    }
    rects
}

// ---------- sections ----------

/// Session names, grouped by the part before the first `-`.
fn names(recs: &[Rec]) -> Vec<String> {
    let mut set: BTreeSet<&str> = BTreeSet::new();
    for r in recs {
        set.insert(&r.from);
        set.insert(&r.to);
    }
    let mut names: Vec<String> = set.into_iter().map(str::to_owned).collect();
    names.sort_by(|a, b| {
        let head = |n: &str| n.split('-').next().unwrap_or("").to_string();
        (head(a), a).cmp(&(head(b), b))
    });
    names
}

fn headline(d: &Data, l: &Links, names: &[String]) -> String {
    let recs = &d.recs;
    let tasks: Vec<&Rec> = recs.iter().filter(|r| r.kind == "task").collect();
    let turn: Vec<f64> = tasks
        .iter()
        .filter_map(|t| l.replies.get(&t.id))
        .zip(tasks.iter().filter(|t| l.replies.contains_key(&t.id)))
        .map(|(&ri, t)| (recs[ri].created - t.created) as f64)
        .collect();
    let waits: Vec<f64> = recs.iter().filter_map(claim_wait).collect();
    let total: u64 = recs.iter().map(|r| r.size).sum();
    let alive = d
        .beats
        .values()
        .filter(|&&t| d.now - t <= STALE_AFTER)
        .count();
    let (lo, hi) = (
        recs.iter().map(|r| r.created).min().unwrap_or(0),
        recs.iter().map(|r| r.created).max().unwrap_or(0),
    );
    let multi = l.threads.iter().filter(|t| t.len() > 1).count();
    let count = |state: &str| recs.iter().filter(|r| r.state == state).count();
    let tiles = [
        (
            "Messages",
            grouped(recs.len()),
            format!("over {:.1} days", (hi - lo) as f64 / 86_400.0),
        ),
        (
            "Sessions",
            names.len().to_string(),
            format!("{alive} watchers beating now"),
        ),
        (
            "Message bytes",
            size_str(total),
            format!("+ {} other files", size_str(d.other.values().sum())),
        ),
        (
            "Threads",
            grouped(multi),
            format!("{} single messages", grouped(l.threads.len() - multi)),
        ),
        (
            "Median claim wait",
            median_f(&waits).map_or("-".into(), dur),
            "created to claimed".into(),
        ),
        (
            "Median task turnaround",
            median_f(&turn).map_or("-".into(), dur),
            format!("{}/{} tasks replied", turn.len(), tasks.len()),
        ),
        (
            "Rejected",
            count("rejected").to_string(),
            format!("{} dead-lettered", count("dlq")),
        ),
        (
            "Watcher exits",
            d.exits.len().to_string(),
            "in watch-exits.log".into(),
        ),
    ];
    let mut out = String::from("<section class=\"tiles\" aria-label=\"Headline figures\">");
    for (label, value, note) in tiles {
        let _ = write!(
            out,
            "<div class=\"tile\"><div class=\"label\">{label}</div>\
             <div class=\"value\">{}</div><div class=\"note\">{}</div></div>",
            esc(&value),
            esc(&note)
        );
    }
    out.push_str("</section>");
    out
}

/// Who talks to whom as a chord diagram: each session's arc is split into sent (solid)
/// and received (faded); ribbon width is the message count, colour the type.
fn chord(recs: &[Rec], names: &[String]) -> String {
    use std::f64::consts::PI;
    let idx: HashMap<&str, usize> = names
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i))
        .collect();
    let mut flows: BTreeMap<(&str, &str, &str), u64> = BTreeMap::new();
    let mut sent: HashMap<&str, u64> = HashMap::new();
    let mut got: HashMap<&str, u64> = HashMap::new();
    for r in recs {
        *flows.entry((&r.from, &r.to, r.kind)).or_default() += 1;
        *sent.entry(&r.from).or_default() += 1;
        *got.entry(&r.to).or_default() += 1;
    }
    let n = names.len();
    let total = (recs.len() * 2).max(1) as f64;
    let pad = 0.035;
    let unit = (2.0 * PI - pad * n as f64).max(0.1) / total;
    let (size, r, ring) = (660.0, 300.0, 14.0);
    let (cx, cy) = (size / 2.0, size / 2.0);
    let pos = |a: f64, rr: f64| (cx + rr * a.sin(), cy - rr * a.cos());
    let arc = |a0: f64, a1: f64, rr: f64, sweep: u8| {
        let (x, y) = pos(a1, rr);
        let large = u8::from((a1 - a0).abs() > PI);
        format!("A{rr},{rr} 0 {large} {sweep} {x:.1},{y:.1}")
    };
    // Per name: (start, end of sent, end).
    let mut arcs = Vec::new();
    let mut out_at: HashMap<&str, f64> = HashMap::new();
    let mut in_at: HashMap<&str, f64> = HashMap::new();
    let mut a = 0.0;
    for name in names {
        let nm = name.as_str();
        let a_out = a + sent.get(nm).copied().unwrap_or(0) as f64 * unit;
        let a1 = a_out + got.get(nm).copied().unwrap_or(0) as f64 * unit;
        arcs.push((nm, a, a_out, a1));
        out_at.insert(nm, a);
        in_at.insert(nm, a_out);
        a = a1 + pad;
    }
    let rank = |k: &str| TYPES.iter().position(|t| *t == k).unwrap_or(0);
    let gap = |s: &str, t: &str| (idx[t] as i64 - idx[s] as i64).rem_euclid(n as i64);
    // Sent ribbons in target order and received ones in source order, so ends do not cross.
    let mut keys: Vec<(&str, &str, &str)> = flows.keys().copied().collect();
    keys.sort_by_key(|k| (idx[k.0], gap(k.0, k.1), rank(k.2)));
    let mut seg_out = HashMap::new();
    for k in &keys {
        let at = out_at.get_mut(k.0).expect("every sender is a name");
        let span = flows[k] as f64 * unit;
        seg_out.insert(*k, (*at, *at + span));
        *at += span;
    }
    keys.sort_by_key(|k| (idx[k.1], -gap(k.0, k.1), rank(k.2)));
    let mut seg_in = HashMap::new();
    for k in &keys {
        let at = in_at.get_mut(k.1).expect("every receiver is a name");
        let span = flows[k] as f64 * unit;
        seg_in.insert(*k, (*at, *at + span));
        *at += span;
    }
    let mut out = String::new();
    keys.sort_by_key(|k| std::cmp::Reverse(flows[k]));
    for k in &keys {
        let ((sa0, sa1), (ta0, ta1)) = (seg_out[k], seg_in[k]);
        let (p, q) = (pos(sa0, r), pos(ta0, r));
        let _ = write!(
            out,
            "<path class=\"rib\" data-s=\"{}\" data-t=\"{}\" fill=\"{}\" \
             d=\"M{:.1},{:.1}{}Q{cx},{cy} {:.1},{:.1}{}Q{cx},{cy} {:.1},{:.1}Z\"{}/>",
            esc(k.0),
            esc(k.1),
            type_var(k.2),
            p.0,
            p.1,
            arc(sa0, sa1, r, 1),
            q.0,
            q.1,
            arc(ta0, ta1, r, 1),
            p.0,
            p.1,
            tip(&format!("{} → {}: {} {}", k.0, k.1, flows[k], k.2))
        );
    }
    for (nm, a0, a_out, a1) in arcs {
        let note = format!(
            "{nm}: sent {}, received {}",
            sent.get(nm).copied().unwrap_or(0),
            got.get(nm).copied().unwrap_or(0)
        );
        for (b0, b1, class) in [(a0, a_out, "out"), (a_out, a1, "in")] {
            if b1 - b0 < 1e-6 {
                continue;
            }
            let (p0, p1) = (pos(b0, r + 2.0), pos(b1, r + 2.0 + ring));
            let _ = write!(
                out,
                "<path class=\"node {class}\" data-n=\"{}\" d=\"M{:.1},{:.1}{}L{:.1},{:.1}{}Z\"{}/>",
                esc(nm),
                p0.0,
                p0.1,
                arc(b0, b1, r + 2.0, 1),
                p1.0,
                p1.1,
                arc(b1, b0, r + 2.0 + ring, 0),
                tip(&note)
            );
        }
        let mid = (a0 + a1) / 2.0;
        let (x, y) = pos(mid, r + ring + 10.0);
        let flip = mid > PI;
        let deg = mid.to_degrees() - 90.0 + if flip { 180.0 } else { 0.0 };
        let _ = write!(
            out,
            "<text class=\"nlab\" data-n=\"{0}\" transform=\"translate({x:.1},{y:.1}) rotate({deg:.1})\" \
             text-anchor=\"{1}\" dominant-baseline=\"middle\">{0}</text>",
            esc(nm),
            if flip { "end" } else { "start" }
        );
    }
    let margin = 125.0;
    svg(
        size + 2.0 * margin,
        size + 2.0 * margin,
        &format!("<g transform=\"translate({margin},{margin})\">{out}</g>"),
        "chart chord",
        "Who talks to whom",
    )
}

/// Rows send, columns receive; shade is the message count on a log scale.
fn matrix(recs: &[Rec], names: &[String]) -> String {
    let mut counts: HashMap<(&str, &str), u64> = HashMap::new();
    for r in recs {
        *counts.entry((&r.from, &r.to)).or_default() += 1;
    }
    let peak = counts.values().copied().max().unwrap_or(1) as f64;
    let (cell, left, top) = (22.0, 150.0, 150.0);
    let n = names.len() as f64;
    let mut out = String::new();
    for (j, name) in names.iter().enumerate() {
        let _ = write!(
            out,
            "<text class=\"tick\" transform=\"translate({:.1},{}) rotate(-60)\">{}</text>",
            left + j as f64 * cell + cell / 2.0 + 4.0,
            top - 6.0,
            esc(name)
        );
    }
    for (i, s) in names.iter().enumerate() {
        let y = top + i as f64 * cell;
        let _ = write!(
            out,
            "<text class=\"tick\" x=\"{}\" y=\"{:.1}\" text-anchor=\"end\">{}</text>",
            left - 6.0,
            y + cell / 2.0 + 4.0,
            esc(s)
        );
        for (j, t) in names.iter().enumerate() {
            let c = counts.get(&(s.as_str(), t.as_str())).copied().unwrap_or(0);
            let fill = if c == 0 {
                "var(--empty)".to_string()
            } else {
                seq_var(((c as f64).ln_1p() / peak.ln_1p() * 6.999) as usize)
            };
            let _ = write!(
                out,
                "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{2}\" height=\"{2}\" rx=\"3\" fill=\"{fill}\"{3}/>",
                left + j as f64 * cell + 1.0,
                y + 1.0,
                cell - 2.0,
                tip(&format!("{s} → {t}: {c} messages"))
            );
        }
    }
    svg(
        left + cell * n + 110.0,
        top + cell * n + 10.0,
        &out,
        "chart",
        "Sender by recipient matrix",
    )
}

fn claim_waits(recs: &[Rec]) -> String {
    let mut by_to: BTreeMap<&str, Vec<Point>> = BTreeMap::new();
    for r in recs {
        if let Some(w) = claim_wait(r) {
            by_to.entry(&r.to).or_default().push((
                w,
                type_var(r.kind),
                format!(
                    "{}  {}  {} → {}  waited {}",
                    r.id,
                    r.kind,
                    r.from,
                    r.to,
                    dur(w)
                ),
                r.id.clone(),
            ));
        }
    }
    let mut rows: Vec<(String, Vec<Point>)> =
        by_to.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.1.len()));
    strip_plot(&rows, "Claim wait by recipient")
}

fn turnarounds(recs: &[Rec], l: &Links) -> String {
    let mut by_worker: BTreeMap<&str, Vec<Point>> = BTreeMap::new();
    for t in recs.iter().filter(|r| r.kind == "task") {
        if let Some(&ri) = l.replies.get(&t.id) {
            let reply = &recs[ri];
            let secs = ((reply.created - t.created) as f64).max(0.0);
            by_worker.entry(&t.to).or_default().push((
                secs,
                status_var(&reply.status),
                format!(
                    "{}  {} → {}  turnaround {}  {}",
                    t.id,
                    t.from,
                    t.to,
                    dur(secs),
                    reply.status
                ),
                t.id.clone(),
            ));
        }
    }
    let mut rows: Vec<(String, Vec<Point>)> = by_worker
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.1.len()));
    strip_plot(&rows, "Task turnaround by worker")
}

/// Tasks per worker by how they were answered.
fn reply_rate(recs: &[Rec], l: &Links) -> String {
    const KEYS: [&str; 4] = ["ok", "failed", "rejected", "no reply"];
    let mut stat: BTreeMap<&str, [u64; 4]> = BTreeMap::new();
    for t in recs.iter().filter(|r| r.kind == "task") {
        let key = match l.replies.get(&t.id) {
            Some(&ri) => match recs[ri].status.as_str() {
                "failed" => 1,
                "rejected" => 2,
                _ => 0,
            },
            None => 3,
        };
        stat.entry(&t.to).or_default()[key] += 1;
    }
    if stat.is_empty() {
        return "<p class=\"sub\">No tasks.</p>".into();
    }
    let mut workers: Vec<(&str, [u64; 4])> = stat.into_iter().collect();
    workers.sort_by_key(|w| std::cmp::Reverse(w.1.iter().sum::<u64>()));
    let peak = workers
        .iter()
        .map(|w| w.1.iter().sum::<u64>())
        .max()
        .unwrap_or(1) as f64;
    let (w, left, row_h) = (540.0, 150.0, 24.0);
    let mut out = String::new();
    for (i, (name, counts)) in workers.iter().enumerate() {
        let y = 4.0 + i as f64 * row_h;
        let total: u64 = counts.iter().sum();
        let mut x = left;
        let _ = write!(
            out,
            "<text class=\"lab\" x=\"{}\" y=\"{:.1}\" text-anchor=\"end\">{}</text>",
            left - 8.0,
            y + 15.0,
            esc(name)
        );
        let present: Vec<usize> = (0..4).filter(|&k| counts[k] > 0).collect();
        for (j, &k) in present.iter().enumerate() {
            let last = j == present.len() - 1;
            let bw = counts[k] as f64 / peak * (w - left - 70.0);
            let _ = write!(
                out,
                "<path class=\"mark\" fill=\"{}\" d=\"{}\"{}/>",
                status_var(KEYS[k]),
                hbar_path(
                    x,
                    y + 4.0,
                    (bw - if last { 0.0 } else { 2.0 }).max(0.5),
                    row_h - 9.0,
                    last
                ),
                tip(&format!(
                    "{name}: {} of {total} tasks {}",
                    counts[k], KEYS[k]
                ))
            );
            x += bw;
        }
        let _ = write!(
            out,
            "<text class=\"val\" x=\"{:.1}\" y=\"{:.1}\">{:.0}% of {total}</text>",
            x + 6.0,
            y + 15.0,
            (1.0 - counts[3] as f64 / total as f64) * 100.0
        );
    }
    legend(&[
        ("✓ ok", "var(--good)"),
        ("✕ failed", "var(--critical)"),
        ("! rejected", "var(--serious)"),
        ("· no reply", "var(--muted)"),
    ]) + &svg(
        w,
        8.0 + row_h * workers.len() as f64,
        &out,
        "chart",
        "Reply rate per worker",
    )
}

fn states_table(recs: &[Rec]) -> String {
    let mut counts: HashMap<(&str, &str), usize> = HashMap::new();
    for r in recs {
        *counts.entry((r.kind, r.state)).or_default() += 1;
    }
    let get = |t: &str, s: &str| counts.get(&(t, s)).copied().unwrap_or(0);
    let present: Vec<&str> = STATES
        .into_iter()
        .filter(|s| TYPES.iter().any(|t| get(t, s) > 0))
        .collect();
    let mut out = String::from("<div class=\"scroll\"><table><thead><tr><th>Type</th>");
    for s in &present {
        let _ = write!(out, "<th class=\"n\">{s}</th>");
    }
    out.push_str("</tr></thead><tbody>");
    for t in TYPES {
        let _ = write!(out, "<tr><td>{t}</td>");
        for s in &present {
            let _ = write!(out, "<td class=\"n\">{}</td>", get(t, s));
        }
        out.push_str("</tr>");
    }
    out.push_str("</tbody></table></div>");
    out
}

fn reject_reasons(d: &Data) -> String {
    let mut counts: BTreeMap<&str, u64> = BTreeMap::new();
    for r in d.reasons.values() {
        *counts.entry(r).or_default() += 1;
    }
    let mut rows: Vec<(String, u64, String)> = counts
        .into_iter()
        .map(|(k, v)| (k.to_string(), v, format!("{k}: {v}")))
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.1));
    hbars(&rows, "var(--serious)")
}

fn in_flight(d: &Data) -> String {
    let live: Vec<&Rec> = d
        .recs
        .iter()
        .filter(|r| matches!(r.state, "inbox" | "claimed"))
        .collect();
    if live.is_empty() {
        return "<p class=\"sub\">Nothing in an inbox or .processing.</p>".into();
    }
    let mut out = String::from(
        "<div class=\"scroll\"><table><thead><tr><th>ID</th><th>State</th><th>Type</th>\
         <th>From</th><th>To</th><th class=\"n\">Age</th></tr></thead><tbody>",
    );
    for r in live {
        let _ = write!(
            out,
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td class=\"n\">{}</td></tr>",
            esc(&r.id),
            r.state,
            r.kind,
            esc(&r.from),
            esc(&r.to),
            dur((d.now - r.mtime) as f64)
        );
    }
    out.push_str("</tbody></table></div>");
    out
}

/// The days the heatmap and sparklines cover: (first, count, messages before the first).
fn day_range(recs: &[Rec]) -> (i64, i64, usize) {
    let days = || recs.iter().map(|r| uk_day(r.created));
    let (lo, hi) = (days().min().unwrap_or(0), days().max().unwrap_or(0));
    let first = lo.max(hi - MAX_DAYS + 1);
    (first, hi - first + 1, days().filter(|&d| d < first).count())
}

fn heatmap(recs: &[Rec], d0: i64, ndays: i64) -> String {
    let mut grid: HashMap<(i64, i64), u64> = HashMap::new();
    for r in recs {
        let day = uk_day(r.created) - d0;
        if day >= 0 {
            let hour = uk_local(r.created).0.rem_euclid(86_400) / 3600;
            *grid.entry((day, hour)).or_default() += 1;
        }
    }
    let peak = grid.values().copied().max().unwrap_or(1) as f64;
    let (cw, ch, left, top) = (40.0, 22.0, 92.0, 22.0);
    let mut out = String::new();
    for hr in (0..24).step_by(3) {
        let _ = write!(
            out,
            "<text class=\"tick\" x=\"{:.0}\" y=\"{}\" text-anchor=\"middle\">{hr:02}</text>",
            left + hr as f64 * cw + cw / 2.0,
            top - 8.0
        );
    }
    for di in 0..ndays {
        let label = day_label(d0 + di);
        let y = top + di as f64 * ch;
        let _ = write!(
            out,
            "<text class=\"tick\" x=\"{}\" y=\"{:.0}\" text-anchor=\"end\">{label}</text>",
            left - 8.0,
            y + ch / 2.0 + 4.0
        );
        for hr in 0..24 {
            let c = grid.get(&(di, hr)).copied().unwrap_or(0);
            let fill = if c == 0 {
                "var(--empty)".to_string()
            } else {
                seq_var(((c as f64 / peak).sqrt() * 6.999) as usize)
            };
            let _ = write!(
                out,
                "<rect x=\"{:.0}\" y=\"{:.0}\" width=\"{}\" height=\"{}\" rx=\"3\" fill=\"{fill}\"{}/>",
                left + hr as f64 * cw + 1.0,
                y + 1.0,
                cw - 2.0,
                ch - 2.0,
                tip(&format!("{label} {hr:02}:00-{hr:02}:59 UK: {c} messages"))
            );
        }
    }
    svg(
        left + 24.0 * cw + 10.0,
        top + ndays as f64 * ch + 10.0,
        &out,
        "chart",
        "Messages by day and hour",
    )
}

fn sparkline(vals: &[u64], d0: i64) -> String {
    let (w, h) = (150.0, 26.0);
    let peak = vals.iter().copied().max().unwrap_or(0).max(1) as f64;
    let bw = w / vals.len().max(1) as f64;
    let mut out = String::new();
    for (i, &v) in vals.iter().enumerate() {
        let bh = v as f64 / peak * (h - 3.0);
        let x = i as f64 * bw;
        if v > 0 {
            let _ = write!(
                out,
                "<rect class=\"spark\" x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{bh:.1}\" rx=\"1\"/>",
                x + 0.5,
                h - bh,
                (bw - 1.5).max(1.0)
            );
        }
        let _ = write!(
            out,
            "<rect class=\"hit\" x=\"{x:.1}\" y=\"0\" width=\"{bw:.1}\" height=\"{h}\"{}/>",
            tip(&format!("{}: {v} sent", day_label(d0 + i as i64)))
        );
    }
    format!(
        "<svg class=\"sparkline\" viewBox=\"0 0 {w} {h}\" width=\"{w}\" height=\"{h}\" \
         role=\"img\" aria-label=\"Messages sent per day\">{out}</svg>"
    )
}

fn session_table(d: &Data, l: &Links, names: &[String], d0: i64, ndays: i64) -> String {
    let recs = &d.recs;
    let mut rows: Vec<(usize, String)> = Vec::new();
    for name in names {
        let sent: Vec<&Rec> = recs.iter().filter(|r| &r.from == name).collect();
        let got: Vec<&Rec> = recs.iter().filter(|r| &r.to == name).collect();
        let tasks_in: Vec<&&Rec> = got.iter().filter(|r| r.kind == "task").collect();
        let waits: Vec<f64> = got.iter().filter_map(|r| claim_wait(r)).collect();
        let replied = tasks_in
            .iter()
            .filter(|t| l.replies.contains_key(&t.id))
            .count();
        let mut per_day = vec![0u64; ndays as usize];
        for r in &sent {
            let day = uk_day(r.created) - d0;
            if (0..ndays).contains(&day) {
                per_day[day as usize] += 1;
            }
        }
        let dash = "<span class=\"dim\">-</span>";
        rows.push((
            sent.len(),
            format!(
                "<tr><td class=\"nowrap\"><strong>{}</strong></td><td>{}</td><td>{}</td>\
                 <td class=\"n\">{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td>\
                 <td class=\"n\">{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td></tr>",
                esc(name),
                d.modes.get(name).copied().unwrap_or("-"),
                sparkline(&per_day, d0),
                sent.len(),
                got.len(),
                size_str(sent.iter().map(|r| r.size).sum()),
                median_f(&waits).map_or(dash.to_string(), dur),
                tasks_in.len(),
                if tasks_in.is_empty() {
                    dash.to_string()
                } else {
                    format!("{:.0}%", replied as f64 * 100.0 / tasks_in.len() as f64)
                }
            ),
        ));
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.0));
    let mut out = String::from(
        "<div class=\"scroll\"><table><thead><tr><th>Session</th><th>Mode</th>\
         <th>Sent per day</th><th class=\"n\">Sent</th><th class=\"n\">Received</th>\
         <th class=\"n\">Bytes sent</th><th class=\"n\">Median claim wait</th>\
         <th class=\"n\">Tasks in</th><th class=\"n\">Replied</th></tr></thead><tbody>",
    );
    for (_, row) in rows {
        out.push_str(&row);
    }
    out.push_str("</tbody></table></div>");
    out
}

/// Type -> (bytes, messages).
type Leaves<'a> = BTreeMap<&'a str, (u64, usize)>;
/// Session folder -> its leaves.
type Boxes<'a> = BTreeMap<&'a str, Leaves<'a>>;

/// Bytes on disk: folder -> session -> type, plus the other files in the root.
fn treemap(d: &Data) -> String {
    let (w, h) = (1110.0, 600.0);
    // state -> folder -> type -> (bytes, messages)
    let mut tree: BTreeMap<&str, Boxes> = BTreeMap::new();
    for r in &d.recs {
        let e = tree
            .entry(r.state)
            .or_default()
            .entry(&r.folder)
            .or_default()
            .entry(r.kind)
            .or_default();
        e.0 += r.size;
        e.1 += 1;
    }
    let sum = |g: &Boxes| -> u64 { g.values().flat_map(|t| t.values()).map(|v| v.0).sum() };
    let mut groups: Vec<(&str, Option<&Boxes>, u64)> =
        tree.iter().map(|(k, g)| (*k, Some(g), sum(g))).collect();
    groups.sort_by_key(|g| std::cmp::Reverse(g.2));
    let other: u64 = d.other.values().sum();
    if other > 0 {
        groups.push(("other files", None, other));
    }
    let vals: Vec<f64> = groups.iter().map(|g| g.2 as f64).collect();
    let mut out = String::new();
    for (&(gname, g, gv), (x, y, gw, gh)) in groups.iter().zip(squarify(&vals, 0.0, 0.0, w, h)) {
        let _ = write!(
            out,
            "<rect class=\"tm-group\" x=\"{x:.1}\" y=\"{y:.1}\" width=\"{gw:.1}\" height=\"{gh:.1}\"/>"
        );
        let head = if gh > 40.0 && gw > 60.0 { 18.0 } else { 0.0 };
        if head > 0.0 {
            let full = format!("{gname} · {}", size_str(gv));
            let label = if gw > 7.0 * full.chars().count() as f64 {
                full
            } else {
                gname.to_string()
            };
            if gw > 7.0 * label.chars().count() as f64 {
                let _ = write!(
                    out,
                    "<text class=\"tm-head\" x=\"{:.1}\" y=\"{:.1}\">{}</text>",
                    x + 6.0,
                    y + 13.0,
                    esc(&label)
                );
            }
        }
        let (ix, iy, iw, ih) = (x + 2.0, y + head + 2.0, gw - 4.0, gh - head - 4.0);
        let Some(g) = g else {
            let mut items: Vec<(&String, &u64)> = d.other.iter().collect();
            items.sort_by_key(|i| std::cmp::Reverse(*i.1));
            let v: Vec<f64> = items.iter().map(|i| *i.1 as f64).collect();
            for ((k, b), (sx, sy, sw, sh)) in items.iter().zip(squarify(&v, ix, iy, iw, ih)) {
                let _ = write!(
                    out,
                    "<rect class=\"tm-leaf other\" x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" rx=\"2\"{}/>",
                    sx + 1.0,
                    sy + 1.0,
                    (sw - 2.0).max(0.0),
                    (sh - 2.0).max(0.0),
                    tip(&format!("{k}: {}", size_str(**b)))
                );
                if sw > 70.0 && sh > 16.0 {
                    let _ = write!(
                        out,
                        "<text class=\"tm-lab\" x=\"{:.1}\" y=\"{:.1}\">{}</text>",
                        sx + 5.0,
                        sy + 13.0,
                        esc(k)
                    );
                }
            }
            continue;
        };
        let mut boxes: Vec<(&str, &Leaves, u64)> = g
            .iter()
            .map(|(k, t)| (*k, t, t.values().map(|v| v.0).sum()))
            .collect();
        boxes.sort_by_key(|b| std::cmp::Reverse(b.2));
        let bvals: Vec<f64> = boxes.iter().map(|b| b.2 as f64).collect();
        for (&(bname, types, bv), (bx, by, bw, bh)) in
            boxes.iter().zip(squarify(&bvals, ix, iy, iw, ih))
        {
            let leaves: Vec<(&str, (u64, usize))> = TYPES
                .iter()
                .filter_map(|t| types.get(t).map(|v| (*t, *v)))
                .collect();
            let lvals: Vec<f64> = leaves.iter().map(|l| l.1.0 as f64).collect();
            for (&(t, (bytes, n)), (sx, sy, sw, sh)) in
                leaves
                    .iter()
                    .zip(squarify(&lvals, bx + 1.0, by + 1.0, bw - 2.0, bh - 2.0))
            {
                let _ = write!(
                    out,
                    "<rect class=\"tm-leaf\" fill=\"{}\" x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" rx=\"2\"{}/>",
                    type_var(t),
                    sx + 1.0,
                    sy + 1.0,
                    (sw - 2.0).max(0.0),
                    (sh - 2.0).max(0.0),
                    tip(&format!(
                        "{gname} / {bname} / {t}: {} in {n} messages",
                        size_str(bytes)
                    ))
                );
            }
            let _ = write!(
                out,
                "<rect class=\"tm-box\" x=\"{bx:.1}\" y=\"{by:.1}\" width=\"{bw:.1}\" height=\"{bh:.1}\" rx=\"3\"/>"
            );
            if bw > 80.0 && bh > 30.0 {
                let _ = write!(
                    out,
                    "<text class=\"tm-lab\" x=\"{0:.1}\" y=\"{1:.1}\">{2}</text>\
                     <text class=\"tm-sub\" x=\"{0:.1}\" y=\"{3:.1}\">{4}</text>",
                    bx + 6.0,
                    by + 15.0,
                    esc(bname),
                    by + 28.0,
                    size_str(bv)
                );
            }
        }
    }
    svg(
        w,
        h,
        &out,
        "chart treemap",
        "Bytes by folder, session and type",
    )
}

/// Body, subject and the rest of the JSON envelope, by type. Byte counts only.
fn anatomy(recs: &[Rec]) -> String {
    const PARTS: [(&str, &str); 3] = [
        ("body", "var(--s1)"),
        ("subject", "var(--s2)"),
        ("envelope", "var(--s3)"),
    ];
    let mut sums: HashMap<&str, [u64; 3]> = HashMap::new();
    for r in recs {
        let s = sums.entry(r.kind).or_default();
        s[0] += r.body_bytes;
        s[1] += r.subject_bytes;
        s[2] += r.size.saturating_sub(r.body_bytes + r.subject_bytes);
    }
    let (w, row_h, left) = (1110.0, 30.0, 70.0);
    let peak = sums
        .values()
        .map(|s| s.iter().sum::<u64>())
        .max()
        .unwrap_or(1)
        .max(1) as f64;
    let mut out = String::new();
    for (i, t) in TYPES.iter().enumerate() {
        let s = sums.get(t).copied().unwrap_or_default();
        let total: u64 = s.iter().sum();
        let y = 6.0 + i as f64 * row_h;
        let mut x = left;
        let _ = write!(
            out,
            "<text class=\"lab\" x=\"{}\" y=\"{}\" text-anchor=\"end\">{t}</text>",
            left - 8.0,
            y + 15.0
        );
        for (k, (part, colour)) in PARTS.iter().enumerate() {
            let bw = s[k] as f64 / peak * (w - left - 90.0);
            if bw <= 0.0 {
                continue;
            }
            let _ = write!(
                out,
                "<path class=\"mark\" fill=\"{colour}\" d=\"{}\"{}/>",
                hbar_path(
                    x,
                    y + 4.0,
                    (bw - if k < 2 { 2.0 } else { 0.0 }).max(0.0),
                    row_h - 10.0,
                    k == 2
                ),
                tip(&format!(
                    "{t} {part}: {} ({:.0}%)",
                    size_str(s[k]),
                    s[k] as f64 * 100.0 / total.max(1) as f64
                ))
            );
            x += bw;
        }
        let _ = write!(
            out,
            "<text class=\"val\" x=\"{:.1}\" y=\"{}\">{}</text>",
            x + 6.0,
            y + 15.0,
            size_str(total)
        );
    }
    legend(&PARTS) + &svg(w, 6.0 + row_h * 3.0, &out, "chart", "Byte anatomy")
}

fn biggest(recs: &[Rec]) -> String {
    let mut big: Vec<&Rec> = recs.iter().collect();
    big.sort_by_key(|r| std::cmp::Reverse(r.size));
    let mut out = String::from(
        "<div class=\"scroll\"><table><thead><tr><th>ID</th><th class=\"n\">Size</th>\
         <th>Type</th><th>From</th><th>To</th></tr></thead><tbody>",
    );
    for r in big.into_iter().take(BIGGEST_SHOWN) {
        let _ = write!(
            out,
            "<tr><td>{}</td><td class=\"n\">{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            esc(&r.id),
            size_str(r.size),
            r.kind,
            esc(&r.from),
            esc(&r.to)
        );
    }
    out.push_str("</tbody></table></div>");
    out
}

/// Watcher exits per day by reason, then one row per session. Only the newest `MAX_DAYS`
/// days up to `now`, so a stray old or future time cannot stretch the charts.
fn exits(d: &Data) -> String {
    let Some(newest) = d.exits.last() else {
        return "<p class=\"sub\">No watcher exits logged.</p>".into();
    };
    let last = uk_day(newest.t).min(uk_day(d.now));
    let days = (last - MAX_DAYS + 1)..=last;
    let shown: Vec<&ExitLine> = d
        .exits
        .iter()
        .filter(|e| days.contains(&uk_day(e.t)))
        .collect();
    let left_out = d.exits.len() - shown.len();
    let note = if left_out > 0 {
        format!("<p class=\"sub\">{left_out} exits outside these days are left out.</p>")
    } else {
        String::new()
    };
    if shown.is_empty() {
        return note;
    }
    let mut by_reason: BTreeMap<&str, usize> = BTreeMap::new();
    for e in &shown {
        *by_reason.entry(&e.reason).or_default() += 1;
    }
    let mut reasons: Vec<(&str, usize)> = by_reason.into_iter().collect();
    reasons.sort_by_key(|r| std::cmp::Reverse(r.1));
    let mut colours: Vec<(String, &str)> = reasons
        .iter()
        .take(EXIT_REASONS_SHOWN)
        .zip([
            "var(--s4)",
            "var(--s5)",
            "var(--s6)",
            "var(--s7)",
            "var(--s8)",
        ])
        .map(|(r, c)| (r.0.to_string(), c))
        .collect();
    if reasons.len() > EXIT_REASONS_SHOWN {
        colours.push(("other".into(), "var(--muted)"));
    }
    let fold = |r: &str| {
        colours
            .iter()
            .position(|c| c.0 == r)
            .unwrap_or(colours.len() - 1)
    };
    let (t0, t1) = (shown[0].t, shown[shown.len() - 1].t);
    let d0 = uk_day(t0);
    let ndays = (uk_day(t1) - d0 + 1) as usize;
    let mut per = vec![vec![0u64; ndays]; colours.len()];
    for e in &shown {
        per[fold(&e.reason)][(uk_day(e.t) - d0) as usize] += 1;
    }
    let labels: Vec<String> = (0..ndays).map(|i| short_day(d0 + i as i64)).collect();
    let tips: Vec<String> = (0..ndays)
        .map(|i| {
            let parts: Vec<String> = colours
                .iter()
                .enumerate()
                .filter(|(k, _)| per[*k][i] > 0)
                .map(|(k, c)| format!("{} {}", per[k][i], c.0))
                .collect();
            format!("{}: {}", labels[i], parts.join(", "))
        })
        .collect();
    let series: Vec<(String, Vec<u64>, &str)> = colours
        .iter()
        .zip(&per)
        .map(|(c, v)| (c.0.clone(), v.clone(), c.1))
        .collect();
    let by_day = stacked_columns(&labels, &series, &tips, "Watcher exits per day by reason");

    let names: Vec<&str> = shown
        .iter()
        .map(|e| e.name.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let (w, left, right, row_h, top) = (1110.0, 150.0, 16.0, 20.0, 10.0);
    let x = |t: i64| left + (t - t0) as f64 / (t1 - t0).max(1) as f64 * (w - left - right);
    let h = top + row_h * names.len() as f64 + 26.0;
    let mut out = String::new();
    for (i, nm) in names.iter().enumerate() {
        let y = top + i as f64 * row_h + row_h / 2.0;
        let _ = write!(
            out,
            "<line class=\"grid\" x1=\"{left}\" x2=\"{}\" y1=\"{y:.1}\" y2=\"{y:.1}\"/>\
             <text class=\"lab\" x=\"{}\" y=\"{:.1}\" text-anchor=\"end\">{}</text>",
            w - right,
            left - 8.0,
            y + 4.0,
            esc(nm)
        );
    }
    for e in &shown {
        let row = names.iter().position(|n| *n == e.name).unwrap_or(0);
        let _ = write!(
            out,
            "<circle class=\"dot\" cx=\"{:.1}\" cy=\"{:.1}\" r=\"5\" fill=\"{}\"{}/>",
            x(e.t),
            top + row as f64 * row_h + row_h / 2.0,
            colours[fold(&e.reason)].1,
            tip(&format!(
                "{}  {} ({}): {}",
                uk_short(e.t),
                e.name,
                e.mode,
                e.reason
            ))
        );
    }
    let _ = write!(
        out,
        "<text class=\"tick\" x=\"{left}\" y=\"{}\">{}</text>\
         <text class=\"tick\" x=\"{}\" y=\"{}\" text-anchor=\"end\">{}</text>",
        h - 6.0,
        uk_short(t0),
        w - right,
        h - 6.0,
        uk_short(t1)
    );
    let items: Vec<(&str, &str)> = colours.iter().map(|c| (c.0.as_str(), c.1)).collect();
    note + &legend(&items)
        + &by_day
        + "<h3>Per session</h3>"
        + &svg(w, h, &out, "chart", "Watcher exits per session")
}

/// How often each name's session ID changed: a new ID means the session restarted or was
/// replaced. The IDs themselves are not shown.
fn restarts(recs: &[Rec]) -> String {
    let mut by_name: BTreeMap<&str, Vec<(i64, &str)>> = BTreeMap::new();
    for r in recs {
        if let Some(id) = &r.session_id {
            by_name.entry(&r.from).or_default().push((r.created, id));
        }
    }
    if by_name.is_empty() {
        return "<p class=\"sub\">No message carries a session ID.</p>".into();
    }
    let mut rows: Vec<(usize, String)> = Vec::new();
    for (name, mut seen) in by_name {
        seen.sort();
        let ids: BTreeSet<&str> = seen.iter().map(|s| s.1).collect();
        let changes = seen.windows(2).filter(|w| w[0].1 != w[1].1).count();
        rows.push((
            changes,
            format!(
                "<tr><td class=\"nowrap\"><strong>{}</strong></td><td class=\"n\">{}</td>\
                 <td class=\"n\">{}</td><td class=\"n\">{changes}</td><td class=\"nowrap\">{}</td>\
                 <td class=\"nowrap\">{}</td></tr>",
                esc(name),
                seen.len(),
                ids.len(),
                uk_short(seen[0].0),
                uk_short(seen[seen.len() - 1].0)
            ),
        ));
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.0));
    let mut out = String::from(
        "<div class=\"scroll\"><table><thead><tr><th>Session</th>\
         <th class=\"n\">Messages with an ID</th><th class=\"n\">Session IDs</th>\
         <th class=\"n\">Changes</th><th>First</th><th>Last</th></tr></thead><tbody>",
    );
    for (_, row) in rows {
        out.push_str(&row);
    }
    out.push_str("</tbody></table></div>");
    out
}

/// One thread as a small tree: x is time since its first message (square root scale), y
/// its depth.
fn thread_mini(recs: &[Rec], l: &Links, thread: &[usize]) -> String {
    let mut g = thread.to_vec();
    g.sort_by_key(|&i| recs[i].created);
    let t0 = recs[g[0]].created;
    let span = (recs[g[g.len() - 1]].created - t0).max(1) as f64;
    let (w, h) = (380.0, 150.0);
    let (left, right, top, bottom) = (14.0, 14.0, 14.0, 26.0);
    let maxd = g.iter().map(|&i| l.depth[i]).max().unwrap_or(0).max(1) as f64;
    let x = |i: usize| left + ((recs[i].created - t0) as f64 / span).sqrt() * (w - left - right);
    let y = |i: usize| top + l.depth[i] as f64 / maxd * (h - top - bottom);
    let in_thread: HashMap<&str, usize> = g.iter().map(|&i| (recs[i].id.as_str(), i)).collect();
    let mut out = String::new();
    for &i in &g {
        let up = recs[i]
            .parent
            .as_deref()
            .or(recs[i].reply_to.as_deref())
            .and_then(|p| in_thread.get(p));
        if let Some(&p) = up {
            let mid = (x(p) + x(i)) / 2.0;
            let _ = write!(
                out,
                "<path class=\"link\" d=\"M{:.1},{:.1}C{mid:.1},{:.1} {mid:.1},{:.1} {:.1},{:.1}\"/>",
                x(p),
                y(p),
                y(p),
                y(i),
                x(i),
                y(i)
            );
        }
    }
    for &i in &g {
        let r = &recs[i];
        let _ = write!(
            out,
            "<circle class=\"dot\" cx=\"{:.1}\" cy=\"{:.1}\" r=\"5\" fill=\"{}\"{}/>",
            x(i),
            y(i),
            type_var(r.kind),
            tip(&format!(
                "{}  {}  {} → {}  +{}  depth {}",
                r.id,
                r.kind,
                r.from,
                r.to,
                dur((r.created - t0) as f64),
                l.depth[i]
            ))
        );
    }
    let _ = write!(
        out,
        "<text class=\"tick\" x=\"{left}\" y=\"{}\">{}</text>\
         <text class=\"tick\" x=\"{}\" y=\"{}\" text-anchor=\"end\">+{} (sqrt time)</text>",
        h - 6.0,
        uk_short(t0),
        w - right,
        h - 6.0,
        dur(span)
    );
    let people: BTreeSet<&str> = g
        .iter()
        .flat_map(|&i| [recs[i].from.as_str(), recs[i].to.as_str()])
        .collect();
    format!(
        "<figure class=\"mini\">{}<figcaption>{} messages, {} sessions, depth {maxd}</figcaption></figure>",
        svg(w, h, &out, "chart", "Thread"),
        g.len(),
        people.len()
    )
}

fn orphans(recs: &[Rec], l: &Links) -> String {
    if l.orphans.is_empty() {
        return "<p class=\"sub\">Every parent and reply_to points to a message on disk.</p>"
            .into();
    }
    let mut out = String::from(
        "<div class=\"scroll\"><table><thead><tr><th>Message</th><th>Link</th>\
         <th>Missing ID</th></tr></thead><tbody>",
    );
    for (i, field, missing) in l.orphans.iter().take(ORPHANS_SHOWN) {
        let _ = write!(
            out,
            "<tr><td>{}</td><td>{field}</td><td>{}</td></tr>",
            esc(&recs[*i].id),
            esc(missing)
        );
    }
    out.push_str("</tbody></table></div>");
    if l.orphans.len() > ORPHANS_SHOWN {
        let _ = write!(
            out,
            "<p class=\"sub\">{} more not shown.</p>",
            l.orphans.len() - ORPHANS_SHOWN
        );
    }
    out
}

// ---------- the page ----------

fn card(title: &str, sub: &str, body: &str) -> String {
    let sub = if sub.is_empty() {
        String::new()
    } else {
        format!("<p class=\"sub\">{}</p>", esc(sub))
    };
    format!(
        "<div class=\"card\"><h3>{}</h3>{sub}{body}</div>",
        esc(title)
    )
}

fn row(cards: &[String]) -> String {
    format!("<div class=\"row\">{}</div>", cards.concat())
}

fn render(d: &Data) -> String {
    let head = format!(
        "<header><h1>cq queue analysis</h1><p class=\"meta\">Written {} · queue root <code>{}</code>. \
         Metadata and sizes only: no message bodies or subjects. Reject reasons show their first \
         words only.</p></header>",
        uk_datetime(d.now),
        esc(&d.root)
    );
    let body = if d.recs.is_empty() {
        format!("{head}<p class=\"sub\">No messages found.</p>")
    } else {
        sections(d, &head)
    };
    format!(
        "<!doctype html>\n<html lang=\"en-GB\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta name=\"color-scheme\" content=\"light dark\">\
         <title>cq queue analysis</title>\n<style>{CSS}</style></head>\n\
         <body><main>{body}<footer>Written by cq-top --report. A read-only snapshot: times are \
         UK local, sizes are file lengths on disk, and done/ keeps 14 days.</footer></main>\
         <div id=\"tip\" role=\"tooltip\"></div><script>{JS}</script></body></html>\n"
    )
}

fn sections(d: &Data, head: &str) -> String {
    let recs = &d.recs;
    let l = links(recs);
    let names = names(recs);
    let (lo, hi) = (
        recs.iter().map(|r| r.created).min().unwrap_or(0),
        recs.iter().map(|r| r.created).max().unwrap_or(0),
    );
    let (d0, ndays, before) = day_range(recs);
    let range_note = if before > 0 {
        format!(" The last {MAX_DAYS} days only; {before} older messages are left out.")
    } else {
        String::new()
    };
    let linked: Vec<usize> = (0..recs.len())
        .filter(|&i| recs[i].parent.is_some() || recs[i].reply_to.is_some())
        .collect();
    let agree = linked
        .iter()
        .filter(|&&i| recs[i].hops == l.depth[i] as u64)
        .count();
    let minis: String = l
        .threads
        .iter()
        .filter(|t| t.len() > 1)
        .take(THREADS_SHOWN)
        .map(|t| thread_mini(recs, &l, t))
        .collect();
    let all: [(&str, &str, String); 6] = [
        (
            "talk",
            "Who talks to whom",
            card(
                "Chord",
                "Each session's arc has two parts: solid = sent, faded = received. Ribbon \
                 width = message count, colour = type. Hover a session to isolate its traffic.",
                &(type_legend() + &chord(recs, &names)),
            ) + &card(
                "Matrix",
                "Rows send, columns receive. Shade = message count (log scale).",
                &format!("<div class=\"scroll\">{}</div>", matrix(recs, &names)),
            ),
        ),
        (
            "lifecycle",
            "Lifecycle",
            card(
                "Claim wait by recipient",
                "Created to claimed (a claim sets the file's modified time). Dots = messages, \
                 colour = type, bar = median. Log scale.",
                &(type_legend() + &claim_waits(recs)),
            ) + &card(
                "Task turnaround by worker",
                "Task created to its result created. Colour = result status.",
                &(legend(&[
                    ("✓ ok", "var(--good)"),
                    ("✕ failed", "var(--critical)"),
                    ("! rejected", "var(--serious)"),
                ]) + &turnarounds(recs, &l)),
            ) + &row(&[
                card(
                    "Reply rate per worker",
                    "Tasks received and how they were answered.",
                    &reply_rate(recs, &l),
                ),
                card(
                    "States by type",
                    "Where each message file is now.",
                    &states_table(recs),
                ),
            ]) + &row(&[
                card(
                    "Reject reasons",
                    "First words of each reason file only.",
                    &reject_reasons(d),
                ),
                card(
                    "In flight now",
                    "Queued and claimed messages.",
                    &in_flight(d),
                ),
            ]),
        ),
        (
            "activity",
            "Activity",
            card(
                "Messages by day and hour",
                &format!("UK local time. Shade = message count (square root scale).{range_note}"),
                &format!("<div class=\"scroll\">{}</div>", heatmap(recs, d0, ndays)),
            ) + &card(
                "Sessions",
                &format!("Sparkline = messages sent per day.{range_note}"),
                &session_table(d, &l, &names, d0, ndays),
            ),
        ),
        (
            "sizes",
            "Data sizes",
            card(
                "Where the bytes are",
                "Folder → session (inbox name) → type. Area = bytes on disk. archive/ is not \
                 counted.",
                &(type_legend() + &treemap(d)),
            ) + &card(
                "Byte anatomy",
                "Body, subject and the rest of the JSON envelope, by type. Byte counts only.",
                &anatomy(recs),
            ) + &card(
                &format!("The {BIGGEST_SHOWN} biggest messages"),
                "",
                &biggest(recs),
            ),
        ),
        (
            "health",
            "Health",
            card(
                &format!("Watcher exits, newest {MAX_DAYS} days"),
                "From watch-exits.log and watch-exits.log.old. Reasons with PIDs and times \
                 removed.",
                &exits(d),
            ) + &card(
                "Session-ID restarts per name",
                "From each message's from_session_id. A change means the sending session \
                 restarted or was replaced. The IDs are not shown.",
                &restarts(recs),
            ),
        ),
        (
            "threads",
            "Threads",
            card(
                "Largest threads",
                "Threads are messages joined by parent or reply_to. x = time since the first \
                 message (square root scale), y = depth in the thread.",
                &format!("{}<div class=\"minis\">{minis}</div>", type_legend()),
            ) + &card(
                "Orphan links",
                &format!(
                    "Links to an ID that is not on disk (pruned, or never existed). The hops \
                     field equals the depth found from the links for {agree} of {} linked \
                     messages.",
                    linked.len()
                ),
                &orphans(recs, &l),
            ),
        ),
    ];
    let mut out = String::from(head);
    let _ = write!(
        out,
        "<p class=\"meta\">{} messages created between {} and {} UK time.</p>",
        grouped(recs.len()),
        esc(&uk_short(lo)),
        esc(&uk_short(hi))
    );
    out.push_str(&headline(d, &l, &names));
    out.push_str("<nav>");
    for (id, title, _) in &all {
        let _ = write!(out, "<a href=\"#{id}\">{title}</a>");
    }
    out.push_str("</nav>");
    for (id, title, body) in &all {
        let _ = write!(out, "<section id=\"{id}\"><h2>{title}</h2>{body}</section>");
    }
    out
}

const CSS: &str = concat!(
    html::theme_css!(),
    r#"
:root{--empty:#eeede9;
 --s1:#2a78d6;--s2:#eb6834;--s3:#1baf7a;--s4:#eda100;--s5:#e87ba4;--s6:#008300;--s7:#6250d6;--s8:#e34948;
 --q1:#cde2fb;--q2:#9ec5f4;--q3:#6da7ec;--q4:#3987e5;--q5:#256abf;--q6:#184f95;--q7:#0d366b}
@media (prefers-color-scheme: dark){:root{--empty:#242422;
 --s1:#3987e5;--s2:#d95926;--s3:#199e70;--s4:#c98500;--s5:#d55181;--s6:#008300;--s7:#9085e9;--s8:#e66767;
 --q1:#0d366b;--q2:#184f95;--q3:#256abf;--q4:#3987e5;--q5:#6da7ec;--q6:#9ec5f4;--q7:#cde2fb}}
main{max-width:1180px;margin:0 auto;padding:20px 16px 48px}
h1{font-size:26px;margin:0;font-weight:650}
h2{font-size:19px;margin:36px 0 4px;font-weight:650}
h3{font-size:14px;margin:0 0 6px;color:var(--ink-2);font-weight:600}
.card h3+h3,.card svg+h3{margin-top:16px}
.meta{margin:4px 0 0;color:var(--ink-2)}
code{font-family:"Cascadia Mono",Consolas,monospace;font-size:13px;overflow-wrap:anywhere}
.sub{margin:0 0 10px;color:var(--ink-2);font-size:13px}
.dim{color:var(--ink-2)}
nav{display:flex;flex-wrap:wrap;gap:6px 14px;margin:14px 0 6px;font-size:13.5px}
nav a{color:var(--ink-2);text-decoration:none;border-bottom:1px solid var(--grid)}
nav a:hover{color:var(--ink)}
.card{background:var(--surface);border:1px solid var(--ring);border-radius:10px;padding:16px;
 margin-top:12px;min-width:0;overflow:hidden}
.row{display:grid;grid-template-columns:repeat(auto-fit,minmax(min(100%,460px),1fr));gap:0 12px}
.tiles{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:10px;margin-top:16px}
.tile{background:var(--surface);border:1px solid var(--ring);border-radius:10px;padding:12px 14px}
.tile .label{color:var(--ink-2);font-size:13px}
.tile .value{font-size:26px;font-weight:650;font-variant-numeric:tabular-nums;line-height:1.25}
.tile .note{color:var(--muted);font-size:12.5px}
svg.chart{width:100%;height:auto;display:block}
svg.chord{margin:0 auto}
text{fill:var(--ink-2);font-size:11px}
.tick{fill:var(--muted);font-size:11px}
.lab{fill:var(--ink);font-size:12px}
.val{fill:var(--ink-2);font-size:11px;font-variant-numeric:tabular-nums}
.grid{stroke:var(--grid);stroke-width:1}.axis{stroke:var(--axis);stroke-width:1}
.hit{fill:transparent}
.mark:hover{filter:brightness(1.12)}
.rib{fill-opacity:.55;stroke:var(--surface);stroke-width:.5;transition:fill-opacity .15s}
.chord:hover .rib{fill-opacity:.12}
.chord .rib:hover,.chord.focus .rib.on{fill-opacity:.9}
.chord.focus .rib:not(.on){fill-opacity:.06}
.node{fill:var(--ink-2);stroke:var(--surface);stroke-width:1}.node.in{fill-opacity:.4}
.node:hover{fill:var(--ink)}
.nlab{fill:var(--ink);font-size:12px}.chord.focus .nlab:not(.on){fill:var(--muted)}
.dot{stroke:var(--surface);stroke-width:2}
.sdot{fill-opacity:.7;stroke:var(--surface);stroke-width:.6}
.sdot:hover,.dot:hover{stroke:var(--ink);stroke-width:1.5;fill-opacity:1}
.med{stroke:var(--ink);stroke-width:2.5;stroke-linecap:round}
.link{fill:none;stroke:var(--muted);stroke-width:1.5}
.tm-group{fill:var(--page);stroke:var(--surface);stroke-width:3}
.tm-box{fill:none;stroke:var(--surface);stroke-width:2;pointer-events:none}
.tm-leaf{stroke:none;fill-opacity:.85}.tm-leaf:hover{fill-opacity:1}.tm-leaf.other{fill:var(--muted)}
.tm-head{fill:var(--ink);font-size:12px;font-weight:650}
.tm-lab,.tm-sub{fill:#fff;pointer-events:none;paint-order:stroke;stroke:rgba(0,0,0,.35);stroke-width:2px}
.tm-lab{font-size:11px;font-weight:600}.tm-sub{font-size:10px}
.spark{fill:var(--s1)}
svg.sparkline{display:block}
.legend{display:flex;flex-wrap:wrap;gap:4px 16px;font-size:12px;color:var(--ink-2);margin:4px 0 8px}
.legend i{display:inline-block;width:10px;height:10px;border-radius:3px;margin-right:6px;vertical-align:-1px}
.minis{display:grid;grid-template-columns:repeat(auto-fit,minmax(min(100%,340px),1fr));gap:10px}
figure.mini{margin:0;background:var(--page);border-radius:10px;padding:8px}
figcaption{font-size:12px;color:var(--ink-2);padding:0 6px}
.scroll{overflow-x:auto}
table{border-collapse:collapse;width:100%;font-size:13px;font-variant-numeric:tabular-nums}
th{text-align:left;font-weight:600;color:var(--ink-2);font-size:12.5px;
 border-bottom:1px solid var(--axis);padding:6px 12px 6px 0;white-space:nowrap}
td{border-bottom:1px solid var(--grid);padding:5px 12px 5px 0;white-space:nowrap}
th:last-child,td:last-child{padding-right:0}
tbody tr:last-child td{border-bottom:0}
.n{text-align:right}
.nowrap{white-space:nowrap}
#tip{position:fixed;pointer-events:none;z-index:9;background:var(--ink);color:var(--surface);
 font-size:12px;padding:6px 9px;border-radius:7px;max-width:380px;opacity:0;
 transition:opacity .08s;white-space:pre-wrap}
footer{color:var(--ink-2);font-size:12.5px;margin-top:32px}
"#
);

/// Tooltips from `data-tip`, and the chord's hover: a session's ribbons stand out.
const JS: &str = r#"
(function(){var t=document.getElementById('tip');
document.addEventListener('mousemove',function(e){var el=e.target.closest('[data-tip]');
if(!el){t.style.opacity=0;return}t.textContent=el.getAttribute('data-tip');t.style.opacity=1;
var x=e.clientX+14,y=e.clientY+14,r=t.getBoundingClientRect();
if(x+r.width>innerWidth-8)x=e.clientX-r.width-14;if(y+r.height>innerHeight-8)y=e.clientY-r.height-14;
t.style.left=x+'px';t.style.top=y+'px'});
document.querySelectorAll('svg.chord').forEach(function(s){
s.querySelectorAll('[data-n]').forEach(function(n){var nm=n.getAttribute('data-n');
n.addEventListener('mouseenter',function(){s.classList.add('focus');
s.querySelectorAll('.rib').forEach(function(r){r.classList.toggle('on',r.dataset.s===nm||r.dataset.t===nm)});
s.querySelectorAll('.nlab').forEach(function(l){l.classList.toggle('on',l.dataset.n===nm)})});
n.addEventListener('mouseleave',function(){s.classList.remove('focus')})})})})();
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::temp_queue;

    /// 2026-10-27T11:30:00Z.
    const NOW: i64 = 1_793_100_600;
    const BODY: &str = "BODY-MARKER-do-not-show";
    const SUBJECT: &str = "SUBJECT-MARKER-do-not-show";
    const REASON_TAIL: &str = "REASON-TAIL-MARKER";

    fn msg(dir: &Path, id: &str, from: &str, to: &str, kind: &str, extra: &str) {
        fs::create_dir_all(dir).unwrap();
        let text = format!(
            r#"{{"schema_version":1,"id":"{id}","from":"{from}","to":"{to}","type":"{kind}","subject":"{SUBJECT}","body":"{BODY}","hops":0,"created":"{}"{extra}}}"#,
            &id[..16]
        );
        fs::write(dir.join(format!("{id}.json")), text).unwrap();
    }

    /// A small queue with every folder used, made-up names only.
    fn fixture() -> (tempfile::TempDir, cq::Root) {
        let (dir, root) = temp_queue();
        let t1 = "20261027T100000Z-lead-0001";
        let t2 = "20261027T100500Z-lead-0002";
        msg(&root.done("w1"), t1, "lead", "w1", "task", "");
        msg(
            &root.done("lead"),
            "20261027T101000Z-w1-0003",
            "w1",
            "lead",
            "result",
            &format!(r#","reply_to":"{t1}","status":"ok","from_session_id":"sess-a""#),
        );
        msg(
            &root.done("lead"),
            "20261027T102000Z-w1-0004",
            "w1",
            "lead",
            "info",
            r#","parent":"20261020T000000Z-gone-0000","from_session_id":"sess-b""#,
        );
        msg(&root.inbox("w1"), t2, "lead", "w1", "task", "");
        msg(
            &root.processing("w1"),
            "20261027T110000Z-lead-0005",
            "<i>evil&co</i>",
            "w1",
            "task",
            "",
        );
        let rejected = "20261027T103000Z-lead-0006";
        msg(&root.rejected("w1"), rejected, "lead", "w1", "task", "");
        fs::write(
            root.rejected("w1").join(format!("{rejected}.reason.json")),
            format!(
                r#"{{"id":"{rejected}","reason":"Out of scope for this worker: {REASON_TAIL}"}}"#
            ),
        )
        .unwrap();
        msg(
            &root.dlq("w1"),
            "20261027T104000Z-lead-0007",
            "lead",
            "w1",
            "info",
            "",
        );
        fs::write(
            root.watch_exits(),
            "2026-10-27T09:00:00Z name=w1 pid=42 mode=inbox version=1.11.0 exit=0 reason=\"--max-seconds 60 reached\"\n\
             2026-10-27T09:30:00Z name=w1 pid=43 mode=inbox version=1.11.0 reason=\"stopped by --takeover from PID 4600\"\n\
             garbage line\n",
        )
        .unwrap();
        fs::create_dir_all(root.members()).unwrap();
        fs::write(
            root.members().join("local_key"),
            "w1\n2026-10-27T09:00:00Z\n/work/w1\ninbox\n",
        )
        .unwrap();
        fs::create_dir_all(root.path().join("heartbeat")).unwrap();
        fs::write(root.heartbeat("w1"), "2026-10-27T11:29:50Z\n").unwrap();
        (dir, root)
    }

    #[test]
    fn report_has_no_bodies_subjects_or_full_reasons() {
        let (_dir, root) = fixture();
        let page = page(&root, NOW);
        assert!(page.starts_with("<!doctype html>"));
        for secret in [BODY, SUBJECT, REASON_TAIL, "sess-a", "sess-b", "local_key"] {
            assert!(!page.contains(secret), "{secret}");
        }
        // The reason is cut to its first words.
        assert!(page.contains("out of scope ..."));
        // Every section is drawn from the fixture.
        for part in [
            "Who talks to whom",
            "Claim wait by recipient",
            "Task turnaround by worker",
            "Messages by day and hour",
            "Where the bytes are",
            "Byte anatomy",
            "biggest messages",
            "Watcher exits",
            "Session-ID restarts per name",
            "Largest threads",
            "Orphan links",
            "20261020T000000Z-gone-0000",
            "Reply rate per worker",
            "In flight now",
        ] {
            assert!(page.contains(part), "{part}");
        }
        // The exit reason is folded: no PID.
        assert!(page.contains("stopped by --takeover"));
        assert!(!page.contains("PID 4600"));
        // Self-contained: nothing is fetched, and one script only.
        for external in ["http://", "https://", "<link", "@import", "src=\""] {
            assert!(!page.contains(external), "{external}");
        }
        assert_eq!(page.matches("<script").count(), 1);
        // The queue is unchanged.
        assert_eq!(json_files(&root.inbox("w1")).len(), 1);
        assert_eq!(json_files(&root.processing("w1")).len(), 1);
    }

    #[test]
    fn report_escapes_queue_text() {
        let (_dir, root) = fixture();
        let mut data = scan(&root, NOW);
        data.root = "/queue/<root>".into();
        let page = render(&data);
        assert!(page.contains("&lt;i&gt;evil&amp;co&lt;/i&gt;"));
        assert!(!page.contains("<i>evil"));
        assert!(page.contains("/queue/&lt;root&gt;"));
        assert!(!page.contains("<root>"));
    }

    #[test]
    fn report_scan_reads_states_links_and_sizes() {
        let (_dir, root) = fixture();
        let d = scan(&root, NOW);
        let states: BTreeSet<&str> = d.recs.iter().map(|r| r.state).collect();
        assert_eq!(states, BTreeSet::from(STATES));
        assert!(d.recs.iter().all(|r| r.body_bytes == BODY.len() as u64));
        assert!(
            d.recs
                .iter()
                .all(|r| r.subject_bytes == SUBJECT.len() as u64)
        );
        assert_eq!(d.reasons.len(), 1);
        assert_eq!(d.exits.len(), 2);
        assert_eq!(d.modes.get("w1"), Some(&"inbox"));
        let l = links(&d.recs);
        assert_eq!(l.replies.len(), 1);
        assert_eq!(l.orphans.len(), 1);
        assert_eq!(l.threads[0].len(), 2);
        // The message files are not counted again as other bytes.
        assert!(!d.other.contains_key("inbox"));
        assert!(d.other.contains_key("rejected"));
    }

    /// A message with no text fields, for figure tests.
    fn rec(id: &str, kind: &'static str, state: &'static str, created: i64, mtime: i64) -> Rec {
        Rec {
            id: id.into(),
            from: "lead".into(),
            to: "w1".into(),
            kind,
            parent: None,
            reply_to: None,
            status: "ok".into(),
            hops: 0,
            created,
            mtime,
            size: 100,
            state,
            folder: "w1".into(),
            body_bytes: 0,
            subject_bytes: 0,
            session_id: None,
        }
    }

    fn data(recs: Vec<Rec>, exits: Vec<ExitLine>) -> Data {
        Data {
            root: "/queue".into(),
            now: NOW,
            recs,
            reasons: HashMap::new(),
            other: BTreeMap::new(),
            exits,
            beats: BTreeMap::new(),
            modes: BTreeMap::new(),
        }
    }

    #[test]
    fn medians_average_the_middle_pair() {
        assert_eq!(median_f(&[10.0, 1.0, 4.0, 2.0]), Some(3.0));
        assert_eq!(median_f(&[3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median_f(&[]), None);
    }

    #[test]
    fn a_zero_wait_reports_zero_seconds() {
        let task = rec(
            "20261027T100000Z-lead-0001",
            "task",
            "done",
            NOW - 60,
            NOW - 60,
        );
        let mut reply = rec(
            "20261027T100100Z-w1-0002",
            "result",
            "done",
            NOW - 60,
            NOW - 60,
        );
        reply.reply_to = Some(task.id.clone());
        assert_eq!(claim_wait(&task), Some(0.0));
        let page = render(&data(vec![task, reply], Vec::new()));
        assert!(page.contains("waited 0s"));
        assert!(page.contains("turnaround 0s"));
        assert!(page.contains("<div class=\"value\">0s</div>"));
        assert!(!page.contains("waited 1s"));
    }

    #[test]
    fn watcher_exits_cover_the_newest_days_only() {
        let exit = |t: i64| ExitLine {
            t,
            name: "w1".into(),
            mode: "inbox".into(),
            reason: "--max-seconds reached".into(),
        };
        // 1970, three recent days, and one far in the future.
        let exits = vec![
            exit(0),
            exit(NOW - 2 * 86_400),
            exit(NOW - 3600),
            exit(NOW),
            exit(NOW + 400 * 86_400),
        ];
        let d = data(
            vec![rec("20261027T100000Z-lead-0001", "task", "done", NOW, NOW)],
            exits,
        );
        let out = super::exits(&d);
        assert!(out.contains("2 exits outside these days are left out."));
        // One column per day from the oldest kept exit: 3 days, not decades.
        assert_eq!(out.matches("<rect class=\"hit\"").count(), 3);
        assert!(!out.contains("1 Jan"));
        // Only exits after now: a note, no chart.
        let d = data(Vec::new(), vec![exit(NOW + 400 * 86_400)]);
        let out = super::exits(&d);
        assert!(out.contains("1 exits outside"));
        assert!(!out.contains("<svg"));
    }

    #[test]
    fn an_empty_queue_gives_a_page() {
        let (_dir, root) = temp_queue();
        let page = page(&root, NOW);
        assert!(page.contains("No messages found."));
    }

    #[test]
    fn exit_reasons_are_folded() {
        assert_eq!(
            exit_reason("--max-seconds 60 reached"),
            "--max-seconds 60 reached"
        );
        assert_eq!(
            exit_reason("stopped by a signal (SIGTERM)"),
            "stopped by a signal"
        );
        assert_eq!(
            exit_reason("console event CTRL_CLOSE_EVENT"),
            "CTRL_CLOSE_EVENT"
        );
        assert_eq!(
            exit_reason("stopped by --takeover from PID 4600"),
            "stopped by --takeover"
        );
        assert_eq!(
            exit_reason(
                "previous watcher ended without logging (last heartbeat 2026-11-04T09:51:50Z); found by PID 4700"
            ),
            "previous watcher ended without logging"
        );
        assert_eq!(
            exit_reason("gave up after 3 tries."),
            "gave up after retries"
        );
    }

    #[test]
    fn reject_reasons_keep_their_first_words() {
        assert_eq!(short_reason("Not mine"), "not mine");
        assert_eq!(short_reason("Out of scope: details"), "out of scope");
        assert_eq!(
            short_reason("this is far too long a reason"),
            "this is far ..."
        );
    }

    #[test]
    fn squarify_fills_the_area() {
        let rects = squarify(&[6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0], 0.0, 0.0, 6.0, 4.0);
        let area: f64 = rects.iter().map(|r| r.2 * r.3).sum();
        assert!((area - 24.0).abs() < 1e-9);
        assert!(
            rects
                .iter()
                .all(|r| r.0 >= -1e-9 && r.0 + r.2 <= 6.0 + 1e-9)
        );
    }

    #[test]
    fn figures_format_for_people() {
        assert_eq!(grouped(2128), "2,128");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_000_000), "1,000,000");
        assert_eq!(size_str(512), "512 B");
        assert_eq!(size_str(1536), "1.5 KB");
        assert_eq!(dur(90_000.0), "1d01h");
        assert_eq!(dur(65.0), "1m05s");
        assert_eq!(day_label(uk_day(NOW)), "Tue 27 Oct");
    }
}
