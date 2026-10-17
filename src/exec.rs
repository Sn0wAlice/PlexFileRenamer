//! Applying a plan to disk, with a journal written as we go, and undo.
//! Journal = JSON Lines (one JSON object per line), so a crash never leaves it unparsable.

use crate::plan::{Kind, Plan, Status};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Move,
    Copy,
    Hardlink,
    Symlink,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Move,
    Copy,
    Hardlink,
    Symlink,
    Mkdir,
    Rmdir,
    Write,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub ts: f64,
    pub action: Action,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub src: Option<String>,
    pub dst: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

pub struct Journal {
    file: File,
}

impl Journal {
    pub fn create(path: &Path) -> Result<Journal> {
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .with_context(|| format!("creating journal {}", path.display()))?;
        Ok(Journal { file })
    }

    fn log(
        &mut self,
        action: Action,
        src: Option<&str>,
        dst: &str,
        size: Option<u64>,
    ) -> Result<()> {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let e = Entry {
            ts,
            action,
            src: src.map(String::from),
            dst: dst.to_string(),
            size,
        };
        writeln!(self.file, "{}", serde_json::to_string(&e)?)?;
        self.file.sync_data()?;
        Ok(())
    }
}

pub fn read_journal(path: &Path) -> Result<Vec<Entry>> {
    let f = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut out = vec![];
    for (i, line) in io::BufReader::new(f).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        out.push(
            serde_json::from_str(&line).with_context(|| format!("{}:{}", path.display(), i + 1))?,
        );
    }
    Ok(out)
}

#[derive(Debug, Default)]
pub struct Report {
    pub done: usize,
    pub errors: Vec<String>,
}

fn exists(p: &str) -> bool {
    Path::new(p).symlink_metadata().is_ok()
}

fn file_len(p: &str) -> Result<u64> {
    Ok(fs::metadata(p)
        .with_context(|| format!("{p} not found"))?
        .len())
}

fn is_cross_device(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::CrossesDevices || e.raw_os_error() == Some(18)
}

/// rename(2) never crosses filesystems; fall back to copy + size check + delete.
/// Callers check `dst` does not exist right before: a concurrent writer could still race us.
// ponytail: check-then-rename, renameat2(RENAME_NOREPLACE) if a TOCTOU-free guarantee is ever needed.
fn move_file(src: &str, dst: &str) -> Result<()> {
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if is_cross_device(&e) => {
            copy_file(src, dst)?;
            fs::remove_file(src).with_context(|| format!("removing {src} after copy"))?;
            Ok(())
        }
        Err(e) => Err(e).with_context(|| format!("rename {src} -> {dst}")),
    }
}

fn copy_file(src: &str, dst: &str) -> Result<()> {
    let want = file_len(src)?;
    {
        let mut from = File::open(src)?;
        let mut to = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(dst)
            .with_context(|| format!("creating {dst}"))?;
        io::copy(&mut from, &mut to)?;
        to.sync_all()?;
    }
    let got = file_len(dst)?;
    if got != want {
        let _ = fs::remove_file(dst);
        bail!("copy of {src} is {got} bytes, expected {want}; copy removed");
    }
    Ok(())
}

fn symlink(src: &str, dst: &str) -> io::Result<()> {
    #[cfg(unix)]
    return std::os::unix::fs::symlink(src, dst);
    #[cfg(windows)]
    return std::os::windows::fs::symlink_file(src, dst);
}

/// Create missing ancestors one by one so each can be journaled (and removed by undo).
fn mkdirs(dir: &Path, j: &mut Journal) -> Result<()> {
    let mut missing = vec![];
    let mut d = Some(dir);
    while let Some(x) = d {
        if x.as_os_str().is_empty() || x.exists() {
            break;
        }
        missing.push(x.to_path_buf());
        d = x.parent();
    }
    for m in missing.iter().rev() {
        fs::create_dir(m).with_context(|| format!("mkdir {}", m.display()))?;
        j.log(Action::Mkdir, None, &m.to_string_lossy(), None)?;
    }
    Ok(())
}

pub fn apply(plan: &Plan, mode: Mode, journal: &mut Journal, clean_empty_dirs: bool) -> Report {
    let mut rep = Report::default();
    let mut failed_groups: HashSet<usize> = HashSet::new();
    let mut emptied: Vec<(String, Option<String>)> = vec![];
    for (i, op) in plan.ops.iter().enumerate() {
        if op.status != Status::Rename {
            continue;
        }
        if failed_groups.contains(&op.group) {
            rep.errors.push(format!(
                "{}: skipped, its video failed",
                op.src.as_deref().unwrap_or(&op.dst)
            ));
            continue;
        }
        match apply_one(op, mode, journal) {
            Ok(()) => {
                rep.done += 1;
                if let (Some(src), Mode::Move) = (&op.src, mode)
                    && let Some(p) = Path::new(src).parent()
                {
                    emptied.push((p.to_string_lossy().into_owned(), op.root.clone()));
                }
            }
            Err(e) => {
                if op.kind == Kind::Video {
                    failed_groups.insert(i);
                }
                rep.errors.push(format!("{e:#}"));
            }
        }
    }
    if clean_empty_dirs {
        emptied.sort();
        emptied.dedup();
        for (dir, root) in emptied {
            remove_empty_up(&dir, root.as_deref(), journal, &mut rep);
        }
    }
    rep
}

fn apply_one(op: &crate::plan::Op, mode: Mode, j: &mut Journal) -> Result<()> {
    let dst = op.dst.as_str();
    let mkparent = |j: &mut Journal| match Path::new(dst).parent() {
        Some(p) => mkdirs(p, j),
        None => Ok(()),
    };
    let Some(src) = op.src.as_deref() else {
        if exists(dst) {
            bail!("{dst} already exists");
        }
        mkparent(j)?;
        // .plexmatch: create_new never overwrites.
        let mut f = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(dst)
            .with_context(|| format!("creating {dst}"))?;
        f.write_all(op.content.as_deref().unwrap_or("").as_bytes())?;
        f.sync_all()?;
        return j.log(Action::Write, None, dst, None);
    };
    let len = file_len(src)?;
    if let Some(want) = op.size
        && want != len
    {
        bail!(
            "{src}: size {len} differs from Plex ({want}), file changed since last scan; not touched"
        );
    }
    let case_only = src.to_lowercase() == dst.to_lowercase() && same_inode(src, dst);
    if exists(dst) && !case_only {
        bail!("{dst} already exists; {src} not touched");
    }
    // Every check passed: only now touch the disk.
    mkparent(j)?;
    let action = match mode {
        Mode::Move => {
            move_file(src, dst)?;
            Action::Move
        }
        Mode::Copy => {
            copy_file(src, dst)?;
            Action::Copy
        }
        Mode::Hardlink => {
            fs::hard_link(src, dst).with_context(|| format!("hardlink {src} -> {dst}"))?;
            Action::Hardlink
        }
        Mode::Symlink => {
            let abs = fs::canonicalize(src)?;
            symlink(&abs.to_string_lossy(), dst)
                .with_context(|| format!("symlink {src} -> {dst}"))?;
            Action::Symlink
        }
    };
    j.log(action, Some(src), dst, Some(len))
}

fn same_inode(a: &str, b: &str) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        matches!((fs::metadata(a), fs::metadata(b)), (Ok(x), Ok(y)) if x.dev() == y.dev() && x.ino() == y.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        true
    }
}

fn remove_empty_up(dir: &str, root: Option<&str>, j: &mut Journal, rep: &mut Report) {
    let Some(root) = root.map(|r| r.trim_end_matches('/')) else {
        return;
    };
    let mut d = Path::new(dir).to_path_buf();
    while d.starts_with(root) && d.as_os_str().len() > root.len() {
        let empty = fs::read_dir(&d)
            .map(|mut r| r.next().is_none())
            .unwrap_or(false);
        if !empty || fs::remove_dir(&d).is_err() {
            break;
        }
        if let Err(e) = j.log(Action::Rmdir, None, &d.to_string_lossy(), None) {
            rep.errors.push(format!("{e:#}"));
        }
        if !d.pop() {
            break;
        }
    }
}

/// Replay a journal backwards. Never overwrites: anything ambiguous is reported and left alone.
pub fn undo(entries: &[Entry]) -> Report {
    let mut rep = Report::default();
    for e in entries.iter().rev() {
        let r = (|| -> Result<()> {
            let dst = e.dst.as_str();
            match e.action {
                Action::Move => {
                    let src = e.src.as_deref().unwrap_or_default();
                    if exists(src) {
                        bail!("{src} exists again; not moving {dst} back");
                    }
                    let len = file_len(dst)?;
                    if e.size.is_some_and(|s| s != len) {
                        bail!("{dst} changed size since apply; left in place");
                    }
                    if let Some(p) = Path::new(src).parent() {
                        fs::create_dir_all(p)?;
                    }
                    move_file(dst, src)
                }
                Action::Copy => {
                    let len = file_len(dst)?;
                    if e.size.is_some_and(|s| s != len) {
                        bail!("{dst} changed size since apply; left in place");
                    }
                    Ok(fs::remove_file(dst)?)
                }
                Action::Hardlink | Action::Symlink | Action::Write => {
                    if exists(dst) {
                        fs::remove_file(dst).with_context(|| format!("removing {dst}"))?;
                    }
                    Ok(())
                }
                Action::Mkdir => {
                    fs::remove_dir(dst).with_context(|| format!("{dst} not empty, kept"))
                }
                Action::Rmdir => Ok(fs::create_dir_all(dst)?),
            }
        })();
        match r {
            Ok(()) => rep.done += 1,
            Err(err) => rep.errors.push(format!("{err:#}")),
        }
    }
    rep
}
