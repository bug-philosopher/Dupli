use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use dupli_core::config::PairConfig;
use dupli_core::exec::Report;
use dupli_core::history::{self, Reason};
use dupli_core::plan::{CompareOptions, Guard, NoteKind, Op, Plan, Snapshot};
use dupli_core::sync::{apply, compare, SyncError};
use dupli_core::{copy, names, Mode, Side};

use Side::{Copy, Source};

const T0: u64 = 1_700_000_000;

struct Lab {
    dir: tempfile::TempDir,
    pair: PairConfig,
    snap: Option<Snapshot>,
}

impl Lab {
    fn new(mode: Mode) -> Lab {
        let dir = tempfile::tempdir().unwrap();
        let (source, copy) = (dir.path().join("source"), dir.path().join("copy"));
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&copy).unwrap();
        let pair = PairConfig {
            id: "lab".into(),
            name: "Lab".into(),
            source: source.to_str().unwrap().into(),
            copy: copy.to_str().unwrap().into(),
            mode,
            ..Default::default()
        };
        Lab { dir, pair, snap: None }
    }

    fn root(&self, side: Side) -> PathBuf {
        self.dir.path().join(if side == Source { "source" } else { "copy" })
    }

    fn at(&self, side: Side, rel: &str) -> PathBuf {
        names::join(&self.root(side), rel)
    }

    fn put(&self, side: Side, rel: &str, data: &str, age: u64) {
        let p = self.at(side, rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, data).unwrap();
        copy::stamp(&p, SystemTime::UNIX_EPOCH + Duration::from_secs(T0 + age), None).unwrap();
    }

    fn del(&self, side: Side, rel: &str) {
        fs::remove_file(self.at(side, rel)).unwrap();
    }

    fn mv(&self, side: Side, from: &str, to: &str) {
        let to = self.at(side, to);
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::rename(self.at(side, from), to).unwrap();
    }

    fn read(&self, side: Side, rel: &str) -> String {
        fs::read_to_string(self.at(side, rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
    }

    fn mtime(&self, side: Side, rel: &str) -> SystemTime {
        fs::metadata(self.at(side, rel)).unwrap().modified().unwrap()
    }

    fn files(&self, side: Side) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        walk(&self.root(side), "", &mut |rel, path, is_dir| {
            if !is_dir {
                out.insert(rel.to_owned(), fs::read_to_string(path).unwrap_or_default());
            }
        });
        out
    }

    fn dirs(&self, side: Side) -> Vec<String> {
        let mut out = Vec::new();
        walk(&self.root(side), "", &mut |rel, _, is_dir| {
            if is_dir {
                out.push(rel.to_owned());
            }
        });
        out.sort();
        out
    }

    fn kept(&self, side: Side) -> BTreeMap<String, (String, Reason)> {
        let root = self.root(side);
        let mut out = BTreeMap::new();
        for run in history::runs(&root) {
            for e in history::entries(&root, &run.id) {
                let held = names::join(&history::history_dir(&root).join(&run.id), &e.rel);
                out.insert(e.rel.clone(), (fs::read_to_string(held).unwrap(), e.reason));
            }
        }
        out
    }

    fn compare(&self) -> Plan {
        compare(&self.pair, self.snap.clone(), &AtomicBool::new(false), &|_, _| {}).unwrap()
    }

    fn apply(&mut self, plan: &Plan) -> Report {
        let out = apply(&self.pair, plan, &AtomicBool::new(false), &mut |_| {});
        if out.snapshot.is_some() {
            self.snap = out.snapshot;
        }
        out.report
    }

    fn sync(&mut self) -> Report {
        let plan = self.compare();
        assert_eq!(plan.guard, None, "guard tripped");
        self.apply(&plan)
    }

    #[track_caller]
    fn assert_in_sync(&self) {
        assert_eq!(self.files(Source), self.files(Copy));
        let plan = self.compare();
        assert!(plan.actions.is_empty(), "still to do: {:?}", ops(&plan));
    }
}

fn walk(dir: &Path, rel: &str, f: &mut dyn FnMut(&str, &Path, bool)) {
    let mut items: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap()).collect();
    items.sort_by_key(|e| e.file_name());
    for item in items {
        let name = item.file_name().to_string_lossy().into_owned();
        if rel.is_empty() && name == ".dupli" {
            continue;
        }
        let child = if rel.is_empty() { name } else { format!("{rel}/{name}") };
        let kind = item.file_type().unwrap();
        if kind.is_dir() {
            f(&child, &item.path(), true);
            walk(&item.path(), &child, f);
        } else {
            f(&child, &item.path(), false);
        }
    }
}

fn ops(p: &Plan) -> Vec<String> {
    p.actions
        .iter()
        .map(|a| {
            let side = if a.side == Source { "S" } else { "C" };
            match (&a.op, &a.from) {
                (Op::Move, Some(f)) => format!("{side} move {f} -> {}", a.rel),
                (op, _) => format!("{side} {} {}", format!("{op:?}").to_lowercase(), a.rel),
            }
        })
        .collect()
}

fn pairs<'a>(items: &[(&'a str, &'a str)]) -> BTreeMap<String, String> {
    items.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

#[cfg(unix)]
fn inode(p: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(p).unwrap().ino()
}

#[test]
fn mirror_makes_the_copy_match_and_keeps_what_it_takes_away() {
    let mut lab = Lab::new(Mode::Mirror);
    lab.put(Source, "a.txt", "alpha", 1);
    lab.put(Source, "Trips/b.txt", "bravo", 2);
    lab.put(Source, "Trips/Japan/c.txt", "charlie", 3);
    fs::create_dir(lab.at(Source, "Empty")).unwrap();

    let r = lab.sync();
    assert_eq!((r.copied, r.dirs_made, r.bytes), (3, 3, 17));
    assert!(r.clean());
    lab.assert_in_sync();
    assert_eq!(lab.dirs(Copy), ["Empty", "Trips", "Trips/Japan"]);
    assert_eq!(lab.mtime(Copy, "Trips/b.txt"), lab.mtime(Source, "Trips/b.txt"));

    lab.put(Source, "Trips/b.txt", "bravo, second take", 50);
    lab.del(Source, "a.txt");
    lab.put(Source, "new.txt", "n", 60);
    fs::remove_dir(lab.at(Source, "Empty")).unwrap();

    let r = lab.sync();
    assert_eq!((r.copied, r.updated, r.removed, r.dirs_removed, r.kept), (1, 1, 1, 1, 2));
    lab.assert_in_sync();
    let kept = lab.kept(Copy);
    assert_eq!(kept["a.txt"], ("alpha".to_string(), Reason::Removed));
    assert_eq!(kept["Trips/b.txt"], ("bravo".to_string(), Reason::Replaced));

    assert!(!lab.at(Source, ".dupli").exists());
    assert!(lab.kept(Source).is_empty());
}

#[test]
fn a_second_sync_with_nothing_changed_does_nothing() {
    let mut lab = Lab::new(Mode::Mirror);
    lab.put(Source, "a/b/c.txt", "x", 1);
    lab.sync();
    let r = lab.sync();
    assert_eq!(r.changes(), 0);
    assert!(!lab.at(Copy, ".dupli").exists(), "an idle sync should leave no trace");
}

#[cfg(unix)]
#[test]
fn reorganized_folders_are_renamed_in_the_copy_not_copied_again() {
    let mut lab = Lab::new(Mode::Mirror);
    for i in 0..6 {
        lab.put(Source, &format!("Trips/Japan 2023/IMG_{i}.jpg"), &"x".repeat(100 + i), i as u64);
    }
    lab.sync();
    let before: Vec<u64> = (0..6).map(|i| inode(&lab.at(Copy, &format!("Trips/Japan 2023/IMG_{i}.jpg")))).collect();

    lab.mv(Source, "Trips/Japan 2023", "2023/Japan");
    fs::remove_dir(lab.at(Source, "Trips")).unwrap();
    lab.put(Source, "2023/Japan/IMG_new.jpg", "fresh", 99);

    let plan = lab.compare();
    assert_eq!((plan.summary.mv.n, plan.summary.copy.n, plan.summary.remove.n), (6, 1, 0));
    let r = lab.apply(&plan);
    assert_eq!((r.moved, r.copied, r.removed, r.kept, r.bytes), (6, 1, 0, 0, 5));
    assert!(r.clean(), "{:?}", r.problems);

    let after: Vec<u64> = (0..6).map(|i| inode(&lab.at(Copy, &format!("2023/Japan/IMG_{i}.jpg")))).collect();
    assert_eq!(before, after);
    assert_eq!(lab.dirs(Copy), ["2023", "2023/Japan"]);
    lab.assert_in_sync();
}

#[test]
fn swapping_two_folders_names_copies_nothing() {
    let mut lab = Lab::new(Mode::Mirror);
    lab.put(Source, "Edits/a.jpg", "first edit of a", 1);
    lab.put(Source, "Edits/b.jpg", "first edit of b!", 2);
    lab.put(Source, "Edits new/a.jpg", "second edit of a, longer", 5);
    lab.put(Source, "Edits new/b.jpg", "second edit of b, longer!", 6);
    lab.sync();

    lab.mv(Source, "Edits", "Edits old");
    lab.mv(Source, "Edits new", "Edits");
    let r = lab.sync();
    assert_eq!((r.moved, r.copied, r.updated, r.removed, r.kept, r.bytes), (4, 0, 0, 0, 0, 0));
    assert!(r.clean(), "{:?}", r.problems);
    lab.assert_in_sync();
    assert_eq!(lab.read(Copy, "Edits/a.jpg"), "second edit of a, longer");
    assert_eq!(lab.read(Copy, "Edits old/b.jpg"), "first edit of b!");
}

#[test]
fn files_that_trade_names_are_untangled() {
    let mut lab = Lab::new(Mode::Mirror);
    lab.put(Source, "a.jpg", "this was a", 1);
    lab.put(Source, "b.jpg", "and this was b", 2);
    lab.put(Source, "c.jpg", "c, the third one", 3);
    lab.sync();

    lab.mv(Source, "c.jpg", "tmp");
    lab.mv(Source, "b.jpg", "c.jpg");
    lab.mv(Source, "a.jpg", "b.jpg");
    lab.mv(Source, "tmp", "a.jpg");
    let r = lab.sync();
    assert_eq!((r.moved, r.copied, r.updated, r.kept), (3, 0, 0, 0));
    assert!(r.clean(), "{:?}", r.problems);
    lab.assert_in_sync();
    assert_eq!(lab.read(Copy, "b.jpg"), "this was a");
    assert_eq!(fs::read_dir(lab.root(Copy)).unwrap().count(), 3);
}

#[test]
fn a_move_is_only_trusted_when_the_contents_agree() {
    let mut lab = Lab::new(Mode::Mirror);
    lab.put(Source, "old/a.bin", "AAAA", 1);
    lab.put(Source, "old/b.bin", "BBBB", 1);
    lab.sync();

    lab.mv(Source, "old/b.bin", "new/renamed.bin");
    lab.del(Source, "old/a.bin");
    fs::remove_dir(lab.at(Source, "old")).unwrap();
    lab.put(Source, "new/other.bin", "ZZZZ", 1);
    let plan = lab.compare();
    assert_eq!(
        ops(&plan),
        ["C copy new/other.bin", "C move old/b.bin -> new/renamed.bin", "C remove old/a.bin", "C makedir new", "C removedir old"]
    );
    lab.apply(&plan);
    lab.assert_in_sync();
    assert_eq!(lab.kept(Copy)["old/a.bin"].0, "AAAA");
}

#[test]
fn big_files_are_recognized_as_moved() {
    let mut lab = Lab::new(Mode::Mirror);
    let clip = |tag: char| {
        let mut s = "v".repeat(400_000);
        s.replace_range(0..1, &tag.to_string());
        s
    };
    lab.put(Source, "card/A.mov", &clip('A'), 1);
    lab.put(Source, "card/B.mov", &clip('B'), 1);
    lab.sync();
    lab.mv(Source, "card/A.mov", "edit/take-1.mov");
    lab.mv(Source, "card/B.mov", "edit/take-2.mov");
    let r = lab.sync();
    assert_eq!((r.moved, r.copied, r.bytes), (2, 0, 0));
    assert_eq!(&lab.read(Copy, "edit/take-2.mov")[..1], "B");
    lab.assert_in_sync();
}

#[test]
fn a_file_that_becomes_a_folder_and_a_folder_that_becomes_a_file() {
    let mut lab = Lab::new(Mode::Mirror);
    lab.put(Source, "notes", "a plain file", 1);
    lab.put(Source, "album/one.jpg", "1", 2);
    lab.sync();

    lab.del(Source, "notes");
    lab.put(Source, "notes/today.txt", "now a folder", 10);
    fs::remove_dir_all(lab.at(Source, "album")).unwrap();
    lab.put(Source, "album", "now a file", 11);
    let r = lab.sync();
    assert!(r.clean(), "{:?}", r.problems);
    lab.assert_in_sync();
    assert_eq!(lab.read(Copy, "album"), "now a file");
    assert_eq!(lab.kept(Copy).len(), 2);
}

#[test]
fn a_date_that_differs_is_corrected_without_copying_when_contents_match() {
    let mut lab = Lab::new(Mode::Mirror);
    lab.put(Source, "same.bin", "identical bytes", 10);
    lab.put(Copy, "same.bin", "identical bytes", 5_000);
    lab.put(Source, "diff.bin", "AAAA", 10);
    lab.put(Copy, "diff.bin", "BBBB", 5_000);

    let plan = lab.compare();
    assert_eq!(plan.summary.update.n, 2);
    let r = lab.apply(&plan);
    assert_eq!((r.touched, r.updated, r.kept, r.bytes), (1, 1, 1, 4));
    assert_eq!(lab.mtime(Copy, "same.bin"), lab.mtime(Source, "same.bin"));
    assert_eq!(lab.kept(Copy)["diff.bin"].0, "BBBB");
    lab.assert_in_sync();

    lab.pair.check_contents = false;
    lab.put(Copy, "same.bin", "identical bytes", 9_000);
    let r = lab.sync();
    assert_eq!((r.touched, r.updated), (0, 1));
}

#[test]
fn verified_copies_read_back_the_same() {
    let mut lab = Lab::new(Mode::Mirror);
    lab.pair.verify = true;
    lab.put(Source, "big.bin", &"0123456789".repeat(150_000), 1);
    lab.put(Source, "empty.bin", "", 2);
    let r = lab.sync();
    assert_eq!((r.copied, r.bytes), (2, 1_500_000));
    assert!(r.clean());
    lab.assert_in_sync();
}

#[test]
fn with_history_off_removals_are_final() {
    let mut lab = Lab::new(Mode::Mirror);
    lab.pair.keep_history = false;
    lab.put(Source, "a.txt", "v1", 1);
    lab.put(Source, "b.txt", "b", 1);
    lab.sync();
    lab.put(Source, "a.txt", "v2!", 50);
    lab.del(Source, "b.txt");
    let r = lab.sync();
    assert_eq!((r.updated, r.removed, r.kept), (1, 1, 0));
    assert!(!lab.at(Copy, ".dupli").exists());
    lab.assert_in_sync();
}

#[test]
fn an_empty_source_does_not_quietly_empty_the_copy() {
    let mut lab = Lab::new(Mode::Mirror);
    for i in 0..5 {
        lab.put(Source, &format!("{i}.jpg"), "photo", i);
    }
    lab.sync();
    for i in 0..5 {
        lab.del(Source, &format!("{i}.jpg"));
    }
    let plan = lab.compare();
    assert_eq!(plan.guard, Some(Guard::EmptySide { side: Source, other_files: 5 }));

    let r = lab.apply(&plan);
    assert_eq!((r.removed, r.kept), (5, 5));
    assert_eq!(lab.kept(Copy).len(), 5);
}

#[test]
fn a_missing_folder_is_reported_not_treated_as_empty() {
    let lab = Lab::new(Mode::Mirror);
    lab.put(Copy, "precious.jpg", "x", 1);
    fs::remove_dir_all(lab.root(Source)).unwrap();
    let r = compare(&lab.pair, None, &AtomicBool::new(false), &|_, _| {});
    assert_eq!(r.err(), Some(SyncError::Missing { side: Source }));
    assert_eq!(lab.read(Copy, "precious.jpg"), "x");
}

#[test]
fn nested_folders_cannot_be_a_pair() {
    let mut lab = Lab::new(Mode::Mirror);
    fs::create_dir_all(lab.at(Source, "inside")).unwrap();
    lab.pair.copy = lab.at(Source, "inside").to_str().unwrap().into();
    let r = compare(&lab.pair, None, &AtomicBool::new(false), &|_, _| {});
    assert_eq!(r.err(), Some(SyncError::Overlap));
    lab.pair.copy = lab.pair.source.clone();
    assert_eq!(compare(&lab.pair, None, &AtomicBool::new(false), &|_, _| {}).err(), Some(SyncError::Overlap));
}

#[test]
fn a_plan_gone_stale_does_not_touch_what_it_has_not_seen() {
    let mut lab = Lab::new(Mode::Mirror);
    lab.put(Source, "changed.txt", "new from source", 100);
    lab.put(Copy, "changed.txt", "old", 1);
    lab.put(Copy, "extra.txt", "to be removed", 1);
    lab.put(Source, "fresh.txt", "new file", 1);
    let plan = lab.compare();
    assert_eq!(ops(&plan), ["C copy fresh.txt", "C update changed.txt", "C remove extra.txt"]);

    lab.put(Copy, "changed.txt", "edited in the copy meanwhile", 200);
    lab.put(Copy, "extra.txt", "edited too, keep me", 200);
    lab.put(Copy, "fresh.txt", "appeared", 200);

    let r = lab.apply(&plan);
    assert_eq!((r.copied, r.updated, r.removed, r.skipped, r.kept), (0, 0, 0, 3, 0));
    assert_eq!(r.problems.len(), 3);
    assert_eq!(lab.read(Copy, "changed.txt"), "edited in the copy meanwhile");
    assert_eq!(lab.read(Copy, "extra.txt"), "edited too, keep me");
    assert_eq!(lab.read(Copy, "fresh.txt"), "appeared");
}

#[test]
fn a_sync_stopped_midway_picks_up_where_it_left_off() {
    let mut lab = Lab::new(Mode::Mirror);
    for i in 0..8 {
        lab.put(Source, &format!("f{i}.bin"), &"d".repeat(1000), i);
    }
    let plan = lab.compare();
    let stop = AtomicBool::new(false);
    let out = apply(&lab.pair, &plan, &stop, &mut |p| {
        if p.files_done >= 3 {
            stop.store(true, Ordering::Relaxed);
        }
    });
    assert!(out.report.cancelled);
    assert!(out.report.copied >= 3 && out.report.copied < 8, "{}", out.report.copied);
    assert!(lab.files(Copy).keys().all(|k| k.starts_with('f') && k.ends_with(".bin")));
    assert!(lab.files(Copy).values().all(|v| v.len() == 1000));

    let r = lab.sync();
    assert_eq!(r.copied, 8 - out.report.copied);
    lab.assert_in_sync();
}

#[test]
fn leftovers_of_an_interrupted_sync_are_cleared() {
    let mut lab = Lab::new(Mode::Mirror);
    lab.put(Source, "a.txt", "a", 1);
    lab.put(Copy, ".dupli-1f-0.part", "half a file", 1);
    lab.put(Copy, "sub/.dupli-1f-1.part", "half a file", 1);
    lab.put(Source, "sub/b.txt", "b", 1);
    lab.sync();
    assert_eq!(lab.files(Copy), pairs(&[("a.txt", "a"), ("sub/b.txt", "b")]));
}

#[cfg(unix)]
#[test]
fn a_folder_that_cannot_be_read_does_not_cost_the_copy_its_files() {
    use std::os::unix::fs::PermissionsExt;
    let mut lab = Lab::new(Mode::Mirror);
    lab.put(Source, "locked/a.txt", "a", 1);
    lab.put(Source, "open/b.txt", "b", 1);
    lab.sync();

    let locked = lab.at(Source, "locked");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let plan = lab.compare();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

    assert!(plan.actions.is_empty(), "{:?}", ops(&plan));
    assert_eq!(plan.notes.iter().map(|n| (n.kind, n.rel.as_str())).collect::<Vec<_>>(), [(NoteKind::Unreadable, "locked")]);
    assert_eq!(lab.read(Copy, "locked/a.txt"), "a");
}

#[cfg(unix)]
#[test]
fn links_are_left_alone_on_both_sides() {
    let mut lab = Lab::new(Mode::Mirror);
    lab.put(Source, "real.txt", "r", 1);
    std::os::unix::fs::symlink(lab.at(Source, "real.txt"), lab.at(Source, "link.txt")).unwrap();
    fs::create_dir_all(lab.at(Copy, "only-here")).unwrap();
    std::os::unix::fs::symlink("/nowhere", lab.at(Copy, "only-here/dangling")).unwrap();

    let plan = lab.compare();
    assert_eq!(ops(&plan), ["C copy real.txt"]);
    assert_eq!(plan.notes.iter().filter(|n| n.kind == NoteKind::Link).count(), 2);
    lab.apply(&plan);
    assert!(lab.at(Copy, "only-here/dangling").symlink_metadata().is_ok());
    assert!(lab.compare().actions.is_empty());
}

#[test]
fn skipped_patterns_and_system_clutter_stay_out_of_it() {
    let mut lab = Lab::new(Mode::Mirror);
    lab.pair.excludes = vec!["*.tmp".into(), "cache/".into()];
    lab.put(Source, "keep.txt", "k", 1);
    lab.put(Source, "scratch.tmp", "no", 1);
    lab.put(Source, "cache/c.bin", "no", 1);
    lab.put(Source, ".DS_Store", "finder", 1);
    lab.put(Copy, "mine.tmp", "left alone", 1);
    lab.put(Copy, "._keep.txt", "resource fork", 1);
    lab.put(Copy, "gone/.DS_Store", "finder", 1);
    lab.put(Copy, "holds-skipped/x.tmp", "left alone", 1);

    let r = lab.sync();
    assert_eq!((r.copied, r.removed, r.dirs_removed), (1, 0, 1));
    assert_eq!(
        lab.files(Copy),
        pairs(&[("._keep.txt", "resource fork"), ("holds-skipped/x.tmp", "left alone"), ("keep.txt", "k"), ("mine.tmp", "left alone")])
    );
    assert!(lab.compare().actions.is_empty());
}

#[test]
fn hidden_files_can_be_skipped() {
    let mut lab = Lab::new(Mode::Mirror);
    lab.pair.skip_hidden = true;
    lab.put(Source, ".git/config", "x", 1);
    lab.put(Source, ".env", "x", 1);
    lab.put(Source, "shown.txt", "x", 1);
    lab.sync();
    assert_eq!(lab.files(Copy), pairs(&[("shown.txt", "x")]));
}

#[cfg(target_os = "macos")]
#[test]
fn names_that_differ_only_by_case_are_one_file_on_a_mac() {
    let mut lab = Lab::new(Mode::Mirror);
    lab.put(Source, "Trips/IMG_1.JPG", "same", 1);
    lab.put(Copy, "trips/img_1.jpg", "same", 1);
    assert!(lab.compare().actions.is_empty());

    lab.put(Source, "Trips/IMG_1.JPG", "changed!", 50);
    lab.put(Source, "Trips/IMG_2.JPG", "two", 51);
    let r = lab.sync();
    assert_eq!((r.updated, r.copied, r.removed), (1, 1, 0));
    assert_eq!(lab.files(Copy), pairs(&[("trips/IMG_2.JPG", "two"), ("trips/img_1.jpg", "changed!")]));
    assert_eq!(lab.kept(Copy)["trips/img_1.jpg"].0, "same");
}

#[test]
fn accented_names_match_however_the_drive_stores_them() {
    let lab = Lab::new(Mode::Mirror);
    lab.put(Source, "Caf\u{e9}/\u{e9}t\u{e9}.txt", "summer", 1);
    lab.put(Copy, "Cafe\u{301}/e\u{301}te\u{301}.txt", "summer", 1);
    let plan = lab.compare();
    assert!(plan.actions.is_empty(), "{:?}", ops(&plan));
}

#[test]
fn add_only_never_takes_anything_away() {
    let mut lab = Lab::new(Mode::AddOnly);
    lab.put(Source, "a.txt", "a", 1);
    lab.put(Copy, "only-in-copy.txt", "stays", 1);
    lab.put(Source, "both.txt", "older in source", 1);
    lab.put(Copy, "both.txt", "newer in copy", 500);
    lab.sync();
    lab.del(Source, "a.txt");
    lab.put(Source, "b.txt", "b", 2);
    let r = lab.sync();
    assert_eq!((r.copied, r.removed, r.updated), (1, 0, 0));
    assert_eq!(
        lab.files(Copy),
        pairs(&[("a.txt", "a"), ("b.txt", "b"), ("both.txt", "newer in copy"), ("only-in-copy.txt", "stays")])
    );
}

#[test]
fn two_way_carries_changes_both_ways_and_remembers() {
    let mut lab = Lab::new(Mode::TwoWay);
    lab.put(Source, "s.txt", "from source", 1);
    lab.put(Copy, "c.txt", "from copy", 2);
    let r = lab.sync();
    assert_eq!(r.copied, 2);
    lab.assert_in_sync();

    lab.del(Source, "c.txt");
    lab.put(Copy, "s.txt", "edited in the copy", 50);
    let plan = lab.compare();
    assert_eq!(ops(&plan), ["S update s.txt", "C remove c.txt"]);
    assert_eq!(plan.summary.conflicts, 0);
    lab.apply(&plan);
    lab.assert_in_sync();
    assert_eq!(lab.files(Source), pairs(&[("s.txt", "edited in the copy")]));
    assert_eq!(lab.kept(Copy)["c.txt"], ("from copy".to_string(), Reason::Removed));
    assert_eq!(lab.kept(Source)["s.txt"], ("from source".to_string(), Reason::Replaced));

    lab.put(Source, "s.txt", "source's edit", 100);
    lab.put(Copy, "s.txt", "copy's edit, later", 200);
    let plan = lab.compare();
    assert_eq!((ops(&plan), plan.summary.conflicts), (vec!["S update s.txt".to_string()], 1));
    lab.apply(&plan);
    assert_eq!(lab.read(Source, "s.txt"), "copy's edit, later");
    lab.assert_in_sync();
}

#[test]
fn two_way_follows_a_reorganization_made_on_either_side() {
    let mut lab = Lab::new(Mode::TwoWay);
    lab.put(Source, "inbox/one.raw", "frame one", 1);
    lab.put(Source, "inbox/two.raw", "frame two!", 2);
    lab.sync();

    lab.mv(Source, "inbox/one.raw", "2024/one.raw");
    lab.mv(Copy, "inbox/two.raw", "picks/two.raw");
    let r = lab.sync();
    assert_eq!((r.moved, r.copied, r.removed, r.kept), (2, 0, 0, 0));
    lab.assert_in_sync();
    assert_eq!(lab.dirs(Source), ["2024", "inbox", "picks"]);
    assert_eq!(lab.dirs(Copy), ["2024", "inbox", "picks"]);

    fs::remove_dir(lab.at(Copy, "inbox")).unwrap();
    let r = lab.sync();
    assert_eq!(r.dirs_removed, 1);
    assert_eq!(lab.dirs(Source), ["2024", "picks"]);
}

#[test]
fn two_way_without_its_memory_merges_instead_of_deleting() {
    let mut lab = Lab::new(Mode::TwoWay);
    lab.put(Source, "a.txt", "a", 1);
    lab.sync();
    lab.del(Source, "a.txt");
    lab.snap = None;
    let r = lab.sync();
    assert_eq!((r.removed, r.copied), (0, 1));
    assert_eq!(lab.read(Source, "a.txt"), "a");
}

#[test]
fn two_way_retries_what_failed_instead_of_forgetting_it() {
    let mut lab = Lab::new(Mode::TwoWay);
    lab.put(Source, "a.txt", "a", 1);
    lab.put(Source, "b.txt", "b", 1);
    lab.sync();
    lab.del(Source, "a.txt");
    let plan = lab.compare();
    assert_eq!(ops(&plan), ["C remove a.txt"]);
    lab.put(Copy, "a.txt", "changed meanwhile", 300);
    let r = lab.apply(&plan);
    assert_eq!((r.removed, r.skipped), (0, 1));
    let plan = lab.compare();
    assert_eq!(ops(&plan), ["S copy a.txt"]);
    lab.apply(&plan);
    assert_eq!(lab.read(Source, "a.txt"), "changed meanwhile");
    lab.assert_in_sync();
}

#[test]
fn an_accidental_delete_can_be_undone_from_history() {
    let mut lab = Lab::new(Mode::Mirror);
    lab.put(Source, "Trips/keep.jpg", "keep", 1);
    lab.put(Source, "Trips/oops.jpg", "deleted by accident", 2);
    lab.sync();
    lab.del(Source, "Trips/oops.jpg");
    lab.sync();
    assert!(!lab.at(Copy, "Trips/oops.jpg").exists());

    let run = history::runs(&lab.root(Copy)).remove(0);
    assert_eq!((run.count, run.bytes), (1, 19));
    let report = history::restore(
        &lab.root(Copy),
        Some(&lab.root(Source)),
        &run.id,
        &["Trips/oops.jpg".to_string()],
        &CompareOptions::default(),
        &AtomicBool::new(false),
    );
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(lab.read(Source, "Trips/oops.jpg"), "deleted by accident");
    assert!(history::runs(&lab.root(Copy)).is_empty());
    lab.assert_in_sync();
}

#[test]
fn history_limits_are_applied_after_a_sync() {
    let mut lab = Lab::new(Mode::Mirror);
    lab.pair.history_days = 30;
    lab.put(Copy, "ancient.txt", "x", 1);
    let old = chrono::Local::now() - chrono::Duration::days(45);
    let mut stash = history::Stash::new(&lab.root(Copy), old);
    stash.put("ancient.txt", Reason::Removed).unwrap();
    assert_eq!(history::runs(&lab.root(Copy)).len(), 1);

    lab.put(Source, "a.txt", "a", 1);
    lab.put(Copy, "b.txt", "b", 1);
    lab.sync();
    let runs = history::runs(&lab.root(Copy));
    assert_eq!(runs.len(), 1, "the 45-day-old run should be gone, the new one kept");
    assert_eq!(history::entries(&lab.root(Copy), &runs[0].id)[0].rel, "b.txt");
}
