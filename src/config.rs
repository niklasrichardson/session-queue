//! Settings in `config.toml` in the queue root.
//!
//! Each setting comes from the first of: the environment variable (`CQ_RULES`,
//! `CQ_TZ`, `CQ_TZ_LABEL`), this file, the build's default (`CQ_RULES_PATH`,
//! `CQ_DEFAULT_TZ`, `CQ_DEFAULT_TZ_LABEL`), then the built-in default. No
//! setting has a command-line flag yet. The file cannot move the root: the
//! root must be known before the file can be found.
//!
//! An unknown key is an error, not a warning, so a typo such as `tz_lable`
//! stops the command with a clear message instead of being ignored. A
//! missing file is fine.

use std::fs;
use std::io;
use std::path::Path;
use std::sync::OnceLock;

use jiff::tz::TimeZone;
use serde::Deserialize;

use crate::error::{CqError, Result};
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

#[derive(Deserialize)]
struct Raw {
    rules: Option<String>,
    tz: Option<String>,
    tz_label: Option<String>,
}

impl Config {
    /// Read `config.toml` in `root`. A missing file gives no settings. A file
    /// that is not valid TOML, has an unknown key or a wrong value is an
    /// error (exit 64) that names the file.
    pub fn load(root: &Root) -> Result<Config> {
        let path = root.path().join(FILE);
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                return Err(CqError::usage(format!(
                    "{}: not UTF-8 text",
                    path.display()
                )));
            }
            Err(e) => return Err(CqError::io(format!("read {}", path.display()), e)),
        };
        parse(&text, root.path()).map_err(|m| CqError::usage(format!("{}: {m}", path.display())))
    }
}

/// The settings in `text`, with a relative `rules` path joined to `root`.
fn parse(text: &str, root: &Path) -> std::result::Result<Config, String> {
    // Keys first: serde's unknown-field error points at the whole table, not
    // at the key.
    let table: toml::Table = toml::from_str(text).map_err(|e| located(text, &e))?;
    if let Some(key) = table.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(format!(
            "unknown key {key:?}; the keys are rules, tz and tz_label"
        ));
    }
    let raw: Raw = toml::from_str(text).map_err(|e| located(text, &e))?;
    let tz = non_empty(raw.tz);
    if let Some(name) = &tz
        && TimeZone::get(name).is_err()
    {
        return Err(format!(
            "tz {name:?} is not a known time zone; use an IANA name such as Europe/London"
        ));
    }
    let rules = non_empty(raw.rules).map(|r| {
        if Path::new(&r).is_relative() {
            root.join(&r).display().to_string()
        } else {
            r
        }
    });
    Ok(Config {
        rules,
        tz,
        tz_label: non_empty(raw.tz_label),
    })
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

fn non_empty(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
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

    #[test]
    fn reads_every_key() {
        let c = parse(
            "rules = 'SESSION-RULES.md'\ntz = 'Europe/Paris'\ntz_label = 'Paris'\n",
            root(),
        )
        .unwrap();
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
        let c = parse(&format!("rules = '{abs}'\n"), root()).unwrap();
        assert_eq!(c.rules.as_deref(), Some(abs));
    }

    #[test]
    fn empty_text_and_empty_values_give_no_settings() {
        assert_eq!(parse("", root()).unwrap(), Config::default());
        assert_eq!(
            parse("rules = ''\ntz = ' '\ntz_label = ''\n", root()).unwrap(),
            Config::default()
        );
    }

    #[test]
    fn rejects_an_unknown_key_by_name() {
        let e = parse("tz = 'Europe/London'\ntz_lable = 'UK'\n", root()).unwrap_err();
        assert!(e.starts_with("unknown key \"tz_lable\""), "{e}");
    }

    #[test]
    fn rejects_bad_toml_wrong_types_and_unknown_zones() {
        assert!(parse("tz = ", root()).is_err());
        let e = parse("tz = 5\n", root()).unwrap_err();
        assert!(e.contains("line 1"), "{e}");
        let e = parse("tz = 'Mars/Olympus'\n", root()).unwrap_err();
        assert!(e.contains("Mars/Olympus"), "{e}");
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
