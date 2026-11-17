use std::fs::{self, File, FileTimes, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::SystemTime;

use crate::names;

pub const BUFFER: usize = 1 << 20;

#[derive(Debug)]
pub enum CopyError {
    Cancelled,
    SourceChanged,
    Mismatch,
    Io(io::Error),
}

impl From<io::Error> for CopyError {
    fn from(e: io::Error) -> Self {
        CopyError::Io(e)
    }
}

pub struct Staged {
    pub path: PathBuf,
    pub size: u64,
}

impl Staged {
    pub fn discard(self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn part_name() -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    format!(".dupli-{:x}-{:x}.part", std::process::id(), N.fetch_add(1, Ordering::Relaxed))
}

// Write to a temporary file. The caller gives the file its correct name.
pub fn stage(
    src: &Path,
    dir: &Path,
    verify: bool,
    buf: &mut Vec<u8>,
    cancel: &AtomicBool,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<Staged, CopyError> {
    let mut input = File::open(src)?;
    let before = input.metadata()?;
    let tmp = dir.join(part_name());
    let result = (|| -> Result<u64, CopyError> {
        let mut out = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        let mut hasher = verify.then(blake3::Hasher::new);
        if buf.len() != BUFFER {
            buf.resize(BUFFER, 0);
        }
        let mut total = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(CopyError::Cancelled);
            }
            let n = match input.read(buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            };
            out.write_all(&buf[..n])?;
            if let Some(h) = hasher.as_mut() {
                h.update(&buf[..n]);
            }
            total += n as u64;
            on_bytes(n as u64);
        }
        out.flush()?;
        if verify {
            out.sync_all()?;
        }
        drop(out);

        // Make sure that the source file did not change during this function.
        let after = fs::metadata(src)?;
        if after.len() != before.len() || after.len() != total || after.modified().ok() != before.modified().ok() {
            return Err(CopyError::SourceChanged);
        }
        if let Some(h) = hasher {
            if hash_file(&tmp, true, cancel, &mut |_| {})? != h.finalize() {
                return Err(CopyError::Mismatch);
            }
        }
        stamp(&tmp, before.modified()?, before.created().ok())?;
        let _ = fs::set_permissions(&tmp, before.permissions());
        Ok(total)
    })();
    match result {
        Ok(size) => Ok(Staged { path: tmp, size }),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

pub fn stamp(path: &Path, modified: SystemTime, created: Option<SystemTime>) -> io::Result<()> {
    let file = OpenOptions::new().write(true).open(path)?;
    #[allow(unused_mut)]
    let mut times = FileTimes::new().set_modified(modified).set_accessed(SystemTime::now());
    #[cfg(target_os = "macos")]
    if let Some(c) = created {
        use std::os::macos::fs::FileTimesExt;
        times = times.set_created(c);
    }
    #[cfg(windows)]
    if let Some(c) = created {
        use std::os::windows::fs::FileTimesExt;
        times = times.set_created(c);
    }
    let _ = created;
    file.set_times(times)
}

pub fn hash_file(
    path: &Path,
    uncached: bool,
    cancel: &AtomicBool,
    on_bytes: &mut dyn FnMut(u64),
) -> io::Result<blake3::Hash> {
    let mut f = File::open(path)?;
    if uncached {
        no_cache(&f);
    }
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; BUFFER];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "stopped"));
        }
        let n = match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        hasher.update(&buf[..n]);
        on_bytes(n as u64);
    }
    Ok(hasher.finalize())
}

#[cfg(target_os = "macos")]
fn no_cache(f: &File) {
    use std::os::fd::AsRawFd;
    unsafe {
        libc::fcntl(f.as_raw_fd(), libc::F_NOCACHE, 1);
    }
}

#[cfg(not(target_os = "macos"))]
fn no_cache(_f: &File) {}

const SAMPLE: u64 = 64 * 1024;

// Read the start, the middle, and the end of the file.
pub fn fingerprint(path: &Path) -> io::Result<[u8; 32]> {
    let mut f = File::open(path)?;
    let size = f.metadata()?.len();
    let mut hasher = blake3::Hasher::new();
    hasher.update(&size.to_le_bytes());
    let mut buf = vec![0u8; SAMPLE as usize];
    let offsets: &[u64] = if size <= SAMPLE * 3 { &[0] } else { &[0, size / 2 - SAMPLE / 2, size - SAMPLE] };
    let whole = size <= SAMPLE * 3;
    for &at in offsets {
        f.seek(SeekFrom::Start(at))?;
        let mut left = if whole { size } else { SAMPLE };
        while left > 0 {
            let want = left.min(SAMPLE) as usize;
            let n = f.read(&mut buf[..want])?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            left -= n as u64;
        }
    }
    Ok(*hasher.finalize().as_bytes())
}

pub fn same_contents(a: &Path, b: &Path, cancel: &AtomicBool, on_bytes: &mut dyn FnMut(u64)) -> io::Result<bool> {
    std::thread::scope(|scope| {
        let other = scope.spawn(|| hash_file(b, false, cancel, &mut |_| {}));
        let mine = hash_file(a, false, cancel, on_bytes);
        let theirs = other.join().map_err(|_| io::Error::other("compare failed"))?;
        Ok(mine? == theirs?)
    })
}

pub enum Found {
    Missing,
    File { size: u64, mtime: i64 },
    Other,
}

pub fn look(path: &Path) -> io::Result<Found> {
    match fs::symlink_metadata(path) {
        Ok(md) if md.is_file() => Ok(Found::File { size: md.len(), mtime: md.modified().map(names::to_ns).unwrap_or(0) }),
        Ok(_) => Ok(Found::Other),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Found::Missing),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn staged_copy_keeps_contents_and_modified_time() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.bin");
        let data: Vec<u8> = (0..300_000u32).flat_map(|i| i.to_le_bytes()).collect();
        fs::write(&src, &data).unwrap();
        let when = SystemTime::UNIX_EPOCH + Duration::from_secs(1_500_000_000);
        stamp(&src, when, None).unwrap();

        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        let mut seen = 0;
        let staged = stage(&src, &out, true, &mut Vec::new(), &AtomicBool::new(false), &mut |n| seen += n).unwrap();
        assert_eq!(seen, data.len() as u64);
        assert_eq!(staged.size, data.len() as u64);
        assert_eq!(fs::read(&staged.path).unwrap(), data);
        assert_eq!(fs::metadata(&staged.path).unwrap().modified().unwrap(), when);
        assert!(crate::scan::is_part_file(staged.path.file_name().unwrap().to_str().unwrap()));
    }

    #[test]
    fn a_cancelled_copy_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.bin");
        fs::write(&src, vec![7u8; 10]).unwrap();
        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        let r = stage(&src, &out, false, &mut Vec::new(), &AtomicBool::new(true), &mut |_| {});
        assert!(matches!(r, Err(CopyError::Cancelled)));
        assert_eq!(fs::read_dir(&out).unwrap().count(), 0);
    }

    #[test]
    fn fingerprints_tell_apart_files_that_differ_at_either_end() {
        let dir = tempfile::tempdir().unwrap();
        let big = |fill: u8, first: u8, last: u8| {
            let mut v = vec![fill; 500_000];
            v[0] = first;
            *v.last_mut().unwrap() = last;
            v
        };
        let p = |n: &str, v: Vec<u8>| {
            let p = dir.path().join(n);
            fs::write(&p, v).unwrap();
            p
        };
        let a = fingerprint(&p("a", big(1, 1, 1))).unwrap();
        assert_eq!(a, fingerprint(&p("a2", big(1, 1, 1))).unwrap());
        assert_ne!(a, fingerprint(&p("b", big(1, 2, 1))).unwrap());
        assert_ne!(a, fingerprint(&p("c", big(1, 1, 2))).unwrap());
        assert_ne!(fingerprint(&p("s1", vec![1, 2, 3])).unwrap(), fingerprint(&p("s2", vec![1, 2, 4])).unwrap());
        assert_eq!(fingerprint(&p("e1", vec![])).unwrap(), fingerprint(&p("e2", vec![])).unwrap());
    }

    #[test]
    fn same_contents_reads_both_files() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = (dir.path().join("a"), dir.path().join("b"), dir.path().join("c"));
        fs::write(&a, b"hello world").unwrap();
        fs::write(&b, b"hello world").unwrap();
        fs::write(&c, b"hello_world").unwrap();
        let no = AtomicBool::new(false);
        assert!(same_contents(&a, &b, &no, &mut |_| {}).unwrap());
        assert!(!same_contents(&a, &c, &no, &mut |_| {}).unwrap());
    }
}
