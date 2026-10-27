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

use ratatui::{
    DefaultTerminal,
    crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
};
use serde_json::Value;

mod draw;
use draw::{THEMES, draw};

/// Watchers write a heartbeat every 30 s; three missed beats means the watcher is gone.
const STALE_AFTER: i64 = 90;
const RECENT_LIMIT: usize = 40;
/// The Flow screen covers the last hour.
const WINDOW_SECS: i64 = 3600;
/// Exit code for any other error, as in `docs/COMMANDS.md`.
const EXIT_ERROR: i32 = 70;

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
}

struct Watcher {
    name: String,
    beat_age: Option<i64>,
    pid: String,
    session: String,
    queued: usize,
    claimed: usize,
    done: usize,
    rejected: usize,
    dead: usize,
}

struct Snapshot {
    now: i64,
    paused: bool,
    watchers: Vec<Watcher>,
    in_flight: Vec<Msg>,
    recent: Vec<Msg>,
    /// Every message sent in the last hour (finished and in flight), for the Flow screen.
    window: Vec<Msg>,
}

fn main() -> std::io::Result<()> {
    if env::args().skip(1).any(|a| a == "-h" || a == "--help") {
        println!(
            "cq-top: live, read-only view of a cq queue.\n\
             The queue root is CQ_ROOT, or cq's per-user default.\n\
             Keys: q quit, b bodies, tab or 1/2 switch screen, t next theme."
        );
        return Ok(());
    }
    let root = match cq::Root::open() {
        Ok(root) => root,
        Err(e) => {
            eprintln!("cq-top: {e}");
            std::process::exit(EXIT_ERROR);
        }
    };
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &root);
    ratatui::restore();
    result
}

fn run(terminal: &mut DefaultTerminal, root: &cq::Root) -> std::io::Result<()> {
    let mut show_bodies = false;
    let (mut flow, mut theme) = load_view();
    loop {
        let snap = snapshot(root);
        terminal.draw(|f| draw(f, root.path(), &snap, show_bodies, flow, &THEMES[theme]))?;
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
                KeyCode::Tab => flow = !flow,
                KeyCode::Char('1') => flow = false,
                KeyCode::Char('2') => flow = true,
                _ => continue,
            }
            save_view(flow, theme);
        }
    }
}

// ---------- the remembered view ----------

/// The last screen and theme are kept in `<config dir>/cq-top/state` as `<flow|overview> <theme>`.
/// The queue root is never written. With no saved choice: Overview screen, first theme (classic).
fn view_path() -> Option<PathBuf> {
    let var = |k: &str| env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let dir = var("XDG_CONFIG_HOME")
        .or_else(|| var("HOME").map(|h| h.join(".config")))
        .or_else(|| var("APPDATA"))?;
    Some(dir.join("cq-top").join("state"))
}

fn parse_view(text: &str) -> (bool, usize) {
    let mut words = text.split_whitespace();
    let flow = words.next() == Some("flow");
    let theme = words
        .next()
        .and_then(|n| THEMES.iter().position(|t| t.name == n))
        .unwrap_or(0);
    (flow, theme)
}

fn format_view(flow: bool, theme: usize) -> String {
    format!(
        "{} {}\n",
        if flow { "flow" } else { "overview" },
        THEMES[theme].name
    )
}

fn load_view() -> (bool, usize) {
    let text = view_path()
        .and_then(|p| fs::read_to_string(p).ok())
        .unwrap_or_default();
    parse_view(&text)
}

fn save_view(flow: bool, theme: usize) {
    let Some(path) = view_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    // LAZY: a failed save is ignored; the view just is not remembered next run.
    let _ = fs::write(path, format_view(flow, theme));
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

fn json_files(p: &Path) -> Vec<(PathBuf, i64)> {
    fs::read_dir(p)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
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
    })
}

fn snapshot(root: &cq::Root) -> Snapshot {
    let now = now_epoch();

    // name -> Claude Code session key, from members/<key> (line 1 is the name).
    let mut sessions: BTreeMap<String, String> = BTreeMap::new();
    for e in fs::read_dir(root.members()).into_iter().flatten().flatten() {
        if let Ok(t) = fs::read_to_string(e.path())
            && let Some(name) = t.lines().next()
        {
            sessions.insert(
                name.trim().to_string(),
                e.file_name().to_string_lossy().into_owned(),
            );
        }
    }

    let mut names: BTreeSet<String> = sessions.keys().cloned().collect();
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

        let beat_age = fs::read_to_string(root.heartbeat(name))
            .ok()
            .and_then(|t| parse_ts(t.trim()))
            .map(|t| now - t);
        // LAZY: liveness is heartbeat age only; the .pid file is shown, not probed.
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
    window.extend(in_flight.iter().cloned());
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
        for flow in [false, true] {
            for theme in 0..THEMES.len() {
                assert_eq!(parse_view(&format_view(flow, theme)), (flow, theme));
            }
        }
        assert_eq!(parse_view(""), (false, 0));
        assert_eq!(parse_view("flow no-such-theme"), (true, 0));
        assert_eq!(parse_view("garbage"), (false, 0));
    }

    #[test]
    fn snapshot_reads_a_queue_without_changing_it() {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: this is the only test that touches the environment.
        unsafe { env::set_var("CQ_ROOT", dir.path()) };
        let root = cq::Root::from_env().unwrap();
        root.init().unwrap();
        root.ensure_session_dirs("w1").unwrap();

        let msg = |id: &str, kind: &str| {
            format!(
                r#"{{"id":"{id}","from":"lead","to":"w1","type":"{kind}","subject":"s","body":"secret"}}"#
            )
        };
        fs::write(
            root.inbox("w1").join("20261027T112500Z-lead-0001.json"),
            msg("20261027T112500Z-lead-0001", "task"),
        )
        .unwrap();
        fs::write(
            root.processing("w1")
                .join("20261027T112501Z-lead-0002.json"),
            msg("20261027T112501Z-lead-0002", "info"),
        )
        .unwrap();
        fs::write(
            root.done("w1").join("20261027T112502Z-lead-0003.json"),
            msg("20261027T112502Z-lead-0003", "task"),
        )
        .unwrap();
        fs::write(
            root.dlq("w1").join("20261027T112503Z-lead-0004.json"),
            "not json",
        )
        .unwrap();
        fs::write(root.heartbeat("w1"), "2026-10-27T11:26:32Z\n").unwrap();

        let s = snapshot(&root);
        assert!(!s.paused);
        let w = s.watchers.iter().find(|w| w.name == "w1").unwrap();
        assert_eq!((w.queued, w.claimed, w.done, w.dead), (1, 1, 1, 1));
        assert!(w.beat_age.is_some());
        let buckets: Vec<_> = s.in_flight.iter().map(|m| m.bucket).collect();
        assert_eq!(buckets, ["queued", "claimed"]);
        assert_eq!(s.recent.len(), 1);

        fs::write(root.pause(), "").unwrap();
        assert!(snapshot(&root).paused);
        // The snapshot left every message where it was.
        assert_eq!(json_files(&root.inbox("w1")).len(), 1);
        assert_eq!(json_files(&root.processing("w1")).len(), 1);
    }

    #[test]
    fn every_theme_draws_both_screens() {
        let msg = |bucket: &'static str, kind: &str| Msg {
            ts: 1_793_100_000,
            from: "lead".into(),
            to: "w1".into(),
            kind: kind.into(),
            subject: "Scan lane".into(),
            status: "ok".into(),
            reply_to: "20261027T111421Z-lead-6256".into(),
            body: "the body".into(),
            bucket,
        };
        let snap = Snapshot {
            now: 1_793_100_400,
            paused: false,
            watchers: vec![Watcher {
                name: "w1".into(),
                beat_age: Some(10),
                pid: "42".into(),
                session: "abcd1234".into(),
                queued: 1,
                claimed: 1,
                done: 2,
                rejected: 0,
                dead: 0,
            }],
            in_flight: vec![msg("queued", "task"), msg("claimed", "task")],
            recent: vec![msg("done", "result")],
            window: vec![msg("done", "result"), msg("queued", "task")],
        };
        for theme in &THEMES {
            for flow in [false, true] {
                let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
                term.draw(|f| draw(f, Path::new("/queue"), &snap, false, flow, theme))
                    .unwrap();
                let text: String = term
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|c| c.symbol())
                    .collect();
                assert!(text.contains("cq-top"), "{} flow={flow}", theme.name);
                assert!(text.contains(if flow { "Turnaround" } else { "Watchers" }));
            }
        }
    }
}
