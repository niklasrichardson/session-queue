//! Settings in `config.toml` in the queue root.
//!
//! Each setting comes from the first of: the environment variable (`CQ_RULES`,
//! `CQ_TZ`, `CQ_TZ_LABEL`), this file, the build's default (`CQ_RULES_PATH`,
//! `CQ_DEFAULT_TZ`, `CQ_DEFAULT_TZ_LABEL`), then the built-in default. No
//! setting has a command-line flag yet. The file cannot move the root: the
//! root must be known before the file can be found.
//!
//! A file that cannot be used never stops a command: these are not security
//! settings, and a typo must not stop the queue for every session. `load`
//! returns one warning that names the file and each problem. An unknown key,
//! or a known key with a wrong value, is ignored and the other keys still
//! apply; a `tz` that is not a known zone is ignored, so the next source
//! applies. A file that is not valid TOML is ignored whole. A missing file
//! is fine.

use std::fs;
use std::io;
use std::path::Path;
use std::sync::OnceLock;

use jiff::tz::TimeZone;

use crate::root::Root;

/// The file name in the queue root.
pub const FILE: &str = "config.toml";

/// The settings in the file. Empty values count as unset.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Config {
    /// The rules file that the `session-start` reminder names. A relative
    /// path in the file is made relative to the root.
    pub rules: Option<String>,
    /// An IANA time zone name, checked when the file is read.
    pub tz: Option<String>,
    pub tz_label: Option<String>,
}

/// The keys the file may hold.
const KEYS: [&str; 3] = ["rules", "tz", "tz_label"];

impl Config {
    /// Read `config.toml` in `root`. A missing file gives no settings and no
    /// warning. Otherwise the warning, if any, names the file and every
    /// problem in it; the settings are the ones that could be used.
    pub fn load(root: &Root) -> (Config, Option<String>) {
        let path = root.path().join(FILE);
        let (config, problems) = match fs::read_to_string(&path) {
            Ok(text) => parse(&text, root.path()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return (Config::default(), None),
            Err(e) if e.kind() == io::ErrorKind::InvalidData => (
                Config::default(),
                vec!["not UTF-8 text; the file is ignored".to_owned()],
            ),
            Err(e) => (
                Config::default(),
                vec![format!("cannot read it ({e}); the file is ignored")],
            ),
        };
        let warning =
            (!problems.is_empty()).then(|| format!("{}: {}", path.display(), problems.join("; ")));
        (config, warning)
    }
}

/// The settings in `text`, with a relative `rules` path joined to `root`,
/// and the problems found. Each problem says what was ignored.
fn parse(text: &str, root: &Path) -> (Config, Vec<String>) {
    let table: toml::Table = match toml::from_str(text) {
        Ok(t) => t,
        Err(e) => {
            return (
                Config::default(),
                vec![format!("{}; the file is ignored", located(text, &e))],
            );
        }
    };
    let mut problems = Vec::new();
    for key in table.keys().filter(|k| !KEYS.contains(&k.as_str())) {
        problems.push(format!(
            "unknown key {key:?} is ignored; the keys are rules, tz and tz_label"
        ));
    }
    let mut string = |key: &str| match table.get(key) {
        None => None,
        Some(toml::Value::String(s)) => non_empty(s),
        Some(other) => {
            problems.push(format!(
                "{key} must be a string, not {}; it is ignored",
                other.type_str()
            ));
            None
        }
    };
    let rules = string("rules");
    let tz = string("tz");
    let tz_label = string("tz_label");
    let tz = tz.filter(|name| {
        let known = TimeZone::get(name).is_ok();
        if !known {
            problems.push(format!(
                "tz {name:?} is not a known time zone and is ignored; use an IANA name such as Europe/London"
            ));
        }
        known
    });
    let rules = rules.map(|r| {
        if Path::new(&r).is_relative() {
            root.join(&r).display().to_string()
        } else {
            r
        }
    });
    (
        Config {
            rules,
            tz,
            tz_label,
        },
        problems,
    )
}

/// The error's message, with the line it points at.
fn located(text: &str, e: &toml::de::Error) -> String {
    match e.span() {
        Some(span) => {
            let line = text[..span.start.min(text.len())].lines().count().max(1);
            format!("line {line}: {}", e.message())
        }
        None => e.message().to_owned(),
    }
}

fn non_empty(s: &str) -> Option<String> {
    Some(s.trim().to_owned()).filter(|s| !s.is_empty())
}

static INSTALLED: OnceLock<Config> = OnceLock::new();

/// Use `config` for this process. Call it once, before the first local time
/// or rules lookup: a later call has no effect.
pub fn install(config: Config) {
    let _ = INSTALLED.set(config);
}

/// The settings given to `install`, else none.
pub fn installed() -> &'static Config {
    INSTALLED.get_or_init(Config::default)
}

/// The values for one setting, in order: the environment variable `var`,
/// the file's value, the build's value. Values are trimmed; empty ones are
/// skipped.
pub fn candidates(var: &str, file: Option<&str>, build: Option<&str>) -> Vec<String> {
    candidates_from(std::env::var(var).ok(), file, build)
}

fn candidates_from(env: Option<String>, file: Option<&str>, build: Option<&str>) -> Vec<String> {
    [env, file.map(str::to_owned), build.map(str::to_owned)]
        .into_iter()
        .flatten()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> &'static Path {
        Path::new(if cfg!(windows) { r"C:\q" } else { "/q" })
    }

    /// The settings in `text`, which must have no problems.
    fn clean(text: &str) -> Config {
        let (c, problems) = parse(text, root());
        assert!(problems.is_empty(), "{problems:?}");
        c
    }

    #[test]
    fn reads_every_key() {
        let c = clean("rules = 'SESSION-RULES.md'\ntz = 'Europe/Paris'\ntz_label = 'Paris'\n");
        assert_eq!(
            c.rules.as_deref(),
            Some(
                root()
                    .join("SESSION-RULES.md")
                    .display()
                    .to_string()
                    .as_str()
            )
        );
        assert_eq!(c.tz.as_deref(), Some("Europe/Paris"));
        assert_eq!(c.tz_label.as_deref(), Some("Paris"));
    }

    #[test]
    fn keeps_an_absolute_rules_path() {
        let abs = if cfg!(windows) {
            r"D:\rules\R.md"
        } else {
            "/rules/R.md"
        };
        let c = clean(&format!("rules = '{abs}'\n"));
        assert_eq!(c.rules.as_deref(), Some(abs));
    }

    #[test]
    fn empty_text_and_empty_values_give_no_settings() {
        assert_eq!(clean(""), Config::default());
        assert_eq!(
            clean("rules = ''\ntz = ' '\ntz_label = ''\n"),
            Config::default()
        );
    }

    #[test]
    fn an_unknown_key_is_named_and_the_other_keys_still_apply() {
        let (c, problems) = parse("tz = 'Europe/London'\ntz_lable = 'UK'\n", root());
        assert_eq!(c.tz.as_deref(), Some("Europe/London"));
        assert_eq!(c.tz_label, None);
        assert_eq!(problems.len(), 1);
        assert!(
            problems[0].starts_with("unknown key \"tz_lable\""),
            "{problems:?}"
        );
    }

    #[test]
    fn a_wrong_type_or_unknown_zone_drops_only_that_key() {
        let (c, problems) = parse("tz = 5\ntz_label = 'UK'\n", root());
        assert_eq!((c.tz, c.tz_label.as_deref()), (None, Some("UK")));
        assert!(
            problems[0].starts_with("tz must be a string"),
            "{problems:?}"
        );
        let (c, problems) = parse("tz = 'Mars/Olympus'\nrules = 'R.md'\n", root());
        assert_eq!(c.tz, None);
        assert!(c.rules.is_some());
        assert!(problems[0].contains("Mars/Olympus"), "{problems:?}");
    }

    #[test]
    fn bad_toml_ignores_the_whole_file() {
        let (c, problems) = parse("tz_label = 'UK'\ntz = \n", root());
        assert_eq!(c, Config::default());
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("line 2"), "{problems:?}");
        assert!(problems[0].ends_with("the file is ignored"), "{problems:?}");
    }

    #[test]
    fn candidates_run_env_then_file_then_build() {
        let c =
            |env: Option<&str>, file, build| candidates_from(env.map(str::to_owned), file, build);
        assert_eq!(c(Some("e"), Some("f"), Some("b")), ["e", "f", "b"]);
        assert_eq!(c(Some(" "), Some("f"), Some("b"))[0], "f");
        assert_eq!(c(None, None, Some(" b "))[0], "b");
        assert_eq!(c(None, Some(""), None), Vec::<String>::new());
    }
}
