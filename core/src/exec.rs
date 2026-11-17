use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use chrono::Local;
use serde::{Deserialize, Serialize};

use crate::copy::{self, CopyError, Found};
use crate::history::{Reason, Stash};
use crate::names::{self, key_of};
use crate::plan::{Action, CompareOptions, Op, Plan, Sig};
use crate::scan::is_junk;
use crate::{volumes, Mode, Side};

#[derive(Clone, Debug)]
pub struct ExecOptions {
    pub keep_history: bool,
    pub verify: bool,
    pub check_contents: bool,
    pub compare: CompareOptions,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Preparing,
    Removing,
    Moving,
    Copying,
    Finishing,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Progress {
    pub phase: Phase,
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub current: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Pending,
    Done,
    Touched,
    Skipped(String),
    Failed(String),
}

impl Status {
    pub fn completed(&self) -> bool {
        matches!(self, Status::Done | Status::Touched)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Problem {
    pub rel: String,
    pub side: Side,
    pub what: String,
}

const PROBLEM_LIMIT: usize = 100;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Report {
    pub copied: u64,
    pub updated: u64,
    pub moved: u64,
    pub removed: u64,
    pub touched: u64,
    pub dirs_made: u64,
    pub dirs_removed: u64,
    pub bytes: u64,
    pub skipped: u64,
    pub failed: u64,
    pub problems: Vec<Problem>,
    pub kept: u64,
    pub kept_bytes: u64,
    pub cancelled: bool,
    pub stopped: Option<String>,
    pub elapsed_ms: u64,
}

impl Report {
    pub fn changes(&self) -> u64 {
        self.copied + self.updated + self.moved + self.removed + self.touched + self.dirs_made + self.dirs_removed
    }

    pub fn clean(&self) -> bool {
        !self.cancelled && self.stopped.is_none() && self.failed == 0 && self.skipped == 0
    }
}

pub struct Outcome {
    pub report: Report,
    pub status: Vec<Status>,
}

pub fn execute(
    plan: &Plan,
    roots: [&Path; 2],
    opts: &ExecOptions,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
) -> Outcome {
    let now = Local::now();
    let mut run = Run {
        plan,
        roots,
        opts,
        cancel,
        progress,
        status: vec![Status::Pending; plan.actions.len()],
        stash: [Stash::new(roots[0], now), Stash::new(roots[1], now)],
        report: Report::default(),
        prog: Progress::default(),
        buf: Vec::new(),
        failures_in_a_row: 0,
        bare: [bare_volume(roots[0]), bare_volume(roots[1])],
    };
    let started = Instant::now();
    run.prepare();
    // The sequence is important: remove files, make folders, move files, remove folders, write files.
    for step in [Run::removals, Run::make_dirs, Run::moves, Run::remove_dirs, Run::copies] {
        if run.halted() {
            break;
        }
        step(&mut run);
    }
    run.prog.phase = Phase::Finishing;
    run.prog.current.clear();
    run.tick();
    run.report.cancelled = run.report.stopped.is_none() && cancel.load(Ordering::Relaxed);
    for s in &run.stash {
        run.report.kept += s.count;
        run.report.kept_bytes += s.bytes;
    }
    if run.report.changes() > 0 {
        flush();
    }
    run.report.elapsed_ms = started.elapsed().as_millis() as u64;
    Outcome { report: run.report, status: run.status }
}

fn flush() {
    #[cfg(unix)]
    unsafe {
        libc::sync();
    }
}

struct Run<'a> {
    plan: &'a Plan,
    roots: [&'a Path; 2],
    opts: &'a ExecOptions,
    cancel: &'a AtomicBool,
    progress: &'a mut dyn FnMut(&Progress),
    status: Vec<Status>,
    stash: [Stash; 2],
    report: Report,
    prog: Progress,
    buf: Vec<u8>,
    failures_in_a_row: u32,
    bare: [bool; 2],
}

fn bare_volume(root: &Path) -> bool {
    cfg!(target_os = "macos")
        && volumes::volume_info(root).is_some_and(|v| matches!(v.fs.to_ascii_lowercase().as_str(), "msdos" | "exfat"))
}

struct Move {
    i: usize,
    at: PathBuf,
    home: PathBuf,
    from_key: (Side, String),
    to_key: (Side, String),
}

fn side_name(side: Side) -> &'static str {
    match side {
        Side::Source => "source",
        Side::Copy => "copy",
    }
}

fn explain(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::PermissionDenied => "Permission denied.".into(),
        io::ErrorKind::NotFound => "It is no longer there.".into(),
        io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded => "The drive is full.".into(),
        io::ErrorKind::ReadOnlyFilesystem => "The drive is read-only.".into(),
        io::ErrorKind::InvalidFilename => "The drive does not allow this name.".into(),
        _ => {
            let mut s = e.to_string();
            if let Some(i) = s.find(" (os error") {
                s.truncate(i);
            }
            if !s.ends_with('.') {
                s.push('.');
            }
            s
        }
    }
}

impl Run<'_> {
    fn path(&self, side: Side, rel: &str) -> PathBuf {
        names::join(self.roots[side.ix()], rel)
    }

    fn tick(&mut self) {
        (self.progress)(&self.prog);
    }

    // Remove the sidecar file that macOS makes on FAT drives.
    fn no_sidecar(&self, side: Side, path: &Path) {
        if !self.bare[side.ix()] {
            return;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            let _ = fs::remove_file(path.with_file_name(format!("._{name}")));
        }
    }

    fn begin(&mut self, phase: Phase, rel: &str) {
        self.prog.phase = phase;
        self.prog.current.clear();
        self.prog.current.push_str(rel);
        self.tick();
    }

    fn halted(&self) -> bool {
        self.report.stopped.is_some() || self.cancel.load(Ordering::Relaxed)
    }

    fn of(&self, op: Op) -> Vec<usize> {
        (0..self.plan.actions.len()).filter(|&i| self.plan.actions[i].op == op).collect()
    }

    fn done(&mut self, i: usize) {
        self.status[i] = Status::Done;
        self.failures_in_a_row = 0;
    }

    fn skip(&mut self, i: usize, why: &str) {
        let a = &self.plan.actions[i];
        self.report.skipped += 1;
        if self.report.problems.len() < PROBLEM_LIMIT {
            self.report.problems.push(Problem { rel: a.rel.clone(), side: a.side, what: why.to_owned() });
        }
        self.status[i] = Status::Skipped(why.to_owned());
    }

    fn fail(&mut self, i: usize, e: &io::Error) {
        let a = &self.plan.actions[i];
        let what = explain(e);
        self.report.failed += 1;
        if self.report.problems.len() < PROBLEM_LIMIT {
            self.report.problems.push(Problem { rel: a.rel.clone(), side: a.side, what: what.clone() });
        }
        self.status[i] = Status::Failed(what);
        self.failures_in_a_row += 1;

        // Stop the sync if a drive is full or not connected.
        if matches!(e.kind(), io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded) {
            self.report.stopped = Some(format!("The {} drive is full.", side_name(a.side)));
        } else if let Some(side) = Side::BOTH.into_iter().find(|s| fs::read_dir(self.roots[s.ix()]).is_err()) {
            self.report.stopped = Some(format!("The {} folder is no longer reachable.", side_name(side)));
        } else if self.failures_in_a_row >= 25 {
            self.report.stopped = Some("Too many files in a row could not be handled.".into());
        }
    }

    // Keep the old file in the history folder.
    fn displace(&mut self, side: Side, rel: &str, reason: Reason) -> io::Result<()> {
        if self.opts.keep_history {
            self.stash[side.ix()].put(rel, reason)
        } else {
            fs::remove_file(self.path(side, rel))
        }
    }

    fn prepare(&mut self) {
        let mut need = [0u64; 2];
        for a in &self.plan.actions {
            match a.op {
                Op::Copy | Op::Update => {
                    self.prog.files_total += 1;
                    self.prog.bytes_total += a.sig.size;
                    need[a.side.ix()] += a.sig.size;
                }
                Op::Move | Op::Remove => self.prog.files_total += 1,
                Op::MakeDir | Op::RemoveDir => {}
            }
        }
        self.tick();

        for side in Side::BOTH {
            if side == Side::Source && self.plan.mode != Mode::TwoWay {
                continue;
            }
            for rel in &self.plan.stale_parts[side.ix()] {
                let _ = fs::remove_file(self.path(side, rel));
            }
        }

        for side in Side::BOTH {
            let wanted = need[side.ix()];
            if wanted == 0 {
                continue;
            }
            if let Some(free) = volumes::free_space(self.roots[side.ix()]) {
                if wanted.saturating_add(8 << 20) > free {
                    self.report.stopped = Some(format!(
                        "Not enough room on the {} drive: {} to copy, {} free.",
                        side_name(side),
                        human(wanted),
                        human(free)
                    ));
                    return;
                }
            }
        }
    }

    fn removals(&mut self) {
        for i in self.of(Op::Remove) {
            if self.halted() {
                return;
            }
            let a = &self.plan.actions[i];
            self.begin(Phase::Removing, &a.rel);
            match copy::look(&self.path(a.side, &a.rel)) {
                Ok(Found::Missing) => self.done(i),
                Ok(Found::File { size, mtime }) if self.opts.compare.same(a.sig, Sig { size, mtime }) => {
                    match self.displace(a.side, &a.rel, Reason::Removed) {
                        Ok(()) => {
                            self.report.removed += 1;
                            self.done(i);
                        }
                        Err(e) => self.fail(i, &e),
                    }
                }
                Ok(Found::File { .. }) => self.skip(i, "It changed after the compare, so it was left alone."),
                Ok(Found::Other) => self.skip(i, "It is no longer a plain file."),
                Err(e) => self.fail(i, &e),
            }
            self.prog.files_done += 1;
        }
    }

    fn make_dirs(&mut self) {
        for i in self.of(Op::MakeDir) {
            if self.halted() {
                return;
            }
            let a = &self.plan.actions[i];
            let path = self.path(a.side, &a.rel);
            if path.is_dir() {
                self.done(i);
                continue;
            }
            match fs::create_dir_all(&path) {
                Ok(()) => {
                    self.no_sidecar(a.side, &path);
                    self.report.dirs_made += 1;
                    self.done(i);
                }
                Err(e) => self.fail(i, &e),
            }
        }
    }

    fn moves(&mut self) {
        let fold = self.plan.fold;
        let mut waiting: Vec<Move> = Vec::new();
        for i in self.of(Op::Move) {
            let a = &self.plan.actions[i];
            let from = a.from.as_deref().unwrap_or_default();
            let at = self.path(a.side, from);
            match copy::look(&at) {
                Ok(Found::File { size, mtime }) if self.opts.compare.same(a.sig, Sig { size, mtime }) => waiting.push(Move {
                    i,
                    home: at.clone(),
                    at,
                    from_key: (a.side, key_of(from, fold)),
                    to_key: (a.side, key_of(&a.rel, fold)),
                }),
                Ok(Found::Missing) => {
                    self.skip(i, "It is no longer there.");
                    self.prog.files_done += 1;
                }
                Ok(_) => {
                    self.skip(i, "It changed after the compare, so it was left alone.");
                    self.prog.files_done += 1;
                }
                Err(e) => {
                    self.fail(i, &e);
                    self.prog.files_done += 1;
                }
            }
        }

        let mut occupied: HashSet<(Side, String)> = waiting.iter().map(|m| m.from_key.clone()).collect();
        while !waiting.is_empty() {
            if self.halted() {
                break;
            }
            let mut progressed = false;
            let mut still = Vec::new();
            for m in waiting {
                if self.halted() || occupied.contains(&m.to_key) {
                    still.push(m);
                    continue;
                }
                occupied.remove(&m.from_key);
                self.one_move(&m);
                self.prog.files_done += 1;
                progressed = true;
            }
            waiting = still;
            if !progressed && !waiting.is_empty() && !self.halted() {
                let aside = waiting[0].at.with_file_name(copy::part_name());
                match fs::rename(&waiting[0].at, &aside) {
                    Ok(()) => {
                        occupied.remove(&waiting[0].from_key);
                        waiting[0].at = aside;
                    }
                    Err(e) => {
                        let m = waiting.remove(0);
                        occupied.remove(&m.from_key);
                        self.fail(m.i, &e);
                        self.prog.files_done += 1;
                    }
                }
            }
        }
        for m in waiting {
            if m.at != m.home && !m.home.exists() {
                let _ = fs::rename(&m.at, &m.home);
            }
        }
    }

    fn one_move(&mut self, m: &Move) {
        let a: &Action = &self.plan.actions[m.i];
        let to = self.path(a.side, &a.rel);
        self.begin(Phase::Moving, &a.rel);
        let result = (|| -> Result<(), Result<io::Error, &'static str>> {
            match copy::look(&to).map_err(Ok)? {
                Found::Missing => {}
                Found::File { size, mtime } => {
                    if same_file::is_same_file(&m.at, &to).unwrap_or(false) {
                    } else if a.old.is_some_and(|old| self.opts.compare.same(old, Sig { size, mtime })) {
                        self.displace(a.side, &a.rel, Reason::Replaced).map_err(Ok)?;
                    } else {
                        return Err(Err("Another file has taken that name since the compare."));
                    }
                }
                Found::Other => return Err(Err("Something that is not a file has that name.")),
            }
            if let Some(dir) = to.parent() {
                fs::create_dir_all(dir).map_err(Ok)?;
            }
            fs::rename(&m.at, &to).map_err(Ok)
        })();
        match result {
            Ok(()) => {
                self.report.moved += 1;
                self.done(m.i);
                return;
            }
            Err(Ok(e)) => self.fail(m.i, &e),
            Err(Err(why)) => self.skip(m.i, why),
        }
        if m.at != m.home && !m.home.exists() {
            let _ = fs::rename(&m.at, &m.home);
        }
    }

    fn remove_dirs(&mut self) {
        for i in self.of(Op::RemoveDir).into_iter().rev() {
            if self.halted() {
                return;
            }
            let a = &self.plan.actions[i];
            let path = self.path(a.side, &a.rel);
            match fs::symlink_metadata(&path) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    self.done(i);
                    continue;
                }
                Ok(md) if !md.is_dir() => {
                    self.status[i] = Status::Skipped("It is no longer a folder.".into());
                    continue;
                }
                _ => {}
            }
            clear_junk(&path);
            match fs::remove_dir(&path) {
                Ok(()) => {
                    self.report.dirs_removed += 1;
                    self.done(i);
                }
                Err(e) if e.kind() == io::ErrorKind::DirectoryNotEmpty => {
                    self.status[i] = Status::Skipped("It is not empty.".into());
                }
                Err(e) => self.fail(i, &e),
            }
        }
    }

    fn copies(&mut self) {
        let mut order = self.of(Op::Copy);
        order.extend(self.of(Op::Update));
        for i in order {
            if self.halted() {
                return;
            }
            let a = &self.plan.actions[i];
            let before = self.prog.bytes_done;
            self.begin(Phase::Copying, &a.rel);
            self.one_copy(i);
            if self.status[i] == Status::Pending {
                self.prog.bytes_done = before;
                return;
            }
            self.prog.bytes_done = before + a.sig.size;
            self.prog.files_done += 1;
        }
    }

    fn one_copy(&mut self, i: usize) {
        let a: &Action = &self.plan.actions[i];
        let cmp = self.opts.compare;
        let src = self.path(a.side.other(), a.from.as_deref().unwrap_or_default());
        let dst = self.path(a.side, &a.rel);

        let original = match copy::look(&src) {
            Ok(Found::File { size, mtime }) => Sig { size, mtime },
            Ok(Found::Missing) => return self.skip(i, "It is no longer there."),
            Ok(Found::Other) => return self.skip(i, "It is no longer a plain file."),
            Err(e) => return self.fail(i, &e),
        };
        // Do not replace a file that changed after the compare.
        let in_the_way = match copy::look(&dst) {
            Ok(Found::Missing) => None,
            Ok(Found::File { size, mtime }) => match a.old {
                Some(old) if cmp.same(old, Sig { size, mtime }) => Some(Sig { size, mtime }),
                Some(_) => return self.skip(i, "The other side's file changed after the compare, so it was not replaced."),
                None => return self.skip(i, "A file appeared there after the compare."),
            },
            Ok(Found::Other) => return self.skip(i, "Something that is not a file has that name."),
            Err(e) => return self.fail(i, &e),
        };

        if let Some(existing) = in_the_way {
            // If only the date is different, compare the contents.
            if self.opts.check_contents && existing.size == original.size && original.size > 0 {
                let start = self.prog.bytes_done;
                let (prog, progress) = (&mut self.prog, &mut self.progress);
                let same = copy::same_contents(&src, &dst, self.cancel, &mut |n| {
                    prog.bytes_done += n;
                    progress(prog);
                });
                match same {
                    Ok(true) => {
                        if copy::stamp(&dst, names::from_ns(original.mtime), None).is_ok() {
                            self.report.touched += 1;
                            self.status[i] = Status::Touched;
                            self.failures_in_a_row = 0;
                            return;
                        }
                    }
                    Ok(false) => {}
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => return,
                    Err(e) => return self.fail(i, &e),
                }
                self.prog.bytes_done = start;
            }
        }

        let dir = dst.parent().unwrap_or(self.roots[a.side.ix()]).to_path_buf();
        if let Err(e) = fs::create_dir_all(&dir) {
            return self.fail(i, &e);
        }
        let staged = {
            let (prog, progress) = (&mut self.prog, &mut self.progress);
            copy::stage(&src, &dir, self.opts.verify, &mut self.buf, self.cancel, &mut |n| {
                prog.bytes_done += n;
                progress(prog);
            })
        };
        let staged = match staged {
            Ok(s) => s,
            Err(CopyError::Cancelled) => return,
            Err(CopyError::SourceChanged) => {
                return self.skip(i, "It changed while it was being copied. The next sync will pick it up.")
            }
            Err(CopyError::Mismatch) => {
                let e = io::Error::other("The copy did not read back the same as the original");
                return self.fail(i, &e);
            }
            Err(CopyError::Io(e)) => return self.fail(i, &e),
        };
        if in_the_way.is_some() {
            if let Err(e) = self.displace(a.side, &a.rel, Reason::Replaced) {
                staged.discard();
                return self.fail(i, &e);
            }
        }
        let size = staged.size;
        if let Err(e) = fs::rename(&staged.path, &dst) {
            staged.discard();
            return self.fail(i, &e);
        }
        self.no_sidecar(a.side, &dst);
        if a.op == Op::Update {
            self.report.updated += 1;
        } else {
            self.report.copied += 1;
        }
        self.report.bytes += size;
        self.done(i);
    }
}

fn clear_junk(dir: &Path) {
    let Ok(listing) = fs::read_dir(dir) else { return };
    for item in listing.flatten() {
        let Some(name) = item.file_name().to_str().map(str::to_owned) else { continue };
        if !is_junk(&name) {
            continue;
        }
        match item.file_type() {
            Ok(t) if t.is_dir() => {
                let _ = fs::remove_dir_all(item.path());
            }
            Ok(_) => {
                let _ = fs::remove_file(item.path());
            }
            Err(_) => {}
        }
    }
}

pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} bytes")
    } else if v >= 100.0 {
        format!("{v:.0} {}", UNITS[u])
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}
