use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use unicode_normalization::{is_nfc_quick, IsNormalized, UnicodeNormalization};

pub fn key_of(rel: &str, fold: bool) -> String {
    if rel.is_ascii() {
        return if fold { rel.to_ascii_lowercase() } else { rel.to_owned() };
    }
    let nfc: String = match is_nfc_quick(rel.chars()) {
        IsNormalized::Yes => rel.to_owned(),
        _ => rel.nfc().collect(),
    };
    if fold {
        nfc.to_lowercase()
    } else {
        nfc
    }
}

pub fn parent(rel: &str) -> &str {
    match rel.rfind('/') {
        Some(i) => &rel[..i],
        None => "",
    }
}

pub fn file_name(rel: &str) -> &str {
    match rel.rfind('/') {
        Some(i) => &rel[i + 1..],
        None => rel,
    }
}

pub fn depth(rel: &str) -> usize {
    rel.bytes().filter(|b| *b == b'/').count()
}

pub fn join(root: &Path, rel: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for part in rel.split('/') {
        if !part.is_empty() {
            p.push(part);
        }
    }
    p
}

pub fn with_suffix(rel: &str, suffix: &str) -> String {
    let name_at = rel.rfind('/').map_or(0, |i| i + 1);
    let name = &rel[name_at..];
    match name.rfind('.') {
        Some(dot) if dot > 0 => format!("{}{}{}{}", &rel[..name_at], &name[..dot], suffix, &name[dot..]),
        _ => format!("{rel}{suffix}"),
    }
}

pub fn is_within(key: &str, dir: &str) -> bool {
    key.len() >= dir.len()
        && key.starts_with(dir)
        && (key.len() == dir.len() || key.as_bytes()[dir.len()] == b'/')
}

pub fn to_ns(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_nanos()).unwrap_or(i64::MAX),
        Err(e) => -i64::try_from(e.duration().as_nanos()).unwrap_or(i64::MAX),
    }
}

pub fn from_ns(ns: i64) -> SystemTime {
    if ns >= 0 {
        UNIX_EPOCH + Duration::from_nanos(ns as u64)
    } else {
        UNIX_EPOCH - Duration::from_nanos(ns.unsigned_abs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_fold_case_and_normalization() {
        let composed = "Caf\u{e9}/IMG.JPG";
        let decomposed = "Cafe\u{301}/IMG.JPG";
        assert_eq!(key_of(composed, false), key_of(decomposed, false));
        assert_ne!(key_of("A/b", false), key_of("a/B", false));
        assert_eq!(key_of("A/b", true), key_of("a/B", true));
        assert_eq!(key_of(composed, true), "caf\u{e9}/img.jpg");
    }

    #[test]
    fn suffix_goes_before_the_extension() {
        assert_eq!(with_suffix("a/b.jpg", " (x)"), "a/b (x).jpg");
        assert_eq!(with_suffix("a.b/c", " (x)"), "a.b/c (x)");
        assert_eq!(with_suffix(".hidden", " (x)"), ".hidden (x)");
        assert_eq!(with_suffix("x.tar.gz", "-2"), "x.tar-2.gz");
    }

    #[test]
    fn within_respects_folder_boundaries() {
        assert!(is_within("a/b/c", "a/b"));
        assert!(is_within("a/b", "a/b"));
        assert!(!is_within("a/bc", "a/b"));
    }

    #[test]
    fn times_round_trip_before_and_after_1970() {
        for ns in [0, 1, 1_700_000_000_123_456_789, -5_000_000_000] {
            assert_eq!(to_ns(from_ns(ns)), ns);
        }
    }
}
