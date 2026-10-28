//! The Sessions screen's data: Claude Code sessions found from their transcripts in
//! `~/.claude/projects`, matched to running Claude processes and cq watchers by folder.
//!
//! Read-only, like the rest of cq-top. Only counts, times, model names and session titles
//! are kept; message text is parsed past and never stored or shown. `git` runs with
//! optional locks off, so it never writes the index.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    env, fs,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant, UNIX_EPOCH},
};

use serde::Deserialize;

use super::{now_epoch, parse_ts};

/// Written to in the last 2 minutes: working.
pub const WORKING_SECS: i64 = 120;
/// Written to in the last 45 minutes: recent. Older is idle.
pub const RECENT_SECS: i64 = 45 * 60;
/// CPU use of a session's process tree that counts as working, in % of one core.
pub const WORKING_CPU: f32 = 5.0;
/// Subagent transcripts written in the last 10 minutes are counted in the Sub column.
pub const SUB_RECENT_SECS: i64 = 10 * 60;
/// A folder with no running Claude process is shown while its newest transcript is this new.
const SHOW_SECS: i64 = 24 * 3600;
/// `git` runs at most this often per folder.
const GIT_EVERY: Duration = Duration::from_secs(60);
/// The background scan runs this often.
const SCAN_EVERY: Duration = Duration::from_secs(3);
/// How much of a transcript's end is read for the latest model and context, first and at most.
const TAIL_FIRST: u64 = 256 * 1024;
const TAIL_MAX: u64 = 4 * 1024 * 1024;
/// How much of a transcript's start is read for its folder and title.
const HEAD_MAX: u64 = 256 * 1024;

// ---------- prices ----------

/// USD per million tokens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Price {
    input: f64,
    output: f64,
    write_5m: f64,
    write_1h: f64,
    read: f64,
}

const fn p(input: f64, write_5m: f64, write_1h: f64, read: f64, output: f64) -> Price {
    Price {
        input,
        output,
        write_5m,
        write_1h,
        read,
    }
}

/// Claude API list prices, read on 28 October 2026 from
/// <https://platform.claude.com/docs/en/about-claude/pricing> ("Model pricing").
/// Keys are model names without `claude-` and without a date suffix.
/// The owner's plan is not billed per token, so the cost is an estimate of API list price.
/// Fast mode, batch and regional (1.1x) prices are not applied.
const PRICES: &[(&str, Price)] = &[
    ("fable-5-1", p(10.0, 12.5, 20.0, 0.25, 50.0)),
    ("mythos-5-1", p(10.0, 12.5, 20.0, 0.25, 50.0)),
    ("fable-5", p(10.0, 12.5, 20.0, 1.0, 50.0)),
    ("mythos-5", p(10.0, 12.5, 20.0, 1.0, 50.0)),
    ("opus-5-5", p(4.0, 5.0, 8.0, 0.20, 20.0)),
    ("opus-5", p(5.0, 6.25, 10.0, 0.50, 25.0)),
    ("opus-4-8", p(5.0, 6.25, 10.0, 0.50, 25.0)),
    ("opus-4-7", p(5.0, 6.25, 10.0, 0.50, 25.0)),
    ("opus-4-6", p(5.0, 6.25, 10.0, 0.50, 25.0)),
    ("opus-4-5", p(5.0, 6.25, 10.0, 0.50, 25.0)),
    ("opus-4-1", p(15.0, 18.75, 30.0, 1.50, 75.0)),
    ("opus-4", p(15.0, 18.75, 30.0, 1.50, 75.0)),
    ("sonnet-5-5", p(2.0, 2.5, 4.0, 0.10, 10.0)),
    ("sonnet-5", p(2.0, 2.5, 4.0, 0.20, 10.0)),
    ("sonnet-4-6", p(3.0, 3.75, 6.0, 0.30, 15.0)),
    ("sonnet-4-5", p(3.0, 3.75, 6.0, 0.30, 15.0)),
    ("sonnet-4", p(3.0, 3.75, 6.0, 0.30, 15.0)),
    ("haiku-4-5", p(1.0, 1.25, 2.0, 0.10, 5.0)),
    ("3-5-haiku", p(0.80, 1.0, 1.60, 0.08, 4.0)),
];
/// Haiku 5.5 is priced per request by prompt length (input + cache writes + cache reads).
const HAIKU_5_5_SMALL: Price = p(0.10, 0.125, 0.20, 0.01, 0.50);
const HAIKU_5_5_LARGE: Price = p(0.50, 0.625, 1.0, 0.05, 2.50);
const HAIKU_5_5_LIMIT: u64 = 100_000;

/// `claude-opus-5-5` -> `opus-5-5`; also drops a date suffix (`-20251001`) and `[1m]`.
pub fn short_model(model: &str) -> String {
    let m = model.trim().trim_end_matches("[1m]");
    let m = m.strip_prefix("claude-").unwrap_or(m);
    match m.rsplit_once('-') {
        Some((head, date)) if date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()) => {
            head.to_string()
        }
        _ => m.to_string(),
    }
}

/// The price of one request, or `None` for a model not in the table.
pub fn price(model: &str, prompt: u64) -> Option<Price> {
    let short = short_model(model);
    if short == "haiku-5-5" {
        return Some(if prompt > HAIKU_5_5_LIMIT {
            HAIKU_5_5_LARGE
        } else {
            HAIKU_5_5_SMALL
        });
    }
    PRICES.iter().find(|(k, _)| *k == short).map(|(_, p)| *p)
}

// ---------- transcript lines ----------

#[derive(Deserialize, Default, Clone, Copy, Debug)]
/// How many of the cache writes went to the 1-hour cache; the rest are 5-minute writes.
struct CacheCreation {
    #[serde(default)]
    ephemeral_1h_input_tokens: Option<u64>,
}

/// The `usage` of one API response.
#[derive(Deserialize, Default, Clone, Copy, Debug)]
pub struct Usage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    cache_creation: Option<CacheCreation>,
}

impl Usage {
    /// What the model read: input + cache writes + cache reads. The latest one is the context.
    pub fn prompt(&self) -> u64 {
        self.input_tokens.unwrap_or(0)
            + self.cache_creation_input_tokens.unwrap_or(0)
            + self.cache_read_input_tokens.unwrap_or(0)
    }

    pub fn total(&self) -> u64 {
        self.prompt() + self.output_tokens.unwrap_or(0)
    }

    /// The cost in USD at `p`. Cache writes are split into 5-minute and 1-hour writes when
    /// the line says how; otherwise they count as 5-minute writes.
    pub fn cost(&self, p: Price) -> f64 {
        let writes = self.cache_creation_input_tokens.unwrap_or(0);
        let (w5m, w1h) = match self.cache_creation {
            Some(c) => {
                let w1h = c.ephemeral_1h_input_tokens.unwrap_or(0).min(writes);
                (writes - w1h, w1h)
            }
            None => (writes, 0),
        };
        let m = |tokens: u64, usd: f64| tokens as f64 * usd / 1_000_000.0;
        m(self.input_tokens.unwrap_or(0), p.input)
            + m(self.output_tokens.unwrap_or(0), p.output)
            + m(w5m, p.write_5m)
            + m(w1h, p.write_1h)
            + m(self.cache_read_input_tokens.unwrap_or(0), p.read)
    }
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<Usage>,
}

/// The parts of a transcript line cq-top uses. Everything else, message text included, is
/// skipped by the parser.
#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(rename = "requestId", default)]
    request_id: Option<String>,
    #[serde(default)]
    message: Option<Message>,
}

/// One real API response: (dedupe key, time, model, usage). `<synthetic>` lines are not
/// API calls and give `None`, as does any line that is not an assistant message.
fn assistant(line: &str) -> Option<(Option<String>, Option<i64>, String, Usage)> {
    // Cheap test first: most of a transcript's bytes are tool results in user lines.
    if !line.contains("\"type\":\"assistant\"") {
        return None;
    }
    let l: Line = serde_json::from_str(line).ok()?;
    if l.kind != "assistant" {
        return None;
    }
    let msg = l.message?;
    let model = msg.model?;
    if model == "<synthetic>" {
        return None;
    }
    let usage = msg.usage?;
    let ts = l.timestamp.as_deref().and_then(iso_secs);
    Some((msg.id.or(l.request_id), ts, model, usage))
}

/// `2026-10-28T00:19:18.797Z` -> epoch seconds (the fraction is dropped).
fn iso_secs(s: &str) -> Option<i64> {
    parse_ts(s.split(['.', 'Z']).next()?)
}

/// The model and context of the newest assistant line.
#[derive(Clone, Debug, PartialEq)]
pub struct Latest {
    pub model: String,
    pub context: u64,
}

/// Running totals for one transcript file. Only bytes appended since the last read are
/// read; a file that shrank is read again from the start.
#[derive(Default, Debug)]
pub struct Tally {
    /// Bytes read so far; always just after a newline.
    offset: u64,
    /// The local midnight that `tokens` and `cost` count from.
    day: i64,
    /// Message ids already counted today: one response is written as several lines.
    seen: HashSet<String>,
    pub tokens: u64,
    pub cost: f64,
    /// A line today had a model with no price, so `cost` is incomplete.
    pub unpriced: bool,
    pub latest: Option<Latest>,
}

impl Tally {
    /// Brings the tally up to date with the file. `today` is local midnight, epoch seconds.
    pub fn update(&mut self, path: &Path, today: i64) {
        let Ok(meta) = fs::metadata(path) else {
            return;
        };
        let (len, mtime) = (meta.len(), mtime_of(&meta));
        if len < self.offset {
            *self = Tally::default();
        }
        if self.day != today {
            // Everything already read was written before this midnight; keep the offset.
            self.day = today;
            self.seen.clear();
            self.tokens = 0;
            self.cost = 0.0;
            self.unpriced = false;
        }
        if self.offset == 0 && mtime < today {
            // Nothing in the file is from today: the tail is enough.
            self.latest = tail_latest(path, len);
            self.offset = len;
            return;
        }
        if len == self.offset {
            return;
        }
        let Ok(mut file) = fs::File::open(path) else {
            return;
        };
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut reader = BufReader::new(file.take(len - self.offset));
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                // A line still being written is read next time, whole.
                Ok(_) if buf.last() != Some(&b'\n') => break,
                Ok(n) => {
                    self.offset += n as u64;
                    if let Ok(line) = std::str::from_utf8(&buf) {
                        self.add(line);
                    }
                }
            }
        }
    }

    fn add(&mut self, line: &str) {
        let Some((key, ts, model, usage)) = assistant(line) else {
            return;
        };
        self.latest = Some(Latest {
            model: model.clone(),
            context: usage.prompt(),
        });
        if ts.is_none_or(|t| t < self.day) {
            return;
        }
        if let Some(k) = key
            && !self.seen.insert(k)
        {
            return;
        }
        self.tokens += usage.total();
        match price(&model, usage.prompt()) {
            Some(p) => self.cost += usage.cost(p),
            None => self.unpriced = true,
        }
    }
}

/// The newest assistant line in the last part of a file, reading more of it when needed.
fn tail_latest(path: &Path, len: u64) -> Option<Latest> {
    let mut file = fs::File::open(path).ok()?;
    let mut size = TAIL_FIRST;
    loop {
        let start = len.saturating_sub(size);
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut buf = Vec::new();
        file.by_ref().take(len - start).read_to_end(&mut buf).ok()?;
        let text = String::from_utf8_lossy(&buf);
        let mut lines: Vec<&str> = text.split('\n').collect();
        if start > 0 {
            // The first piece is the end of a line that began before `start`.
            lines.remove(0);
        }
        let found = lines.iter().rev().find_map(|l| assistant(l));
        if let Some((_, _, model, usage)) = found {
            return Some(Latest {
                model,
                context: usage.prompt(),
            });
        }
        if start == 0 || size >= TAIL_MAX {
            return None;
        }
        size *= 4;
    }
}

#[derive(Deserialize)]
struct HeadLine {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(rename = "customTitle", default)]
    custom_title: Option<String>,
}

/// From the start of a transcript: the session's folder (the first `cwd` that encodes to
/// `encoded`, so a later `cd` does not count) and a `custom-title` line's title.
fn head_info(path: &Path, encoded: &str) -> (Option<PathBuf>, Option<String>) {
    let mut buf = Vec::new();
    if let Ok(f) = fs::File::open(path) {
        let _ = f.take(HEAD_MAX).read_to_end(&mut buf);
    }
    let text = String::from_utf8_lossy(&buf);
    let (mut folder, mut title) = (None, None);
    // The last piece may be cut short; it is skipped by the parser.
    for line in text.split('\n') {
        if !line.contains("\"cwd\"") && !line.contains("\"custom-title\"") {
            continue;
        }
        let Ok(h) = serde_json::from_str::<HeadLine>(line) else {
            continue;
        };
        if h.kind == "custom-title" && title.is_none() {
            title = h.custom_title;
        }
        if folder.is_none()
            && let Some(cwd) = h.cwd
            && same_key(&encode(Path::new(&cwd)), encoded)
        {
            folder = Some(PathBuf::from(cwd));
        }
        if folder.is_some() && title.is_some() {
            break;
        }
    }
    (folder, title)
}

// ---------- folders and names ----------

/// A project folder as Claude Code names its transcripts folder: every character that is
/// not an ASCII letter or digit becomes `-`. A trailing separator is dropped first.
pub fn encode(path: &Path) -> String {
    let s = path.to_string_lossy();
    s.trim_end_matches(['/', '\\'])
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The key of a working folder, as the Sessions screen matches folders.
pub fn folder_key(path: &Path) -> String {
    key(&encode(path))
}

/// Windows folders are not case-sensitive, so `d:\` and `D:\` are the same folder there.
fn key(encoded: &str) -> String {
    if cfg!(windows) {
        encoded.to_ascii_lowercase()
    } else {
        encoded.to_string()
    }
}

fn same_key(a: &str, b: &str) -> bool {
    key(a) == key(b)
}

/// The folder's last part; for a worktree in `<repo>/.claude/worktrees/<X>`, `<repo>/<X>`.
pub fn folder_label(path: &Path) -> String {
    let parts: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .filter(|c| !c.is_empty() && c != "\\" && c != "/")
        .collect();
    let n = parts.len();
    if n >= 4 && parts[n - 3] == ".claude" && parts[n - 2] == "worktrees" {
        return format!("{}/{}", parts[n - 4], parts[n - 1]);
    }
    parts.last().cloned().unwrap_or_default()
}

/// The name order: the cq name whose watcher runs in the folder, then the session's custom
/// title without a leading `RC: `, then the folder label.
pub fn display_name(watcher: Option<&str>, title: Option<&str>, folder: &str) -> String {
    if let Some(w) = watcher.filter(|w| !w.is_empty()) {
        return w.to_string();
    }
    if let Some(t) = title.map(str::trim).filter(|t| !t.is_empty()) {
        return t.strip_prefix("RC: ").unwrap_or(t).trim().to_string();
    }
    folder.to_string()
}

/// The name in `cq watch --as NAME` (or `--as=NAME`), from a process's arguments.
fn watch_name(args: &[String]) -> Option<String> {
    if !args.iter().any(|a| a == "watch") {
        return None;
    }
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--as" {
            return it.next().cloned();
        }
        if let Some(n) = a.strip_prefix("--as=") {
            return Some(n.to_string());
        }
    }
    None
}

// ---------- the state rule ----------

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub enum State {
    Working,
    Recent,
    #[default]
    Idle,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::Working => "working",
            State::Recent => "recent",
            State::Idle => "idle",
        }
    }
}

/// Working: a transcript or subagent transcript was written in the last 2 minutes, or the
/// session's process tree uses at least 5 % of one core. Recent: written in the last
/// 45 minutes. Idle: older. `last_write` is the newest of the transcript and its subagents.
pub fn state(now: i64, last_write: i64, cpu: Option<f32>) -> State {
    let ago = now - last_write;
    if ago <= WORKING_SECS || cpu.is_some_and(|c| c >= WORKING_CPU) {
        State::Working
    } else if ago <= RECENT_SECS {
        State::Recent
    } else {
        State::Idle
    }
}

/// An idle session with cq messages waiting or claimed: something is not being picked up.
pub fn warn(state: State, waiting: usize, claimed: usize) -> bool {
    state == State::Idle && waiting + claimed > 0
}

// ---------- git ----------

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Change {
    Commit,
    Edit,
}

impl Change {
    pub fn label(self) -> &'static str {
        match self {
            Change::Commit => "commit",
            Change::Edit => "edit",
        }
    }
}

/// The newest of the last commit on the folder's own `HEAD` (its worktree, not the whole
/// repo) and the newest change to a file `git status` lists there. `None` outside a repo or without `git`.
pub fn last_change(folder: &Path) -> Option<(Change, i64)> {
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(folder)
            .args(args)
            // Read-only: `git status` would otherwise refresh and write the index.
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| o.stdout)
    };
    let commit = git(&["log", "-1", "--format=%ct", "HEAD"])
        .and_then(|o| String::from_utf8_lossy(&o).trim().parse::<i64>().ok())
        .map(|t| (Change::Commit, t));

    // Porcelain paths are relative to the top of the worktree.
    let top = git(&["rev-parse", "--show-toplevel"])
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o).trim()));
    let edit = top.and_then(|top| {
        let out = git(&["status", "--porcelain", "-z"])?;
        let mut fields = out.split(|b| *b == 0);
        let mut newest = None;
        while let Some(f) = fields.next() {
            if f.len() < 4 {
                continue;
            }
            // A rename or copy is followed by its old path.
            if matches!(f[0], b'R' | b'C') || matches!(f[1], b'R' | b'C') {
                fields.next();
            }
            let path = top.join(String::from_utf8_lossy(&f[3..]).trim_end_matches('/'));
            if let Ok(meta) = fs::symlink_metadata(&path) {
                newest = newest.max(Some(mtime_of(&meta)));
            }
        }
        newest.map(|t| (Change::Edit, t))
    });
    match (commit, edit) {
        (Some(c), Some(e)) => Some(if e.1 > c.1 { e } else { c }),
        (c, e) => c.or(e),
    }
}

#[derive(Default)]
struct GitEntry {
    checked: Option<Instant>,
    result: Option<(Change, i64)>,
}

type GitCache = Arc<Mutex<HashMap<PathBuf, GitEntry>>>;

/// How the Last change column is filled.
enum Git {
    /// Not at all (tests).
    #[cfg(test)]
    Off,
    /// Inline, once per folder (a one-shot export).
    Inline(HashMap<PathBuf, Option<(Change, i64)>>),
    /// From a background thread that runs `git` at most every 60 s per folder.
    Background(GitCache),
}

/// Runs `git` for the folders the scanner asks about, oldest check first.
fn git_worker(cache: GitCache) {
    loop {
        let due = {
            let c = cache.lock().unwrap_or_else(|e| e.into_inner());
            c.iter()
                .filter(|(_, e)| e.checked.is_none_or(|t| t.elapsed() >= GIT_EVERY))
                .min_by_key(|(_, e)| e.checked)
                .map(|(p, _)| p.clone())
        };
        let Some(folder) = due else {
            thread::sleep(Duration::from_secs(1));
            continue;
        };
        let result = last_change(&folder);
        let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
        let e = c.entry(folder).or_default();
        e.checked = Some(Instant::now());
        e.result = result;
    }
}

// ---------- processes ----------

/// A core at least this busy (% of that core) counts in the busy-cores figure.
pub const BUSY_CORE: f32 = 50.0;
/// Samples kept for the CPU and Memory graphs: 6 minutes at one per scan.
pub const HISTORY: usize = 120;

/// Whole-computer figures for the CPU and Memory boxes at the top.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Machine {
    /// 0 to 100, all cores together.
    pub cpu: f32,
    pub cores: usize,
    /// Cores at `BUSY_CORE` % or more.
    pub busy_cores: usize,
    pub mem_used: u64,
    pub mem_total: u64,
    pub mem_available: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    /// Every process named `claude`, sessions and helpers: % of one core, and bytes.
    pub claude_cpu: f32,
    pub claude_mem: u64,
    pub claude_procs: usize,
}

/// `part` as a % of `total`, 0 to 100; an empty total gives 0.
fn percent(part: f64, total: f64) -> f32 {
    if total > 0.0 {
        (100.0 * part / total).clamp(0.0, 100.0) as f32
    } else {
        0.0
    }
}

impl Machine {
    /// Claude's CPU as a % of the whole computer, on the same scale as `cpu`.
    pub fn claude_share(&self) -> f32 {
        percent(f64::from(self.claude_cpu), 100.0 * self.cores as f64)
    }

    pub fn sample(&self) -> Sample {
        let total = self.mem_total as f64;
        Sample {
            cpu: self.cpu.clamp(0.0, 100.0),
            claude_cpu: self.claude_share(),
            mem: percent(self.mem_used as f64, total),
            claude_mem: percent(self.claude_mem as f64, total),
        }
    }

    /// The CPU box's figures: (label, value).
    pub fn cpu_figures(&self) -> [(&'static str, String); 4] {
        [
            ("Total", format!("{:.1}%", self.cpu)),
            ("Claude", format!("{:.1}%", self.claude_share())),
            ("Busy cores", self.busy_cores.to_string()),
            ("Cores", self.cores.to_string()),
        ]
    }

    /// The Memory box's figures: (label, value).
    pub fn mem_figures(&self) -> [(&'static str, String); 4] {
        [
            ("Used", of_gb(self.mem_used, self.mem_total)),
            ("Available", gb(self.mem_available)),
            ("Swap", of_gb(self.swap_used, self.swap_total)),
            ("Claude", gb(self.claude_mem)),
        ]
    }
}

/// One point on the graphs, every figure 0 to 100 % of the whole computer.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sample {
    pub cpu: f32,
    pub claude_cpu: f32,
    pub mem: f32,
    pub claude_mem: f32,
}

/// The newest `cap` samples, oldest first. Lives only in the running view.
#[derive(Debug)]
pub struct History {
    cap: usize,
    samples: VecDeque<Sample>,
}

impl History {
    pub fn new(cap: usize) -> History {
        History {
            cap: cap.max(1),
            samples: VecDeque::with_capacity(cap.max(1)),
        }
    }

    pub fn push(&mut self, s: Sample) {
        if self.samples.len() == self.cap {
            self.samples.pop_front();
        }
        self.samples.push_back(s);
    }

    pub fn to_vec(&self) -> Vec<Sample> {
        self.samples.iter().copied().collect()
    }
}

/// What the process list says, keyed by encoded folder.
#[derive(Default)]
struct Procs {
    machine: Machine,
    /// Folder -> (real folder, CPU % of one core, memory) of its Claude processes and their
    /// descendants.
    sessions: HashMap<String, (PathBuf, f32, u64)>,
    /// Session folder -> cq name of its watcher.
    watchers: HashMap<String, String>,
}

/// One process, as far as watcher matching needs it.
struct ProcInfo {
    parent: Option<sysinfo::Pid>,
    /// A Claude Code session process (named `claude`, not a `--type=` helper).
    session: bool,
    cwd: Option<PathBuf>,
}

/// How far up a watcher's parents to look for its session: `cq` runs under a few shells.
const ANCESTRY_STEPS: usize = 10;

/// Folder key -> cq name, for watchers given as (watcher pid, name), first name wins. A watcher
/// belongs to the nearest session process among its parents, because `cq watch` often runs in
/// another folder than its session. Without one, it belongs to its own working folder.
fn match_watchers(
    table: &HashMap<sysinfo::Pid, ProcInfo>,
    watchers: impl IntoIterator<Item = (sysinfo::Pid, String)>,
) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (pid, name) in watchers {
        let mut up = table.get(&pid).and_then(|p| p.parent);
        let mut owner = None;
        for _ in 0..ANCESTRY_STEPS {
            let Some(p) = up.and_then(|u| table.get(&u)) else {
                break;
            };
            if p.session && p.cwd.is_some() {
                owner = p.cwd.as_deref();
                break;
            }
            up = p.parent;
        }
        if let Some(dir) = owner.or_else(|| table.get(&pid)?.cwd.as_deref()) {
            out.entry(key(&encode(dir))).or_insert(name);
        }
    }
    out
}

fn read_processes(sys: &mut sysinfo::System, heartbeat: &Path) -> Option<Procs> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, UpdateKind};
    if !sysinfo::IS_SUPPORTED_SYSTEM {
        return None;
    }
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_cwd(UpdateKind::OnlyIfNotSet)
            .with_cmd(UpdateKind::OnlyIfNotSet),
    );
    sys.refresh_cpu_usage();
    sys.refresh_memory();
    let all = sys.processes();
    if all.is_empty() {
        return None;
    }
    let stem = |p: &sysinfo::Process| {
        let n = p.name().to_string_lossy().to_ascii_lowercase();
        n.strip_suffix(".exe").map(str::to_owned).unwrap_or(n)
    };
    let args = |p: &sysinfo::Process| -> Vec<String> {
        p.cmd()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    };
    fn cwd(p: &sysinfo::Process) -> Option<&Path> {
        p.cwd().filter(|c| !c.as_os_str().is_empty())
    }

    let mut out = Procs {
        machine: Machine {
            cpu: sys.global_cpu_usage(),
            cores: sys.cpus().len(),
            busy_cores: sys
                .cpus()
                .iter()
                .filter(|c| c.cpu_usage() >= BUSY_CORE)
                .count(),
            mem_used: sys.used_memory(),
            mem_total: sys.total_memory(),
            mem_available: sys.available_memory(),
            swap_used: sys.used_swap(),
            swap_total: sys.total_swap(),
            ..Machine::default()
        },
        ..Procs::default()
    };
    let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
    let mut roots: HashMap<String, (PathBuf, Vec<Pid>)> = HashMap::new();
    let mut table = HashMap::new();
    let mut watchers = Vec::new();
    for (pid, p) in all {
        if let Some(parent) = p.parent() {
            children.entry(parent).or_default().push(*pid);
        }
        let mut session = false;
        match stem(p).as_str() {
            "claude" => {
                out.machine.claude_cpu += p.cpu_usage();
                out.machine.claude_mem += p.memory();
                out.machine.claude_procs += 1;
                // Electron helpers (`--type=renderer` and so on) are not sessions.
                session = !args(p).iter().any(|a| a.starts_with("--type="));
                if session && let Some(dir) = cwd(p) {
                    roots
                        .entry(key(&encode(dir)))
                        .or_insert_with(|| (dir.to_path_buf(), Vec::new()))
                        .1
                        .push(*pid);
                }
            }
            "cq" => watchers.extend(watch_name(&args(p)).map(|name| (*pid, name))),
            _ => {}
        }
        let info = ProcInfo {
            parent: p.parent(),
            session,
            cwd: cwd(p).map(Path::to_path_buf),
        };
        table.insert(*pid, info);
    }
    // `<root>/heartbeat/NAME.pid` names a watcher whose arguments could not be read.
    for e in fs::read_dir(heartbeat).into_iter().flatten().flatten() {
        let file = e.file_name().to_string_lossy().into_owned();
        let Some(name) = file.strip_suffix(".pid") else {
            continue;
        };
        let pid = fs::read_to_string(e.path())
            .ok()
            .and_then(|t| t.trim().parse::<u32>().ok())
            .map(Pid::from_u32);
        if let Some(pid) = pid
            && all.get(&pid).is_some_and(|p| stem(p) == "cq")
        {
            watchers.push((pid, name.to_string()));
        }
    }
    out.watchers = match_watchers(&table, watchers);
    // Each session's CPU and memory: its Claude processes and everything below them.
    for (k, (dir, pids)) in roots {
        let (mut cpu, mut mem) = (0.0, 0);
        let mut seen = HashSet::new();
        let mut stack = pids;
        while let Some(pid) = stack.pop() {
            if !seen.insert(pid) {
                continue;
            }
            if let Some(p) = all.get(&pid) {
                cpu += p.cpu_usage();
                mem += p.memory();
            }
            stack.extend(children.get(&pid).into_iter().flatten().copied());
        }
        out.sessions.insert(k, (dir, cpu, mem));
    }
    Some(out)
}

// ---------- the scan ----------

/// One session: the newest transcript in a project folder.
#[derive(Clone, Debug, Default)]
pub struct Row {
    pub name: String,
    /// The cq name whose watcher runs in this folder; the cq columns come from the queue.
    pub cq_name: Option<String>,
    /// The cq name of the last watcher seen in this folder while cq-top runs, kept after
    /// the watcher stops, so a name that went to sleep stays beside its session.
    pub cq_last: Option<String>,
    /// The folder key (see `folder_key`), to match a sleeping name's `members/` record.
    pub folder_key: String,
    pub state: State,
    /// Short form, such as `opus-5-5`; empty when no assistant line was found.
    pub model: String,
    pub context: Option<u64>,
    /// Today (since local midnight), the transcript and its subagent transcripts together.
    pub tokens_today: u64,
    /// `None` when a model used today has no price.
    pub cost_today: Option<f64>,
    /// % of one core, and bytes; `None` without a matching process.
    pub cpu: Option<f32>,
    pub mem: Option<u64>,
    /// Subagent transcripts written in the last 10 minutes.
    pub subagents: usize,
    pub last_change: Option<(Change, i64)>,
    /// The newest write to the transcript or a subagent transcript.
    pub active: i64,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    /// False until the first scan is done.
    pub ready: bool,
    /// `None` when process data could not be read: CPU and memory show `-`.
    pub machine: Option<Machine>,
    pub rows: Vec<Row>,
    /// Where the transcripts were looked for.
    pub projects: PathBuf,
    /// One sample per scan, oldest first; empty in a one-shot export.
    pub history: Vec<Sample>,
}

/// Where the Sessions screen reads from.
pub struct Sources {
    /// `~/.claude/projects`, or `$CLAUDE_CONFIG_DIR/projects`.
    pub projects: PathBuf,
    /// The queue root's heartbeat folder, for `NAME.pid`.
    pub heartbeat: PathBuf,
    /// Read the process list. Off in tests.
    pub processes: bool,
}

impl Sources {
    pub fn live(root: &cq::Root) -> Sources {
        let var = |k: &str| env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let home = if cfg!(windows) {
            var("USERPROFILE").or_else(|| var("HOME"))
        } else {
            var("HOME")
        };
        let config = var("CLAUDE_CONFIG_DIR").or_else(|| home.map(|h| h.join(".claude")));
        Sources {
            projects: config.map(|c| c.join("projects")).unwrap_or_default(),
            heartbeat: root.path().join("heartbeat"),
            processes: true,
        }
    }
}

/// Folder facts that do not change between scans.
#[derive(Default)]
struct Folder {
    /// The real folder, once a process or a transcript has shown it.
    path: Option<PathBuf>,
    /// The transcript `head_info` last read, so it is read once per session.
    head_of: PathBuf,
    head_title: Option<String>,
    /// The cq name of the last watcher seen in this folder.
    cq_last: Option<String>,
}

pub struct Scanner {
    src: Sources,
    sys: Option<sysinfo::System>,
    git: Git,
    tallies: HashMap<PathBuf, Tally>,
    folders: HashMap<String, Folder>,
}

fn mtime_of(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64)
}

/// The files in `dir` named `<prefix>*.jsonl`, with their mtimes.
fn jsonl_files(dir: &Path, prefix: &str) -> Vec<(PathBuf, i64)> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.starts_with(prefix) && n.ends_with(".jsonl")
        })
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            meta.is_file().then(|| (e.path(), mtime_of(&meta)))
        })
        .collect()
}

/// Local midnight at the start of the day that holds `now`.
pub fn local_midnight(now: i64) -> i64 {
    jiff::Timestamp::from_second(now)
        .ok()
        .and_then(|t| t.to_zoned(jiff::tz::TimeZone::system()).start_of_day().ok())
        .map_or(now - now.rem_euclid(86_400), |z| z.timestamp().as_second())
}

impl Scanner {
    fn new(src: Sources, git: Git) -> Scanner {
        let sys = src.processes.then(sysinfo::System::new);
        Scanner {
            src,
            sys,
            git,
            tallies: HashMap::new(),
            folders: HashMap::new(),
        }
    }

    pub fn scan(&mut self) -> Report {
        let now = now_epoch();
        self.scan_at(now, local_midnight(now))
    }

    pub fn scan_at(&mut self, now: i64, today: i64) -> Report {
        let procs = match self.sys.as_mut() {
            Some(sys) => read_processes(sys, &self.src.heartbeat),
            None => None,
        };
        let mut rows = Vec::new();
        let mut used = HashSet::new();
        for e in fs::read_dir(&self.src.projects)
            .into_iter()
            .flatten()
            .flatten()
        {
            if !e.path().is_dir() {
                continue;
            }
            let encoded = e.file_name().to_string_lossy().into_owned();
            let k = key(&encoded);
            let proc = procs.as_ref().and_then(|p| p.sessions.get(&k));
            let Some((main, written)) = jsonl_files(&e.path(), "").into_iter().max_by_key(|f| f.1)
            else {
                continue;
            };
            if proc.is_none() && now - written > SHOW_SECS {
                continue;
            }
            rows.push(self.row(
                &e.path(),
                &encoded,
                &main,
                written,
                now,
                today,
                procs.as_ref(),
                &mut used,
            ));
        }
        // Forget files that are no longer read, so the cache does not grow for ever.
        self.tallies.retain(|p, _| used.contains(p));
        rows.sort_by_key(|r| (r.state, std::cmp::Reverse(r.active)));
        Report {
            ready: true,
            machine: procs.map(|p| p.machine),
            rows,
            projects: self.src.projects.clone(),
            history: Vec::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn row(
        &mut self,
        dir: &Path,
        encoded: &str,
        main: &Path,
        written: i64,
        now: i64,
        today: i64,
        procs: Option<&Procs>,
        used: &mut HashSet<PathBuf>,
    ) -> Row {
        let k = key(encoded);
        let proc = procs.and_then(|p| p.sessions.get(&k));
        let session_dir = dir.join(main.file_stem().unwrap_or_default());

        // The real folder: a matching process's working folder, else the transcript's own.
        let folder = self.folders.entry(k.clone()).or_default();
        if let Some((path, _, _)) = proc {
            folder.path = Some(path.clone());
        }
        if folder.head_of != main {
            let (path, title) = head_info(main, encoded);
            folder.head_of = main.to_path_buf();
            folder.head_title = title;
            if folder.path.is_none() {
                folder.path = path;
            }
        }
        let path = folder.path.clone();
        let title = fs::read_to_string(session_dir.join("custom-title.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| v.get("customTitle")?.as_str().map(str::to_owned))
            .or_else(|| folder.head_title.clone());

        // Today's sums: the transcript and every subagent transcript written today.
        let tally = self.tallies.entry(main.to_path_buf()).or_default();
        tally.update(main, today);
        used.insert(main.to_path_buf());
        let latest = tally.latest.clone();
        let (mut tokens, mut cost, mut unpriced) = (tally.tokens, tally.cost, tally.unpriced);
        let subs = jsonl_files(&session_dir.join("subagents"), "agent-");
        for (sub, mtime) in &subs {
            if *mtime < today {
                continue;
            }
            let t = self.tallies.entry(sub.clone()).or_default();
            t.update(sub, today);
            used.insert(sub.clone());
            tokens += t.tokens;
            cost += t.cost;
            unpriced |= t.unpriced;
        }
        let active = subs.iter().map(|s| s.1).fold(written, i64::max);
        let cpu = proc.map(|p| p.1);

        let watcher = procs.and_then(|p| p.watchers.get(&k)).cloned();
        let seen = &mut self.folders.entry(k.clone()).or_default().cq_last;
        if watcher.is_some() {
            seen.clone_from(&watcher);
        }
        let cq_last = seen.clone();
        let label = path
            .as_deref()
            .map(folder_label)
            .unwrap_or_else(|| encoded.to_string());
        let last_change = path.and_then(|p| self.last_change(p));
        Row {
            name: display_name(watcher.as_deref(), title.as_deref(), &label),
            cq_name: watcher,
            cq_last,
            folder_key: k,
            state: state(now, active, cpu),
            model: latest
                .as_ref()
                .map(|l| short_model(&l.model))
                .unwrap_or_default(),
            context: latest.map(|l| l.context),
            tokens_today: tokens,
            cost_today: (!unpriced).then_some(cost),
            cpu,
            mem: proc.map(|p| p.2),
            subagents: subs.iter().filter(|s| now - s.1 <= SUB_RECENT_SECS).count(),
            last_change,
            active,
        }
    }

    fn last_change(&mut self, folder: PathBuf) -> Option<(Change, i64)> {
        match &mut self.git {
            #[cfg(test)]
            Git::Off => None,
            Git::Inline(done) => *done.entry(folder).or_insert_with_key(|f| last_change(f)),
            Git::Background(cache) => {
                let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
                c.entry(folder).or_default().result
            }
        }
    }
}

/// Collects session data on background threads, so a key press never waits on a scan.
pub struct Collector {
    latest: Arc<Mutex<Report>>,
}

impl Collector {
    pub fn start(src: Sources) -> Collector {
        let latest = Arc::new(Mutex::new(Report::default()));
        let cache = GitCache::default();
        let git_cache = cache.clone();
        thread::spawn(move || git_worker(git_cache));
        let out = latest.clone();
        thread::spawn(move || {
            let mut sc = Scanner::new(src, Git::Background(cache));
            sc.prime();
            let mut history = History::new(HISTORY);
            loop {
                let mut r = sc.scan();
                if let Some(m) = &r.machine {
                    history.push(m.sample());
                }
                r.history = history.to_vec();
                *out.lock().unwrap_or_else(|e| e.into_inner()) = r;
                thread::sleep(SCAN_EVERY);
            }
        });
        Collector { latest }
    }

    pub fn latest(&self) -> Report {
        self.latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl Scanner {
    /// A first process read, so the next one can measure CPU use over an interval.
    fn prime(&mut self) {
        if let Some(sys) = self.sys.as_mut() {
            let _ = read_processes(sys, &self.src.heartbeat);
            thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL.max(Duration::from_millis(500)));
        }
    }
}

/// One full scan for an export, with `git` run inline.
pub fn collect_once(src: Sources) -> Report {
    let mut sc = Scanner::new(src, Git::Inline(HashMap::new()));
    sc.prime();
    sc.scan()
}

/// A scanner that reads no processes and runs no `git`, for tests.
#[cfg(test)]
pub fn test_scanner(projects: &Path) -> Scanner {
    Scanner::new(
        Sources {
            projects: projects.to_path_buf(),
            heartbeat: PathBuf::new(),
            processes: false,
        },
        Git::Off,
    )
}

// ---------- figures for the screen ----------

/// `950`, `135k`, `66.3M`.
pub fn count(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{}k", n / 1_000),
        1_000_000..=999_999_999 => format!("{:.1}M", n as f64 / 1e6),
        _ => format!("{:.1}B", n as f64 / 1e9),
    }
}

/// `850M`, `1.2G`.
pub fn bytes(n: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let mb = n as f64 / MB;
    if mb < 1000.0 {
        format!("{mb:.0}M")
    } else {
        format!("{:.1}G", mb / 1024.0)
    }
}

/// `37.4 GB`, in GiB as Task Manager and `top` count them.
pub fn gb(n: u64) -> String {
    format!("{:.1} GB", n as f64 / f64::from(1u32 << 30))
}

/// `37.4 / 63.9 GB`.
pub fn of_gb(part: u64, total: u64) -> String {
    let g = |n: u64| n as f64 / f64::from(1u32 << 30);
    format!("{:.1} / {:.1} GB", g(part), g(total))
}

/// `$12.34`, or `?` when a model had no price.
pub fn usd(cost: Option<f64>) -> String {
    cost.map_or("?".into(), |c| format!("${c:.2}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// 2026-10-28T00:00:00Z, standing in for local midnight.
    const TODAY: i64 = 1_793_145_600;

    fn assistant_line(
        id: &str,
        ts: &str,
        model: &str,
        input: u64,
        write: u64,
        read: u64,
        out: u64,
    ) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"{ts}","requestId":"req_{id}","message":{{"id":"{id}","model":"{model}","content":[{{"type":"text","text":"SECRET-TEXT"}}],"usage":{{"input_tokens":{input},"cache_creation_input_tokens":{write},"cache_read_input_tokens":{read},"output_tokens":{out},"cache_creation":{{"ephemeral_1h_input_tokens":{write},"ephemeral_5m_input_tokens":0}}}}}}}}"#
        ) + "\n"
    }

    /// Sets a file's mtime, so tests do not depend on the clock.
    fn touch(path: &Path, epoch: i64) {
        let f = fs::File::options().write(true).open(path).unwrap();
        f.set_modified(UNIX_EPOCH + Duration::from_secs(epoch as u64))
            .unwrap();
    }

    fn append(path: &Path, text: &str) {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }

    #[test]
    fn folders_encode_like_claude_code() {
        assert_eq!(
            encode(Path::new(
                r"D:\Code\photo-projects\placefix\.claude\worktrees\placefix-bugs-queue-0646a8\"
            )),
            "D--Code-photo-projects-placefix--claude-worktrees-placefix-bugs-queue-0646a8"
        );
        assert_eq!(
            encode(Path::new("/home/me/my_app.v2/")),
            "-home-me-my-app-v2"
        );
        assert!(same_key(
            "D--Code",
            if cfg!(windows) { "d--code" } else { "D--Code" }
        ));
    }

    #[test]
    fn names_follow_watcher_then_title_then_folder() {
        assert_eq!(
            display_name(Some("placefix-dev"), Some("RC: Placefix"), "x"),
            "placefix-dev"
        );
        assert_eq!(
            display_name(None, Some("RC: Session Messaging"), "x"),
            "Session Messaging"
        );
        assert_eq!(display_name(None, Some("Plain title"), "x"), "Plain title");
        assert_eq!(display_name(Some(""), Some("  "), "repo/wt"), "repo/wt");
        let wt = Path::new("D:/Code/photo-projects/placefix/.claude/worktrees/bugs-0646a8");
        assert_eq!(folder_label(wt), "placefix/bugs-0646a8");
        assert_eq!(
            folder_label(Path::new("D:/Code/raw-salvage")),
            "raw-salvage"
        );
        assert_eq!(
            watch_name(&["cq".into(), "watch".into(), "--as".into(), "w1".into()]),
            Some("w1".into())
        );
        assert_eq!(
            watch_name(&["cq".into(), "watch".into(), "--as=w2".into()]),
            Some("w2".into())
        );
        assert_eq!(
            watch_name(&["cq".into(), "send".into(), "--as".into(), "w1".into()]),
            None
        );
    }

    #[test]
    fn a_watcher_belongs_to_the_session_above_it() {
        use sysinfo::Pid;
        let mut table = HashMap::new();
        let mut add = |pid: u32, parent: Option<u32>, session: bool, cwd: Option<&str>| {
            let info = ProcInfo {
                parent: parent.map(Pid::from_u32),
                session,
                cwd: cwd.map(PathBuf::from),
            };
            table.insert(Pid::from_u32(pid), info);
        };
        // The app, a session under it, three shells, then `cq watch` in another folder.
        add(1, None, true, Some("C:/Apps/Claude"));
        add(
            2,
            Some(1),
            true,
            Some("D:/Code/tools/.claude/worktrees/wt-1"),
        );
        add(3, Some(2), false, Some("D:/Code/tools"));
        add(4, Some(3), false, None);
        add(5, Some(4), false, None);
        add(10, Some(5), false, Some("D:/Code/queue"));
        // A session without a readable folder is skipped for the one above it.
        add(20, None, true, Some("D:/Code/app"));
        add(21, Some(20), true, None);
        add(22, Some(21), false, Some("D:/Code/elsewhere"));
        // No session above: the watcher's own folder.
        add(30, None, false, Some("D:/Code/solo"));
        // Too far from its session: more than ANCESTRY_STEPS shells.
        add(40, None, true, Some("D:/Code/far"));
        for pid in 41..41 + ANCESTRY_STEPS as u32 {
            add(pid, Some(pid - 1), false, None);
        }
        let deep = 41 + ANCESTRY_STEPS as u32;
        add(deep, Some(deep - 1), false, Some("D:/Code/deep"));
        // A parent loop ends.
        add(80, Some(81), false, None);
        add(81, Some(80), false, None);
        add(82, Some(81), false, Some("D:/Code/loop"));

        let names = [
            (10, "queue-admin"),
            (22, "w1"),
            (30, "solo"),
            (deep, "deep"),
            (82, "l"),
        ];
        let mut got = match_watchers(
            &table,
            names
                .iter()
                .map(|(p, n)| (Pid::from_u32(*p), n.to_string())),
        )
        .into_iter()
        .collect::<Vec<_>>();
        got.sort();
        let want = [
            ("D:/Code/app", "w1"),
            ("D:/Code/deep", "deep"),
            ("D:/Code/loop", "l"),
            ("D:/Code/solo", "solo"),
            ("D:/Code/tools/.claude/worktrees/wt-1", "queue-admin"),
        ];
        let mut want = want
            .iter()
            .map(|(d, n)| (key(&encode(Path::new(d))), n.to_string()))
            .collect::<Vec<_>>();
        want.sort();
        assert_eq!(got, want);

        // The first name for a folder wins: the arguments come before heartbeat files.
        let two = [(10, "from-args"), (10, "from-heartbeat")];
        let got = match_watchers(&table, two.map(|(p, n)| (Pid::from_u32(p), n.to_string())));
        assert_eq!(got.values().collect::<Vec<_>>(), ["from-args"]);
    }

    #[test]
    fn the_state_rule() {
        let now = 10_000;
        assert_eq!(state(now, now - 60, None), State::Working);
        assert_eq!(state(now, now - WORKING_SECS, Some(0.0)), State::Working);
        assert_eq!(state(now, now - 600, Some(1.0)), State::Recent);
        assert_eq!(state(now, now - RECENT_SECS, None), State::Recent);
        assert_eq!(state(now, now - RECENT_SECS - 1, None), State::Idle);
        // A busy process tree makes an old transcript "working".
        assert_eq!(
            state(now, now - 5 * 3600, Some(WORKING_CPU)),
            State::Working
        );
        assert_eq!(state(now, now - 5 * 3600, Some(4.9)), State::Idle);
        // Only idle sessions with cq messages warn.
        assert!(warn(State::Idle, 1, 0));
        assert!(warn(State::Idle, 0, 2));
        assert!(!warn(State::Idle, 0, 0));
        assert!(!warn(State::Recent, 3, 1));
        assert!(!warn(State::Working, 3, 1));
        // Working sorts first, then recent, then idle.
        assert!(State::Working < State::Recent && State::Recent < State::Idle);
    }

    #[test]
    fn models_are_shortened_and_priced() {
        assert_eq!(short_model("claude-opus-5-5"), "opus-5-5");
        assert_eq!(short_model("claude-haiku-4-5-20251001"), "haiku-4-5");
        assert_eq!(short_model("claude-sonnet-4-5[1m]"), "sonnet-4-5");
        assert_eq!(
            price("claude-opus-5-5", 0),
            Some(p(4.0, 5.0, 8.0, 0.20, 20.0))
        );
        assert_eq!(price("claude-haiku-5-5", 100_000), Some(HAIKU_5_5_SMALL));
        assert_eq!(price("claude-haiku-5-5", 100_001), Some(HAIKU_5_5_LARGE));
        assert_eq!(price("gpt-9", 0), None);
        // 1M of each at Opus 5.5: 4 + 20 + 8 (1 h writes) + 0.20.
        let u = Usage {
            input_tokens: Some(1_000_000),
            output_tokens: Some(1_000_000),
            cache_creation_input_tokens: Some(1_000_000),
            cache_read_input_tokens: Some(1_000_000),
            cache_creation: Some(CacheCreation {
                ephemeral_1h_input_tokens: Some(1_000_000),
            }),
        };
        assert!((u.cost(price("claude-opus-5-5", 0).unwrap()) - 32.2).abs() < 1e-9);
        // Without the split, writes count as 5-minute writes.
        let u = Usage {
            cache_creation: None,
            ..u
        };
        assert!((u.cost(price("claude-opus-5-5", 0).unwrap()) - 29.2).abs() < 1e-9);
    }

    #[test]
    fn tally_dedupes_skips_synthetic_and_starts_at_midnight() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("s.jsonl");
        let mut text = String::new();
        // Yesterday: not counted, but still the latest model until a newer line.
        text += &assistant_line(
            "m0",
            "2026-10-27T23:59:59.999Z",
            "claude-sonnet-4-5",
            5,
            5,
            5,
            5,
        );
        // One response written as three lines with the same message id.
        for _ in 0..3 {
            text += &assistant_line(
                "m1",
                "2026-10-28T00:00:00.100Z",
                "claude-opus-5-5",
                2,
                1000,
                100_000,
                500,
            );
        }
        text += r#"{"type":"user","timestamp":"2026-10-28T00:01:00.000Z","message":{"content":"hello"}}"#;
        text += "\n";
        text += &assistant_line("s1", "2026-10-28T00:02:00.000Z", "<synthetic>", 0, 0, 0, 0);
        fs::write(&f, &text).unwrap();
        touch(&f, TODAY + 3600);

        let mut t = Tally::default();
        t.update(&f, TODAY);
        assert_eq!(t.tokens, 2 + 1000 + 100_000 + 500);
        let want = (2.0 * 4.0 + 500.0 * 20.0 + 1000.0 * 8.0 + 100_000.0 * 0.2) / 1e6;
        assert!((t.cost - want).abs() < 1e-12);
        assert!(!t.unpriced);
        assert_eq!(
            t.latest,
            Some(Latest {
                model: "claude-opus-5-5".into(),
                context: 101_002
            })
        );

        // Appended lines are read on the next update, a half-written one only once whole.
        let line = assistant_line(
            "m2",
            "2026-10-28T09:00:00Z",
            "claude-mystery-1",
            10,
            0,
            0,
            10,
        );
        let (a, b) = line.split_at(40);
        append(&f, a);
        touch(&f, TODAY + 9 * 3600);
        t.update(&f, TODAY);
        assert_eq!(t.tokens, 101_502);
        append(&f, b);
        touch(&f, TODAY + 9 * 3600);
        t.update(&f, TODAY);
        assert_eq!(t.tokens, 101_522);
        // An unknown model makes the cost unknown.
        assert!(t.unpriced);
        assert_eq!(t.latest.as_ref().unwrap().context, 10);

        // A new day starts the sums again without re-reading old lines.
        t.update(&f, TODAY + 86_400);
        assert_eq!((t.tokens, t.unpriced), (0, false));

        // A file that shrank is read again from the start.
        fs::write(
            &f,
            assistant_line("m9", "2026-10-29T08:00:00Z", "claude-opus-5-5", 1, 0, 0, 1),
        )
        .unwrap();
        touch(&f, TODAY + 86_400 + 8 * 3600);
        t.update(&f, TODAY + 86_400);
        assert_eq!(t.tokens, 2);
    }

    #[test]
    fn an_old_file_is_read_from_its_tail() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("old.jsonl");
        let mut text = assistant_line("a", "2026-10-20T10:00:00Z", "claude-opus-4-5", 1, 2, 3, 4);
        // Enough later lines that the first one is outside the first tail read.
        let filler = format!(
            "{{\"type\":\"user\",\"message\":{{\"content\":\"{}\"}}}}\n",
            "x".repeat(1000)
        );
        for _ in 0..400 {
            text += &filler;
        }
        fs::write(&f, &text).unwrap();
        // Last written three days ago.
        touch(&f, TODAY - 3 * 86_400);
        let mut t = Tally::default();
        t.update(&f, TODAY);
        assert_eq!(t.tokens, 0);
        assert_eq!(
            t.latest,
            Some(Latest {
                model: "claude-opus-4-5".into(),
                context: 6
            })
        );
    }

    /// A projects folder with one session and one subagent.
    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let folder = PathBuf::from("D:/Code/demo/.claude/worktrees/wt-1");
        let proj = dir.path().join(encode(&folder));
        let sid = "1111-2222";
        fs::create_dir_all(proj.join(sid).join("subagents")).unwrap();
        let cwd = folder.to_string_lossy();
        let mut text = format!(
            "{{\"type\":\"custom-title\",\"customTitle\":\"RC: Head title\"}}\n\
             {{\"type\":\"user\",\"cwd\":\"{cwd}\",\"message\":{{\"content\":\"hi\"}}}}\n"
        );
        text += &assistant_line(
            "m1",
            "2026-10-28T08:00:00Z",
            "claude-opus-5-5",
            1,
            0,
            99,
            10,
        );
        fs::write(proj.join(format!("{sid}.jsonl")), text).unwrap();
        fs::write(
            proj.join(sid).join("subagents").join("agent-a1.jsonl"),
            assistant_line(
                "x1",
                "2026-10-28T08:01:00Z",
                "claude-haiku-4-5",
                100,
                0,
                0,
                100,
            ),
        )
        .unwrap();
        // Not a transcript.
        fs::write(
            proj.join(sid).join("subagents").join("agent-a1.meta.json"),
            "{}",
        )
        .unwrap();
        (dir, proj)
    }

    #[test]
    fn a_scan_sums_the_session_and_its_subagents() {
        let (dir, proj) = fixture();
        let now = now_epoch();
        let mut sc = test_scanner(dir.path());
        // Files were just written, so "today" can be any time before now.
        let r = sc.scan_at(now, TODAY);
        assert!(r.ready && r.machine.is_none());
        assert_eq!(r.rows.len(), 1);
        let row = &r.rows[0];
        assert_eq!(row.name, "Head title");
        assert_eq!(row.cq_name, None);
        assert_eq!(row.state, State::Working);
        assert_eq!(row.model, "opus-5-5");
        assert_eq!(row.context, Some(100));
        assert_eq!(row.tokens_today, 110 + 200);
        assert_eq!(row.subagents, 1);
        assert_eq!((row.cpu, row.mem), (None, None));
        let want = (4.0 + 99.0 * 0.2 + 10.0 * 20.0 + 100.0 * 1.0 + 100.0 * 5.0) / 1e6;
        assert!((row.cost_today.unwrap() - want).abs() < 1e-12);

        // The title file wins over the transcript's own title line.
        fs::write(
            proj.join("1111-2222").join("custom-title.json"),
            r#"{"customTitle":"RC: File title"}"#,
        )
        .unwrap();
        assert_eq!(sc.scan_at(now, TODAY).rows[0].name, "File title");

        // Without a title, the folder label; and a day later, idle and hidden.
        fs::remove_file(proj.join("1111-2222").join("custom-title.json")).unwrap();
        let text = fs::read_to_string(proj.join("1111-2222.jsonl")).unwrap();
        fs::write(
            proj.join("1111-2222.jsonl"),
            text.lines().skip(1).collect::<Vec<_>>().join("\n") + "\n",
        )
        .unwrap();
        let mut sc = test_scanner(dir.path());
        assert_eq!(sc.scan_at(now, TODAY).rows[0].name, "demo/wt-1");
        assert_eq!(
            sc.scan_at(now + RECENT_SECS + 60, TODAY).rows[0].state,
            State::Idle
        );
        assert!(sc.scan_at(now + SHOW_SECS + 60, TODAY).rows.is_empty());
    }

    #[test]
    fn last_change_is_the_worktrees_own() {
        let dir = tempfile::tempdir().unwrap();
        let (main, wt) = (dir.path().join("main"), dir.path().join("wt"));
        fs::create_dir(&main).unwrap();
        let git = |cwd: &Path, at: i64, args: &[&str]| {
            let date = format!("@{at} +0000");
            Command::new("git")
                .current_dir(cwd)
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .env("GIT_AUTHOR_DATE", &date)
                .env("GIT_COMMITTER_DATE", &date)
                .output()
                .is_ok_and(|o| o.status.success())
        };
        if !git(&main, 0, &["init", "-q", "-b", "main"]) {
            eprintln!("no git: skipped");
            return;
        }
        fs::write(main.join("a.txt"), "a").unwrap();
        assert!(git(&main, 0, &["add", "a.txt"]));
        assert!(git(&main, 1_700_000_000, &["commit", "-q", "-m", "a"]));
        let wt_arg = wt.to_string_lossy().into_owned();
        assert!(git(
            &main,
            0,
            &["worktree", "add", "-q", "-b", "b", &wt_arg]
        ));
        assert!(git(
            &wt,
            1_700_000_500,
            &["commit", "-q", "--allow-empty", "-m", "b"]
        ));

        // The newer commit on the other worktree's branch does not count here.
        assert_eq!(last_change(&main), Some((Change::Commit, 1_700_000_000)));
        assert_eq!(last_change(&wt), Some((Change::Commit, 1_700_000_500)));
        // An uncommitted edit newer than the commit wins, in its own worktree only.
        fs::write(main.join("a.txt"), "edited").unwrap();
        touch(&main.join("a.txt"), 1_700_000_900);
        assert_eq!(last_change(&main), Some((Change::Edit, 1_700_000_900)));
        assert_eq!(last_change(&wt), Some((Change::Commit, 1_700_000_500)));
    }

    #[test]
    fn history_keeps_the_newest_samples_in_order() {
        let at = |n: f32| Sample {
            cpu: n,
            ..Sample::default()
        };
        let mut h = History::new(3);
        assert!(h.to_vec().is_empty());
        h.push(at(1.0));
        h.push(at(2.0));
        assert_eq!(h.to_vec(), [at(1.0), at(2.0)]);
        h.push(at(3.0));
        h.push(at(4.0));
        h.push(at(5.0));
        assert_eq!(h.to_vec(), [at(3.0), at(4.0), at(5.0)]);
        // A capacity of 0 still keeps the newest sample.
        let mut one = History::new(0);
        one.push(at(1.0));
        one.push(at(2.0));
        assert_eq!(one.to_vec(), [at(2.0)]);
    }

    #[test]
    fn machine_figures_are_short_and_share_one_scale() {
        let m = Machine {
            cpu: 12.84,
            cores: 32,
            busy_cores: 3,
            mem_used: 40_157_247_078,  // 37.4 GiB
            mem_total: 68_611_850_240, // 63.9 GiB
            mem_available: 28_454_603_162,
            swap_used: 0,
            swap_total: 4 << 30,
            claude_cpu: 160.0,
            claude_mem: 7 << 30,
            claude_procs: 26,
        };
        let cpu = m.cpu_figures().map(|(l, v)| format!("{l} {v}"));
        assert_eq!(
            cpu,
            ["Total 12.8%", "Claude 5.0%", "Busy cores 3", "Cores 32"]
        );
        let mem = m.mem_figures().map(|(l, v)| format!("{l} {v}"));
        assert_eq!(
            mem,
            [
                "Used 37.4 / 63.9 GB",
                "Available 26.5 GB",
                "Swap 0.0 / 4.0 GB",
                "Claude 7.0 GB"
            ]
        );
        // 160 % of one core on 32 cores is 5 % of the computer.
        let s = m.sample();
        assert!((s.claude_cpu - 5.0).abs() < 1e-4);
        assert!((s.mem - 58.53).abs() < 0.01 && (s.claude_mem - 10.95).abs() < 0.01);
        // Nothing known gives zeros, not NaN.
        assert_eq!(Machine::default().sample(), Sample::default());
    }

    #[test]
    fn figures_are_short() {
        assert_eq!(count(950), "950");
        assert_eq!(count(135_312), "135k");
        assert_eq!(count(66_300_000), "66.3M");
        assert_eq!(bytes(850 * 1024 * 1024), "850M");
        assert_eq!(bytes(1288 * 1024 * 1024), "1.3G");
        assert_eq!(usd(Some(12.346)), "$12.35");
        assert_eq!(usd(None), "?");
    }
}
