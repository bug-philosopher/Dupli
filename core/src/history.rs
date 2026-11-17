use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use serde::{Deserialize, Serialize};

use crate::copy::{self, CopyError, Found};
use crate::names;
use crate::plan::{CompareOptions, Sig};

pub const META_DIR: &str = ".dupli";
const README: &str = "This folder is kept by Dupli, the folder sync app.\n\n\
history/ holds files that a sync removed from this folder or replaced with a\n\
newer version, in one dated folder per sync, under their original paths.\n\
Restore them from Dupli's History view, or copy them out by hand.\n\n\
Dated folders inside history/ can be deleted to free space.\n";

pub fn meta_dir(root: &Path) -> PathBuf {
    root.join(META_DIR)
}

pub fn history_dir(root: &Path) -> PathBuf {
    root.join(META_DIR).join("history")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Removed,
    Replaced,
}

#[derive(Serialize, Deserialize)]
struct Line {
    p: String,
    s: u64,
    m: i64,
    r: Reason,
}

pub struct Stash {
    root: PathBuf,
    pub run: String,
    run_dir: PathBuf,
    manifest: Option<File>,
    pub count: u64,
    pub bytes: u64,
}

impl Stash {
    pub fn new(root: &Path, now: DateTime<Local>) -> Stash {
        let dir = history_dir(root);
        let stamp = now.format("%Y-%m-%d %H.%M.%S").to_string();
        let mut run = stamp.clone();
        let mut n = 2;
        while dir.join(&run).exists() || dir.join(format!("{run}.jsonl")).exists() {
            run = format!("{stamp}-{n}");
            n += 1;
        }
        Stash { root: root.to_path_buf(), run_dir: dir.join(&run), run, manifest: None, count: 0, bytes: 0 }
    }

    // Move the file into the history folder.
    pub fn put(&mut self, rel: &str, reason: Reason) -> io::Result<()> {
        let from = names::join(&self.root, rel);
        let md = fs::symlink_metadata(&from)?;
        if self.manifest.is_none() {
            prepare_meta(&self.root)?;
            fs::create_dir_all(&self.run_dir)?;
            let path = history_dir(&self.root).join(format!("{}.jsonl", self.run));
            self.manifest = Some(OpenOptions::new().create(true).append(true).open(path)?);
        }
        let mut to = names::join(&self.run_dir, rel);
        let mut kept_as = rel.to_owned();
        let mut n = 2;
        while to.exists() {
            kept_as = names::with_suffix(rel, &format!(" ({n})"));
            to = names::join(&self.run_dir, &kept_as);
            n += 1;
        }
        if let Some(dir) = to.parent() {
            fs::create_dir_all(dir)?;
        }
        move_file(&from, &to)?;
        let line = Line { p: kept_as, s: md.len(), m: md.modified().map(names::to_ns).unwrap_or(0), r: reason };
        if let (Some(f), Ok(mut json)) = (self.manifest.as_mut(), serde_json::to_string(&line)) {
            json.push('\n');
            let _ = f.write_all(json.as_bytes());
        }
        self.count += 1;
        self.bytes += md.len();
        Ok(())
    }
}

fn move_file(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
            fs::copy(from, to)?;
            if let Ok(md) = fs::metadata(from) {
                if let Ok(m) = md.modified() {
                    let _ = copy::stamp(to, m, md.created().ok());
                }
            }
            fs::remove_file(from)
        }
        Err(e) => Err(e),
    }
}

fn prepare_meta(root: &Path) -> io::Result<()> {
    let meta = meta_dir(root);
    if meta.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(&meta)?;
    let _ = fs::write(meta.join("README.txt"), README);
    hide(&meta);
    Ok(())
}

#[cfg(windows)]
fn hide(path: &Path) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{SetFileAttributesW, FILE_ATTRIBUTE_HIDDEN};
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        SetFileAttributesW(wide.as_ptr(), FILE_ATTRIBUTE_HIDDEN);
    }
}

#[cfg(not(windows))]
fn hide(_path: &Path) {}

#[derive(Clone, Debug, Serialize)]
pub struct RunInfo {
    pub id: String,
    pub time_ms: i64,
    pub count: u64,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct HistEntry {
    pub run: String,
    pub rel: String,
    pub size: u64,
    pub mtime: i64,
    pub reason: Reason,
}

fn run_time_ms(dir: &Path, id: &str) -> i64 {
    id.get(..19)
        .and_then(|s| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H.%M.%S").ok())
        .and_then(|n| Local.from_local_datetime(&n).earliest())
        .map(|d| d.timestamp_millis())
        .or_else(|| fs::metadata(dir).and_then(|m| m.modified()).ok().map(|t| names::to_ns(t) / 1_000_000))
        .unwrap_or(0)
}

pub fn runs(root: &Path) -> Vec<RunInfo> {
    let dir = history_dir(root);
    let Ok(listing) = fs::read_dir(&dir) else { return Vec::new() };
    let mut out = Vec::new();
    for item in listing.flatten() {
        if !item.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let Some(id) = item.file_name().to_str().map(str::to_owned) else { continue };
        let list = entries(root, &id);
        if list.is_empty() {
            continue;
        }
        out.push(RunInfo {
            time_ms: run_time_ms(&item.path(), &id),
            count: list.len() as u64,
            bytes: list.iter().map(|e| e.size).sum(),
            id,
        });
    }
    out.sort_by(|a, b| b.time_ms.cmp(&a.time_ms).then_with(|| b.id.cmp(&a.id)));
    out
}

pub fn entries(root: &Path, run: &str) -> Vec<HistEntry> {
    let dir = history_dir(root);
    let mut out: Vec<HistEntry> = Vec::new();
    let mut listed = std::collections::HashSet::new();
    if let Ok(f) = File::open(dir.join(format!("{run}.jsonl"))) {
        for line in BufReader::new(f).lines().map_while(Result::ok) {
            if let Ok(l) = serde_json::from_str::<Line>(&line) {
                if listed.insert(l.p.clone()) {
                    out.push(HistEntry { run: run.to_owned(), rel: l.p, size: l.s, mtime: l.m, reason: l.r });
                }
            }
        }
    }
    if out.is_empty() {
        for (rel, size, mtime) in walk_files(&dir.join(run)) {
            out.push(HistEntry { run: run.to_owned(), rel, size, mtime, reason: Reason::Removed });
        }
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

fn walk_files(dir: &Path) -> Vec<(String, u64, i64)> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), String::new())];
    while let Some((d, rel_dir)) = stack.pop() {
        let Ok(listing) = fs::read_dir(&d) else { continue };
        for item in listing.flatten() {
            let Some(name) = item.file_name().to_str().map(str::to_owned) else { continue };
            let rel = if rel_dir.is_empty() { name } else { format!("{rel_dir}/{name}") };
            match item.metadata() {
                Ok(md) if md.is_dir() => stack.push((item.path(), rel)),
                Ok(md) if md.is_file() => out.push((rel, md.len(), md.modified().map(names::to_ns).unwrap_or(0))),
                _ => {}
            }
        }
    }
    out
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Usage {
    pub runs: u64,
    pub files: u64,
    pub bytes: u64,
}

pub fn usage(root: &Path) -> Usage {
    let all = runs(root);
    Usage { runs: all.len() as u64, files: all.iter().map(|r| r.count).sum(), bytes: all.iter().map(|r| r.bytes).sum() }
}

fn delete_run(root: &Path, run: &str) {
    let dir = history_dir(root);
    let run_dir = dir.join(run);
    if fs::remove_dir_all(&run_dir).is_err() {
        clear_read_only(&run_dir);
        let _ = fs::remove_dir_all(&run_dir);
    }
    let _ = fs::remove_file(dir.join(format!("{run}.jsonl")));
}

fn clear_read_only(dir: &Path) {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(listing) = fs::read_dir(&d) else { continue };
        for item in listing.flatten() {
            let Ok(md) = item.metadata() else { continue };
            if md.is_dir() {
                stack.push(item.path());
            } else if md.permissions().readonly() {
                let mut p = md.permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                p.set_readonly(false);
                let _ = fs::set_permissions(item.path(), p);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Pruned {
    pub runs: u64,
    pub files: u64,
    pub bytes: u64,
}

// This function removes files permanently.
pub fn prune(root: &Path, days: u32, max_bytes: u64, now_ms: i64) -> Pruned {
    let mut all = runs(root);
    let mut gone = Pruned::default();
    let drop_run = |r: &RunInfo, gone: &mut Pruned| {
        delete_run(root, &r.id);
        gone.runs += 1;
        gone.files += r.count;
        gone.bytes += r.bytes;
    };
    if days > 0 {
        let cutoff = now_ms - days as i64 * 86_400_000;
        all.retain(|r| {
            let old = r.time_ms < cutoff;
            if old {
                drop_run(r, &mut gone);
            }
            !old
        });
    }
    if max_bytes > 0 {
        let mut total: u64 = all.iter().map(|r| r.bytes).sum();
        while total > max_bytes {
            if all.len() <= 1 {
                break;
            }
            let Some(r) = all.pop() else { break };
            total -= r.bytes;
            drop_run(&r, &mut gone);
        }
    }
    remove_empty_runs(root);
    gone
}

pub fn clear(root: &Path) -> Pruned {
    let mut gone = Pruned::default();
    for r in runs(root) {
        delete_run(root, &r.id);
        gone.runs += 1;
        gone.files += r.count;
        gone.bytes += r.bytes;
    }
    remove_empty_runs(root);
    gone
}

fn remove_empty_runs(root: &Path) {
    let dir = history_dir(root);
    let Ok(listing) = fs::read_dir(&dir) else { return };
    for item in listing.flatten() {
        if !item.path().is_dir() {
            continue;
        }
        if let Some(id) = item.file_name().to_str() {
            if entries(root, id).is_empty() {
                delete_run(root, id);
            }
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Restored {
    pub rel: String,
    pub placed: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct RestoreReport {
    pub restored: Vec<Restored>,
    pub failed: Vec<(String, String)>,
}

// Do not replace a file. Use a different name.
pub fn restore(
    root: &Path,
    other: Option<&Path>,
    run: &str,
    rels: &[String],
    cmp: &CompareOptions,
    cancel: &AtomicBool,
) -> RestoreReport {
    let run_dir = history_dir(root).join(run);
    let known = entries(root, run);
    let day = Local::now().format("%Y-%m-%d").to_string();
    let mut report = RestoreReport::default();
    let mut buf = Vec::new();
    let mut taken_out: Vec<String> = Vec::new();

    for rel in rels {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        let held = names::join(&run_dir, rel);
        let sig = match copy::look(&held) {
            Ok(Found::File { size, mtime }) => Sig { size, mtime },
            _ => {
                report.failed.push((rel.clone(), "It is no longer in the history folder.".into()));
                continue;
            }
        };
        let fits = |dir: &Path, name: &str| match copy::look(&names::join(dir, name)) {
            Ok(Found::Missing) => Some(false),
            Ok(Found::File { size, mtime }) if cmp.same(sig, Sig { size, mtime }) => Some(true),
            _ => None,
        };
        let mut placed = rel.clone();
        let mut n = 1;
        let (here, there) = loop {
            let here = fits(root, &placed);
            let there = other.map(|o| fits(o, &placed));
            if let (Some(h), Some(t)) = (here, there.unwrap_or(Some(true))) {
                break (h, t);
            }
            let suffix = if n == 1 { format!(" (restored {day})") } else { format!(" (restored {day} {n})") };
            placed = names::with_suffix(rel, &suffix);
            n += 1;
        };

        let result = (|| -> Result<(), String> {
            if let (Some(o), false) = (other, there) {
                let to = names::join(o, &placed);
                let dir = to.parent().unwrap_or(o);
                fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                let staged = copy::stage(&held, dir, false, &mut buf, cancel, &mut |_| {}).map_err(|e| match e {
                    CopyError::Cancelled => "Stopped.".to_string(),
                    CopyError::Io(e) => e.to_string(),
                    _ => "The file could not be read back.".to_string(),
                })?;
                fs::rename(&staged.path, &to).map_err(|e| e.to_string())?;
            }
            if !here {
                let to = names::join(root, &placed);
                if let Some(dir) = to.parent() {
                    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                }
                move_file(&held, &to).map_err(|e| e.to_string())?;
            } else {
                let _ = fs::remove_file(&held);
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                taken_out.push(rel.clone());
                report.restored.push(Restored { rel: rel.clone(), placed });
            }
            Err(e) => report.failed.push((rel.clone(), e)),
        }
    }

    if !taken_out.is_empty() {
        rewrite_manifest(root, run, &known, &taken_out);
        prune_empty_dirs(&run_dir);
        remove_empty_runs(root);
    }
    report
}

fn rewrite_manifest(root: &Path, run: &str, known: &[HistEntry], removed: &[String]) {
    let path = history_dir(root).join(format!("{run}.jsonl"));
    let gone: std::collections::HashSet<&str> = removed.iter().map(String::as_str).collect();
    let mut text = String::new();
    for e in known.iter().filter(|e| !gone.contains(e.rel.as_str())) {
        if let Ok(json) = serde_json::to_string(&Line { p: e.rel.clone(), s: e.size, m: e.mtime, r: e.reason }) {
            text.push_str(&json);
            text.push('\n');
        }
    }
    let tmp = path.with_extension("tmp");
    if fs::write(&tmp, text).is_ok() {
        let _ = fs::rename(&tmp, &path);
    }
}

fn prune_empty_dirs(dir: &Path) -> bool {
    let Ok(listing) = fs::read_dir(dir) else { return false };
    let mut empty = true;
    for item in listing.flatten() {
        let path = item.path();
        if path.is_dir() && prune_empty_dirs(&path) {
            let _ = fs::remove_dir(&path);
        } else {
            empty = false;
        }
    }
    empty
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn write(root: &Path, rel: &str, data: &str) {
        let p = names::join(root, rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, data).unwrap();
    }

    fn at(y: i32, mo: u32, d: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, mo, d, 8, 42, 10).unwrap()
    }

    #[test]
    fn stashed_files_keep_their_paths_and_are_listed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "Trips/a.jpg", "aaaa");
        write(root, "b.jpg", "bb");
        let mut stash = Stash::new(root, at(2026, 11, 16));
        assert!(!meta_dir(root).exists());
        stash.put("Trips/a.jpg", Reason::Removed).unwrap();
        stash.put("b.jpg", Reason::Replaced).unwrap();

        assert!(!root.join("Trips/a.jpg").exists());
        assert_eq!(fs::read_to_string(root.join(".dupli/history/2026-11-16 08.42.10/Trips/a.jpg")).unwrap(), "aaaa");
        assert!(meta_dir(root).join("README.txt").exists());

        let all = runs(root);
        assert_eq!(all.len(), 1);
        assert_eq!((all[0].count, all[0].bytes), (2, 6));
        assert_eq!(all[0].time_ms, at(2026, 11, 16).timestamp_millis());
        let list = entries(root, &all[0].id);
        assert_eq!(list.iter().map(|e| (e.rel.as_str(), e.reason)).collect::<Vec<_>>(), [
            ("Trips/a.jpg", Reason::Removed),
            ("b.jpg", Reason::Replaced)
        ]);
    }

    #[test]
    fn two_runs_in_one_second_do_not_share_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a", "1");
        write(dir.path(), "b", "2");
        let mut one = Stash::new(dir.path(), at(2026, 1, 1));
        one.put("a", Reason::Removed).unwrap();
        let mut two = Stash::new(dir.path(), at(2026, 1, 1));
        two.put("b", Reason::Removed).unwrap();
        assert_ne!(one.run, two.run);
        assert_eq!(runs(dir.path()).len(), 2);
    }

    #[test]
    fn a_run_without_a_manifest_is_still_listed() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "x/a.jpg", "abc");
        let mut s = Stash::new(dir.path(), at(2026, 1, 1));
        s.put("x/a.jpg", Reason::Replaced).unwrap();
        fs::remove_file(history_dir(dir.path()).join(format!("{}.jsonl", s.run))).unwrap();
        let list = entries(dir.path(), &s.run);
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].rel.as_str(), list[0].size), ("x/a.jpg", 3));
    }

    #[test]
    fn restore_puts_files_back_on_both_sides() {
        let dir = tempfile::tempdir().unwrap();
        let (copy, source) = (dir.path().join("copy"), dir.path().join("source"));
        fs::create_dir_all(&source).unwrap();
        write(&copy, "Trips/a.jpg", "aaaa");
        let mut s = Stash::new(&copy, at(2026, 1, 1));
        s.put("Trips/a.jpg", Reason::Removed).unwrap();

        let r = restore(&copy, Some(&source), &s.run, &["Trips/a.jpg".into()], &CompareOptions::default(), &AtomicBool::new(false));
        assert!(r.failed.is_empty(), "{:?}", r.failed);
        assert_eq!(r.restored[0].placed, "Trips/a.jpg");
        assert_eq!(fs::read_to_string(copy.join("Trips/a.jpg")).unwrap(), "aaaa");
        assert_eq!(fs::read_to_string(source.join("Trips/a.jpg")).unwrap(), "aaaa");
        assert_eq!(
            fs::metadata(copy.join("Trips/a.jpg")).unwrap().modified().unwrap(),
            fs::metadata(source.join("Trips/a.jpg")).unwrap().modified().unwrap()
        );
        assert!(runs(&copy).is_empty());
        assert!(!history_dir(&copy).join(&s.run).exists());
    }

    #[test]
    fn restore_never_overwrites_the_current_version() {
        let dir = tempfile::tempdir().unwrap();
        let (copy, source) = (dir.path().join("copy"), dir.path().join("source"));
        write(&copy, "a.jpg", "old version");
        let mut s = Stash::new(&copy, at(2026, 1, 1));
        s.put("a.jpg", Reason::Replaced).unwrap();
        write(&copy, "a.jpg", "new");
        write(&source, "a.jpg", "new");

        let r = restore(&copy, Some(&source), &s.run, &["a.jpg".into()], &CompareOptions::default(), &AtomicBool::new(false));
        let placed = &r.restored[0].placed;
        assert!(placed.starts_with("a (restored ") && placed.ends_with(").jpg"), "{placed}");
        assert_eq!(fs::read_to_string(source.join("a.jpg")).unwrap(), "new");
        assert_eq!(fs::read_to_string(source.join(placed)).unwrap(), "old version");
        assert_eq!(fs::read_to_string(copy.join(placed)).unwrap(), "old version");
    }

    #[test]
    fn restoring_part_of_a_run_leaves_the_rest_listed() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a", "1");
        write(dir.path(), "b", "2");
        let mut s = Stash::new(dir.path(), at(2026, 1, 1));
        s.put("a", Reason::Removed).unwrap();
        s.put("b", Reason::Removed).unwrap();
        let r = restore(dir.path(), None, &s.run, &["a".into(), "nope".into()], &CompareOptions::default(), &AtomicBool::new(false));
        assert_eq!(r.restored.len(), 1);
        assert_eq!(r.failed.len(), 1);
        assert_eq!(entries(dir.path(), &s.run).iter().map(|e| e.rel.as_str()).collect::<Vec<_>>(), ["b"]);
    }

    #[test]
    fn prune_drops_old_runs_then_the_oldest_over_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (i, day) in [1u32, 10, 20, 28].iter().enumerate() {
            write(root, &format!("f{i}"), &"x".repeat(100));
            Stash::new(root, at(2026, 11, *day)).put(&format!("f{i}"), Reason::Removed).unwrap();
        }
        let now = at(2026, 11, 30).timestamp_millis();
        assert_eq!(prune(root, 0, 0, now).runs, 0);
        let gone = prune(root, 25, 0, now);
        assert_eq!((gone.runs, gone.bytes), (1, 100));
        let gone = prune(root, 0, 150, now);
        assert_eq!(gone.runs, 2);
        let left = runs(root);
        assert_eq!(left.len(), 1);
        assert!(left[0].id.starts_with("2026-11-28"));
        assert_eq!(prune(root, 0, 10, now).runs, 0);
        assert_eq!(clear(root).files, 1);
        assert!(runs(root).is_empty());
    }

    #[test]
    fn modified_time_survives_the_trip_through_history() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a", "1");
        let when = SystemTime::UNIX_EPOCH + Duration::from_secs(1_400_000_000);
        copy::stamp(&dir.path().join("a"), when, None).unwrap();
        let mut s = Stash::new(dir.path(), at(2026, 1, 1));
        s.put("a", Reason::Removed).unwrap();
        assert_eq!(entries(dir.path(), &s.run)[0].mtime, names::to_ns(when));
    }
}
