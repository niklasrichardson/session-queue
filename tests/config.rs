//! `config.toml` in the queue root: its settings, their order against the
//! environment variables, and the errors for a file that cannot be used.

mod common;

use std::fs;
use std::process::{Output, Stdio};

use common::*;

const PARIS: &str = "tz = 'Europe/Paris'\ntz_label = 'Paris'\n";

fn write_config(env: &Env, text: &str) {
    fs::write(env.path("config.toml"), text).unwrap();
}

/// `cq` without the harness's `CQ_TZ`, `CQ_TZ_LABEL` and `CQ_RULES`.
fn run_without_env(env: &Env, args: &[&str]) -> Output {
    let mut c = env.cmd(args);
    c.env_remove("CQ_TZ")
        .env_remove("CQ_TZ_LABEL")
        .env_remove("CQ_RULES")
        .stdin(Stdio::null());
    c.output().unwrap()
}

/// The first line of `cq list --as b` for one planted message, sent at
/// 12:00 UTC.
fn list_line(out: &Output) -> String {
    assert_eq!(code(out), 0, "{}", stderr(out));
    stdout(out).lines().next().unwrap().to_owned()
}

fn plant_noon(env: &Env) {
    let id = "20261201T120000Z-a-0001";
    env.plant("b", id, v1(id, "a", "b", "info", 0).to_string().as_bytes());
}

#[test]
fn tz_and_label_come_from_the_file_unless_the_variables_are_set() {
    let env = Env::new();
    plant_noon(&env);
    write_config(&env, PARIS);

    let line = list_line(&run_without_env(&env, &["list", "--as", "b"]));
    assert!(line.contains("(13:00 Paris) "), "{line}");
    // The harness sets CQ_TZ=Europe/London and CQ_TZ_LABEL=UK, which win.
    let line = list_line(&env.run(&["list", "--as", "b"], None));
    assert!(line.contains("(12:00 UK) "), "{line}");
}

#[test]
fn help_shows_the_files_zone() {
    let env = Env::new();
    write_config(&env, PARIS);
    let out = run_without_env(&env, &["--help"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(
        stdout(&out).contains("Europe/Paris, labelled Paris"),
        "{}",
        stdout(&out)
    );
}

/// The reminder names the file's rules path, made relative to the root,
/// unless `CQ_RULES` is set.
#[test]
fn rules_come_from_the_file_unless_cq_rules_is_set() {
    let env = Env::new();
    write_config(&env, "rules = 'SESSION-RULES.md'\n");
    fs::write(env.path("members/local_cfg"), "b\n").unwrap();
    let reminder = |with_env: bool| {
        let mut c = env.cmd(&["session-start"]);
        if !with_env {
            c.env_remove("CQ_RULES");
        }
        c.env("CLAUDE_CODE_HOST_SESSION_ID", "local_cfg")
            .stdin(Stdio::null());
        let out = c.output().unwrap();
        assert_eq!(code(&out), 0, "{}", stderr(&out));
        stdout(&out)
    };

    let from_file = env.path("SESSION-RULES.md").display().to_string();
    let text = reminder(false);
    assert!(text.contains(&format!("re-read {from_file} and")), "{text}");
    let text = reminder(true);
    assert!(text.contains(&format!("re-read {RULES} and")), "{text}");
}

/// An unknown key stops every command that uses the queue, a watcher
/// included, with exit 64 and a message that names the file and the key.
/// `init` and the hooks still run.
#[test]
fn an_unknown_key_is_refused_by_name() {
    let env = Env::new();
    plant_noon(&env);
    write_config(&env, "tz = 'Europe/Paris'\ntz_lable = 'Paris'\n");

    for args in [
        &["list", "--as", "b"][..],
        &["watch", "--as", "b", "--max-seconds", "1"],
        &["status", "--as", "b"],
    ] {
        let out = env.run(args, None);
        assert_eq!(code(&out), 64, "{args:?}: {}", stderr(&out));
        let err = stderr(&out);
        assert!(err.starts_with("cq: "), "{err}");
        assert!(
            err.contains("config.toml: unknown key \"tz_lable\""),
            "{err}"
        );
    }
    let out = env.send_raw(&["--from", "a", "--to", "b", "--type", "info"], "s", "b");
    assert_eq!(code(&out), 64, "{}", stderr(&out));
    assert!(!env.path("heartbeat/b").exists(), "the watcher started");

    let out = env.run(&["init"], None);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let out = env.run(&["session-start"], None);
    assert_eq!(code(&out), 0);
    assert_eq!(stderr(&out), "");
}

#[test]
fn a_malformed_file_or_unknown_zone_is_an_error_naming_the_file() {
    let env = Env::new();
    for text in ["tz = \n", "tz = 5\n", "tz = 'Mars/Olympus'\n", "[rules]\n"] {
        write_config(&env, text);
        let out = env.run(&["list", "--as", "b"], None);
        assert_eq!(code(&out), 64, "{text:?}: {}", stderr(&out));
        let want = format!("cq: {}: ", env.path("config.toml").display());
        assert!(
            stderr(&out).starts_with(&want),
            "{text:?}: {}",
            stderr(&out)
        );
    }
}

#[test]
fn a_missing_or_empty_file_changes_nothing() {
    let env = Env::new();
    plant_noon(&env);
    let line = list_line(&env.run(&["list", "--as", "b"], None));
    assert!(line.contains("(12:00 UK) "), "{line}");
    write_config(&env, "# no settings\n");
    let line = list_line(&env.run(&["list", "--as", "b"], None));
    assert!(line.contains("(12:00 UK) "), "{line}");
}
