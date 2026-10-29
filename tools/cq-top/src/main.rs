//! cq-top: a live, read-only view of a `cq` queue.
//!
//! Reads the queue folder only. It never claims, sends or moves a message, and it keeps
//! message bodies hidden until `b` is pressed.

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use cq::members::Mode;
use ratatui::{
    DefaultTerminal,
    crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
};
use serde_json::Value;

mod draw;
mod html;
mod screens;
mod sessions;
use draw::{THEMES, draw};

/// Watchers write a heartbeat every 30 s; three missed beats means the watcher is gone.
const STALE_AFTER: i64 = 90;
const RECENT_LIMIT: usize = 40;
/// Rejected and dead-lettered messages read for their own table, newest first.
const PROBLEM_LIMIT: usize = 40;
/// The Flow screen covers the last hour.
const WINDOW_SECS: i64 = 3600;
/// Exit codes for bad arguments and for any other error, as in `docs/COMMANDS.md`.
const EXIT_USAGE: i32 = 64;
const EXIT_ERROR: i32 = 70;
/// The size of an exported screen, in terminal cells.
const SCREEN_SIZE: (u16, u16) = (140, 42);

#[derive(Clone)]
struct Msg {
    ts: i64,
    from: String,
    to: String,
    kind: String,
    subject: String,
    status: String,
    reply_to: String,
    body: String,
    bucket: &'static str,
    /// Why it was rejected or dead-lettered, from `<id>.reason.json`; empty otherwise.
    reason: String,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Screen {
    Overview,
    Sessions,
    Flow,
    Rejected,
}

impl Screen {
    /// The screens in tab order; key `1` is the first.
    const ALL: [Screen; 4] = [
        Screen::Overview,
        Screen::Sessions,
        Screen::Flow,
        Screen::Rejected,
    ];

    /// The name in the saved view and in exported file names.
    fn name(self) -> &'static str {
        match self {
            Screen::Overview => "overview",
            Screen::Flow => "flow",
            Screen::Rejected => "rejected",
            Screen::Sessions => "sessions",
        }
    }

    /// The name on the tab bar.
    fn title(self) -> &'static str {
        match self {
            Screen::Overview => "Overview",
            Screen::Sessions => "Sessions",
            Screen::Flow => "Flow",
            Screen::Rejected => "Rejected",
        }
    }

    fn next(self) -> Screen {
        self.step(1)
    }

    fn prev(self) -> Screen {
        self.step(Screen::ALL.len() - 1)
    }

    fn step(self, by: usize) -> Screen {
        let i = Screen::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Screen::ALL[(i + by) % Screen::ALL.len()]
    }

    /// The screen for a number key: `1` is the first in tab order.
    fn for_key(c: char) -> Option<Screen> {
        let i = c.to_digit(10)?.checked_sub(1)?;
        Screen::ALL.get(i as usize).copied()
    }
}

struct Watcher {
    name: String,
    beat_age: Option<i64>,
    /// `sleep/<name>` exists: the watcher stopped on purpose and is not stale.
    asleep: bool,
    /// The folder key of the watcher's working folder, from line 3 of its `members/` record,
    /// so the Sessions screen can find a sleeping name, whose watcher has no process.
    folder: Option<String>,
    /// Line 4 of its `members/` record: `inbox` for `cq watch --to-inbox`, else `monitor`.
    /// `None` without a record. `Snapshot::mode` prefers the process's own arguments.
    mode: Option<Mode>,
    pid: String,
    session: String,
    queued: usize,
    claimed: usize,
    done: usize,
    rejected: usize,
    dead: usize,
}

impl Watcher {
    fn stale(&self) -> bool {
        self.beat_age.is_none_or(|a| a > STALE_AFTER)
    }

    fn busy(&self) -> Busy {
        if self.asleep {
            return Busy::Asleep;
        }
        match (self.stale(), self.claimed + self.queued) {
            (true, _) => Busy::Stale,
            (false, 0) => Busy::Idle,
            _ if self.queued > 0 => Busy::Backlog,
            _ => Busy::Working,
        }
    }
}

/// What a watcher is doing right now, for the colour of its load bar.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Busy {
    Asleep,
    Stale,
    Idle,
    Backlog,
    Working,
}

/// A watcher's mode as the tables show it. A Monitor watcher restarts every 30 minutes; an
/// inbox or Codex watcher runs until it stops. Nothing in cq-top depends on any of them.
fn mode_label(m: Mode) -> &'static str {
    m.as_str()
}

struct Snapshot {
    now: i64,
    paused: bool,
    watchers: Vec<Watcher>,
    in_flight: Vec<Msg>,
    recent: Vec<Msg>,
    /// Every message sent in the last hour (finished and in flight), for the Flow screen.
    window: Vec<Msg>,
    /// The newest rejected and dead-lettered messages, with their reasons, newest first.
    problems: Vec<Msg>,
    /// How many rejected and dead-lettered messages there are in all.
    problems_total: usize,
    /// Claude Code sessions, collected apart from the queue (see `sessions.rs`).
    sessions: sessions::Report,
}

impl Snapshot {
    /// The watcher's mode: `--to-inbox` or `--to-codex` in its process's arguments while cq-top can see the
    /// process, else its `members/` record. `None` with neither.
    fn mode(&self, w: &Watcher) -> Option<Mode> {
        self.sessions.modes.get(&w.name).copied().or(w.mode)
    }
}

fn main() -> std::io::Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "cq-top: live, read-only view of a cq queue.\n\
             The queue root is CQ_ROOT, or cq's per-user default.\n\
             Keys: q quit, b bodies, t next theme, tab, arrows or 1/2/3/4 switch screen\n\
             (1 Overview, 2 Sessions, 3 Flow, 4 Rejected).\n\
             \n\
             --html PATH          write one snapshot as an HTML dashboard to PATH, then exit\n\
             --html-screens DIR   write the four screens as HTML pages (overview.html,\n\
             \x20                    sessions.html, flow.html, rejected.html) to DIR, then exit\n\
             Neither may point inside the queue root. Message bodies are never exported."
        );
        return Ok(());
    }
    let exports = match parse_exports(&args) {
        Ok(e) => e,
        Err(msg) => {
            eprintln!("cq-top: {msg}");
            std::process::exit(EXIT_USAGE);
        }
    };
    let root = match cq::Root::open() {
        Ok(root) => root,
        Err(e) => {
            eprintln!("cq-top: {e}");
            std::process::exit(EXIT_ERROR);
        }
    };
    if exports.html.is_some() || exports.screens.is_some() {
        let scan = || sessions::collect_once(sessions::Sources::live(&root));
        if let Err(e) = export(&root, &exports, scan) {
            eprintln!("cq-top: {e}");
            std::process::exit(EXIT_ERROR);
        }
        return Ok(());
    }
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &root);
    ratatui::restore();
    result
}

fn run(terminal: &mut DefaultTerminal, root: &cq::Root) -> std::io::Result<()> {
    let mut show_bodies = false;
    let (mut screen, mut theme) = load_view();
    let mut beats = Beats::new();
    // Sessions are scanned on their own threads; each frame takes the latest scan.
    let collector = sessions::Collector::start(sessions::Sources::live(root));
    loop {
        let mut snap = snapshot(root, &mut beats);
        snap.sessions = collector.latest();
        terminal.draw(|f| draw(f, root.path(), &snap, show_bodies, screen, &THEMES[theme]))?;
        if event::poll(Duration::from_millis(500))?
            && let Event::Key(k) = event::read()?
        {
            if k.kind != KeyEventKind::Press {
                continue;
            }
            match k.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
                KeyCode::Char('b') => show_bodies = !show_bodies,
                KeyCode::Char('t') => theme = (theme + 1) % THEMES.len(),
                KeyCode::Tab | KeyCode::Right => screen = screen.next(),
                KeyCode::BackTab | KeyCode::Left => screen = screen.prev(),
                KeyCode::Char(c) => match Screen::for_key(c) {
                    Some(s) => screen = s,
                    None => continue,
                },
                _ => continue,
            }
            save_view(screen, theme);
        }
    }
}

// ---------- the remembered view ----------

/// The last screen and theme are kept in `<config dir>/cq-top/state` as `<screen> <theme>`,
/// for example `flow neon`.
/// The queue root is never written. With no saved choice: Overview screen, first theme (classic).
fn view_path() -> Option<PathBuf> {
    let var = |k: &str| env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let xdg = || var("XDG_CONFIG_HOME").or_else(|| var("HOME").map(|h| h.join(".config")));
    // Git Bash sets HOME on Windows, so prefer %APPDATA% there: the file is the same
    // whichever shell started cq-top.
    let dir = if cfg!(windows) {
        var("APPDATA").or_else(xdg)?
    } else {
        xdg().or_else(|| var("APPDATA"))?
    };
    Some(dir.join("cq-top").join("state"))
}

fn parse_view(text: &str) -> (Screen, usize) {
    let mut words = text.split_whitespace();
    let screen = words
        .next()
        .and_then(|n| Screen::ALL.into_iter().find(|s| s.name() == n))
        .unwrap_or(Screen::Overview);
    let theme = words
        .next()
        .and_then(|n| THEMES.iter().position(|t| t.name == n))
        .unwrap_or(0);
    (screen, theme)
}

fn format_view(screen: Screen, theme: usize) -> String {
    format!("{} {}\n", screen.name(), THEMES[theme].name)
}

fn load_view() -> (Screen, usize) {
    let text = view_path()
        .and_then(|p| fs::read_to_string(p).ok())
        .unwrap_or_default();
    parse_view(&text)
}

fn save_view(screen: Screen, theme: usize) {
    let Some(path) = view_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    // LAZY: a failed save is ignored; the view just is not remembered next run.
    let _ = fs::write(path, format_view(screen, theme));
}

// ---------- one-shot exports ----------

#[derive(Default, Debug, PartialEq)]
struct Exports {
    html: Option<PathBuf>,
    screens: Option<PathBuf>,
}

fn parse_exports(args: &[String]) -> Result<Exports, String> {
    let mut e = Exports::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let slot = match a.as_str() {
            "--html" => &mut e.html,
            "--html-screens" => &mut e.screens,
            _ => return Err(format!("unknown argument {a:?}; see --help")),
        };
        let Some(v) = it.next().filter(|v| !v.is_empty()) else {
            return Err(format!("{a} needs a path"));
        };
        *slot = Some(PathBuf::from(v));
    }
    Ok(e)
}

/// The full path for an export, refused when it is the queue root or inside it: an export
/// must never write to the queue. The deepest folder that exists is resolved, so a link or
/// a different spelling of the root is caught too.
fn export_path(root: &Path, path: &Path) -> std::io::Result<PathBuf> {
    let refuse = |why: String| std::io::Error::new(std::io::ErrorKind::InvalidInput, why);
    let abs = std::path::absolute(path)?;
    let mut base = abs.as_path();
    let mut rest = Vec::new();
    while !base.exists() {
        match (base.file_name(), base.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name.to_owned());
                base = parent;
            }
            _ => return Err(refuse(format!("cannot resolve {}", path.display()))),
        }
    }
    let mut full = base.canonicalize()?;
    for name in rest.iter().rev() {
        full.push(name);
    }
    if full.starts_with(root.canonicalize()?) {
        return Err(refuse(format!(
            "refusing to write {} inside the queue root {}",
            path.display(),
            root.display()
        )));
    }
    Ok(full)
}

/// Takes one snapshot and writes the exports asked for. Every path is checked before
/// anything is written, and before `scan` collects the sessions.
fn export(
    root: &cq::Root,
    e: &Exports,
    scan: impl FnOnce() -> sessions::Report,
) -> std::io::Result<()> {
    let check = |p: &Option<PathBuf>| {
        p.as_deref()
            .map(|p| export_path(root.path(), p))
            .transpose()
    };
    let (html, screens) = (check(&e.html)?, check(&e.screens)?);
    let mut snap = snapshot(root, &mut Beats::new());
    snap.sessions = scan();
    // Writes go to the checked full path; messages show the path as given.
    if let (Some(path), Some(given)) = (html, &e.html) {
        fs::write(&path, html::dashboard(root.path(), &snap))?;
        println!("wrote {}", given.display());
    }
    if let (Some(dir), Some(given)) = (screens, &e.screens) {
        fs::create_dir_all(&dir)?;
        let (_, theme) = load_view();
        let (w, h) = SCREEN_SIZE;
        for screen in Screen::ALL {
            let name = screen.name();
            let path = dir.join(format!("{name}.html"));
            fs::write(
                &path,
                screens::page(root.path(), &snap, screen, &THEMES[theme], w, h),
            )?;
            println!("wrote {}", given.join(format!("{name}.html")).display());
        }
    }
    Ok(())
}

// ---------- figures shared by the screens and the dashboard ----------

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

/// Messages sent in each of `n` equal slices of the last hour, oldest first.
fn slices(s: &Snapshot, n: usize) -> Vec<u64> {
    let n = n.max(1);
    let mut slices = vec![0u64; n];
    for m in &s.window {
        let ago = (s.now - m.ts).clamp(0, WINDOW_SECS - 1);
        slices[n - 1 - (ago as usize * n / WINDOW_SECS as usize)] += 1;
    }
    slices
}

/// Messages from one sender to one receiver in the last hour.
#[derive(Debug, PartialEq)]
struct Pair {
    from: String,
    to: String,
    count: u64,
    /// When the newest of them was sent.
    last: i64,
}

/// Messages per (sender, receiver) in the last hour, most first, then newest first.
fn pairs(s: &Snapshot) -> Vec<Pair> {
    let mut pairs: BTreeMap<(&str, &str), (u64, i64)> = BTreeMap::new();
    for m in &s.window {
        let p = pairs.entry((&m.from, &m.to)).or_insert((0, m.ts));
        p.0 += 1;
        p.1 = p.1.max(m.ts);
    }
    let mut pairs: Vec<Pair> = pairs
        .into_iter()
        .map(|((from, to), (count, last))| Pair {
            from: from.to_string(),
            to: to.to_string(),
            count,
            last,
        })
        .collect();
    // A stable sort: pairs with the same count and time keep their name order.
    pairs.sort_by_key(|p| (std::cmp::Reverse(p.count), std::cmp::Reverse(p.last)));
    pairs
}

/// A watcher's load bar is full at this many queued + claimed messages.
const LOAD_FULL: usize = 4;

/// One row of the worker table: a watcher, or a name that replied in the last hour without
/// being one.
struct Worker<'a> {
    name: String,
    watcher: Option<&'a Watcher>,
    /// Replies sent in the last hour.
    replies: usize,
    /// Median seconds from a task being sent to its reply being sent; `None` without replies.
    median: Option<i64>,
}

/// Every watcher in name order, then any other name that replied in the last hour.
fn workers(s: &Snapshot) -> Vec<Worker<'_>> {
    let mut turn: BTreeMap<String, (usize, i64)> = turnaround(s)
        .into_iter()
        .map(|(name, n, med)| (name, (n, med)))
        .collect();
    let mut rows: Vec<Worker> = s
        .watchers
        .iter()
        .map(|w| {
            let t = turn.remove(&w.name);
            Worker {
                name: w.name.clone(),
                watcher: Some(w),
                replies: t.map_or(0, |t| t.0),
                median: t.map(|t| t.1),
            }
        })
        .collect();
    rows.extend(turn.into_iter().map(|(name, (replies, med))| Worker {
        name,
        watcher: None,
        replies,
        median: Some(med),
    }));
    rows
}

/// Task -> reply turnaround per worker in the last hour: reply send time minus task send
/// time. Each entry is (worker, replies, median seconds).
fn turnaround(s: &Snapshot) -> Vec<(String, usize, i64)> {
    let mut turn: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    for m in s.window.iter().filter(|m| m.kind == "result") {
        if let Some(task_ts) = parse_ts(&m.reply_to) {
            turn.entry(m.from.clone())
                .or_default()
                .push((m.ts - task_ts).max(0));
        }
    }
    turn.into_iter()
        .map(|(w, mut v)| (w, v.len(), median(&mut v)))
        .collect()
}

// ---------- reading the queue ----------

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Parses `2026-10-27T11:26:32Z` and the compact id prefix `20261027T112505Z` (both UTC).
fn parse_ts(s: &str) -> Option<i64> {
    let head = s.split('Z').next()?;
    let d: Vec<i64> = head
        .chars()
        .filter_map(|c| c.to_digit(10))
        .map(i64::from)
        .collect();
    if d.len() != 14 {
        return None;
    }
    let n = |r: std::ops::Range<usize>| d[r].iter().fold(0, |a, x| a * 10 + x);
    let (y, m, day) = (n(0..4), n(4..6), n(6..8));
    let (y, m) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + n(8..10) * 3600 + n(10..12) * 60 + n(12..14))
}

fn subdirs(p: &Path) -> Vec<String> {
    fs::read_dir(p)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .collect()
}

/// The message files in a folder. `<id>.reason.json` files sit beside the messages in
/// `dlq/` and `rejected/`; they are not messages.
fn json_files(p: &Path) -> Vec<(PathBuf, i64)> {
    fs::read_dir(p)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.ends_with(".json") && !name.ends_with(".reason.json")
        })
        .map(|e| {
            let mtime = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs() as i64);
            (e.path(), mtime)
        })
        .collect()
}

fn read_msg(path: &Path, mtime: i64, bucket: &'static str) -> Option<Msg> {
    let v: Value = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let ts = parse_ts(&s("id"))
        .or_else(|| parse_ts(&s("created")))
        .unwrap_or(mtime);
    Some(Msg {
        ts,
        from: s("from"),
        to: s("to"),
        kind: s("type"),
        subject: s("subject"),
        status: s("status"),
        reply_to: s("reply_to"),
        body: s("body"),
        bucket,
        reason: String::new(),
    })
}

/// A rejected or dead-lettered message with the reason from its sidecar. A message that
/// cannot be read (often why it was dead-lettered) still gets a row: its time and sender
/// come from the file name `<time>-<sender>-<suffix>.json`, its receiver from the folder.
fn read_problem(path: &Path, mtime: i64, bucket: &'static str) -> Msg {
    let mut m = read_msg(path, mtime, bucket).unwrap_or_else(|| {
        let id = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let from = match (id.find('-'), id.rfind('-')) {
            (Some(a), Some(b)) if a < b => id[a + 1..b].to_string(),
            _ => String::new(),
        };
        let to = path
            .parent()
            .and_then(Path::file_name)
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        Msg {
            ts: parse_ts(&id).unwrap_or(mtime),
            from,
            to,
            kind: String::new(),
            subject: "(unreadable message)".into(),
            status: String::new(),
            reply_to: String::new(),
            body: String::new(),
            bucket,
            reason: String::new(),
        }
    });
    // `<id>.json` -> `<id>.reason.json`. A missing or unreadable sidecar leaves no reason.
    m.reason = fs::read_to_string(path.with_extension("reason.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("reason").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    m
}

/// The last good heartbeat time for each name. `cq watch` rewrites the file in place, so a
/// read can catch it empty or half written; that read keeps the previous value.
type Beats = BTreeMap<String, i64>;

fn snapshot(root: &cq::Root, beats: &mut Beats) -> Snapshot {
    snapshot_at(root, beats, now_epoch())
}

/// A message moving between folders during a refresh can be counted twice or missed once;
/// the next refresh is right again (see `docs/CQ-TOP.md`).
fn snapshot_at(root: &cq::Root, beats: &mut Beats, now: i64) -> Snapshot {
    // name -> Claude Code session key, from members/<key> (line 1 is the name),
    // name -> the watcher's working folder (line 3, written by newer cq), and
    // name -> the watcher's mode (line 4; an older record without it is a Monitor watcher).
    let mut sessions: BTreeMap<String, String> = BTreeMap::new();
    let mut folders: BTreeMap<String, String> = BTreeMap::new();
    let mut modes: BTreeMap<String, Mode> = BTreeMap::new();
    for e in fs::read_dir(root.members()).into_iter().flatten().flatten() {
        if let Ok(t) = fs::read_to_string(e.path())
            && let Some(name) = t.lines().next()
        {
            let name = name.trim().to_string();
            if let Some(dir) = t.lines().nth(2).map(str::trim).filter(|d| !d.is_empty()) {
                folders.insert(name.clone(), sessions::folder_key(Path::new(dir)));
            }
            let mode = Mode::parse(t.lines().nth(3));
            modes.insert(name.clone(), mode);
            sessions.insert(name, e.file_name().to_string_lossy().into_owned());
        }
    }
    let asleep = cq::sleep::asleep(root);

    let mut names: BTreeSet<String> = sessions.keys().cloned().collect();
    names.extend(asleep.iter().cloned());
    for d in ["inbox", "done", "rejected", "dlq"] {
        names.extend(subdirs(&root.path().join(d)));
    }
    for e in fs::read_dir(root.path().join("heartbeat"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let f = e.file_name().to_string_lossy().into_owned();
        if !f.contains('.') {
            names.insert(f);
        }
    }

    let mut watchers = Vec::new();
    let mut in_flight = Vec::new();
    for name in &names {
        let queued = json_files(&root.inbox(name));
        let claimed = json_files(&root.processing(name));
        let dead = json_files(&root.dlq(name));

        let beat = fs::read_to_string(root.heartbeat(name))
            .ok()
            .and_then(|t| parse_ts(t.trim()));
        if let Some(t) = beat {
            beats.insert(name.clone(), t);
        }
        let beat_age = beats.get(name).map(|t| now - t);
        // Liveness is heartbeat age only. cq-top never opens the lock file (opening it
        // creates it, and a probe could stop a starting watcher); the .pid file is a hint.
        let pid = fs::read_to_string(root.pid_file(name))
            .map(|t| t.trim().to_string())
            .unwrap_or_default();
        let session = sessions
            .get(name)
            .map(|k| k.trim_start_matches("local_").chars().take(8).collect())
            .unwrap_or_default();

        watchers.push(Watcher {
            name: name.clone(),
            beat_age,
            asleep: asleep.contains(name),
            folder: folders.get(name).cloned(),
            mode: modes.get(name).copied(),
            pid,
            session,
            queued: queued.len(),
            claimed: claimed.len(),
            done: json_files(&root.done(name)).len(),
            rejected: json_files(&root.rejected(name)).len(),
            dead: dead.len(),
        });
        for (p, m) in &queued {
            in_flight.extend(read_msg(p, *m, "queued"));
        }
        for (p, m) in &claimed {
            in_flight.extend(read_msg(p, *m, "claimed"));
        }
        for (p, m) in &dead {
            in_flight.extend(read_msg(p, *m, "dead"));
        }
    }
    in_flight.sort_by_key(|m| m.ts);

    // Read only the newest files by mtime; done/ and rejected/ keep 14 days.
    let mut files = Vec::new();
    for (dir, bucket) in [("done", "done"), ("rejected", "rejected"), ("dlq", "dead")] {
        for name in subdirs(&root.path().join(dir)) {
            for (p, m) in json_files(&root.path().join(dir).join(&name)) {
                files.push((p, m, bucket));
            }
        }
    }
    files.sort_by_key(|f| std::cmp::Reverse(f.1));
    let mut window: Vec<Msg> = files
        .iter()
        .filter(|f| f.1 >= now - WINDOW_SECS)
        .filter_map(|(p, m, b)| read_msg(p, *m, b))
        .collect();
    // Dead messages are already in `files`; queued and claimed ones count only when sent
    // within the window, so an old stuck message is not "last hour" traffic.
    window.extend(
        in_flight
            .iter()
            .filter(|m| m.bucket != "dead" && m.ts >= now - WINDOW_SECS)
            .cloned(),
    );
    let problem_files: Vec<_> = files.iter().filter(|f| f.2 != "done").collect();
    let problems_total = problem_files.len();
    let mut problems: Vec<Msg> = problem_files
        .into_iter()
        .take(PROBLEM_LIMIT)
        .map(|(p, m, b)| read_problem(p, *m, b))
        .collect();
    problems.sort_by_key(|m| std::cmp::Reverse(m.ts));
    let mut recent: Vec<Msg> = files
        .into_iter()
        .take(RECENT_LIMIT)
        .filter_map(|(p, m, b)| read_msg(&p, m, b))
        .collect();
    recent.sort_by_key(|m| std::cmp::Reverse(m.ts));

    Snapshot {
        now,
        paused: root.pause().exists(),
        watchers,
        in_flight,
        recent,
        window,
        problems,
        problems_total,
        sessions: sessions::Report::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn parse_ts_reads_both_forms() {
        assert_eq!(parse_ts("2026-10-27T11:26:32Z"), Some(1_793_100_392));
        assert_eq!(parse_ts("20261027T112632Z-lead-ab12"), Some(1_793_100_392));
        assert_eq!(parse_ts("2000-01-01T00:00:00Z"), Some(946_684_800));
        // Leap day, last second.
        assert_eq!(parse_ts("2024-02-29T23:59:59Z"), Some(1_709_251_199));
    }

    #[test]
    fn parse_ts_rejects_other_text() {
        assert_eq!(parse_ts(""), None);
        assert_eq!(parse_ts("not a time"), None);
        assert_eq!(parse_ts("2026-10-27"), None);
    }

    #[test]
    fn saved_view_round_trips_and_falls_back() {
        for screen in Screen::ALL {
            for theme in 0..THEMES.len() {
                assert_eq!(parse_view(&format_view(screen, theme)), (screen, theme));
            }
        }
        assert_eq!(parse_view(""), (Screen::Overview, 0));
        assert_eq!(parse_view("flow no-such-theme"), (Screen::Flow, 0));
        assert_eq!(parse_view("garbage"), (Screen::Overview, 0));
        // Tab goes round all four screens.
        assert_eq!(Screen::Sessions.next(), Screen::Flow);
        assert_eq!(Screen::Rejected.next(), Screen::Overview);
        assert_eq!(Screen::Overview.next().next(), Screen::Flow);
        assert_eq!(parse_view("sessions amber"), (Screen::Sessions, 1));
    }

    /// A fixed "now" so ages do not depend on the clock: 2026-10-27T11:30:00Z.
    const NOW: i64 = 1_793_100_600;

    /// A temporary queue root. `Root` can only be made from `CQ_ROOT`, so tests take a lock
    /// while they set it; the root keeps its own copy of the path afterwards.
    fn temp_queue() -> (tempfile::TempDir, cq::Root) {
        static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: every test that sets CQ_ROOT holds `ENV`, and nothing else reads it.
        unsafe { env::set_var("CQ_ROOT", dir.path()) };
        let root = cq::Root::from_env().unwrap();
        root.init().unwrap();
        root.ensure_session_dirs("w1").unwrap();
        (dir, root)
    }

    fn put(dir: &Path, id: &str, kind: &str) {
        let body = format!(
            r#"{{"id":"{id}","from":"lead","to":"w1","type":"{kind}","subject":"s","body":"secret"}}"#
        );
        fs::write(dir.join(format!("{id}.json")), body).unwrap();
    }

    fn snap(root: &cq::Root, beats: &mut Beats) -> Snapshot {
        snapshot_at(root, beats, NOW)
    }

    #[test]
    fn snapshot_reads_a_queue_without_changing_it() {
        let (_dir, root) = temp_queue();
        put(&root.inbox("w1"), "20261027T112500Z-lead-0001", "task");
        put(&root.processing("w1"), "20261027T112501Z-lead-0002", "info");
        put(&root.done("w1"), "20261027T112502Z-lead-0003", "task");
        fs::write(root.heartbeat("w1"), "2026-10-27T11:26:32Z\n").unwrap();

        let s = snap(&root, &mut Beats::new());
        assert!(!s.paused);
        let w = s.watchers.iter().find(|w| w.name == "w1").unwrap();
        assert_eq!((w.queued, w.claimed, w.done, w.dead), (1, 1, 1, 0));
        assert_eq!(w.beat_age, Some(NOW - 1_793_100_392));
        let buckets: Vec<_> = s.in_flight.iter().map(|m| m.bucket).collect();
        assert_eq!(buckets, ["queued", "claimed"]);
        assert_eq!(s.recent.len(), 1);

        fs::write(root.pause(), "").unwrap();
        assert!(snap(&root, &mut Beats::new()).paused);
        // The snapshot left every message where it was.
        assert_eq!(json_files(&root.inbox("w1")).len(), 1);
        assert_eq!(json_files(&root.processing("w1")).len(), 1);
    }

    #[test]
    fn reason_files_are_not_messages() {
        let (_dir, root) = temp_queue();
        put(&root.dlq("w1"), "20261027T112503Z-lead-0004", "task");
        fs::write(
            root.dlq("w1")
                .join("20261027T112503Z-lead-0004.reason.json"),
            r#"{"reason":"bad"}"#,
        )
        .unwrap();
        put(&root.rejected("w1"), "20261027T112504Z-lead-0005", "task");
        fs::write(
            root.rejected("w1")
                .join("20261027T112504Z-lead-0005.reason.json"),
            r#"{"reason":"no"}"#,
        )
        .unwrap();

        let s = snap(&root, &mut Beats::new());
        let w = s.watchers.iter().find(|w| w.name == "w1").unwrap();
        assert_eq!((w.dead, w.rejected), (1, 1));
        assert_eq!(s.recent.len(), 2);
        assert!(s.recent.iter().all(|m| !m.from.is_empty()));
    }

    #[test]
    fn window_skips_old_in_flight_and_counts_each_message_once() {
        let (_dir, root) = temp_queue();
        // Stuck for 23 hours: still in flight, but not "last hour" traffic.
        put(&root.inbox("w1"), "20261026T103000Z-lead-0001", "task");
        put(&root.inbox("w1"), "20261027T112500Z-lead-0002", "task");
        // Dead and recent: listed in flight, counted once in the window.
        put(&root.dlq("w1"), "20261027T112503Z-lead-0003", "task");

        let s = snap(&root, &mut Beats::new());
        assert_eq!(s.in_flight.len(), 3);
        assert_eq!(s.window.len(), 2);
        assert!(s.window.iter().all(|m| m.ts >= NOW - WINDOW_SECS));
    }

    #[test]
    fn a_bad_heartbeat_read_keeps_the_last_good_value() {
        let (_dir, root) = temp_queue();
        root.ensure_session_dirs("w2").unwrap();
        let mut beats = Beats::new();
        let age = |s: &Snapshot, n: &str| s.watchers.iter().find(|w| w.name == n).unwrap().beat_age;

        fs::write(root.heartbeat("w1"), "2026-10-27T11:29:00Z\n").unwrap();
        assert_eq!(age(&snap(&root, &mut beats), "w1"), Some(60));
        // Caught half written: empty, then garbage. The age keeps growing from the last good time.
        for bad in ["", "2026-10-27T1"] {
            fs::write(root.heartbeat("w1"), bad).unwrap();
            assert_eq!(
                age(&snapshot_at(&root, &mut beats, NOW + 30), "w1"),
                Some(90)
            );
        }
        // A watcher that never wrote a heartbeat has none.
        assert_eq!(age(&snap(&root, &mut beats), "w2"), None);
    }

    /// A message whose subject tries HTML injection and whose body must never be shown.
    const EVIL_SUBJECT: &str = "<script>alert('x')</script> & \"Scan lane\"";
    const BODY: &str = "BODY-TEXT-must-not-appear";
    const EVIL_REASON: &str = "<img src=x onerror=alert(1)> not my job";

    fn sample() -> Snapshot {
        let msg = |bucket: &'static str, kind: &str| Msg {
            ts: 1_793_100_000,
            from: "lead".into(),
            to: "w1".into(),
            kind: kind.into(),
            subject: EVIL_SUBJECT.into(),
            status: "ok".into(),
            reply_to: "20261027T111421Z-lead-6256".into(),
            body: BODY.into(),
            bucket,
            reason: String::new(),
        };
        Snapshot {
            now: 1_793_100_400,
            paused: false,
            watchers: vec![
                Watcher {
                    name: "w1".into(),
                    beat_age: Some(10),
                    asleep: false,
                    folder: None,
                    mode: Some(Mode::Monitor),
                    pid: "42".into(),
                    session: "abcd1234".into(),
                    queued: 1,
                    claimed: 1,
                    done: 2,
                    rejected: 0,
                    dead: 0,
                },
                // Asleep: its watcher stopped, so its heartbeat is old but it is not stale.
                Watcher {
                    name: "napper".into(),
                    beat_age: Some(4000),
                    asleep: true,
                    folder: Some("d--code-nap".into()),
                    mode: None,
                    pid: String::new(),
                    session: "nap12345".into(),
                    queued: 2,
                    claimed: 0,
                    done: 0,
                    rejected: 0,
                    dead: 0,
                },
            ],
            in_flight: vec![msg("queued", "task"), msg("claimed", "task")],
            recent: vec![msg("done", "result")],
            window: vec![msg("done", "result"), msg("queued", "task")],
            problems: vec![Msg {
                reason: EVIL_REASON.into(),
                ..msg("rejected", "task")
            }],
            problems_total: 3,
            sessions: sessions::Report {
                ready: true,
                machine: Some(sessions::Machine {
                    cpu: 12.5,
                    cores: 8,
                    busy_cores: 1,
                    mem_used: 8 << 30,
                    mem_total: 32 << 30,
                    mem_available: 24 << 30,
                    swap_used: 1 << 30,
                    swap_total: 4 << 30,
                    claude_cpu: 7.0,
                    claude_mem: 2 << 30,
                    claude_procs: 3,
                    codex_cpu: 4.0,
                    codex_mem: 1 << 30,
                    codex_procs: 2,
                }),
                rows: vec![
                    sessions::Row {
                        name: EVIL_TITLE.into(),
                        state: sessions::State::Working,
                        model: "opus-5-5".into(),
                        context: Some(135_000),
                        tokens_today: 66_300_000,
                        cost_today: Some(12.5),
                        cpu: Some(6.0),
                        mem: Some(900 << 20),
                        subagents: 2,
                        last_change: Some((sessions::Change::Edit, 1_793_100_300)),
                        active: 1_793_100_390,
                        ..Default::default()
                    },
                    // Idle with a waiting message: drawn as a warning.
                    sessions::Row {
                        name: "w1".into(),
                        cq_name: Some("w1".into()),
                        state: sessions::State::Idle,
                        model: "haiku-4-5".into(),
                        active: 1_793_090_000,
                        ..Default::default()
                    },
                ],
                projects: PathBuf::from("/home/me/.claude/projects"),
                history: Vec::new(),
                modes: Default::default(),
            },
        }
    }

    /// A session title that tries HTML injection.
    const EVIL_TITLE: &str = "<b>RC</b> & \"x\"";

    /// The screen drawn into a `w` x `h` off-screen terminal, one string per line.
    fn screen_lines(
        snap: &Snapshot,
        screen: Screen,
        theme: &draw::Theme,
        w: u16,
        h: u16,
    ) -> Vec<String> {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, Path::new("/q"), snap, false, screen, theme))
            .unwrap();
        let buf = term.backend().buffer();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    /// The sample with an hour of traffic: three tasks from lead to w1 (two answered, after
    /// 65 s and 130 s), a note to the sleeping napper, and a reply from a name with no watcher.
    /// `sample()`'s now is 2026-10-27T11:26:40Z.
    fn flow_sample() -> Snapshot {
        let msg = |ts: i64, from: &str, to: &str, kind: &str, reply_to: &str, bucket| Msg {
            ts,
            from: from.into(),
            to: to.into(),
            kind: kind.into(),
            subject: "s".into(),
            status: String::new(),
            reply_to: reply_to.into(),
            body: BODY.into(),
            bucket,
            reason: String::new(),
        };
        let mut snap = sample();
        snap.window = vec![
            msg(1_793_097_600, "lead", "w1", "task", "", "done"),
            msg(
                1_793_097_665,
                "w1",
                "lead",
                "result",
                "20261027T104000Z-lead-1",
                "done",
            ),
            msg(1_793_099_400, "lead", "w1", "task", "", "done"),
            msg(
                1_793_099_530,
                "w1",
                "lead",
                "result",
                "20261027T111000Z-lead-2",
                "done",
            ),
            msg(1_793_100_300, "lead", "w1", "task", "", "queued"),
            msg(1_793_098_800, "design-a", "napper", "info", "", "queued"),
            msg(
                1_793_100_000,
                "reviewer",
                "lead",
                "result",
                "20261027T111900Z-lead-3",
                "done",
            ),
        ];
        snap
    }

    /// The four screens in the order Overview, Sessions, Flow, Rejected: on the tab bar, for
    /// the number keys and for Tab, Shift-Tab and the arrows. Saved names still load.
    #[test]
    fn screens_are_in_order_overview_sessions_flow_rejected() {
        use Screen::*;
        assert_eq!(Screen::ALL, [Overview, Sessions, Flow, Rejected]);
        let keys: Vec<_> = "01234x".chars().map(Screen::for_key).collect();
        assert_eq!(
            keys,
            [
                None,
                Some(Overview),
                Some(Sessions),
                Some(Flow),
                Some(Rejected),
                None
            ]
        );
        assert_eq!(Overview.next(), Sessions);
        assert_eq!(Rejected.next(), Overview);
        assert_eq!(Overview.prev(), Rejected);
        assert_eq!(Flow.prev(), Sessions);
        for screen in Screen::ALL {
            assert_eq!(screen.next().prev(), screen);
        }
        // A state file from before the reorder names screens, not numbers, so it still loads.
        assert_eq!(parse_view("flow neon"), (Flow, 2));
        assert_eq!(parse_view("rejected classic"), (Rejected, 0));
        assert_eq!(parse_view("sessions amber"), (Sessions, 1));

        let top = &screen_lines(&sample(), Flow, &THEMES[0], 160, 30)[0];
        let at = |tab: &str| top.find(tab).unwrap_or_else(|| panic!("{tab} in {top}"));
        assert!(at("1 Overview") < at("2 Sessions"));
        assert!(at("2 Sessions") < at("3 Flow"));
        assert!(at("3 Flow") < at("4 Rejected"));
    }

    #[test]
    fn pairs_sort_by_count_then_newest() {
        let snap = flow_sample();
        let got: Vec<_> = pairs(&snap)
            .into_iter()
            .map(|p| (p.from, p.to, p.count, snap.now - p.last))
            .collect();
        let want = [
            ("lead", "w1", 3, 100),
            ("w1", "lead", 2, 870),
            ("reviewer", "lead", 1, 400),
            ("design-a", "napper", 1, 1600),
        ];
        assert_eq!(got.len(), want.len());
        for (g, w) in got.iter().zip(want) {
            assert_eq!((g.0.as_str(), g.1.as_str(), g.2, g.3), w);
        }
    }

    #[test]
    fn workers_join_watchers_and_reply_times() {
        let snap = flow_sample();
        let got: Vec<_> = workers(&snap)
            .iter()
            .map(|w| (w.name.clone(), w.watcher.is_some(), w.replies, w.median))
            .collect();
        assert_eq!(
            got,
            [
                ("w1".to_string(), true, 2, Some(130)),
                ("napper".to_string(), true, 0, None),
                // Replied, but has no watcher: listed after the watchers.
                ("reviewer".to_string(), false, 1, Some(60)),
            ]
        );
    }

    /// The Flow screen at 100 columns: the Messages box has figures and a graph, and both
    /// tables have a header row with names in their own columns.
    #[test]
    fn flow_screen_has_figures_a_graph_and_real_columns() {
        let snap = flow_sample();
        for theme in &THEMES {
            let lines = screen_lines(&snap, Screen::Flow, theme, 100, 30);
            let text = lines.join("\n");
            for want in [
                "Messages sent, last hour",
                "Total",
                "Last 5 min",
                "Peak",
                "1/min",
                "oldest left, now right",
                "Who talks to whom, last hour: 4 pairs",
                "Workers: load now, replies in the last hour",
                "load: queued + claimed, full at 4",
            ] {
                assert!(text.contains(want), "{want} in {}\n{text}", theme.name);
            }
            assert!(!text.contains(BODY));
            // The graph is drawn in Braille, as on the Sessions screen.
            assert!(text.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)));

            // A row's words, with the left border as the first: `│`, or `║` in amber.
            let words = |l: &String| {
                l.replace(['│', '║'], " | ")
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            };
            let line_with = |first: &str| {
                lines
                    .iter()
                    .map(words)
                    .find(|w| w.get(1).map(String::as_str) == Some(first))
                    .unwrap_or_else(|| panic!("a row starting {first}\n{text}"))
            };
            // Header rows, then rows whose cells are separate words between the borders.
            assert_eq!(line_with("from")[1..5], ["from", "to", "messages", "last"]);
            assert_eq!(line_with("lead")[1..5], ["lead", "w1", "3", "1m40s"]);
            assert_eq!(
                line_with("design-a")[1..5],
                ["design-a", "napper", "1", "26m40s"]
            );
            assert_eq!(
                line_with("worker")[1..10],
                [
                    "worker",
                    "state",
                    "mode",
                    "queued",
                    "claimed",
                    "load",
                    "median",
                    "task→reply",
                    "replies"
                ]
            );
            let w1 = line_with("w1");
            // `w1 -> lead` in the pairs table comes first; the worker row has its state.
            let worker = lines
                .iter()
                .map(words)
                .find(|w| {
                    w.get(1).map(String::as_str) == Some("w1") && w.contains(&"live".to_string())
                })
                .unwrap();
            assert_eq!(w1[1..3], ["w1", "lead"]);
            assert_eq!(worker[2..6], ["live", "mon", "1", "1"]);
            assert!(worker.contains(&"2m10s".to_string()));
            let napper = line_with("napper");
            assert_eq!(napper[2..3], ["asleep"]);
            let reviewer = lines
                .iter()
                .map(words)
                .find(|w| {
                    w.get(1).map(String::as_str) == Some("reviewer")
                        && w.contains(&"1m00s".to_string())
                })
                .unwrap();
            assert_eq!(reviewer[2], "-");
            // Never a joined name such as `lead>w1` or `lead -> w1`.
            assert!(!text.contains(">w1") && !text.contains("->"));
        }
    }

    /// A narrow terminal drops the least important columns first, never the names.
    #[test]
    fn flow_screen_drops_columns_when_narrow() {
        let snap = flow_sample();
        for theme in &THEMES {
            for (w, h) in [(60, 30), (40, 24), (20, 10), (1, 1)] {
                // Must not panic at any size.
                let text = screen_lines(&snap, Screen::Flow, theme, w, h).join("\n");
                if w == 60 {
                    let head = text.lines().find(|l| l.contains("worker")).unwrap();
                    assert!(head.contains("state") && head.contains("median"), "{head}");
                    assert!(
                        !head.contains("replies") && !head.contains("mode"),
                        "{head}"
                    );
                    assert!(text.contains("design-a") && text.contains("napper"));
                }
            }
        }
        // No traffic: the tables say so.
        let mut quiet = sample();
        quiet.window.clear();
        let text = screen_lines(&quiet, Screen::Flow, &THEMES[0], 100, 30).join("\n");
        assert!(text.contains("No messages in the last hour."));
        assert!(text.contains("0 pairs"));
    }

    #[test]
    fn every_theme_draws_both_screens() {
        let snap = sample();
        for theme in &THEMES {
            for screen in Screen::ALL {
                let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
                term.draw(|f| draw(f, Path::new("/queue"), &snap, false, screen, theme))
                    .unwrap();
                let text: String = term
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|c| c.symbol())
                    .collect();
                assert!(text.contains("cq-top"), "{} {screen:?}", theme.name);
                assert!(text.contains(match screen {
                    Screen::Overview => "Watchers",
                    Screen::Flow => "Who talks to whom",
                    Screen::Rejected => "not my job",
                    Screen::Sessions => "opus-5-5",
                }));
                assert!(!text.contains(BODY));
            }
        }
    }

    #[test]
    fn export_arguments_parse() {
        let args = |a: &[&str]| parse_exports(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(args(&[]), Ok(Exports::default()));
        let e = args(&["--html", "a.html", "--html-screens", "shots"]).unwrap();
        assert_eq!(e.html, Some(PathBuf::from("a.html")));
        assert_eq!(e.screens, Some(PathBuf::from("shots")));
        assert!(args(&["--html"]).is_err());
        assert!(args(&["--html", ""]).is_err());
        assert!(args(&["--bogus"]).is_err());
    }

    #[test]
    fn exports_refuse_the_queue_root() {
        let (dir, root) = temp_queue();
        let out = tempfile::tempdir().unwrap();
        let r = root.path();
        for inside in [
            r.to_path_buf(),
            r.join("dash.html"),
            r.join("inbox").join("dash.html"),
            r.join("new-dir").join("deeper").join("dash.html"),
            r.parent()
                .unwrap()
                .join("no-such")
                .join("..")
                .join(r.file_name().unwrap())
                .join("dash.html"),
            r.join("tmp").join("..").join("dash.html"),
        ] {
            assert!(export_path(r, &inside).is_err(), "{}", inside.display());
        }
        let ok = export_path(r, &out.path().join("sub").join("dash.html")).unwrap();
        assert!(!ok.starts_with(dir.path().canonicalize().unwrap()));

        // A refused export writes nothing, not even the other, allowed path.
        let e = Exports {
            html: Some(out.path().join("dash.html")),
            screens: Some(r.join("shots")),
        };
        assert!(export(&root, &e, sessions::Report::default).is_err());
        assert!(!out.path().join("dash.html").exists());
        assert!(!r.join("shots").exists());
    }

    #[test]
    fn export_writes_the_dashboard_and_both_screens() {
        let (_dir, root) = temp_queue();
        put(&root.inbox("w1"), "20261027T112500Z-lead-0001", "task");
        let out = tempfile::tempdir().unwrap();
        let e = Exports {
            html: Some(out.path().join("dash.html")),
            screens: Some(out.path().join("shots")),
        };
        export(&root, &e, sessions::Report::default).unwrap();
        for f in [
            "dash.html",
            "shots/overview.html",
            "shots/flow.html",
            "shots/rejected.html",
            "shots/sessions.html",
        ] {
            let text = fs::read_to_string(out.path().join(f)).unwrap();
            assert!(text.starts_with("<!doctype html>"), "{f}");
            // `put` writes the body "secret".
            assert!(!text.contains("secret"), "{f}");
        }
        // The queue itself is unchanged.
        assert_eq!(json_files(&root.inbox("w1")).len(), 1);
    }

    #[test]
    fn dashboard_escapes_queue_text_and_hides_bodies() {
        let mut snap = sample();
        snap.watchers[0].name = "w<1>".into();
        let page = html::dashboard(Path::new("/queue/<root>"), &snap);
        assert!(!page.contains("<script"));
        assert!(page.contains(
            "&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt; &amp; &quot;Scan lane&quot;"
        ));
        assert!(page.contains("w&lt;1&gt;"));
        assert!(!page.contains("w<1>"));
        assert!(page.contains("/queue/&lt;root&gt;"));
        assert!(!page.contains(BODY));
        // Self-contained: nothing is fetched.
        for external in ["http://", "https://", "<link", "@import", "src=\""] {
            assert!(!page.contains(external), "{external}");
        }
        for section in [
            "Watchers",
            "In flight",
            "Recent",
            "Who talks to whom",
            "Workers",
            "Median task → reply",
        ] {
            assert!(page.contains(section), "{section}");
        }
    }

    /// Line 4 of a `members/` record gives the mode; a record without it is a Monitor
    /// watcher, and a name without a record has none. A process's arguments win.
    #[test]
    fn snapshot_reads_the_watcher_mode() {
        let (_dir, root) = temp_queue();
        fs::create_dir_all(root.members()).unwrap();
        fs::write(
            root.members().join("local_a"),
            "w1\n2026-10-27T11:00:00Z\nD:\\Code\\a\ninbox\n",
        )
        .unwrap();
        fs::write(
            root.members().join("local_b"),
            "w2\n2026-10-27T11:00:00Z\nD:\\Code\\b\n",
        )
        .unwrap();
        fs::write(
            root.members().join("codex-w4"),
            "w4\n2026-10-27T11:00:00Z\nD:\\Code\\d\ncodex\n",
        )
        .unwrap();
        fs::write(root.heartbeat("w3"), "2026-10-27T11:29:00Z\n").unwrap();
        fs::write(root.heartbeat("w4"), "2026-10-27T11:29:00Z\n").unwrap();

        let mut s = snap(&root, &mut Beats::new());
        let mode = |s: &Snapshot, n: &str| s.mode(s.watchers.iter().find(|w| w.name == n).unwrap());
        assert_eq!(mode(&s, "w1"), Some(Mode::Inbox));
        assert_eq!(mode(&s, "w2"), Some(Mode::Monitor));
        assert_eq!(mode(&s, "w3"), None);
        assert_eq!(mode(&s, "w4"), Some(Mode::Codex));

        s.sessions.modes.insert("w1".into(), Mode::Monitor);
        s.sessions.modes.insert("w3".into(), Mode::Inbox);
        assert_eq!(mode(&s, "w1"), Some(Mode::Monitor));
        assert_eq!(mode(&s, "w3"), Some(Mode::Inbox));
    }

    /// The mode shows on the Overview and Sessions screens and in the dashboard.
    #[test]
    fn watcher_mode_is_shown_on_screens_and_exports() {
        let mut snap = sample();
        // The record says monitor; the running process says inbox.
        snap.sessions.modes.insert("w1".into(), Mode::Inbox);
        for screen in [Screen::Overview, Screen::Sessions] {
            let mut term = Terminal::new(TestBackend::new(140, 42)).unwrap();
            term.draw(|f| draw(f, Path::new("/queue"), &snap, false, screen, &THEMES[0]))
                .unwrap();
            let text: String = term
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(text.contains("mode"), "{screen:?}");
            assert!(text.contains("inbox"), "{screen:?}");
            assert!(!text.contains("monitor"), "{screen:?}");
        }

        let page = html::dashboard(Path::new("/queue"), &snap);
        assert!(page.contains("<th>Mode</th>"));
        assert!(page.contains("<td>inbox</td>"));
        snap.sessions.modes.clear();
        let page = html::dashboard(Path::new("/queue"), &snap);
        assert!(page.contains("<td>monitor</td>"));
        assert!(!page.contains("<td>inbox</td>"));
    }

    #[test]
    fn snapshot_reads_sleeping_names_and_their_folders() {
        let (_dir, root) = temp_queue();
        fs::create_dir_all(root.sleep_dir()).unwrap();
        fs::write(root.sleep_file("napper"), r#"{"by":"napper","time":"x"}"#).unwrap();
        fs::write(
            root.members().join("local_nap"),
            "napper\n2026-10-27T11:00:00Z\nD:\\Code\\nap\n",
        )
        .unwrap();
        fs::write(root.heartbeat("napper"), "2026-10-27T10:00:00Z\n").unwrap();

        let s = snap(&root, &mut Beats::new());
        let w = s.watchers.iter().find(|w| w.name == "napper").unwrap();
        assert!(w.asleep);
        assert_eq!(w.busy(), Busy::Asleep);
        assert_eq!(
            w.folder.as_deref(),
            Some(sessions::folder_key(Path::new("D:\\Code\\nap")).as_str())
        );
        let w1 = s.watchers.iter().find(|w| w.name == "w1").unwrap();
        assert!(!w1.asleep && w1.folder.is_none());
    }

    /// A sleeping watcher shows `asleep`, not `STALE`, on Overview, on the Sessions screen
    /// (found by the folder in its `members/` record), and in both HTML exports.
    #[test]
    fn asleep_is_shown_on_screens_and_exports() {
        let mut snap = sample();
        snap.sessions.rows.push(sessions::Row {
            name: "Nap session".into(),
            folder_key: "d--code-nap".into(),
            state: sessions::State::Idle,
            active: 1_793_090_000,
            ..Default::default()
        });
        let row = snap.sessions.rows.last().unwrap();
        assert_eq!(
            draw::session_watcher(&snap, row).map(|w| w.name.as_str()),
            Some("napper")
        );
        // Last seen in this folder while cq-top ran: found by name too.
        let seen = sessions::Row {
            cq_last: Some("napper".into()),
            ..Default::default()
        };
        assert!(draw::session_watcher(&snap, &seen).is_some());
        // An awake watcher is found only while its process runs.
        let gone = sessions::Row {
            cq_last: Some("w1".into()),
            ..Default::default()
        };
        assert!(draw::session_watcher(&snap, &gone).is_none());

        for screen in [Screen::Overview, Screen::Sessions] {
            let mut term = Terminal::new(TestBackend::new(140, 42)).unwrap();
            term.draw(|f| draw(f, Path::new("/queue"), &snap, false, screen, &THEMES[0]))
                .unwrap();
            let text: String = term
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(text.contains("asleep"), "{screen:?}");
            assert!(!text.contains("STALE"), "{screen:?}");
        }

        let page = html::dashboard(Path::new("/queue"), &snap);
        assert!(page.contains("asleep"));
        assert!(!page.contains("STALE"));
        assert!(!page.contains("stale (heartbeat"));
        let sessions = screens::page(
            Path::new("/queue"),
            &snap,
            Screen::Sessions,
            &THEMES[0],
            140,
            42,
        );
        // One span per cell: join the cells' text.
        let cells: String = sessions
            .split("</span>")
            .filter_map(|p| p.rsplit('>').next())
            .collect();
        assert!(cells.contains("asleep"));
    }

    #[test]
    fn screen_pages_escape_cells_and_hide_bodies() {
        let snap = sample();
        for screen in Screen::ALL {
            let page = screens::page(Path::new("/queue"), &snap, screen, &THEMES[0], 140, 42);
            assert!(!page.contains("<script"));
            assert!(!page.contains(BODY));
            // One row per terminal line.
            assert_eq!(page.matches("<div>").count(), 42);
        }
        let overview = screens::page(
            Path::new("/queue"),
            &snap,
            Screen::Overview,
            &THEMES[0],
            140,
            42,
        );
        assert!(overview.contains("&lt;"));
    }

    #[test]
    fn rejected_and_dead_messages_carry_their_reasons() {
        let (_dir, root) = temp_queue();
        put(&root.rejected("w1"), "20261027T112504Z-lead-0005", "task");
        fs::write(
            root.rejected("w1")
                .join("20261027T112504Z-lead-0005.reason.json"),
            r#"{"id":"20261027T112504Z-lead-0005","reason":"out of scope","time":"x"}"#,
        )
        .unwrap();
        // Unreadable, as dead-lettered messages often are; the sender has a hyphen.
        fs::write(
            root.dlq("w1").join("20261027T112506Z-lead-0007.json"),
            "{not json",
        )
        .unwrap();
        fs::write(
            root.dlq("w1")
                .join("20261027T112506Z-lead-0007.reason.json"),
            r#"{"reason":"invalid JSON"}"#,
        )
        .unwrap();
        // No sidecar at all.
        put(&root.dlq("w1"), "20261027T112505Z-lead-0006", "task");
        put(&root.done("w1"), "20261027T112507Z-lead-0008", "task");

        let s = snap(&root, &mut Beats::new());
        assert_eq!(s.problems_total, 3);
        let rows: Vec<_> = s
            .problems
            .iter()
            .map(|m| (m.bucket, m.from.as_str(), m.to.as_str(), m.reason.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                ("dead", "lead", "w1", "invalid JSON"),
                ("dead", "lead", "w1", ""),
                ("rejected", "lead", "w1", "out of scope"),
            ]
        );
        // Sidecars are still not counted as messages.
        let w = s.watchers.iter().find(|w| w.name == "w1").unwrap();
        assert_eq!((w.rejected, w.dead, w.done), (1, 2, 1));
    }

    #[test]
    fn sessions_are_exported_with_escaped_titles() {
        let snap = sample();
        let page = html::dashboard(Path::new("/queue"), &snap);
        assert!(page.contains("<h2>Sessions"));
        assert!(page.contains("&lt;b&gt;RC&lt;/b&gt; &amp; &quot;x&quot;"));
        assert!(!page.contains("<b>RC"));
        // The idle session with a waiting message is a warning; the cost is labelled.
        assert!(page.contains("idle, messages waiting"));
        assert!(page.contains("Est. cost today"));
        assert!(page.contains("Busy cores 1, Cores 8") && page.contains("Swap 1.0 / 4.0 GB"));
        let screen = screens::page(
            Path::new("/q"),
            &snap,
            Screen::Sessions,
            &THEMES[0],
            140,
            42,
        );
        assert!(!screen.contains("<b>"));
        assert!(screen.contains("&lt;"));

        // The rows and the strip show on the screen, at 140 columns with every column.
        let mut term = Terminal::new(TestBackend::new(140, 42)).unwrap();
        term.draw(|f| {
            draw(
                f,
                Path::new("/q"),
                &snap,
                false,
                Screen::Sessions,
                &THEMES[0],
            )
        })
        .unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        for want in [
            "▲ idle",
            "● working",
            "135k",
            "66.3M",
            "$12.50",
            "edit",
            "last change",
            "Busy cores",
            "24.0 GB",
            "now · the graph builds",
        ] {
            assert!(text.contains(want), "{want}");
        }
        // A narrow terminal drops the least important columns, never the name or state.
        let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
        term.draw(|f| {
            draw(
                f,
                Path::new("/q"),
                &snap,
                false,
                Screen::Sessions,
                &THEMES[0],
            )
        })
        .unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("● working") && text.contains("w1"));
        assert!(!text.contains("last change"));
    }

    #[test]
    fn only_the_context_cell_is_coloured_when_the_context_is_large() {
        use ratatui::style::{Color, Style};
        let mut snap = sample();
        let row = |name: &str, context| sessions::Row {
            name: name.into(),
            state: sessions::State::Idle,
            model: "opus-5-5".into(),
            context: Some(context),
            tokens_today: 7_000,
            active: snap.now - 4000,
            ..Default::default()
        };
        snap.sessions.rows = vec![
            row("small", 120_000),
            row("large", 200_000),
            row("full", 300_000),
        ];
        let page = html::dashboard(Path::new("/queue"), &snap);
        assert!(page.contains("<td class=\"n\">120k</td>"));
        assert!(page.contains("<span class=\"serious\" title=\"over 150k tokens\">200k</span>"));
        assert!(page.contains("<span class=\"critical\" title=\"over 250k tokens\">300k</span>"));

        for theme in &THEMES {
            let mut term = Terminal::new(TestBackend::new(140, 42)).unwrap();
            term.draw(|f| draw(f, Path::new("/q"), &snap, false, Screen::Sessions, theme))
                .unwrap();
            let buf = term.backend().buffer();
            // The style of the cell where `text` starts, and of the tokens cell beside it.
            let styles = |text: &str| -> (Style, Style) {
                for y in 0..42 {
                    let line: Vec<&str> = (0..140).map(|x| buf[(x, y)].symbol()).collect();
                    let n = text.chars().count();
                    if let Some(x) = (0..=140 - n).find(|&x| line[x..x + n].concat() == text) {
                        let tokens = (x + n..140).find(|&x| line[x] == "7").unwrap();
                        let at = |x: usize| buf[(x as u16, y)].style();
                        return (at(x), at(tokens));
                    }
                }
                panic!("{text} not drawn in {}", theme.name);
            };
            let (small, small_tokens) = styles("120k");
            let (large, large_tokens) = styles("200k");
            let (full, full_tokens) = styles("300k");
            assert!(
                small != large && large != full && small != full,
                "{}",
                theme.name
            );
            // The rest of the row keeps its style.
            assert_eq!(small, small_tokens, "{}", theme.name);
            assert_eq!(large_tokens, small_tokens, "{}", theme.name);
            assert_eq!(full_tokens, small_tokens, "{}", theme.name);
            if theme.name == "classic" {
                assert_eq!(
                    (large.fg, full.fg),
                    (Some(Color::Indexed(208)), Some(Color::Red))
                );
            }
        }
    }

    #[test]
    fn every_theme_draws_the_cpu_and_memory_boxes() {
        let text = |snap: &Snapshot, theme, w, h| {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| draw(f, Path::new("/q"), snap, false, Screen::Sessions, theme))
                .unwrap();
            term.backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect::<String>()
        };
        let still = sample();
        let mut live = sample();
        live.sessions.history = (0..200)
            .map(|i| sessions::Sample {
                cpu: (i % 100) as f32,
                claude_cpu: (i % 10) as f32,
                codex_cpu: (i % 7) as f32,
                mem: 40.0,
                claude_mem: 5.0,
                codex_mem: 3.0,
            })
            .collect();
        for theme in &THEMES {
            for (w, h) in [(140, 42), (60, 20), (20, 8), (1, 1)] {
                // Small sizes must not panic; the figures show whenever they fit.
                let (a, b) = (text(&still, theme, w, h), text(&live, theme, w, h));
                if w >= 60 {
                    for t in [&a, &b] {
                        assert!(t.contains("CPU") && t.contains("Memory"), "{}", theme.name);
                        assert!(
                            t.contains("Claude") && t.contains("Codex"),
                            "{}",
                            theme.name
                        );
                    }
                }
                if w == 140 {
                    // Braille appears only with a history; the export shows a bar instead.
                    let braille =
                        |t: &str| t.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c));
                    assert!(braille(&b) && !braille(&a), "{}", theme.name);
                    assert!(a.contains("the graph builds") && !b.contains("the graph builds"));
                    assert!(a.contains("Busy cores") && a.contains("12.5%"));
                }
            }
        }
    }

    #[test]
    fn dashboard_lists_rejected_and_dead_with_escaped_reasons() {
        let page = html::dashboard(Path::new("/queue"), &sample());
        assert!(page.contains("Rejected and dead-lettered"));
        assert!(page.contains("&lt;img src=x onerror=alert(1)&gt; not my job"));
        assert!(!page.contains("<img"));
        // The total is shown even when only some rows are.
        assert!(page.contains("1 of 3"));
    }
}
