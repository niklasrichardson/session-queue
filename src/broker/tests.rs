use std::cell::RefCell;
use std::rc::Rc;

use super::*;
use crate::inbox::fake_clock::FakeClock;
use crate::send::{self, Hops, Outgoing};

const SID: &str = "S-1-15-3-1024-1-2-3-4-5-6-7-8";
const NAME: &str = "agy-w";
const CONV: &str = "conv-1";
const SECRET: &str = "SECRET";

type ReplyFn = Rc<dyn Fn(&Value) -> Vec<u8>>;

struct FakeState {
    specs: Vec<TurnSpec>,
    requests: Vec<Value>,
    spawn_errors: u32,
    exit: Option<i32>,
    status: String,
    conversation: String,
    reply: Option<ReplyFn>,
    hang: bool,
    running: bool,
    stops: u32,
    /// After the spawn, turn the record's state.json into a folder, so the
    /// `running` write fails; the old file is kept in `saved_state`.
    break_state: bool,
    saved_state: Option<Vec<u8>>,
}

/// A turn runner that writes agy's stdout and, if set, `reply.json`.
#[derive(Clone)]
struct Fake(Rc<RefCell<FakeState>>);

impl Fake {
    fn new() -> Self {
        Fake(Rc::new(RefCell::new(FakeState {
            specs: Vec::new(),
            requests: Vec::new(),
            spawn_errors: 0,
            exit: Some(0),
            status: SUCCESS.into(),
            conversation: CONV.into(),
            reply: None,
            hang: false,
            running: false,
            stops: 0,
            break_state: false,
            saved_state: None,
        })))
    }
    fn set(&self, f: impl FnOnce(&mut FakeState)) {
        f(&mut self.0.borrow_mut());
    }
}

impl TurnRunner for Fake {
    fn start(&mut self, spec: &TurnSpec) -> io::Result<Option<u32>> {
        let mut s = self.0.borrow_mut();
        if s.spawn_errors > 0 {
            s.spawn_errors -= 1;
            return Err(io::Error::other("cannot spawn"));
        }
        s.specs.push(spec.clone());
        let out = json!({"status": s.status, "conversation_id": s.conversation, "response": "SECRET echo"});
        fs::write(&spec.stdout, format!("{out}\n"))?;
        let req: Value = serde_json::from_slice(&fs::read(spec.dir.join(REQUEST))?).unwrap();
        if let Some(f) = s.reply.clone() {
            fs::write(spec.dir.join(REPLY), f(&req))?;
        }
        s.requests.push(req);
        s.running = true;
        if s.break_state {
            let state = spec.stdout.with_file_name(STATE);
            s.saved_state = Some(fs::read(&state)?);
            fs::remove_file(&state)?;
            fs::create_dir(&state)?;
        }
        Ok(Some(42))
    }

    fn poll(&mut self) -> Option<io::Result<Option<i32>>> {
        let mut s = self.0.borrow_mut();
        if !s.running || s.hang {
            return None;
        }
        s.running = false;
        Some(Ok(s.exit))
    }

    fn stop(&mut self) {
        let mut s = self.0.borrow_mut();
        s.stops += 1;
        s.running = false;
    }
}

fn answer(outcome: &str, status: Option<&str>, text: Option<&str>) -> ReplyFn {
    let (o, st, t) = (
        outcome.to_owned(),
        status.map(str::to_owned),
        text.map(str::to_owned),
    );
    Rc::new(move |req| {
        let mut r = json!({"id": req["id"], "nonce": req["nonce"], "outcome": o});
        if let Some(s) = &st {
            r["status"] = json!(s);
        }
        if let Some(t) = &t {
            r["text"] = json!(t);
        }
        serde_json::to_vec(&r).unwrap()
    })
}

#[derive(Clone, Default)]
struct Log(Rc<RefCell<Vec<u8>>>);

impl Write for Log {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Log {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.borrow()).into_owned()
    }
}

type Broker = AgyBroker<Fake, FakeClock>;

struct Env {
    _dir: tempfile::TempDir,
    root: Root,
    clock: FakeClock,
    log: Log,
    fake: Fake,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path().join("cq"));
        root.init().unwrap();
        Env {
            _dir: dir,
            root,
            clock: FakeClock::new(),
            log: Log::default(),
            fake: Fake::new(),
        }
    }

    fn cfg(&self) -> Config {
        Config {
            name: NAME.into(),
            conversation: CONV.into(),
            allow_from: vec!["boss".into()],
            exchange: default_exchange(&self.root, NAME),
            sandbox_sid: Some(SID.into()),
            bin: "agy".into(),
            prompt: None,
        }
    }

    fn broker(&self) -> Broker {
        AgyBroker::with(
            self.root.clone(),
            self.cfg(),
            self.fake.clone(),
            self.clock.clone(),
            Box::new(self.log.clone()),
        )
        .unwrap()
    }

    fn send(&self, from: &str, kind: MsgType) -> String {
        let out = Outgoing {
            from: from.into(),
            to: NAME.into(),
            kind,
            subject: format!("{SECRET} subject"),
            body: format!("{SECRET} body"),
            reply_to: None,
            parent: None,
            status: None,
        };
        send::send(&self.root, out, Hops::Lookup).unwrap()
    }

    fn record(&self, id: &str) -> PathBuf {
        job::records(&self.root, NAME).join(id)
    }

    fn state(&self, id: &str) -> JobState {
        read_record(&self.record(id)).unwrap()
    }

    fn inbox(&self, name: &str) -> Vec<Message> {
        let dir = self.root.inbox(name);
        fsutil::json_stems(&dir, is_valid_id)
            .iter()
            .map(|id| serde_json::from_slice(&fs::read(message_file(&dir, id)).unwrap()).unwrap())
            .collect()
    }

    fn claimed(&self, id: &str) -> bool {
        message_file(&self.root.processing(NAME), id).exists()
    }

    /// Run a hanging task to phase `completing`, held there by PAUSE, then
    /// drop the watcher.
    fn completing(&self) -> String {
        self.fake.set(|s| {
            s.reply = Some(answer("reply", Some("ok"), Some("fine")));
            s.hang = true;
        });
        let id = self.send("boss", MsgType::Task);
        let mut b = self.broker();
        b.tick().unwrap();
        fs::write(self.root.pause(), "").unwrap();
        self.fake.set(|s| s.hang = false);
        b.tick().unwrap();
        assert_eq!(self.state(&id).phase, Phase::Completing);
        drop(b);
        fs::remove_file(self.root.pause()).unwrap();
        id
    }
}

fn tick_err(b: &mut Broker) -> CqError {
    let e = b.tick().expect_err("the tick must fail");
    b.failed(e).expect_err("failed() gives the stored error")
}

fn rewrite_state(env: &Env, id: &str, f: impl FnOnce(&mut Value)) {
    let path = env.record(id).join(STATE);
    let mut v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    f(&mut v);
    fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
}

// --- The verdict table -------------------------------------------------------

fn reply(outcome: Outcome, status: Option<Status>, text: Option<&str>) -> Reply {
    Reply {
        id: "i".into(),
        nonce: "n".into(),
        outcome,
        status,
        text: text.map(str::to_owned),
    }
}

fn exited(code: i32, status: &str, r: Option<Reply>) -> Verdict {
    Verdict {
        end: End::Exited,
        exit_code: Some(code),
        agy_status: Some(status.into()),
        conversation: Conversation::Same,
        reply_check: if r.is_some() {
            ReplyCheck::Valid
        } else {
            ReplyCheck::Missing
        },
        reply_problem: None,
        reply: r,
    }
}

#[test]
fn decide_follows_the_verdict_table() {
    let task = MsgType::Task;
    let ok = reply(Outcome::Reply, Some(Status::Ok), Some("done"));
    assert_eq!(
        decide(task, &exited(0, SUCCESS, Some(ok.clone())), "i", "n"),
        Ok(Finish::Reply {
            status: Status::Ok,
            text: "done".into()
        })
    );
    let no = reply(Outcome::Reject, None, Some("out of scope"));
    assert_eq!(
        decide(task, &exited(0, SUCCESS, Some(no)), "i", "n"),
        Ok(Finish::Reject {
            text: "out of scope".into()
        })
    );
    assert_eq!(
        decide(task, &exited(0, SUCCESS, None), "i", "n"),
        Ok(Finish::Reply {
            status: Status::Failed,
            text: NO_REPLY.into()
        })
    );
    assert_eq!(
        decide(task, &exited(1, "ERROR", None), "i", "n"),
        Ok(Finish::Reply {
            status: Status::Failed,
            text: turn_failed("status ERROR")
        })
    );
    assert_eq!(
        decide(task, &exited(3, SUCCESS, None), "i", "n"),
        Ok(Finish::Reply {
            status: Status::Failed,
            text: turn_failed("exit 3")
        })
    );
    // A failed turn with a valid reply, or a reply for another job.
    assert!(decide(task, &exited(1, "ERROR", Some(ok.clone())), "i", "n").is_err());
    assert!(decide(task, &exited(0, SUCCESS, Some(ok.clone())), "i", "other").is_err());
    // A turn that did not end on its own, or ran elsewhere.
    assert!(decide(task, &Verdict::not_read(End::TimedOut), "i", "n").is_err());
    assert!(decide(task, &Verdict::not_read(End::Stopped), "i", "n").is_err());
    for c in [Conversation::Other, Conversation::Unknown] {
        let mut v = exited(0, SUCCESS, Some(ok.clone()));
        v.conversation = c;
        assert!(decide(task, &v, "i", "n").is_err(), "{c:?}");
    }
    // Info and results are finished without a reply, read or not.
    let read = reply(Outcome::Read, None, None);
    for kind in [MsgType::Info, MsgType::Result] {
        assert_eq!(
            decide(kind, &exited(0, SUCCESS, Some(read.clone())), "i", "n"),
            Ok(Finish::Done)
        );
        assert_eq!(
            decide(kind, &exited(1, "ERROR", None), "i", "n"),
            Ok(Finish::Done)
        );
        assert!(decide(kind, &Verdict::not_read(End::TimedOut), "i", "n").is_err());
    }
}

#[test]
fn check_reply_takes_only_a_strict_reply_for_this_job() {
    let task = MsgType::Task;
    let good = br#"{"id":"i","nonce":"n","outcome":"reply","status":"ok","text":"t"}"#;
    assert!(check_reply(good, task, "i", "n").is_ok());
    let mut bom = vec![0xEF, 0xBB, 0xBF];
    bom.extend_from_slice(good);
    assert!(check_reply(&bom, task, "i", "n").is_ok());
    let utf16: Vec<u8> = [0xFF, 0xFE]
        .into_iter()
        .chain(good.iter().flat_map(|&b| [b, 0]))
        .collect();
    assert!(
        check_reply(&utf16, task, "i", "n")
            .unwrap_err()
            .contains("UTF-16")
    );
    assert!(check_reply(b"{\"id\":\"i\"\0}", task, "i", "n").is_err());
    assert!(check_reply(&[0xC3, 0x28], task, "i", "n").is_err());
    assert!(check_reply(good, task, "i", "m").is_err());
    assert!(check_reply(good, task, "j", "n").is_err());
    let bad = [
        r#"{"id":"i","nonce":"n","outcome":"reply","status":"ok","text":"t","x":1}"#,
        r#"{"id":"i","nonce":"n","outcome":"reply","text":"t"}"#,
        r#"{"id":"i","nonce":"n","outcome":"reply","status":"rejected","text":"t"}"#,
        r#"{"id":"i","nonce":"n","outcome":"reply","status":"ok","text":"  "}"#,
        r#"{"id":"i","nonce":"n","outcome":"reject","status":"ok","text":"t"}"#,
        r#"{"id":"i","nonce":"n","outcome":"reject"}"#,
        r#"{"id":"i","nonce":"n","outcome":"read"}"#,
    ];
    for b in bad {
        assert!(check_reply(b.as_bytes(), task, "i", "n").is_err(), "{b}");
    }
    let long = format!(
        r#"{{"id":"i","nonce":"n","outcome":"reply","status":"ok","text":"{}"}}"#,
        "x".repeat(MAX_BODY_BYTES + 1)
    );
    assert!(check_reply(long.as_bytes(), task, "i", "n").is_err());
    // Info: only `read`, with no status or text.
    let info = MsgType::Info;
    assert!(
        check_reply(
            br#"{"id":"i","nonce":"n","outcome":"read"}"#,
            info,
            "i",
            "n"
        )
        .is_ok()
    );
    assert!(
        check_reply(
            br#"{"id":"i","nonce":"n","outcome":"read","text":"x"}"#,
            info,
            "i",
            "n"
        )
        .is_err()
    );
    assert!(check_reply(good, info, "i", "n").is_err());
}

#[test]
fn verdict_reads_agy_status_conversation_and_the_reply() {
    let out = format!("noise\n{{\"status\":\"SUCCESS\",\"conversation_id\":\"{CONV}\"}}\n");
    let ok = br#"{"id":"i","nonce":"n","outcome":"read"}"#.to_vec();
    let v = verdict(
        Some(0),
        &out,
        CONV,
        FileRead::Bytes(ok),
        MsgType::Info,
        "i",
        "n",
    );
    assert_eq!(
        (v.conversation, v.reply_check),
        (Conversation::Same, ReplyCheck::Valid)
    );
    assert_eq!(v.agy_status.as_deref(), Some(SUCCESS));
    let v = verdict(
        Some(0),
        &out,
        "conv-2",
        FileRead::TooBig,
        MsgType::Info,
        "i",
        "n",
    );
    assert_eq!(
        (v.conversation, v.reply_check),
        (Conversation::Other, ReplyCheck::Invalid)
    );
    let v = verdict(
        Some(1),
        "",
        CONV,
        FileRead::Missing,
        MsgType::Task,
        "i",
        "n",
    );
    assert_eq!(
        (v.conversation, v.reply_check),
        (Conversation::Unknown, ReplyCheck::Missing)
    );
    // agy's status is kept only as a plain word.
    let odd = "{\"status\":\"line\\nbreak\",\"conversation_id\":\"conv-1\"}\n";
    let v = verdict(
        Some(1),
        odd,
        CONV,
        FileRead::Missing,
        MsgType::Task,
        "i",
        "n",
    );
    assert_eq!(v.agy_status.as_deref(), Some("INVALID"));
}

#[test]
fn the_prompt_holds_paths_only_and_a_template_needs_both_paths() {
    let dir = Path::new("C:\\x\\ex\\ID1");
    let p = prompt(NAME, None, dir);
    assert!(p.contains("C:/x/ex/ID1/request.json") && p.contains("C:/x/ex/ID1/reply.json"));
    let t = prompt(NAME, Some("{name}: read {request}, write {reply}"), dir);
    assert_eq!(
        t,
        "agy-w: read C:/x/ex/ID1/request.json, write C:/x/ex/ID1/reply.json"
    );
    let env = Env::new();
    let mut cfg = env.cfg();
    cfg.prompt = Some("read {request}".into());
    let e = AgyBroker::with(
        env.root.clone(),
        cfg,
        env.fake.clone(),
        env.clock.clone(),
        Box::new(Log::default()),
    )
    .err()
    .unwrap();
    assert_eq!(e.code, exit::USAGE);
    let mut cfg = env.cfg();
    cfg.allow_from.clear();
    assert!(
        AgyBroker::with(
            env.root.clone(),
            cfg,
            env.fake.clone(),
            env.clock.clone(),
            Box::new(Log::default())
        )
        .is_err()
    );
    let mut cfg = env.cfg();
    cfg.sandbox_sid = Some("S-1-x".into());
    if cfg!(windows) {
        assert!(
            AgyBroker::with(
                env.root.clone(),
                cfg,
                env.fake.clone(),
                env.clock.clone(),
                Box::new(Log::default())
            )
            .is_err()
        );
    }
}

#[test]
fn the_exchange_path_rules() {
    let root = Root::at(PathBuf::from(if cfg!(windows) {
        "D:\\q\\root"
    } else {
        "/q/root"
    }));
    let p = |s: &str| root.path().join(s);
    assert!(check_exchange_path(&root, NAME, Path::new("rel/ex")).is_err());
    assert!(check_exchange_path(&root, NAME, root.path()).is_err());
    assert!(check_exchange_path(&root, NAME, root.path().parent().unwrap()).is_err());
    assert!(check_exchange_path(&root, NAME, &p("inbox")).is_err());
    assert!(check_exchange_path(&root, NAME, &p("agy-exchange")).is_err());
    assert!(check_exchange_path(&root, NAME, &default_exchange(&root, NAME)).is_ok());
    let outside = root.path().parent().unwrap().join("ex");
    assert!(check_exchange_path(&root, NAME, &outside).is_ok());
}

// --- Jobs ---------------------------------------------------------------------

#[test]
fn a_task_gets_the_turns_reply_and_no_message_text_reaches_the_prompt_or_log() {
    let env = Env::new();
    env.fake
        .set(|s| s.reply = Some(answer("reply", Some("ok"), Some("all done"))));
    let id = env.send("boss", MsgType::Task);
    let mut b = env.broker();
    b.tick().unwrap();

    let st = env.state(&id);
    assert_eq!(st.phase, Phase::Completed);
    assert_eq!(st.pid, Some(42));
    assert!(message_file(&env.root.done(NAME), &id).exists());
    let replies = env.inbox("boss");
    assert_eq!(replies.len(), 1);
    assert_eq!(
        (replies[0].status, replies[0].body.as_str()),
        (Some(Status::Ok), "all done")
    );
    assert!(!default_exchange(&env.root, NAME).join(&id).exists());
    assert!(env.record(&id).join(COMPLETION).exists());

    let s = env.fake.0.borrow();
    let spec = &s.specs[0];
    let dir = spec.dir.display().to_string();
    assert!(
        spec.args
            .windows(2)
            .any(|w| w[0] == "--add-dir" && w[1] == dir)
    );
    assert!(
        spec.args
            .windows(2)
            .any(|w| w[0] == "--conversation" && w[1] == CONV)
    );
    let req = &s.requests[0];
    assert_eq!(req["schema"], REQUEST_SCHEMA);
    assert_eq!(req["outcomes"], json!(["reply", "reject"]));
    assert!(req["message"].get("from_session_id").is_none());
    assert_eq!(req["message"]["body"], format!("{SECRET} body"));
    let seen = format!("{:?}\n{}", spec.args, env.log.text());
    assert!(!seen.contains(SECRET), "{seen}");
    assert!(env.log.text().contains(&format!("agy job {id} status=ok")));
}

#[test]
fn a_sender_outside_the_allow_list_gets_no_turn() {
    let env = Env::new();
    let task = env.send("stranger", MsgType::Task);
    let mut b = env.broker();
    b.tick().unwrap();
    let info = env.send("stranger", MsgType::Info);
    b.line(&format!("CQ-NEW {info}")).unwrap();
    b.tick().unwrap();
    assert!(env.fake.0.borrow().specs.is_empty());
    assert!(message_file(&env.root.rejected(NAME), &task).exists());
    assert!(message_file(&env.root.done(NAME), &info).exists());
    let replies = env.inbox("stranger");
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].body, NOT_ALLOWED);
    assert_eq!(env.state(&info).phase, Phase::Completed);
}

#[test]
fn an_info_message_is_read_and_a_bad_task_reply_fails_the_task() {
    let env = Env::new();
    env.fake.set(|s| s.reply = Some(answer("read", None, None)));
    let info = env.send("boss", MsgType::Info);
    let mut b = env.broker();
    b.tick().unwrap();
    assert!(message_file(&env.root.done(NAME), &info).exists());
    assert!(
        env.log
            .text()
            .contains(&format!("agy job {info} status=read"))
    );
    assert!(env.inbox("boss").is_empty());

    env.fake
        .set(|s| s.reply = Some(answer("reply", Some("ok"), Some(" "))));
    let task = env.send("boss", MsgType::Task);
    b.line("CQ-NEW x").unwrap();
    b.tick().unwrap();
    let replies = env.inbox("boss");
    assert_eq!(
        (replies[0].status, replies[0].body.as_str()),
        (Some(Status::Failed), NO_REPLY)
    );
    let v = env.state(&task).verdict.unwrap();
    assert_eq!(v.reply_check, ReplyCheck::Invalid);
}

#[test]
fn a_spawn_failure_is_tried_again_after_1_2_and_4_seconds() {
    let env = Env::new();
    env.fake.set(|s| {
        s.spawn_errors = 3;
        s.reply = Some(answer("reply", Some("ok"), Some("ok")));
    });
    let id = env.send("boss", MsgType::Task);
    let mut b = env.broker();
    b.tick().unwrap();
    assert_eq!(env.state(&id).phase, Phase::Prepared);
    assert!(env.state(&id).launch.is_none());
    for wait in [1, 2, 4] {
        b.tick().unwrap();
        env.clock.advance(Duration::from_millis(wait * 1000 - 1));
        b.tick().unwrap();
        assert!(env.fake.0.borrow().specs.is_empty());
        env.clock.advance(Duration::from_millis(1));
        b.tick().unwrap();
    }
    assert_eq!(env.state(&id).phase, Phase::Completed);

    // Four in a row: exit 11, and the job stays in phase prepared.
    env.fake.set(|s| s.spawn_errors = 4);
    let id = env.send("boss", MsgType::Task);
    b.line("CQ-NEW x").unwrap();
    b.tick().unwrap();
    for wait in [1, 2] {
        env.clock.advance(Duration::from_secs(wait));
        b.tick().unwrap();
    }
    env.clock.advance(Duration::from_secs(4));
    assert_eq!(tick_err(&mut b).code, exit::AGY_TURN_FAILED);
    assert_eq!(env.state(&id).phase, Phase::Prepared);
    assert!(env.claimed(&id));
}

#[test]
fn a_turn_past_the_timeout_is_stopped_and_held() {
    let env = Env::new();
    env.fake.set(|s| s.hang = true);
    let id = env.send("boss", MsgType::Task);
    let mut b = env.broker();
    b.tick().unwrap();
    assert_eq!(env.state(&id).phase, Phase::Running);
    assert!(env.state(&id).launch.is_some());
    env.clock.advance(TURN_TIMEOUT);
    assert_eq!(tick_err(&mut b).code, exit::AGY_TURN_FAILED);
    let st = env.state(&id);
    assert_eq!(st.phase, Phase::NeedsReview);
    assert_eq!(st.held_in, Some(Phase::Ended));
    assert_eq!(st.verdict.unwrap().end, End::TimedOut);
    assert_eq!(env.fake.0.borrow().stops, 1);
    assert!(env.claimed(&id));
    assert!(env.inbox("boss").is_empty());
}

#[test]
fn pause_holds_new_jobs_launches_and_completions() {
    let env = Env::new();
    env.fake
        .set(|s| s.reply = Some(answer("reply", Some("ok"), Some("ok"))));
    let id = env.send("boss", MsgType::Task);
    fs::write(env.root.pause(), "").unwrap();
    let mut b = env.broker();
    b.tick().unwrap();
    assert!(!env.claimed(&id));
    fs::remove_file(env.root.pause()).unwrap();
    env.fake.set(|s| s.spawn_errors = 1);
    b.tick().unwrap();
    assert_eq!(env.state(&id).phase, Phase::Prepared);
    fs::write(env.root.pause(), "").unwrap();
    env.clock.advance(Duration::from_secs(10));
    b.tick().unwrap();
    assert!(env.fake.0.borrow().specs.is_empty());
    fs::remove_file(env.root.pause()).unwrap();
    b.tick().unwrap();
    assert_eq!(env.state(&id).phase, Phase::Completed);
}

#[test]
fn a_watcher_stopped_during_a_turn_stops_it_and_holds_the_job() {
    let env = Env::new();
    env.fake.set(|s| s.hang = true);
    let id = env.send("boss", MsgType::Task);
    let mut b = env.broker();
    b.tick().unwrap();
    assert!(b.stopped("interrupted", Ended::Normal));
    let st = env.state(&id);
    assert_eq!(st.phase, Phase::NeedsReview);
    assert_eq!(st.verdict.unwrap().end, End::Stopped);
    assert_eq!(env.fake.0.borrow().stops, 1);
    drop(b);
    // The next start refuses until a person has looked.
    assert_eq!(tick_err(&mut env.broker()).code, exit::REFUSED);
}

#[test]
fn a_record_write_that_fails_after_the_spawn_stops_the_turn_once() {
    let env = Env::new();
    env.fake.set(|s| {
        s.hang = true;
        s.break_state = true;
    });
    let id = env.send("boss", MsgType::Task);
    let mut b = env.broker();
    let e = tick_err(&mut b);
    // The original error, not the hold that could not be written.
    assert!(e.message.contains(STATE), "{}", e.message);
    assert!(!e.message.contains("needs review"), "{}", e.message);
    assert_eq!(env.fake.0.borrow().stops, 1);
    assert!(b.stopped("error", Ended::Normal));
    assert_eq!(env.fake.0.borrow().stops, 1);
    drop(b);
    // The record as the failed write left it: `launching`.
    let state = env.record(&id).join(STATE);
    fs::remove_dir(&state).unwrap();
    let saved = env.fake.0.borrow().saved_state.clone().unwrap();
    fs::write(&state, saved).unwrap();
    assert_eq!(env.state(&id).phase, Phase::Launching);
    // The next start holds it and never runs the task again.
    assert_eq!(tick_err(&mut env.broker()).code, exit::AGY_TURN_FAILED);
    assert_eq!(env.state(&id).held_in, Some(Phase::Launching));
    assert_eq!(env.fake.0.borrow().specs.len(), 1);
    assert!(env.claimed(&id));
}

// --- Start-up and recovery -------------------------------------------------------

#[test]
fn a_record_left_launching_or_running_is_held() {
    let env = Env::new();
    env.fake.set(|s| s.hang = true);
    let id = env.send("boss", MsgType::Task);
    env.broker().tick().unwrap();
    let mut b = env.broker();
    assert_eq!(tick_err(&mut b).code, exit::AGY_TURN_FAILED);
    let st = env.state(&id);
    assert_eq!(
        (st.phase, st.held_in),
        (Phase::NeedsReview, Some(Phase::Running))
    );
    assert!(env.claimed(&id));
    drop(b);

    // Launching too: the turn may have started.
    rewrite_state(&env, &id, |v| {
        v["phase"] = json!("launching");
        v["held_in"] = Value::Null;
    });
    assert_eq!(tick_err(&mut env.broker()).code, exit::AGY_TURN_FAILED);
    assert_eq!(env.state(&id).held_in, Some(Phase::Launching));
}

#[test]
fn a_record_left_prepared_is_prepared_again_with_a_new_nonce() {
    let env = Env::new();
    env.fake.set(|s| {
        s.spawn_errors = 1;
        s.reply = Some(answer("reply", Some("ok"), Some("ok")));
    });
    let id = env.send("boss", MsgType::Task);
    env.broker().tick().unwrap();
    let old = env.state(&id).nonce.unwrap();
    let mut b = env.broker();
    b.tick().unwrap();
    let st = env.state(&id);
    assert_eq!(st.phase, Phase::Completed);
    assert_ne!(st.nonce.as_deref(), Some(old.as_str()));
    assert_eq!(env.fake.0.borrow().requests[0]["nonce"], json!(st.nonce));
}

#[test]
fn a_record_left_claimed_with_a_job_folder_is_prepared_again() {
    // A stop after the job folder (and maybe request.json) was made but
    // before `prepared` was saved.
    for keep_request in [true, false] {
        let env = Env::new();
        env.fake.set(|s| s.spawn_errors = 1);
        let id = env.send("boss", MsgType::Task);
        env.broker().tick().unwrap();
        let old = env.state(&id).nonce.unwrap();
        let folder = default_exchange(&env.root, NAME).join(&id);
        if !keep_request {
            fs::remove_file(folder.join(REQUEST)).unwrap();
        }
        rewrite_state(&env, &id, |v| {
            v["phase"] = json!("claimed");
            v["nonce"] = Value::Null;
        });
        env.fake
            .set(|s| s.reply = Some(answer("reply", Some("ok"), Some("ok"))));
        let mut b = env.broker();
        b.tick().unwrap();
        let st = env.state(&id);
        assert_eq!(st.phase, Phase::Completed, "keep_request={keep_request}");
        assert_ne!(st.nonce.as_deref(), Some(old.as_str()));
        assert_eq!(env.fake.0.borrow().requests[0]["nonce"], json!(st.nonce));
        assert!(!folder.exists());
    }
}

#[test]
fn a_record_left_completing_is_finished_once_its_completion_matches() {
    let env = Env::new();
    let id = env.completing();
    env.broker().tick().unwrap();
    assert_eq!(env.state(&id).phase, Phase::Completed);
    assert_eq!(env.inbox("boss").len(), 1);
    assert!(!default_exchange(&env.root, NAME).join(&id).exists());
}

#[test]
fn a_completion_that_does_not_match_the_verdict_is_held() {
    let env = Env::new();
    let id = env.completing();
    let path = env.record(&id).join(COMPLETION);
    let text = fs::read_to_string(&path).unwrap().replace("fine", "other");
    fs::write(&path, text).unwrap();
    assert_eq!(tick_err(&mut env.broker()).code, exit::AGY_TURN_FAILED);
    assert_eq!(env.state(&id).phase, Phase::NeedsReview);
    assert!(env.inbox("boss").is_empty());
}

#[test]
fn a_record_left_ended_is_decided_from_its_stored_verdict() {
    let env = Env::new();
    let id = env.completing();
    fs::remove_file(env.record(&id).join(COMPLETION)).unwrap();
    rewrite_state(&env, &id, |v| v["phase"] = json!("ended"));
    env.broker().tick().unwrap();
    assert_eq!(env.state(&id).phase, Phase::Completed);
    let replies = env.inbox("boss");
    assert_eq!(replies[0].body, "fine");
}

#[test]
fn a_torn_or_unknown_record_is_refused() {
    let env = Env::new();
    env.fake
        .set(|s| s.reply = Some(answer("reply", Some("ok"), Some("ok"))));
    let id = env.send("boss", MsgType::Task);
    env.broker().tick().unwrap();
    let path = env.record(&id).join(STATE);
    let good = fs::read(&path).unwrap();
    fs::write(&path, &good[..good.len() / 2]).unwrap();
    assert_eq!(tick_err(&mut env.broker()).code, exit::REFUSED);
    fs::write(&path, &good).unwrap();
    rewrite_state(&env, &id, |v| v["extra"] = json!(1));
    assert_eq!(tick_err(&mut env.broker()).code, exit::REFUSED);
    fs::write(&path, &good).unwrap();
    rewrite_state(&env, &id, |v| v["phase"] = json!("dreaming"));
    assert_eq!(tick_err(&mut env.broker()).code, exit::REFUSED);
    fs::write(&path, &good).unwrap();
    rewrite_state(&env, &id, |v| v["schema"] = json!("cq-agy-job/2"));
    assert_eq!(tick_err(&mut env.broker()).code, exit::REFUSED);
    fs::remove_file(&path).unwrap();
    assert_eq!(tick_err(&mut env.broker()).code, exit::REFUSED);
}

#[test]
fn a_claimed_message_without_a_record_or_two_open_records_are_refused() {
    let env = Env::new();
    env.fake.set(|s| s.hang = true);
    let id = env.send("boss", MsgType::Task);
    env.broker().tick().unwrap();
    let saved = env.root.path().join("saved");
    fs::rename(env.record(&id), &saved).unwrap();
    assert_eq!(tick_err(&mut env.broker()).code, exit::REFUSED);
    fs::rename(&saved, env.record(&id)).unwrap();

    // A second unfinished record.
    let other = "20260101T000000Z-boss-ab12";
    assert!(is_valid_id(other), "test ID");
    fs::create_dir(env.record(other)).unwrap();
    let mut v: Value =
        serde_json::from_slice(&fs::read(env.record(&id).join(STATE)).unwrap()).unwrap();
    v["id"] = json!(other);
    fs::write(
        env.record(other).join(STATE),
        serde_json::to_vec(&v).unwrap(),
    )
    .unwrap();
    let e = tick_err(&mut env.broker());
    assert_eq!(e.code, exit::REFUSED);
    assert!(e.message.contains("more than one"), "{}", e.message);
}

#[test]
fn an_unexpected_exchange_entry_is_refused_and_a_completed_leftover_removed() {
    let env = Env::new();
    env.fake
        .set(|s| s.reply = Some(answer("reply", Some("ok"), Some("ok"))));
    let id = env.send("boss", MsgType::Task);
    env.broker().tick().unwrap();
    let ex = default_exchange(&env.root, NAME);
    // A leftover folder of a completed job is removed at start.
    fs::create_dir(ex.join(&id)).unwrap();
    env.broker().tick().unwrap();
    assert!(!ex.join(&id).exists());
    fs::create_dir(ex.join("junk")).unwrap();
    assert_eq!(tick_err(&mut env.broker()).code, exit::USAGE);
    fs::remove_dir(ex.join("junk")).unwrap();
    fs::write(ex.join("file.txt"), "x").unwrap();
    assert_eq!(tick_err(&mut env.broker()).code, exit::USAGE);
}

#[cfg(windows)]
#[test]
fn a_start_through_a_junction_makes_no_folder_at_its_target() {
    let env = Env::new();
    let real = env._dir.path().join("real");
    fs::create_dir(&real).unwrap();
    let link = env._dir.path().join("jn");
    let ok = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(&link)
        .arg(&real)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "mklink /J failed");
    let mut cfg = env.cfg();
    cfg.exchange = link.join("missing").join("ex");
    let mut b = AgyBroker::with(
        env.root.clone(),
        cfg,
        env.fake.clone(),
        env.clock.clone(),
        Box::new(env.log.clone()),
    )
    .unwrap();
    let e = tick_err(&mut b);
    assert_eq!(e.code, exit::USAGE, "{}", e.message);
    assert_eq!(fs::read_dir(&real).unwrap().count(), 0);
}
