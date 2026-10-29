//! Rejections: the notice an info or result's sender gets, `cq list
//! --rejected`, and `cq requeue`.

mod common;

use std::fs;

use common::*;

/// Reject a claimed message with `reason`.
fn reject(env: &Env, name: &str, id: &str, reason: &str) -> std::process::Output {
    let input = serde_json::json!({ "reason": reason }).to_string();
    env.run(&["done", "--as", name, id, "--reject"], Some(&input))
}

/// The only message in `inbox/<name>/`, as JSON.
fn only_message(env: &Env, name: &str) -> serde_json::Value {
    let ids = env.ids(&format!("inbox/{name}"));
    assert_eq!(ids.len(), 1, "inbox/{name}: {ids:?}");
    env.read_json(&format!("inbox/{name}/{}.json", ids[0]))
}

/// Rejecting an info sends its sender an info notice with the message ID,
/// subject and reason, as a reply to the rejected message.
#[test]
fn reject_an_info_tells_the_sender() {
    let env = Env::new();
    let info = env.send("alice", "bob", "info", "hello from alice", "SECRET");
    assert_eq!(code(&env.claim("bob", &info)), 0);
    let out = reject(&env, "bob", &info, "sender not in scope");
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(env.ids("rejected/bob"), vec![info.clone()]);

    let n = only_message(&env, "alice");
    assert_eq!(n["type"], "info");
    assert_eq!(n["from"], "bob");
    assert_eq!(n["status"], serde_json::Value::Null);
    assert_eq!(n["subject"], "Rejected: hello from alice");
    assert_eq!(n["reply_to"], info.as_str());
    assert_eq!(n["hops"], 1);
    let body = n["body"].as_str().unwrap();
    assert!(body.contains(&info), "{body}");
    assert!(body.contains("\"hello from alice\""), "{body}");
    assert!(body.contains("Reason: sender not in scope"), "{body}");
    assert!(!body.contains("SECRET"), "no original body: {body}");
    assert!(
        stdout(&out).contains(&format!("reply={}", n["id"].as_str().unwrap())),
        "{}",
        stdout(&out)
    );
}

/// Rejecting a result sends its sender an info notice too.
#[test]
fn reject_a_result_tells_the_sender() {
    let env = Env::new();
    let task = env.send("alice", "bob", "task", "run it", "x");
    assert_eq!(code(&env.claim("bob", &task)), 0);
    let out = env.run(
        &["done", "--as", "bob", &task, "--reply"],
        Some(r#"{"reply":"ran it"}"#),
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let result = env.ids("inbox/alice")[0].clone();
    assert_eq!(code(&env.claim("alice", &result)), 0);

    let out = reject(&env, "alice", &result, "not what I asked");
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(env.ids("rejected/alice"), vec![result.clone()]);
    let n = only_message(&env, "bob");
    assert_eq!(n["type"], "info");
    assert_eq!(n["subject"], "Rejected: Re: run it");
    assert_eq!(n["reply_to"], result.as_str());
    assert_eq!(n["hops"], 2);
    assert!(n["body"].as_str().unwrap().contains("not what I asked"));
}

/// The task case is unchanged: one `rejected` result, no extra notice.
#[test]
fn reject_a_task_sends_only_the_rejected_result() {
    let env = Env::new();
    let task = env.send("alice", "bob", "task", "deploy", "x");
    assert_eq!(code(&env.claim("bob", &task)), 0);
    let out = reject(&env, "bob", &task, "out of scope");
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let r = only_message(&env, "alice");
    assert_eq!(r["type"], "result");
    assert_eq!(r["status"], "rejected");
    assert_eq!(r["body"], "out of scope");
}

/// Rejecting a reject notice, or a `rejected` result, sends nothing back,
/// so two sessions cannot reject back and forth.
#[test]
fn rejecting_a_rejection_sends_nothing() {
    let env = Env::new();
    // A notice: bob rejects alice's info, alice rejects the notice.
    let info = env.send("alice", "bob", "info", "hello", "x");
    assert_eq!(code(&env.claim("bob", &info)), 0);
    assert_eq!(code(&reject(&env, "bob", &info, "no")), 0);
    let notice = env.ids("inbox/alice")[0].clone();
    assert_eq!(code(&env.claim("alice", &notice)), 0);
    let out = reject(&env, "alice", &notice, "no to you too");
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("reply=none"), "{}", stdout(&out));
    assert_eq!(env.ids("rejected/alice"), vec![notice]);
    assert!(env.ids("inbox/bob").is_empty(), "bob got nothing back");

    // A rejected result: bob rejects alice's task, alice rejects the result.
    let task = env.send("alice", "bob", "task", "do it", "x");
    assert_eq!(code(&env.claim("bob", &task)), 0);
    assert_eq!(code(&reject(&env, "bob", &task, "no")), 0);
    let result = env.ids("inbox/alice")[0].clone();
    assert_eq!(code(&env.claim("alice", &result)), 0);
    let out = reject(&env, "alice", &result, "no");
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("reply=none"));
    assert!(env.ids("inbox/bob").is_empty(), "bob got nothing back");

    // An info whose subject only looks like a notice still gets one.
    let look = env.send("alice", "bob", "info", "Rejected: plan B", "x");
    assert_eq!(code(&env.claim("bob", &look)), 0);
    assert_eq!(code(&reject(&env, "bob", &look, "no")), 0);
    assert_eq!(env.ids("inbox/alice").len(), 1);
}

/// When a send limit blocks the notice, the reject still succeeds (exit 0)
/// and stderr says the sender was not told.
#[test]
fn a_blocked_notice_does_not_block_the_reject() {
    let env = Env::new();
    let info = env.send("alice", "bob", "info", "hello", "x");
    assert_eq!(code(&env.claim("bob", &info)), 0);
    fs::write(env.path("PAUSE"), b"").unwrap();
    let out = reject(&env, "bob", &info, "no");
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("moved=rejected reply=none"));
    let err = stderr(&out);
    assert!(err.contains("alice was not told"), "{err}");
    assert!(err.contains("queue paused"), "{err}");
    assert_eq!(env.ids("rejected/bob"), vec![info]);
    assert!(env.ids("inbox/alice").is_empty());
    fs::remove_file(env.path("PAUSE")).unwrap();

    // The hop limit: a notice to a message with 4 hops would have 5.
    let deep = fake_id("alice", "4444");
    env.plant(
        "bob",
        &deep,
        &serde_json::to_vec(&v1(&deep, "alice", "bob", "info", 4)).unwrap(),
    );
    assert_eq!(code(&env.claim("bob", &deep)), 0);
    let out = reject(&env, "bob", &deep, "no");
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stderr(&out).contains("hop limit"), "{}", stderr(&out));
    assert!(env.ids("inbox/alice").is_empty());
}

/// A reason that uses the whole 20 KB limit still gives a notice within it.
#[test]
fn a_long_reason_is_cut_to_the_body_limit() {
    let env = Env::new();
    let info = env.send("alice", "bob", "info", "hello", "x");
    assert_eq!(code(&env.claim("bob", &info)), 0);
    let out = reject(&env, "bob", &info, &"é".repeat(10 * 1024));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let n = only_message(&env, "alice");
    let body = n["body"].as_str().unwrap();
    assert!(body.len() <= 20 * 1024, "{}", body.len());
    assert!(body.starts_with("bob rejected your info"));
}

/// `cq list --rejected` shows what a session rejected and what others
/// rejected from it, with reasons and no bodies.
#[test]
fn list_rejected_shows_both_directions() {
    let env = Env::new();
    let incoming = env.send("alice", "bob", "info", "hello bob", "SECRET-1");
    assert_eq!(code(&env.claim("bob", &incoming)), 0);
    assert_eq!(
        code(&reject(&env, "bob", &incoming, "sender not in scope")),
        0
    );
    let outgoing = env.send("bob", "carol", "task", "deploy it", "SECRET-2");
    assert_eq!(code(&env.claim("carol", &outgoing)), 0);
    assert_eq!(code(&reject(&env, "carol", &outgoing, "no deploys")), 0);
    // Neither rejected by bob nor sent by bob: not listed for bob.
    let other = env.send("alice", "carol", "info", "unrelated", "SECRET-3");
    assert_eq!(code(&env.claim("carol", &other)), 0);
    assert_eq!(code(&reject(&env, "carol", &other, "no")), 0);

    let out = env.run(&["list", "--as", "bob", "--rejected"], None);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&out);
    assert!(!text.contains("SECRET"), "no bodies: {text}");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert!(lines[0].starts_with("REJECTED-IN "), "{text}");
    for part in [
        "by=bob",
        "from=alice",
        "type=info",
        &format!("id={incoming}"),
        "subject=\"hello bob\"",
        "reason=\"sender not in scope\"",
    ] {
        assert!(lines[0].contains(part), "{part}: {}", lines[0]);
    }
    assert!(lines[0].contains(" rejected=20"), "{}", lines[0]);
    assert!(lines[1].starts_with("REJECTED-OUT "), "{text}");
    for part in [
        "by=carol",
        "from=bob",
        "type=task",
        &format!("id={outgoing}"),
        "subject=\"deploy it\"",
        "reason=\"no deploys\"",
    ] {
        assert!(lines[1].contains(part), "{part}: {}", lines[1]);
    }
    assert_eq!(lines[2], "TOTAL rejected_in=1 rejected_out=1");

    // The other side sees the same rejections the other way round.
    let out = env.run(&["list", "--as", "alice", "--rejected"], None);
    let text = stdout(&out);
    assert_eq!(
        text.lines()
            .filter(|l| l.starts_with("REJECTED-OUT "))
            .count(),
        2,
        "{text}"
    );
    assert!(
        text.contains("TOTAL rejected_in=0 rejected_out=2"),
        "{text}"
    );

    // Nothing rejected: only the total.
    let out = env.run(&["list", "--as", "dave", "--rejected"], None);
    assert_eq!(stdout(&out), "TOTAL rejected_in=0 rejected_out=0\n");

    // --rejected does not take --stale-minutes.
    let out = env.run(
        &["list", "--as", "bob", "--rejected", "--stale-minutes", "5"],
        None,
    );
    assert_eq!(code(&out), 64, "{}", stderr(&out));
}

/// `cq requeue` moves a rejected message back to the inbox with its ID,
/// removes its reason file, and the message can be claimed again.
#[test]
fn requeue_moves_a_rejected_message_back() {
    let env = Env::new();
    let info = env.send("alice", "bob", "info", "hello", "body");
    assert_eq!(code(&env.claim("bob", &info)), 0);
    assert_eq!(code(&reject(&env, "bob", &info, "sender not in scope")), 0);

    let out = env.run(&["requeue", "--as", "bob", &info], None);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        stdout(&out).trim(),
        format!("requeued id={info} to=inbox/bob/")
    );
    assert_eq!(env.ids("inbox/bob"), vec![info.clone()]);
    assert!(env.ids("rejected/bob").is_empty());
    assert!(
        !env.path(&format!("rejected/bob/{info}.reason.json"))
            .exists()
    );
    assert_eq!(
        env.read_json(&format!("inbox/bob/{info}.json"))["id"],
        info.as_str()
    );

    let out = env.claim("bob", &info);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let out = env.run(&["done", "--as", "bob", &info], None);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(env.ids("done/bob"), vec![info]);
}

/// `cq requeue` refuses any message outside the session's own `rejected/`
/// and moves nothing.
#[test]
fn requeue_only_takes_from_its_own_rejected_folder() {
    let env = Env::new();
    // Rejected by carol: bob may not requeue it, even though bob sent it.
    let theirs = env.send("bob", "carol", "info", "hi", "x");
    assert_eq!(code(&env.claim("carol", &theirs)), 0);
    assert_eq!(code(&reject(&env, "carol", &theirs, "no")), 0);
    let out = env.run(&["requeue", "--as", "bob", &theirs], None);
    assert_eq!(code(&out), 6, "{}", stderr(&out));
    assert!(
        stderr(&out).contains("not in rejected/bob/"),
        "{}",
        stderr(&out)
    );
    assert!(!env.ids("inbox/bob").contains(&theirs), "not moved");
    assert_eq!(env.ids("rejected/carol"), vec![theirs]);

    // Unclaimed, claimed and finished messages are not rejected.
    let waiting = env.send("alice", "bob", "info", "waiting", "x");
    let claimed = env.send("alice", "bob", "info", "claimed", "x");
    let finished = env.send("alice", "bob", "info", "finished", "x");
    assert_eq!(code(&env.claim("bob", &claimed)), 0);
    assert_eq!(code(&env.claim("bob", &finished)), 0);
    assert_eq!(code(&env.run(&["done", "--as", "bob", &finished], None)), 0);
    for id in [&waiting, &claimed, &finished] {
        let out = env.run(&["requeue", "--as", "bob", id], None);
        assert_eq!(code(&out), 6, "{id}: {}", stderr(&out));
    }
    assert_eq!(env.ids("inbox/bob/.processing"), vec![claimed]);
    assert_eq!(env.ids("done/bob"), vec![finished]);

    // A path is not an ID.
    let out = env.run(&["requeue", "--as", "bob", "../carol/x"], None);
    assert_eq!(code(&out), 5, "{}", stderr(&out));
}

/// A running watcher announces a requeued message again, with its ID.
#[test]
fn a_watcher_announces_a_requeued_message() {
    let env = Env::new();
    let w = env.watch("bob", &[]);
    let info = env.send("alice", "bob", "info", "hello", "x");
    let line = w
        .line(std::time::Duration::from_secs(5))
        .expect("a CQ-NEW line");
    assert_eq!(event_id(&line), info);
    assert_eq!(code(&env.claim("bob", &info)), 0);
    assert_eq!(code(&reject(&env, "bob", &info, "no")), 0);

    let out = env.run(&["requeue", "--as", "bob", &info], None);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let line = w
        .line(std::time::Duration::from_secs(5))
        .expect("a second CQ-NEW line");
    assert!(line.starts_with("CQ-NEW "), "{line}");
    assert_eq!(event_id(&line), info);
}

/// A message that is claimed, rejected and requeued between two scans is
/// still announced again: the watcher never sees the inbox without it, but
/// the requeue gives the file a new modified time.
#[test]
fn a_requeue_between_two_scans_is_announced() {
    let env = Env::new();
    let info = env.send("alice", "bob", "info", "hello", "x");
    let w = env.watch("bob", &["--no-fs-events", "--rescan-seconds", "4"]);
    let line = w
        .line(std::time::Duration::from_secs(5))
        .expect("a CQ-NEW line from the start scan");
    assert_eq!(event_id(&line), info);

    assert_eq!(code(&env.claim("bob", &info)), 0);
    assert_eq!(code(&reject(&env, "bob", &info, "no")), 0);
    let out = env.run(&["requeue", "--as", "bob", &info], None);
    assert_eq!(code(&out), 0, "{}", stderr(&out));

    let line = w
        .line(std::time::Duration::from_secs(8))
        .expect("a second CQ-NEW line after the next scan");
    assert!(line.starts_with("CQ-NEW "), "{line}");
    assert_eq!(event_id(&line), info);
    // Only once: later scans see the same modified time.
    assert!(w.lines_for(std::time::Duration::from_secs(5)).is_empty());
}

/// `cq requeue` waits for the recipient's claim lock, so no claim of the
/// requeued message can start before its old reason file is removed.
#[test]
fn requeue_holds_the_claim_lock() {
    let env = Env::new();
    let info = env.send("alice", "bob", "info", "hello", "x");
    assert_eq!(code(&env.claim("bob", &info)), 0);
    assert_eq!(code(&reject(&env, "bob", &info, "no")), 0);

    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(env.path("heartbeat/bob.claim.lock"))
        .unwrap();
    lock.lock().unwrap();
    let mut c = env.cmd(&["requeue", "--as", "bob", &info]);
    c.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = c.spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(700));
    assert!(
        env.ids("inbox/bob").is_empty(),
        "requeue waits for the lock"
    );
    assert_eq!(env.ids("rejected/bob"), vec![info.clone()]);

    lock.unlock().unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(env.ids("inbox/bob"), vec![info.clone()]);
    assert!(
        !env.path(&format!("rejected/bob/{info}.reason.json"))
            .exists()
    );
}
