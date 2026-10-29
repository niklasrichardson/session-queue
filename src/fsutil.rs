//! File operations with the Windows retry rules.
//!
//! Windows can refuse a rename while another process (antivirus, the search
//! indexer, a reader) holds the file open, so moves retry 3 times over about
//! 1 second before they fail.

use std::fs::{self, File, OpenOptions};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, SystemTime};

pub const RETRIES: u32 = 3;
pub const RETRY_DELAY: Duration = Duration::from_millis(333);

#[derive(Debug)]
pub enum MoveError {
    /// The source no longer exists.
    Gone,
    /// The destination already exists. Nothing was moved.
    Exists,
    /// The rename still failed after the retries.
    Failed(io::Error),
}

/// Rename `src` to `dst` without replacing an existing `dst`.
pub fn move_with_retry(src: &Path, dst: &Path) -> Result<(), MoveError> {
    let mut last = None;
    for attempt in 0..=RETRIES {
        // Gone wins over Exists: a second claim of one message is "gone".
        if !src.exists() {
            return Err(MoveError::Gone);
        }
        if dst.exists() {
            return Err(MoveError::Exists);
        }
        match fs::rename(src, dst) {
            Ok(()) => return Ok(()),
            Err(e) => {
                if !src.exists() {
                    return Err(MoveError::Gone);
                }
                last = Some(e);
            }
        }
        if attempt < RETRIES {
            sleep(RETRY_DELAY);
        }
    }
    Err(MoveError::Failed(last.expect("at least one attempt")))
}

/// Read a whole file, retrying errors other than "not found".
pub fn read_with_retry(path: &Path) -> io::Result<Vec<u8>> {
    let mut attempt = 0;
    loop {
        match fs::read(path) {
            Ok(b) => return Ok(b),
            Err(e) if e.kind() == io::ErrorKind::NotFound || attempt >= RETRIES => return Err(e),
            Err(_) => {
                attempt += 1;
                sleep(RETRY_DELAY);
            }
        }
    }
}

/// Write `bytes` to a new file in `tmp_dir` and flush it to disk.
pub fn write_tmp(tmp_dir: &Path, stem: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    let path = tmp_dir.join(format!(
        "{stem}.{}.{:04x}.tmp",
        std::process::id(),
        random_u16()
    ));
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(path)
}

/// Write a small file through `tmp/` so readers never see it half-written.
/// Replaces an existing file.
pub fn write_atomic(tmp_dir: &Path, dst: &Path, bytes: &[u8]) -> io::Result<()> {
    let stem = dst
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = write_tmp(tmp_dir, &stem, bytes)?;
    let mut attempt = 0;
    loop {
        match fs::rename(&tmp, dst) {
            Ok(()) => return Ok(()),
            Err(e) if attempt >= RETRIES => {
                let _ = fs::remove_file(&tmp);
                return Err(e);
            }
            Err(_) => {
                attempt += 1;
                sleep(RETRY_DELAY);
            }
        }
    }
}

/// Set a file's modified time to now.
pub fn touch(path: &Path) -> io::Result<()> {
    let f = OpenOptions::new().write(true).open(path)?;
    f.set_modified(SystemTime::now())
}

/// `touch`, retrying errors other than "not found".
pub fn touch_with_retry(path: &Path) -> io::Result<()> {
    let mut attempt = 0;
    loop {
        match touch(path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound || attempt >= RETRIES => return Err(e),
            Err(_) => {
                attempt += 1;
                sleep(RETRY_DELAY);
            }
        }
    }
}

pub fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Names of the `*.json` files in `dir` whose stem passes `keep`, sorted.
/// A missing directory gives an empty list.
pub fn json_stems(dir: &Path, keep: impl Fn(&str) -> bool) -> Vec<String> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let stem = name.strip_suffix(".json")?;
            keep(stem).then(|| stem.to_owned())
        })
        .collect();
    out.sort();
    out
}

/// Open (or create) a file for locking.
pub fn open_lock_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

/// 16 random bits for IDs and temp names. `RandomState` is seeded from the
/// operating system's random source; the time and process ID add more.
pub fn random_u16() -> u16 {
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u32(std::process::id());
    if let Ok(d) = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        h.write_u128(d.as_nanos());
    }
    let v = h.finish();
    (v ^ (v >> 16) ^ (v >> 32) ^ (v >> 48)) as u16
}
