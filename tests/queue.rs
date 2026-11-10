//! Send, claim, done and list: spec tests T1 to T4, T7, T8 and T11 to T13,
//! plus the data-safety and schema tests.

mod common;

use std::fs;
use std::process::{Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, SystemTime};

use common::*;
use serde_json::{Value, json};

/// T1: a reader polling the inbox never sees a partial file.
#[test]
fn t1_reader_never_sees_a_partial_file() {
    let env = Env::new();
    fs::create_dir_all(env.path("inbox/b")).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let inbox = env.path("inbox/b");
    let reader = {
        let stop = stop.clone();
        thread::spawn(move || {
            let (mut seen, mut bad) = (0usize, Vec::new());
            while !stop.load(Ordering::Relaxed) {
                for e in fs::read_dir(&inbox).unwrap().filter_map(|e| e.ok()) {
                    let p = e.path();
                    if p.extension().is_none_or(|x| x != "json") {
                        continue;
                    }
                    // A sharing violation or a vanished file is not a partial file.
                    let Ok(bytes) = fs::read(&p) else { continue };
                    seen += 1;
                    if serde_json::from_slice::<Value>(&bytes).is_err() {
                        bad.push(p);
                    }
                }
            }
            (seen, bad)
        })
    };
    let body = "x".repeat(15_000);
    for sender in ["a1", "a2", "a3"] {
        for _ in 0..10 {
            env.send(sender, "b", "info", "t1", &body);
        }
    }
    stop.store(true, Ordering::Relaxed);
    let (seen, bad) = reader.join().unwrap();
    assert!(bad.is_empty(), "partial files seen: {bad:?}");
    assert!(seen > 0);
    assert_eq!(env.ids("inbox/b").len(), 30);
    assert!(env.ids("tmp").is_empty(), "no temp files left behind");
}

/// T2: two claims of one file at once; exactly one succeeds.
#[test]
fn t2_two_claims_exactly_one_wins() {
    let env = Env::new();
    for _ in 0..8 {
        let id = env.send("a", "b", "task", "race", "body");
        let spawn = || {
            env.cmd(&["claim", "--as", "b", &id])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap()
        };
        let (mut c1, mut c2) = (spawn(), spawn());
        let mut codes = [
            c1.wait().unwrap().code().unwrap(),
            c2.wait().unwrap().code().unwrap(),
        ];
        codes.sort();
        assert_eq!(codes, [0, 1], "one claim wins, the other exits 1");
    }
}

/// T3: a send whose parent is at the hop limit is refused.
#[test]
fn t3_hop_limit() {
    let env = Env::new();
    // a has claimed a message with 4 hops; a reply would have 5.
    let deep = fake_id("x", "0004");
    env.plant(
        "a",
        &deep,
        v1(&deep, "x", "a", "task", 4).to_string().as_bytes(),
    );
    assert_eq!(code(&env.claim("a", &deep)), 0);
    let out = env.send_raw(
        &[
            "--from", "a", "--to", "b", "--type", "task", "--parent", &deep,
        ],
        "s",
        "b",
    );
    assert_eq!(code(&out), 5, "{}", stderr(&out));
    assert!(stderr(&out).contains("hop limit"));

    // 3 hops is fine and gives 4.
    let ok = fake_id("x", "0003");
    env.plant(
        "a",
        &ok,
        v1(&ok, "x", "a", "task", 3).to_string().as_bytes(),
    );
    assert_eq!(code(&env.claim("a", &ok)), 0);
    let out = env.send_raw(
        &[
            "--from", "a", "--to", "b", "--type", "task", "--parent", &ok,
        ],
        "s",
        "b",
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let id = stdout(&out).trim().to_owned();
    assert_eq!(env.read_json(&format!("inbox/b/{id}.json"))["hops"], 4);

    // A parent that a has not claimed is refused.
    let missing = fake_id("x", "0009");
    let out = env.send_raw(
        &[
            "--from",
            "a",
            "--to",
            "b",
            "--type",
            "info",
            "--reply-to",
            &missing,
        ],
        "s",
        "b",
    );
    assert_eq!(code(&out), 5);

    // No parent: 0 hops.
    let id = env.send("a", "b", "task", "s", "b");
    assert_eq!(env.read_json(&format!("inbox/b/{id}.json"))["hops"], 0);
}

/// T4: the 21st message in an hour between one pair is refused, counting
/// messages that have moved on to `done/` and `rejected/`.
#[test]
fn t4_rate_limit() {
    let env = Env::new();
    for i in 0..4 {
        let id = env.send("a", "b", "info", "r", "b");
        assert_eq!(code(&env.claim("b", &id)), 0);
        let mut args = vec!["done", "--as", "b", id.as_str()];
        let input = if i % 2 == 0 {
            None
        } else {
            args.push("--reject");
            Some(r#"{"reason":"no"}"#)
        };
        let out = env.run(&args, input);
        assert_eq!(code(&out), 0, "{}", stderr(&out));
    }
    for _ in 4..20 {
        env.send("a", "b", "info", "r", "b");
    }
    let out = env.send_raw(&["--from", "a", "--to", "b", "--type", "info"], "r", "b");
    assert_eq!(code(&out), 5, "{}", stderr(&out));
    assert!(stderr(&out).contains("rate limit"));
    // Another pair is not affected.
    env.send("c", "b", "info", "r", "b");
    env.send("a", "d", "info", "r", "b");
}

/// The 41st message in an hour to one recipient is refused, though each
/// sender is under the pair limit. Messages that moved on to `.processing/`,
/// `done/`, `rejected/` and `dlq/` still count.
#[test]
fn recipient_limit() {
    let env = Env::new();
    let mut sent = Vec::new();
    for s in ["s0", "s1", "s2", "s3"] {
        for _ in 0..10 {
            sent.push(env.send(s, "b", "info", "r", "b"));
        }
    }
    assert_eq!(code(&env.claim("b", &sent[0])), 0);
    assert_eq!(code(&env.claim("b", &sent[1])), 0);
    let out = env.run(&["done", "--as", "b", &sent[1]], None);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(code(&env.claim("b", &sent[2])), 0);
    let out = env.run(
        &["done", "--as", "b", &sent[2], "--reject"],
        Some(r#"{"reason":"no"}"#),
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    fs::create_dir_all(env.path("dlq/b")).unwrap();
    fs::rename(
        env.path(&format!("inbox/b/{}.json", sent[3])),
        env.path(&format!("dlq/b/{}.json", sent[3])),
    )
    .unwrap();

    let out = env.send_raw(&["--from", "s4", "--to", "b", "--type", "info"], "r", "b");
    assert_eq!(code(&out), 5, "{}", stderr(&out));
    assert!(stderr(&out).contains("recipient limit"), "{}", stderr(&out));
    assert!(stderr(&out).contains("limit 40"), "{}", stderr(&out));
    assert_eq!(env.ids("inbox/b").len(), 36, "nothing written");
    // Another recipient is not affected.
    env.send("s4", "c", "info", "r", "b");
}

/// The 61st message in an hour from one sender is refused, though each
/// recipient is under the pair and recipient limits.
#[test]
fn sender_limit() {
    let env = Env::new();
    let mut first = None;
    for r in ["r0", "r1", "r2", "r3"] {
        for _ in 0..15 {
            let id = env.send("a", r, "info", "s", "b");
            first.get_or_insert((r, id));
        }
    }
    // Messages that moved on to done/ still count.
    let (r, id) = first.unwrap();
    assert_eq!(code(&env.claim(r, &id)), 0);
    let out = env.run(&["done", "--as", r, &id], None);
    assert_eq!(code(&out), 0, "{}", stderr(&out));

    let out = env.send_raw(&["--from", "a", "--to", "r4", "--type", "info"], "s", "b");
    assert_eq!(code(&out), 5, "{}", stderr(&out));
    assert!(stderr(&out).contains("sender limit"), "{}", stderr(&out));
    assert!(stderr(&out).contains("limit 60"), "{}", stderr(&out));
    assert!(env.ids("inbox/r4").is_empty(), "nothing written");
    // Another sender is not affected.
    env.send("b", "r4", "info", "s", "b");
}

/// Messages created more than an hour ago count toward none of the limits.
#[test]
fn old_messages_do_not_count() {
    let env = Env::new();
    let old = |from: &str, n: usize| format!("20200101T000000Z-{from}-{n:04x}");
    // 40 old messages to b, from a among others, in every directory.
    for n in 0..40 {
        let from = if n % 2 == 0 { "a" } else { "x" };
        let id = old(from, n);
        let dir = [
            "inbox/b",
            "inbox/b/.processing",
            "done/b",
            "rejected/b",
            "dlq/b",
        ][n % 5];
        fs::create_dir_all(env.path(dir)).unwrap();
        fs::write(env.path(&format!("{dir}/{id}.json")), b"{}").unwrap();
    }
    // 60 more old messages from a, to other recipients.
    for n in 40..100 {
        env.plant(&format!("r{}", n % 3), &old("a", n), b"{}");
    }
    env.send("a", "b", "info", "s", "b");
}

/// While `PAUSE` exists, `cq send` refuses (exit 5) and writes nothing.
#[test]
fn pause_blocks_send() {
    let env = Env::new();
    fs::write(env.path("PAUSE"), b"").unwrap();
    let out = env.send_raw(&["--from", "a", "--to", "b", "--type", "info"], "s", "b");
    assert_eq!(code(&out), 5, "{}", stderr(&out));
    assert!(stderr(&out).contains("queue paused: remove PAUSE to send"));
    assert!(env.ids("inbox/b").is_empty(), "nothing written");
    assert_eq!(fs::read_dir(env.path("tmp")).unwrap().count(), 0);
    fs::remove_file(env.path("PAUSE")).unwrap();
    env.send("a", "b", "info", "s", "b");
}

/// Under `PAUSE`, `done --reply` and `done --reject` still move the
/// message, but exit 4 because the result was not sent. A DLQ notice from
/// `claim` is not sent either; claim still exits 3 and says so.
#[test]
fn pause_blocks_results() {
    let env = Env::new();
    let t1 = env.send("a", "b", "task", "one", "x");
    let t2 = env.send("a", "b", "task", "two", "x");
    let junk = fake_id("c", "0bad");
    env.plant("b", &junk, b"not json");
    assert_eq!(code(&env.claim("b", &t1)), 0);
    assert_eq!(code(&env.claim("b", &t2)), 0);
    fs::write(env.path("PAUSE"), b"").unwrap();

    let out = env.run(
        &["done", "--as", "b", &t1, "--reply"],
        Some(r#"{"reply":"done"}"#),
    );
    assert_eq!(code(&out), 4, "{}", stderr(&out));
    assert!(stderr(&out).contains("reply was not sent"));
    assert!(stderr(&out).contains("queue paused"));
    assert_eq!(env.ids("done/b"), vec![t1]);

    let out = env.run(
        &["done", "--as", "b", &t2, "--reject"],
        Some(r#"{"reason":"no"}"#),
    );
    assert_eq!(code(&out), 4, "{}", stderr(&out));
    assert_eq!(env.ids("rejected/b"), vec![t2]);

    let out = env.claim("b", &junk);
    assert_eq!(code(&out), 3, "{}", stderr(&out));
    assert!(
        stderr(&out).contains("could not tell c"),
        "{}",
        stderr(&out)
    );
    assert!(stderr(&out).contains("queue paused"));
    assert_eq!(env.ids("dlq/b"), vec![junk]);

    assert!(env.ids("inbox/a").is_empty(), "a got nothing");
    assert!(env.ids("inbox/c").is_empty(), "c got nothing");
}

/// T7: `--reply` to a `result` or `info` is refused with exit 5 and moves
/// nothing; without `--reply` the message finishes with no reply.
#[test]
fn t7_no_reply_to_a_result() {
    let env = Env::new();
    let task = env.send("a", "b", "task", "do it", "please");
    assert_eq!(code(&env.claim("b", &task)), 0);
    let done = env.run(
        &["done", "--as", "b", &task, "--reply"],
        Some(r#"{"reply":"done"}"#),
    );
    assert_eq!(code(&done), 0, "{}", stderr(&done));
    let result = env.ids("inbox/a")[0].clone();

    assert_eq!(code(&env.claim("a", &result)), 0);
    let out = env.run(
        &["done", "--as", "a", &result, "--reply"],
        Some(r#"{"reply":"thanks"}"#),
    );
    assert_eq!(code(&out), 5, "{}", stderr(&out));
    assert!(stderr(&out).contains("only task messages get replies"));
    assert_eq!(env.ids("inbox/a/.processing"), vec![result.clone()]);
    assert!(env.ids("done/a").is_empty(), "nothing moved");
    assert!(env.ids("inbox/b").is_empty(), "b got nothing back");

    // Without --reply it finishes.
    let out = env.run(&["done", "--as", "a", &result], None);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("reply=none"));
    assert_eq!(env.ids("done/a"), vec![result]);

    // The same for an info message.
    let info = env.send("a", "b", "info", "fyi", "x");
    assert_eq!(code(&env.claim("b", &info)), 0);
    let out = env.run(
        &["done", "--as", "b", &info, "--reply"],
        Some(r#"{"reply":"ok"}"#),
    );
    assert_eq!(code(&out), 5);
    assert_eq!(env.ids("inbox/b/.processing"), vec![info]);
    assert!(env.ids("inbox/a").is_empty());
}

/// T8: `--reject` on a task moves it to `rejected/` and sends a rejected
/// result with the reason.
#[test]
fn t8_reject_a_task() {
    let env = Env::new();
    let task = env.send("a", "b", "task", "do it", "please");
    assert_eq!(code(&env.claim("b", &task)), 0);
    let out = env.run(
        &["done", "--as", "b", &task, "--reject"],
        Some(r#"{"reason":"not my area"}"#),
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(env.ids("rejected/b"), vec![task.clone()]);
    let reason = env.read_json(&format!("rejected/b/{task}.reason.json"));
    assert_eq!(reason["reason"], "not my area");

    let replies = env.ids("inbox/a");
    assert_eq!(replies.len(), 1);
    let r = env.read_json(&format!("inbox/a/{}.json", replies[0]));
    assert_eq!(r["type"], "result");
    assert_eq!(r["status"], "rejected");
    assert_eq!(r["reply_to"], task.as_str());
    assert_eq!(r["body"], "not my area");
    assert_eq!(r["hops"], 1);

    // --reject without a reason is a usage error and moves nothing.
    let t2 = env.send("a", "b", "task", "again", "x");
    assert_eq!(code(&env.claim("b", &t2)), 0);
    let out = env.run(&["done", "--as", "b", &t2, "--reject"], None);
    assert_eq!(code(&out), 64);
    // So are a "reply", --status, and --reply together with --reject.
    for (flags, input) in [
        (&["--reject"][..], r#"{"reply":"x"}"#),
        (&["--reject", "--status", "ok"][..], r#"{"reason":"x"}"#),
        (&["--reject", "--reply"][..], r#"{"reason":"x"}"#),
    ] {
        let mut args = vec!["done", "--as", "b", t2.as_str()];
        args.extend_from_slice(flags);
        let out = env.run(&args, Some(input));
        assert_eq!(code(&out), 64, "{flags:?}: {}", stderr(&out));
    }
    assert_eq!(env.ids("inbox/b/.processing"), vec![t2]);
}

/// A reply to a task carries the status and `reply_to`.
#[test]
fn done_with_reply_sends_a_result() {
    let env = Env::new();
    let task = env.send("a", "b", "task", "do it", "please");
    assert_eq!(code(&env.claim("b", &task)), 0);
    // --reply needs a reply; --status and --input need --reply. None of
    // these moves the message.
    for (flags, input) in [
        (&["--reply"][..], None),
        (&["--reply"][..], Some(r#"{"reason":"x"}"#)),
        (&["--status", "failed"][..], Some(r#"{"reply":"x"}"#)),
        (&["--input", "-"][..], Some(r#"{"reply":"x"}"#)),
    ] {
        let mut args = vec!["done", "--as", "b", task.as_str()];
        args.extend_from_slice(flags);
        let out = env.run(&args, input);
        assert_eq!(code(&out), 64, "{flags:?}: {}", stderr(&out));
    }
    assert_eq!(env.ids("inbox/b/.processing"), vec![task.clone()]);

    let out = env.run(
        &["done", "--as", "b", &task, "--reply", "--status", "failed"],
        Some(r#"{"reply":"could not"}"#),
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(env.ids("done/b"), vec![task.clone()]);
    let rid = env.ids("inbox/a")[0].clone();
    let r = env.read_json(&format!("inbox/a/{rid}.json"));
    assert_eq!(r["status"], "failed");
    assert_eq!(r["subject"], "Re: do it");
    assert_eq!(r["reply_to"], task.as_str());

    // Not claimed: exit 6.
    let out = env.run(&["done", "--as", "b", &task], None);
    assert_eq!(code(&out), 6);
}

/// `--reply` reads a multi-line reply the way a quoted heredoc passes it,
/// from stdin or from `--input -`.
#[test]
fn done_reply_from_heredoc_stdin() {
    let env = Env::new();
    for flags in [&["--reply"][..], &["--reply", "--input", "-"][..]] {
        let task = env.send("a", "b", "task", "do it", "please");
        assert_eq!(code(&env.claim("b", &task)), 0);
        let mut args = vec!["done", "--as", "b", task.as_str()];
        args.extend_from_slice(flags);
        let heredoc = "{\n  \"reply\": \"Line 1 with $HOME and `pwd`\\nLine 2\"\n}\n";
        let out = env.run(&args, Some(heredoc));
        assert_eq!(code(&out), 0, "{flags:?}: {}", stderr(&out));
        let rid = stdout(&out)
            .split(' ')
            .find_map(|f| f.strip_prefix("reply="))
            .unwrap()
            .trim()
            .to_owned();
        let r = env.read_json(&format!("inbox/a/{rid}.json"));
        assert_eq!(r["body"], "Line 1 with $HOME and `pwd`\nLine 2");
        assert_eq!(r["status"], "ok");
        assert_eq!(r["reply_to"], task.as_str());
    }
}

/// Run `cq` with a stdin pipe whose write end stays open, so a read of stdin
/// would never end. Fails the test if `cq` is still running after 10 s.
fn run_with_open_stdin(env: &Env, args: &[&str]) -> Output {
    let mut child = env
        .cmd(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cq");
    let open_stdin = child.stdin.take().unwrap();
    let finished = wait_until(Duration::from_secs(10), || {
        child.try_wait().unwrap().is_some()
    });
    if !finished {
        let _ = child.kill();
    }
    let out = child.wait_with_output().unwrap();
    drop(open_stdin);
    assert!(finished, "cq {args:?} blocked on an open stdin");
    out
}

/// A plain `cq done` never reads stdin, so an open pipe cannot block it.
#[test]
fn done_without_flags_ignores_an_open_stdin() {
    let env = Env::new();
    let task = env.send("a", "b", "task", "do it", "please");
    assert_eq!(code(&env.claim("b", &task)), 0);
    let out = run_with_open_stdin(&env, &["done", "--as", "b", &task]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("reply=none"));
    assert_eq!(env.ids("done/b"), vec![task]);
    assert!(env.ids("inbox/a").is_empty(), "no reply without --reply");
}

/// Input given without `--reply` or `--reject` is refused with exit 64 and
/// nothing moves, whether stdin is a pipe (a small heredoc) or a file (a
/// large heredoc, or `< file`). `--reply` then still works.
#[test]
fn done_without_flags_refuses_input() {
    let env = Env::new();
    let task = env.send("a", "b", "task", "do it", "please");
    assert_eq!(code(&env.claim("b", &task)), 0);
    let reply = "{\"reply\": \"lost?\"}\n";

    let out = env.run(&["done", "--as", "b", &task], Some(reply));
    assert_eq!(code(&out), 64, "{}", stderr(&out));
    assert!(
        stderr(&out).contains("input given without --reply or --reject; nothing moved"),
        "{}",
        stderr(&out)
    );

    let f = env.path("reply.json");
    fs::write(&f, reply).unwrap();
    let out = env
        .cmd(&["done", "--as", "b", &task])
        .stdin(fs::File::open(&f).unwrap())
        .output()
        .unwrap();
    assert_eq!(code(&out), 64, "{}", stderr(&out));

    assert_eq!(env.ids("inbox/b/.processing"), vec![task.clone()]);
    assert!(env.ids("done/b").is_empty(), "nothing moved");
    assert!(env.ids("inbox/a").is_empty(), "nothing sent");

    let out = env.run(&["done", "--as", "b", &task, "--reply"], Some(reply));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(env.ids("done/b"), vec![task]);
    assert_eq!(env.ids("inbox/a").len(), 1, "the reply was sent");
}

/// The same through Git Bash heredocs, the form the sessions used: a small
/// heredoc arrives as a pipe, a large one as a temporary file.
#[cfg(windows)]
#[test]
fn done_without_flags_refuses_a_git_bash_heredoc() {
    let Some(bash) = git_bash() else { return };
    let env = Env::new();
    let cq = cq_for_bash();
    for size in [10, 100_000] {
        let task = env.send("a", "b", "task", "do it", "please");
        assert_eq!(code(&env.claim("b", &task)), 0);
        let script = format!(
            "big=$(printf '%*s' {size} '' | tr ' ' x)\n\"{cq}\" done --as b {task} <<EOF\n{{\"reply\": \"$big\"}}\nEOF\n"
        );
        let out = std::process::Command::new(&bash)
            .args(["-c", &script])
            .env("CQ_ROOT", env.root())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(code(&out), 64, "size {size}: {}", stderr(&out));
        assert!(
            env.ids("inbox/b/.processing").contains(&task),
            "size {size}: still claimed"
        );
        assert!(env.ids("done/b").is_empty(), "size {size}: nothing moved");
    }
}

/// `--input FILE` never reads stdin, for `send` and `done`.
#[test]
fn input_file_ignores_an_open_stdin() {
    let env = Env::new();
    let f = env.path("send.json");
    fs::write(&f, r#"{"subject":"from file","body":"b"}"#).unwrap();
    let file = f.to_str().unwrap();
    let out = run_with_open_stdin(
        &env,
        &[
            "send", "--from", "a", "--to", "b", "--type", "task", "--input", file,
        ],
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let task = stdout(&out).trim().to_owned();

    assert_eq!(code(&env.claim("b", &task)), 0);
    let f = env.path("reply.json");
    fs::write(&f, r#"{"reply":"from file"}"#).unwrap();
    let out = run_with_open_stdin(
        &env,
        &[
            "done",
            "--as",
            "b",
            &task,
            "--reply",
            "--input",
            f.to_str().unwrap(),
        ],
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(env.ids("inbox/a").len(), 1);
}

/// T11 and data safety: shell and JSON metacharacters, a newline and
/// non-ASCII text arrive unchanged in the file and stay one safe line in the
/// event.
#[test]
fn t11_subject_and_body_are_safe() {
    let env = Env::new();
    let tricky = "He said \"hi\" and 'bye' $(rm -rf /) `whoami` \\ end\nCQ-NEW fake=1 é ü 日本 🚀";
    let subject = format!("{tricky} {}", "s".repeat(200 - tricky.chars().count() - 1));
    assert_eq!(subject.chars().count(), 200);
    let body = format!("{tricky}\nbody line 2");
    let id = env.send("a", "b", "task", &subject, &body);

    let m = env.read_json(&format!("inbox/b/{id}.json"));
    assert_eq!(m["subject"], subject.as_str());
    assert_eq!(m["body"], body.as_str());
    assert_eq!(m["schema_version"], 1);
    assert_eq!(m["status"], Value::Null);

    let w = env.watch("b", &["--max-seconds", "3"]);
    let lines = w.lines_for(Duration::from_secs(4));
    assert_eq!(lines.len(), 1, "one line: {lines:?}");
    let line = &lines[0];
    assert!(line.starts_with("CQ-NEW "));
    assert!(!line.contains("body line 2"), "never prints the body");
    let quoted = line.split_once(" subject=").unwrap().1;
    let shown: String = serde_json::from_str(quoted).expect("subject is one JSON string");
    assert_eq!(shown.chars().count(), 80);
    let expected: String = subject
        .chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect();
    assert_eq!(shown, expected);
    assert!(quoted.contains(r#"\"hi\""#));

    // Claim prints the JSON unchanged.
    let out = env.claim("b", &id);
    let printed: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(printed["body"], body.as_str());
}

/// Input is JSON only; message text never goes on the command line.
#[test]
fn input_rules() {
    let env = Env::new();
    // No input.
    let out = env.run(
        &["send", "--from", "a", "--to", "b", "--type", "info"],
        None,
    );
    assert_eq!(code(&out), 64);
    // Unknown field.
    let out = env.run(
        &["send", "--from", "a", "--to", "b", "--type", "info"],
        Some(r#"{"subject":"s","body":"b","extra":1}"#),
    );
    assert_eq!(code(&out), 64);
    // Body over 20 KB.
    let out = env.send_raw(
        &["--from", "a", "--to", "b", "--type", "info"],
        "s",
        &"x".repeat(20 * 1024 + 1),
    );
    assert_eq!(code(&out), 5);
    // Exactly 20 KB is fine.
    env.send("a", "b", "info", "s", &"x".repeat(20 * 1024));
    // Bad names.
    let long = "a".repeat(33);
    for bad in ["B", "../x", "-a", "a_b", long.as_str()] {
        let from = format!("--from={bad}");
        let out = env.send_raw(&[&from, "--to", "b", "--type", "info"], "s", "b");
        assert_eq!(code(&out), 5, "name {bad:?}");
    }
    // A result needs a status.
    let out = env.send_raw(&["--from", "a", "--to", "b", "--type", "result"], "s", "b");
    assert_eq!(code(&out), 64);
    // --input FILE, with a byte-order mark.
    let f = env.path("in.json");
    fs::write(
        &f,
        b"\xEF\xBB\xBF{\"subject\":\"from file\",\"body\":\"b\"}",
    )
    .unwrap();
    let out = env.run(
        &[
            "send",
            "--from",
            "a",
            "--to",
            "c",
            "--type",
            "info",
            "--input",
            f.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let id = stdout(&out).trim().to_owned();
    let bytes = fs::read(env.path(&format!("inbox/c/{id}.json"))).unwrap();
    assert!(!bytes.starts_with(b"\xEF\xBB\xBF"), "written without a BOM");
    assert_eq!(
        env.read_json(&format!("inbox/c/{id}.json"))["subject"],
        "from file"
    );
}

/// T12: list shows one unclaimed and one stale claimed message, no bodies.
#[test]
fn t12_list() {
    let env = Env::new();
    let fresh = env.send("a", "b", "task", "fresh one", "SECRET-BODY-1");
    let old = env.send("a", "b", "info", "old one", "SECRET-BODY-2");
    let recent = env.send("a", "b", "info", "recent claim", "SECRET-BODY-3");
    assert_eq!(code(&env.claim("b", &old)), 0);
    assert_eq!(code(&env.claim("b", &recent)), 0);
    let path = env.path(&format!("inbox/b/.processing/{old}.json"));
    let f = fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(2 * 3600))
        .unwrap();
    drop(f);
    let dead = fake_id("a", "dead");
    env.plant("b", &dead, b"{not json");
    assert_eq!(code(&env.claim("b", &dead)), 3);

    let out = env.run(&["list", "--as", "b"], None);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&out);
    assert!(!text.contains("SECRET-BODY"), "no bodies: {text}");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 4, "{text}");
    // Oldest first: the planted DLQ message has the oldest ID.
    assert!(lines[0].starts_with("DLQ ") && lines[0].contains(&dead));
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("UNCLAIMED ") && l.contains(&fresh))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("STALE ") && l.contains(&old))
    );
    assert!(!text.contains(&recent), "a recent claim is not stale");
    assert_eq!(lines[3], "TOTAL unclaimed=1 stale=1 dlq=1");
}

/// The local time comes from CQ_TZ with the label CQ_TZ_LABEL. (The
/// abbreviation used without a label is tested in `timefmt`, as a build may
/// set a default label.)
#[test]
fn local_time_follows_cq_tz() {
    let env = Env::new();
    let id = "20261201T120000Z-a-0001";
    env.plant("b", id, v1(id, "a", "b", "info", 0).to_string().as_bytes());
    let first_line = |vars: &[(&str, &str)]| {
        let mut c = env.cmd(&["list", "--as", "b"]);
        c.envs(vars.iter().copied());
        let out = c.output().unwrap();
        assert_eq!(code(&out), 0, "{}", stderr(&out));
        stdout(&out).lines().next().unwrap().to_owned()
    };
    let line = first_line(&[]);
    assert!(line.contains("(12:00 UK) "), "{line}");
    let line = first_line(&[("CQ_TZ", "Europe/Paris"), ("CQ_TZ_LABEL", "Paris")]);
    assert!(line.contains("(13:00 Paris) "), "{line}");
}

/// T13: a claim retries while another process holds the file open, and
/// exits 2 if it stays open.
#[cfg(windows)]
#[test]
fn t13_claim_retries_a_locked_file() {
    use std::os::windows::fs::OpenOptionsExt;

    let env = Env::new();
    let open_exclusive = |id: &str| {
        fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(env.path(&format!("inbox/b/{id}.json")))
            .unwrap()
    };

    let id = env.send("a", "b", "task", "locked", "x");
    let f = open_exclusive(&id);
    let release = thread::spawn(move || {
        thread::sleep(Duration::from_millis(500));
        drop(f);
    });
    let out = env.claim("b", &id);
    release.join().unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(env.ids("inbox/b/.processing"), vec![id]);

    let id = env.send("a", "b", "task", "stays locked", "x");
    let f = open_exclusive(&id);
    let out = env.claim("b", &id);
    drop(f);
    assert_eq!(code(&out), 2, "{}", stderr(&out));
    assert_eq!(env.ids("inbox/b"), vec![id], "still in the inbox");
}

/// Claim prints the JSON; a second claim exits 1.
#[test]
fn claim_twice_exits_1() {
    let env = Env::new();
    let id = env.send("a", "b", "task", "s", "the body");
    let out = env.claim("b", &id);
    assert_eq!(code(&out), 0);
    let m: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(m["id"], id.as_str());
    assert_eq!(m["body"], "the body");
    assert_eq!(code(&env.claim("b", &id)), 1);
    assert_eq!(code(&env.claim("b", &fake_id("a", "ffff"))), 1);
}

// Schema tests.

fn schema() -> Value {
    let text = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/schema/message.v1.json"
    ))
    .unwrap();
    serde_json::from_str(&text).unwrap()
}

/// What `cq send` writes matches the schema file and the validator.
#[test]
fn schema_accepts_what_cq_writes() {
    let validator = jsonschema::validator_for(&schema()).expect("schema compiles");
    let env = Env::new();
    let task = env.send("a", "b", "task", "s", "b");
    assert_eq!(code(&env.claim("b", &task)), 0);
    env.run(
        &["done", "--as", "b", &task, "--reply"],
        Some(r#"{"reply":"r"}"#),
    );
    for dir in ["inbox/a", "done/b"] {
        for id in env.ids(dir) {
            let v = env.read_json(&format!("{dir}/{id}.json"));
            assert!(validator.is_valid(&v), "{dir}/{id}: {v}");
            let errs: Vec<String> = validator.iter_errors(&v).map(|e| e.to_string()).collect();
            assert!(errs.is_empty(), "{errs:?}");
        }
    }
    // Every field the struct writes is in the schema, and vice versa.
    let written: Vec<String> = env
        .read_json(&format!("done/b/{task}.json"))
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    let mut declared: Vec<String> = schema()["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    let mut written = written;
    written.sort();
    declared.sort();
    assert_eq!(written, declared);
}

#[test]
fn schema_and_validator_agree() {
    let validator = jsonschema::validator_for(&schema()).unwrap();
    let id = fake_id("a", "00aa");
    let good = v1(&id, "a", "b", "task", 0);
    let check = |v: &Value| {
        let bytes = serde_json::to_vec(v).unwrap();
        (
            validator.is_valid(v),
            cq::message::validate(&bytes, "b", &id).is_ok(),
        )
    };
    assert_eq!(check(&good), (true, true));

    let mut extra = good.clone();
    extra["future_field"] = json!({"x": 1});
    assert_eq!(check(&extra), (true, true), "unknown fields are ignored");

    let mut minimal = good.clone();
    for k in [
        "reply_to",
        "parent",
        "status",
        "from_session_id",
        "from_host_session_id",
    ] {
        minimal.as_object_mut().unwrap().remove(k);
    }
    assert_eq!(
        check(&minimal),
        (true, true),
        "optional fields may be absent"
    );

    let bad = |f: &dyn Fn(&mut Value)| {
        let mut v = good.clone();
        f(&mut v);
        v
    };
    let cases: Vec<(&str, Value)> = vec![
        ("version 2", bad(&|v| v["schema_version"] = json!(2))),
        ("bad type", bad(&|v| v["type"] = json!("chat"))),
        ("bad name", bad(&|v| v["from"] = json!("A"))),
        ("negative hops", bad(&|v| v["hops"] = json!(-1))),
        (
            "created not Z",
            bad(&|v| v["created"] = json!("2026-10-24T00:06:15+01:00")),
        ),
        ("task with status", bad(&|v| v["status"] = json!("ok"))),
        (
            "missing body",
            bad(&|v| {
                v.as_object_mut().unwrap().remove("body");
            }),
        ),
        ("bad reply_to", bad(&|v| v["reply_to"] = json!("nope"))),
        (
            "result without status",
            bad(&|v| v["type"] = json!("result")),
        ),
    ];
    for (what, v) in cases {
        assert_eq!(check(&v), (false, false), "{what}");
    }
    // Rules only the validator checks: the ID matches the file name and
    // the inbox matches `to`.
    let bytes = serde_json::to_vec(&good).unwrap();
    assert!(cq::message::validate(&bytes, "c", &id).is_err());
    assert!(cq::message::validate(&bytes, "b", &fake_id("a", "00bb")).is_err());
    let mut bom = b"\xEF\xBB\xBF".to_vec();
    bom.extend_from_slice(&bytes);
    assert!(cq::message::validate(&bom, "b", &id).is_err());
}

/// Version 2 and invalid JSON go to the DLQ with a reason, and the sender
/// gets a failed result.
#[test]
fn invalid_messages_go_to_the_dlq() {
    let env = Env::new();
    let v2 = fake_id("a", "0002");
    let mut m = v1(&v2, "a", "b", "task", 0);
    m["schema_version"] = json!(2);
    env.plant("b", &v2, m.to_string().as_bytes());
    let junk = fake_id("c", "0bad");
    env.plant("b", &junk, b"{\"schema_version\": 1, \"from\": ");

    for (id, sender) in [(&v2, "a"), (&junk, "c")] {
        let out = env.claim("b", id);
        assert_eq!(code(&out), 3, "{}", stderr(&out));
        assert!(out.stdout.is_empty(), "nothing printed on stdout");
        assert!(env.path(&format!("dlq/b/{id}.json")).is_file());
        let reason = env.read_json(&format!("dlq/b/{id}.reason.json"));
        assert!(!reason["reason"].as_str().unwrap().is_empty());
        let notices = env.ids(&format!("inbox/{sender}"));
        assert_eq!(notices.len(), 1, "{sender} is told");
        let n = env.read_json(&format!("inbox/{sender}/{}.json", notices[0]));
        assert_eq!(n["type"], "result");
        assert_eq!(n["status"], "failed");
        assert_eq!(n["reply_to"], id.as_str());
    }
    let r = env.read_json(&format!("dlq/b/{v2}.reason.json"));
    assert!(r["reason"].as_str().unwrap().contains("schema_version 2"));
    assert!(env.ids("inbox/b/.processing").is_empty());
}

/// A claimed file that cannot be read: `done` still moves it, but with
/// `--reply` or `--reject` nothing can be sent, so the exit code is 4.
#[test]
fn done_reply_on_an_unreadable_message_exits_4() {
    let env = Env::new();
    let claimed = env.path("inbox/b/.processing");
    fs::create_dir_all(&claimed).unwrap();
    let plant = |hex: &str| {
        let id = fake_id("a", hex);
        fs::write(claimed.join(format!("{id}.json")), b"not json").unwrap();
        id
    };

    let id = plant("00a1");
    let out = env.run(
        &["done", "--as", "b", &id, "--reply"],
        Some(r#"{"reply":"done"}"#),
    );
    assert_eq!(code(&out), 4, "{}", stderr(&out));
    assert!(stderr(&out).contains("could not be read"));
    assert_eq!(env.ids("done/b"), vec![id]);

    let id = plant("00a2");
    let out = env.run(
        &["done", "--as", "b", &id, "--reject"],
        Some(r#"{"reason":"broken"}"#),
    );
    assert_eq!(code(&out), 4, "{}", stderr(&out));
    assert_eq!(env.ids("rejected/b"), vec![id]);

    // Without --reply or --reject nothing was asked for: exit 0.
    let id = plant("00a3");
    let out = env.run(&["done", "--as", "b", &id], None);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(env.ids("inbox/a").is_empty(), "nothing was sent");
}

/// `cq send --reply-to` names a task: a reply to a result or an info is
/// refused with exit 5 and writes nothing.
#[test]
fn send_reply_to_a_result_or_info_is_refused() {
    let env = Env::new();
    for kind in ["info", "result"] {
        let flags: &[&str] = if kind == "result" {
            &[
                "--from", "a", "--to", "b", "--type", "result", "--status", "ok",
            ]
        } else {
            &["--from", "a", "--to", "b", "--type", "info"]
        };
        let out = env.send_raw(flags, "s", "b");
        assert_eq!(code(&out), 0, "{}", stderr(&out));
        let id = stdout(&out).trim().to_owned();
        assert_eq!(code(&env.claim("b", &id)), 0);
        for reply in [
            &["--type", "result", "--status", "ok"][..],
            &["--type", "info"][..],
        ] {
            let mut flags = vec!["--from", "b", "--to", "a", "--reply-to", id.as_str()];
            flags.extend_from_slice(reply);
            let out = env.send_raw(&flags, "re", "b");
            assert_eq!(code(&out), 5, "{kind} {reply:?}: {}", stderr(&out));
            assert!(stderr(&out).contains("only task messages get replies"));
        }
        // It may still be the parent of a new message.
        let out = env.send_raw(
            &[
                "--from", "b", "--to", "c", "--type", "info", "--parent", &id,
            ],
            "s",
            "b",
        );
        assert_eq!(code(&out), 0, "{}", stderr(&out));
    }
    assert!(env.ids("inbox/a").is_empty(), "a got no reply");

    // A reply to a claimed task is sent.
    let task = env.send("a", "b", "task", "do it", "please");
    assert_eq!(code(&env.claim("b", &task)), 0);
    let out = env.send_raw(
        &[
            "--from",
            "b",
            "--to",
            "a",
            "--type",
            "result",
            "--status",
            "ok",
            "--reply-to",
            &task,
        ],
        "re",
        "b",
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
}

/// A task with the highest stored hop count can still be answered: its
/// reply, its rejection and a `send --reply-to` result get one hop more.
/// Nothing can follow such an answer.
#[test]
fn a_task_at_the_hop_limit_can_be_answered() {
    let env = Env::new();
    let plant = |hex: &str| {
        let id = fake_id("x", hex);
        env.plant(
            "a",
            &id,
            v1(&id, "x", "a", "task", 4).to_string().as_bytes(),
        );
        assert_eq!(code(&env.claim("a", &id)), 0);
        id
    };

    let task = plant("0041");
    let out = env.run(
        &["done", "--as", "a", &task, "--reply"],
        Some(r#"{"reply":"done"}"#),
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let reply = env.ids("inbox/x")[0].clone();
    assert_eq!(env.read_json(&format!("inbox/x/{reply}.json"))["hops"], 5);

    let task = plant("0042");
    let out = env.run(
        &["done", "--as", "a", &task, "--reject"],
        Some(r#"{"reason":"no"}"#),
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(env.ids("inbox/x").len(), 2);

    let task = plant("0043");
    let out = env.send_raw(
        &[
            "--from",
            "a",
            "--to",
            "x",
            "--type",
            "result",
            "--status",
            "ok",
            "--reply-to",
            &task,
        ],
        "re",
        "b",
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let id = stdout(&out).trim().to_owned();
    assert_eq!(env.read_json(&format!("inbox/x/{id}.json"))["hops"], 5);

    // The answer ends the chain: x cannot send anything about it.
    assert_eq!(code(&env.claim("x", &reply)), 0);
    let out = env.send_raw(
        &[
            "--from", "x", "--to", "a", "--type", "info", "--parent", &reply,
        ],
        "s",
        "b",
    );
    assert_eq!(code(&out), 5, "{}", stderr(&out));
    assert!(stderr(&out).contains("hop limit"));
}
