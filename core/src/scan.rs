use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

use crate::history::META_DIR;
use crate::names;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    File,
    Dir,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub rel: String,
    pub kind: Kind,
    pub size: u64,
    pub mtime: i64,
}

#[derive(Debug, Default)]
pub struct Tree {
    pub entries: Vec<Entry>,
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    pub links: Vec<String>,
    pub unreadable: Vec<String>,
    pub bad_names: u64,
    pub stale_parts: Vec<String>,
    pub pinned: HashSet<String>,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct ScanProgress {
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
}

#[derive(Debug)]
pub enum ScanError {
    Missing,
    Cancelled,
    Io(String),
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanError::Missing => write!(f, "the folder is not reachable"),
            ScanError::Cancelled => write!(f, "stopped"),
            ScanError::Io(m) => write!(f, "{m}"),
        }
    }
}

// Do not include these system files in a scan.
const JUNK: &[&str] = &[
    ".ds_store",
    ".localized",
    "thumbs.db",
    "ehthumbs.db",
    "desktop.ini",
    ".spotlight-v100",
    ".trashes",
    ".fseventsd",
    ".temporaryitems",
    ".documentrevisions-v100",
    ".apdisk",
    ".volumeicon.icns",
    ".com.apple.timemachine.donotpresent",
    "$recycle.bin",
    "system volume information",
    "@eadir",
    "#recycle",
    ".appledouble",
    ".appledb",
    ".appledesktop",
    "network trash folder",
    "temporary items",
    "icon\r",
];

pub fn is_junk(name: &str) -> bool {
    if name.starts_with("._") || name.starts_with(".Trash-") {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    JUNK.contains(&lower.as_str())
}

pub fn is_part_file(name: &str) -> bool {
    name.starts_with(".dupli-") && name.ends_with(".part")
}

#[derive(Clone, Default)]
pub struct Excludes {
    names: GlobSet,
    names_dir_only: Vec<bool>,
    paths: GlobSet,
    paths_dir_only: Vec<bool>,
    empty: bool,
}

impl Excludes {
    pub fn new(patterns: &[String]) -> (Excludes, Vec<String>) {
        let mut names = GlobSetBuilder::new();
        let mut paths = GlobSetBuilder::new();
        let mut ex = Excludes { empty: true, ..Default::default() };
        let mut bad = Vec::new();
        for raw in patterns {
            let mut p = raw.trim().replace('\\', "/");
            if p.is_empty() || p.starts_with('#') {
                continue;
            }
            let dir_only = p.ends_with('/');
            while p.ends_with('/') {
                p.pop();
            }
            let anchored = p.contains('/');
            let p = p.trim_start_matches('/');
            if p.is_empty() {
                continue;
            }
            match GlobBuilder::new(p).case_insensitive(true).literal_separator(true).build() {
                Ok(g) => {
                    ex.empty = false;
                    if anchored {
                        paths.add(g);
                        ex.paths_dir_only.push(dir_only);
                    } else {
                        names.add(g);
                        ex.names_dir_only.push(dir_only);
                    }
                }
                Err(_) => bad.push(raw.clone()),
            }
        }
        ex.names = names.build().unwrap_or_else(|_| GlobSet::empty());
        ex.paths = paths.build().unwrap_or_else(|_| GlobSet::empty());
        (ex, bad)
    }

    pub fn matches(&self, rel: &str, name: &str, is_dir: bool) -> bool {
        if self.empty {
            return false;
        }
        self.names.matches(name).into_iter().any(|i| is_dir || !self.names_dir_only[i])
            || self.paths.matches(rel).into_iter().any(|i| is_dir || !self.paths_dir_only[i])
    }
}

#[derive(Clone, Default)]
pub struct ScanOptions {
    pub excludes: Excludes,
    pub skip_hidden: bool,
}

pub fn scan(
    root: &Path,
    opts: &ScanOptions,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&ScanProgress),
) -> Result<Tree, ScanError> {
    match fs::metadata(root) {
        Ok(md) if md.is_dir() => {}
        _ => return Err(ScanError::Missing),
    }
    let mut tree = Tree::default();
    let mut stack: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
    let mut last_tick = Instant::now();

    while let Some((dir, rel_dir)) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return Err(ScanError::Cancelled);
        }
        if last_tick.elapsed() >= Duration::from_millis(150) {
            last_tick = Instant::now();
            progress(&ScanProgress { files: tree.files, dirs: tree.dirs, bytes: tree.bytes });
        }
        let listing = match fs::read_dir(&dir) {
            Ok(l) => l,
            Err(e) if rel_dir.is_empty() => return Err(root_error(root, e)),
            Err(e) => match e.kind() {
                io::ErrorKind::NotFound => {
                    forget(&mut tree, &rel_dir);
                    continue;
                }
                io::ErrorKind::PermissionDenied => {
                    tree.pinned.insert(names::parent(&rel_dir).to_owned());
                    tree.unreadable.push(rel_dir);
                    continue;
                }
                _ => return Err(gone_or(root, format!("Couldn't read \u{201c}{rel_dir}\u{201d}: {e}"))),
            },
        };
        for item in listing {
            let item = match item {
                Ok(i) => i,
                Err(e) => return Err(gone_or(root, format!("Couldn't read \u{201c}{rel_dir}\u{201d}: {e}"))),
            };
            let os_name = item.file_name();
            let Some(name) = os_name.to_str() else {
                tree.bad_names += 1;
                tree.pinned.insert(rel_dir.clone());
                continue;
            };
            if rel_dir.is_empty() && name == META_DIR {
                continue;
            }
            let rel = if rel_dir.is_empty() { name.to_owned() } else { format!("{rel_dir}/{name}") };
            if is_part_file(name) {
                tree.stale_parts.push(rel);
                continue;
            }
            if is_junk(name) {
                continue;
            }
            if opts.skip_hidden && name.starts_with('.') {
                tree.pinned.insert(rel_dir.clone());
                continue;
            }
            let kind = match item.file_type() {
                Ok(t) => t,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                    tree.pinned.insert(rel_dir.clone());
                    tree.unreadable.push(rel);
                    continue;
                }
                Err(e) => return Err(gone_or(root, format!("Couldn't read \u{201c}{rel}\u{201d}: {e}"))),
            };
            if kind.is_symlink() {
                tree.pinned.insert(rel_dir.clone());
                tree.links.push(rel);
            } else if kind.is_dir() {
                if opts.excludes.matches(&rel, name, true) {
                    tree.pinned.insert(rel_dir.clone());
                    continue;
                }
                tree.dirs += 1;
                tree.entries.push(Entry { rel: rel.clone(), kind: Kind::Dir, size: 0, mtime: 0 });
                stack.push((item.path(), rel));
            } else if kind.is_file() {
                if opts.excludes.matches(&rel, name, false) {
                    tree.pinned.insert(rel_dir.clone());
                    continue;
                }
                let md = match item.metadata() {
                    Ok(m) => m,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                        tree.pinned.insert(rel_dir.clone());
                        tree.unreadable.push(rel);
                        continue;
                    }
                    Err(e) => return Err(gone_or(root, format!("Couldn't read \u{201c}{rel}\u{201d}: {e}"))),
                };
                if opts.skip_hidden && is_hidden(&md) {
                    tree.pinned.insert(rel_dir.clone());
                    continue;
                }
                let mtime = md.modified().map(names::to_ns).unwrap_or(0);
                tree.files += 1;
                tree.bytes += md.len();
                tree.entries.push(Entry { rel, kind: Kind::File, size: md.len(), mtime });
            }
            else {
                tree.pinned.insert(rel_dir.clone());
            }
        }
    }

    let mut above = Vec::new();
    for p in &tree.pinned {
        let mut d = names::parent(p);
        while !d.is_empty() {
            above.push(d.to_owned());
            d = names::parent(d);
        }
    }
    tree.pinned.extend(above);
    tree.pinned.remove("");

    // Make sure that the drive is connected at the end of the scan.
    if fs::read_dir(root).is_err() {
        return Err(ScanError::Missing);
    }
    progress(&ScanProgress { files: tree.files, dirs: tree.dirs, bytes: tree.bytes });
    Ok(tree)
}

fn forget(tree: &mut Tree, rel_dir: &str) {
    if let Some(at) = tree.entries.iter().rposition(|e| e.kind == Kind::Dir && e.rel == rel_dir) {
        tree.entries.swap_remove(at);
        tree.dirs = tree.dirs.saturating_sub(1);
    }
}

fn root_error(root: &Path, e: io::Error) -> ScanError {
    match e.kind() {
        io::ErrorKind::NotFound => ScanError::Missing,
        _ => ScanError::Io(format!("Couldn't open {}: {e}", root.display())),
    }
}

fn gone_or(root: &Path, message: String) -> ScanError {
    if fs::read_dir(root).is_err() {
        ScanError::Missing
    } else {
        ScanError::Io(message)
    }
}

#[cfg(windows)]
fn is_hidden(md: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    md.file_attributes() & 0x2 != 0
}

#[cfg(not(windows))]
fn is_hidden(_md: &fs::Metadata) -> bool {
    false
}

pub fn probe_case_insensitive(root: &Path, tree: &Tree) -> Option<bool> {
    let sample = tree.entries.iter().find(|e| names::file_name(&e.rel).bytes().any(|b| b.is_ascii_alphabetic()));
    let (path, name) = match sample {
        Some(e) => (names::join(root, &e.rel), names::file_name(&e.rel).to_owned()),
        None => {
            let name = root.file_name()?.to_str()?.to_owned();
            (root.to_path_buf(), name)
        }
    };
    let mut flipped = String::with_capacity(name.len());
    let mut done = false;
    for c in name.chars() {
        if !done && c.is_ascii_alphabetic() {
            done = true;
            flipped.push(if c.is_ascii_lowercase() { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() });
        } else {
            flipped.push(c);
        }
    }
    if !done {
        return None;
    }
    let alt = path.with_file_name(flipped);
    match fs::symlink_metadata(&alt) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
        Ok(_) => same_file::is_same_file(&path, &alt).ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ex(patterns: &[&str]) -> Excludes {
        let (e, bad) = Excludes::new(&patterns.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert!(bad.is_empty(), "{bad:?}");
        e
    }

    #[test]
    fn name_patterns_match_anywhere() {
        let e = ex(&["*.tmp", "Lightroom Previews.lrdata"]);
        assert!(e.matches("a/b/x.TMP", "x.TMP", false));
        assert!(e.matches("cat/Lightroom Previews.lrdata", "Lightroom Previews.lrdata", true));
        assert!(!e.matches("a/b/x.jpg", "x.jpg", false));
    }

    #[test]
    fn path_patterns_are_anchored_and_stars_stay_in_one_folder() {
        let e = ex(&["/Exports/Cache", "Trips/*/proofs", "raw/**/*.xmp"]);
        assert!(e.matches("Exports/Cache", "Cache", true));
        assert!(!e.matches("Old/Exports/Cache", "Cache", true));
        assert!(e.matches("Trips/Japan/proofs", "proofs", true));
        assert!(!e.matches("Trips/Japan/2023/proofs", "proofs", true));
        assert!(e.matches("raw/2023/05/a.xmp", "a.xmp", false));
    }

    #[test]
    fn trailing_slash_means_folders_only() {
        let e = ex(&["cache/"]);
        assert!(e.matches("x/cache", "cache", true));
        assert!(!e.matches("x/cache", "cache", false));
    }

    #[test]
    fn blank_lines_and_comments_are_ignored() {
        let (e, bad) = Excludes::new(&["".into(), "  ".into(), "# note".into(), "[".into()]);
        assert_eq!(bad, vec!["[".to_string()]);
        assert!(!e.matches("a", "a", false));
    }

    #[test]
    fn junk_is_recognized() {
        for n in [".DS_Store", "Thumbs.db", "._IMG_1.JPG", "$RECYCLE.BIN", ".Trash-1000", "@eaDir"] {
            assert!(is_junk(n), "{n}");
        }
        for n in ["IMG_1.JPG", ".hidden", "notes.txt", ".dupli"] {
            assert!(!is_junk(n), "{n}");
        }
    }
}
