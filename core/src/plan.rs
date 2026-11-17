use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::names::{self, key_of};
use crate::scan::{Entry, Kind, Tree};
use crate::{Mode, Side};

pub const SECOND_NS: i64 = 1_000_000_000;
pub const HOUR_NS: i64 = 3_600 * SECOND_NS;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Sig {
    pub size: u64,
    pub mtime: i64,
}

impl Sig {
    pub fn of(e: &Entry) -> Sig {
        Sig { size: e.size, mtime: e.mtime }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CompareOptions {
    pub tolerance_ns: i64,
    pub dst_shift: bool,
}

impl Default for CompareOptions {
    fn default() -> Self {
        CompareOptions { tolerance_ns: 2 * SECOND_NS, dst_shift: false }
    }
}

impl CompareOptions {
    pub fn same_time(&self, a: i64, b: i64) -> bool {
        let d = a.saturating_sub(b).saturating_abs();
        d <= self.tolerance_ns || (self.dst_shift && (d - HOUR_NS).abs() <= self.tolerance_ns)
    }

    pub fn same(&self, a: Sig, b: Sig) -> bool {
        a.size == b.size && self.same_time(a.mtime, b.mtime)
    }
}

#[derive(Clone, Debug)]
pub struct PlanOptions {
    pub mode: Mode,
    pub fold: bool,
    pub compare: CompareOptions,
    pub detect_moves: bool,
    pub guard_percent: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Copy,
    Update,
    Move,
    Remove,
    MakeDir,
    RemoveDir,
}

#[derive(Clone, Debug, Serialize)]
pub struct Action {
    pub op: Op,
    pub side: Side,
    pub rel: String,
    pub from: Option<String>,
    pub sig: Sig,
    pub old: Option<Sig>,
    pub conflict: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Count {
    pub n: u64,
    pub bytes: u64,
}

impl Count {
    fn add(&mut self, bytes: u64) {
        self.n += 1;
        self.bytes += bytes;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub copy: Count,
    pub update: Count,
    #[serde(rename = "move")]
    pub mv: Count,
    pub remove: Count,
    pub make_dir: u64,
    pub remove_dir: u64,
    pub conflicts: u64,
    pub to_source: u64,
}

impl Summary {
    pub fn changes(&self) -> u64 {
        self.copy.n + self.update.n + self.mv.n + self.remove.n + self.make_dir + self.remove_dir
    }

    pub fn file_changes(&self) -> u64 {
        self.copy.n + self.update.n + self.mv.n + self.remove.n
    }

    pub fn bytes_to_write(&self) -> u64 {
        self.copy.bytes + self.update.bytes
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Guard {
    EmptySide { side: Side, other_files: u64 },
    ManyRemovals { side: Side, removals: u64, total: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteKind {
    Link,
    Unreadable,
    NameClash,
    NewerInCopy,
    TypeClash,
}

#[derive(Clone, Debug, Serialize)]
pub struct Note {
    pub kind: NoteKind,
    pub side: Side,
    pub rel: String,
}

const NOTE_LIMIT: usize = 300;

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct TreeStats {
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
}

// This is the condition of the two folders after the last sync.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub fold: bool,
    pub files: HashMap<String, [Sig; 2]>,
    pub dirs: HashSet<String>,
}

impl Snapshot {
    pub fn for_fold(self, fold: bool) -> Option<Snapshot> {
        if self.fold == fold {
            return Some(self);
        }
        if !fold {
            return None;
        }
        Some(Snapshot {
            fold: true,
            files: self.files.into_iter().map(|(k, v)| (k.to_lowercase(), v)).collect(),
            dirs: self.dirs.into_iter().map(|k| k.to_lowercase()).collect(),
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Plan {
    pub mode: Mode,
    pub fold: bool,
    pub actions: Vec<Action>,
    pub summary: Summary,
    pub guard: Option<Guard>,
    pub notes: Vec<Note>,
    pub notes_total: u64,
    pub source: TreeStats,
    pub copy: TreeStats,
    #[serde(skip)]
    pub stale_parts: [Vec<String>; 2],
    #[serde(skip)]
    pub after: Option<Snapshot>,
    #[serde(skip)]
    pub affected: Vec<Vec<String>>,
    #[serde(skip)]
    pub base: Option<Snapshot>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    pub fn snapshot_after(&self, completed: &dyn Fn(usize) -> bool) -> Option<Snapshot> {
        let mut snap = self.after.clone()?;
        let empty = Snapshot::default();
        let base = self.base.as_ref().unwrap_or(&empty);
        for (i, keys) in self.affected.iter().enumerate() {
            if completed(i) {
                continue;
            }
            for k in keys {
                match base.files.get(k) {
                    Some(v) => {
                        snap.files.insert(k.clone(), *v);
                    }
                    None => {
                        snap.files.remove(k);
                    }
                }
                if base.dirs.contains(k) {
                    snap.dirs.insert(k.clone());
                } else if self.actions[i].op == Op::MakeDir {
                    snap.dirs.remove(k);
                }
            }
        }
        Some(snap)
    }
}

struct Half<'a> {
    tree: &'a Tree,
    map: HashMap<String, u32>,
}

impl<'a> Half<'a> {
    fn new(tree: &'a Tree, fold: bool, side: Side, notes: &mut Notes) -> Half<'a> {
        let mut map: HashMap<String, u32> = HashMap::with_capacity(tree.entries.len());
        for (i, e) in tree.entries.iter().enumerate() {
            match map.entry(key_of(&e.rel, fold)) {
                std::collections::hash_map::Entry::Occupied(_) => notes.push(NoteKind::NameClash, side, &e.rel),
                std::collections::hash_map::Entry::Vacant(v) => {
                    v.insert(i as u32);
                }
            }
        }
        Half { tree, map }
    }

    fn at(&self, i: u32) -> &'a Entry {
        &self.tree.entries[i as usize]
    }
}

#[derive(Default)]
struct Notes {
    list: Vec<Note>,
    total: u64,
}

impl Notes {
    fn push(&mut self, kind: NoteKind, side: Side, rel: &str) {
        self.total += 1;
        if self.list.len() < NOTE_LIMIT {
            self.list.push(Note { kind, side, rel: rel.to_owned() });
        }
    }
}

struct Put {
    to: Side,
    key: String,
    from: u32,
    existing: Option<u32>,
    conflict: bool,
    blocked_by_dir: bool,
    moved_from: Option<u32>,
    vacates: bool,
}

struct Del {
    on: Side,
    idx: u32,
    taken: bool,
}

struct Resolver {
    fold: bool,
    cache: [HashMap<String, String>; 2],
}

impl Resolver {
    fn resolve(&mut self, to: Side, half: &Half, from_rel: &str) -> String {
        let parent = names::parent(from_rel);
        if parent.is_empty() {
            return from_rel.to_owned();
        }
        let name = names::file_name(from_rel);
        if let Some(p) = self.cache[to.ix()].get(parent) {
            return format!("{p}/{name}");
        }
        let real = self.resolve_dir(half, parent);
        let out = format!("{real}/{name}");
        self.cache[to.ix()].insert(parent.to_owned(), real);
        out
    }

    fn resolve_dir(&self, half: &Half, dir: &str) -> String {
        let mut end = dir.len();
        loop {
            let prefix = &dir[..end];
            if let Some(&i) = half.map.get(&key_of(prefix, self.fold)) {
                let e = half.at(i);
                if e.kind == Kind::Dir {
                    return format!("{}{}", e.rel, &dir[end..]);
                }
            }
            match prefix.rfind('/') {
                Some(i) => end = i,
                None => return dir.to_owned(),
            }
        }
    }
}

pub type MoveCheck<'a> = dyn FnMut(Side, &str, &str) -> bool + 'a;

pub fn build_plan(
    source: &Tree,
    copy: &Tree,
    base: Option<Snapshot>,
    opts: &PlanOptions,
    confirm_move: &mut MoveCheck,
) -> Plan {
    let cmp = opts.compare;
    let fold = opts.fold;
    let two_way = opts.mode == Mode::TwoWay;
    let mut notes = Notes::default();
    let halves = [Half::new(source, fold, Side::Source, &mut notes), Half::new(copy, fold, Side::Copy, &mut notes)];

    for side in Side::BOTH {
        let tree = halves[side.ix()].tree;
        for rel in &tree.links {
            notes.push(NoteKind::Link, side, rel);
        }
        for rel in &tree.unreadable {
            notes.push(NoteKind::Unreadable, side, rel);
        }
    }
    let blocked: Vec<String> =
        source.unreadable.iter().chain(copy.unreadable.iter()).map(|r| key_of(r, fold)).collect();
    let is_blocked = |key: &str| blocked.iter().any(|b| names::is_within(key, b));

    let empty_base = Snapshot::default();
    let base_ref = base.as_ref().unwrap_or(&empty_base);
    let mut after = Snapshot { fold, ..Default::default() };

    let mut keys: Vec<&String> = halves[0].map.keys().chain(halves[1].map.keys()).collect();
    keys.sort_unstable();
    keys.dedup();

    let mut puts: Vec<Put> = Vec::new();
    let mut dels: Vec<Del> = Vec::new();
    let mut mkdirs: Vec<(Side, &String, u32)> = Vec::new();
    let mut rmdirs: Vec<(Side, u32)> = Vec::new();
    let mut kept_dirs: Vec<&String> = Vec::new();

    let carry = |after: &mut Snapshot, key: &String| {
        if let Some(v) = base_ref.files.get(key) {
            after.files.insert(key.clone(), *v);
        }
        if base_ref.dirs.contains(key) {
            after.dirs.insert(key.clone());
        }
    };

    // Compare each path in the two folders.
    for key in keys {
        let found = [halves[0].map.get(key).copied(), halves[1].map.get(key).copied()];
        if !blocked.is_empty() && is_blocked(key) {
            if two_way {
                carry(&mut after, key);
            }
            continue;
        }

        let mut lone = |side: Side, idx: u32, clash: bool, puts: &mut Vec<Put>, dels: &mut Vec<Del>| {
            let e = halves[side.ix()].at(idx);
            let other = side.other();
            match (e.kind, opts.mode) {
                (Kind::File, Mode::Mirror | Mode::AddOnly) if side == Side::Source => puts.push(Put {
                    to: Side::Copy,
                    key: key.clone(),
                    from: idx,
                    existing: None,
                    conflict: false,
                    blocked_by_dir: clash,
                    moved_from: None,
                    vacates: false,
                }),
                (Kind::File, Mode::Mirror) => dels.push(Del { on: Side::Copy, idx, taken: false }),
                (Kind::File, Mode::AddOnly) => {}
                (Kind::File, Mode::TwoWay) => match base_ref.files.get(key) {
                    Some(b) if cmp.same(Sig::of(e), b[side.ix()]) => dels.push(Del { on: side, idx, taken: false }),
                    known => puts.push(Put {
                        to: other,
                        key: key.clone(),
                        from: idx,
                        existing: None,
                        conflict: known.is_some(),
                        blocked_by_dir: false,
                        moved_from: None,
                        vacates: false,
                    }),
                },
                (Kind::Dir, Mode::Mirror | Mode::AddOnly) if side == Side::Source => {
                    mkdirs.push((Side::Copy, key, idx))
                }
                (Kind::Dir, Mode::Mirror) => {
                    if !halves[side.ix()].tree.pinned.contains(&e.rel) {
                        rmdirs.push((Side::Copy, idx));
                    }
                }
                (Kind::Dir, Mode::AddOnly) => {}
                (Kind::Dir, Mode::TwoWay) => {
                    if !base_ref.dirs.contains(key) {
                        mkdirs.push((other, key, idx));
                    } else if halves[side.ix()].tree.pinned.contains(&e.rel) {
                        kept_dirs.push(key);
                    } else {
                        rmdirs.push((side, idx));
                    }
                }
            }
        };

        match found {
            [Some(si), Some(ci)] => {
                let (s, c) = (halves[0].at(si), halves[1].at(ci));
                match (s.kind, c.kind) {
                    (Kind::Dir, Kind::Dir) => {
                        after.dirs.insert(key.clone());
                    }
                    (Kind::File, Kind::File) => {
                        let (ss, cs) = (Sig::of(s), Sig::of(c));
                        let mut put = |to: Side, conflict: bool| {
                            let (from, existing) = if to == Side::Copy { (si, ci) } else { (ci, si) };
                            puts.push(Put {
                                to,
                                key: key.clone(),
                                from,
                                existing: Some(existing),
                                conflict,
                                blocked_by_dir: false,
                                moved_from: None,
                                vacates: false,
                            });
                        };
                        if cmp.same(ss, cs) {
                            after.files.insert(key.clone(), [ss, cs]);
                        } else {
                            match opts.mode {
                                Mode::Mirror => put(Side::Copy, false),
                                Mode::AddOnly => {
                                    if cs.mtime > ss.mtime && !cmp.same_time(ss.mtime, cs.mtime) {
                                        notes.push(NoteKind::NewerInCopy, Side::Copy, &c.rel);
                                    } else {
                                        put(Side::Copy, false);
                                    }
                                }
                                Mode::TwoWay => {
                                    let b = base_ref.files.get(key);
                                    let s_changed = b.is_none_or(|b| !cmp.same(ss, b[0]));
                                    let c_changed = b.is_none_or(|b| !cmp.same(cs, b[1]));
                                    match (s_changed, c_changed) {
                                        (true, false) => put(Side::Copy, false),
                                        (false, true) => put(Side::Source, false),
                                        (false, false) => carry(&mut after, key),
                                        (true, true) => {
                                            let copy_newer = cs.mtime > ss.mtime && !cmp.same_time(ss.mtime, cs.mtime);
                                            put(if copy_newer { Side::Source } else { Side::Copy }, true);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {
                        if opts.mode == Mode::Mirror {
                            lone(Side::Copy, ci, false, &mut puts, &mut dels);
                            lone(Side::Source, si, c.kind == Kind::Dir, &mut puts, &mut dels);
                        } else {
                            notes.push(NoteKind::TypeClash, Side::Copy, &c.rel);
                            if two_way {
                                carry(&mut after, key);
                            }
                        }
                    }
                }
            }
            [Some(si), None] => lone(Side::Source, si, false, &mut puts, &mut dels),
            [None, Some(ci)] => lone(Side::Copy, ci, false, &mut puts, &mut dels),
            [None, None] => {}
        }
    }

    for key in kept_dirs {
        after.dirs.insert(key.clone());
    }

    // Find the files that the user moved. Change their paths in place.
    if opts.detect_moves {
        struct Spare {
            idx: u32,
            del: Option<usize>,
            put: Option<usize>,
            taken: bool,
        }
        let mut spares: Vec<Spare> = Vec::new();
        let mut by_size: HashMap<(Side, u64), Vec<usize>> = HashMap::new();
        let mut offer = |on: Side, idx: u32, del: Option<usize>, put: Option<usize>| {
            let size = halves[on.ix()].at(idx).size;
            if size > 0 {
                by_size.entry((on, size)).or_default().push(spares.len());
                spares.push(Spare { idx, del, put, taken: false });
            }
        };
        for (i, d) in dels.iter().enumerate() {
            offer(d.on, d.idx, Some(i), None);
        }
        for (i, p) in puts.iter().enumerate() {
            if let Some(e) = p.existing {
                offer(p.to, e, None, Some(i));
            }
        }
        for pi in 0..puts.len() {
            if puts[pi].blocked_by_dir {
                continue;
            }
            let to = puts[pi].to;
            let wanted = halves[to.other().ix()].at(puts[pi].from);
            let Some(pool) = by_size.get(&(to, wanted.size)) else { continue };
            let name = key_of(names::file_name(&wanted.rel), fold);
            let mut candidates: Vec<usize> = pool
                .iter()
                .copied()
                .filter(|&i| {
                    let s = &spares[i];
                    !s.taken && s.put != Some(pi) && cmp.same_time(halves[to.ix()].at(s.idx).mtime, wanted.mtime)
                })
                .collect();
            candidates.sort_by_key(|&i| key_of(names::file_name(&halves[to.ix()].at(spares[i].idx).rel), fold) != name);
            for i in candidates.into_iter().take(8) {
                let cand = halves[to.ix()].at(spares[i].idx);
                if confirm_move(to, &wanted.rel, &cand.rel) {
                    spares[i].taken = true;
                    if let Some(d) = spares[i].del {
                        dels[d].taken = true;
                    }
                    puts[pi].moved_from = Some(spares[i].idx);
                    puts[pi].vacates = spares[i].del.is_some();
                    break;
                }
            }
        }
    }

    let mut resolver = Resolver { fold, cache: [HashMap::new(), HashMap::new()] };
    let mut summary = Summary::default();
    let mut groups: [Vec<(Action, Vec<String>)>; 6] = Default::default();
    let mut removals = [0u64; 2];

    for d in dels.iter().filter(|d| !d.taken) {
        let e = halves[d.on.ix()].at(d.idx);
        summary.remove.add(e.size);
        removals[d.on.ix()] += 1;
        let key = key_of(&e.rel, fold);
        after.files.remove(&key);
        groups[3].push((
            Action { op: Op::Remove, side: d.on, rel: e.rel.clone(), from: None, sig: Sig::of(e), old: None, conflict: false },
            vec![key],
        ));
    }
    for put in &puts {
        let to = &halves[put.to.ix()];
        let wanted = halves[put.to.other().ix()].at(put.from);
        let (rel, old) = match put.existing {
            Some(i) => (to.at(i).rel.clone(), Some(Sig::of(to.at(i)))),
            None => (resolver.resolve(put.to, to, &wanted.rel), None),
        };
        summary.conflicts += put.conflict as u64;
        let mut touched = vec![put.key.clone()];
        let (slot, action, placed) = match put.moved_from {
            Some(ci) => {
                let cand = to.at(ci);
                summary.mv.add(cand.size);
                let old_key = key_of(&cand.rel, fold);
                if put.vacates {
                    after.files.remove(&old_key);
                }
                touched.push(old_key);
                let a = Action {
                    op: Op::Move,
                    side: put.to,
                    rel,
                    from: Some(cand.rel.clone()),
                    sig: Sig::of(cand),
                    old,
                    conflict: put.conflict,
                };
                (2, a, Sig::of(cand))
            }
            None => {
                let op = if old.is_some() { Op::Update } else { Op::Copy };
                if op == Op::Update { summary.update.add(wanted.size) } else { summary.copy.add(wanted.size) }
                let a = Action {
                    op,
                    side: put.to,
                    rel,
                    from: Some(wanted.rel.clone()),
                    sig: Sig::of(wanted),
                    old,
                    conflict: put.conflict,
                };
                (if op == Op::Update { 1 } else { 0 }, a, Sig::of(wanted))
            }
        };
        let mut pair = [Sig::of(wanted); 2];
        pair[put.to.ix()] = placed;
        after.files.insert(put.key.clone(), pair);
        groups[slot].push((action, touched));
    }
    for (side, key, idx) in &mkdirs {
        let wanted = halves[side.other().ix()].at(*idx);
        let rel = resolver.resolve(*side, &halves[side.ix()], &wanted.rel);
        summary.make_dir += 1;
        after.dirs.insert((*key).clone());
        groups[4].push((
            Action { op: Op::MakeDir, side: *side, rel, from: None, sig: Sig::default(), old: None, conflict: false },
            vec![(*key).clone()],
        ));
    }
    for (side, idx) in &rmdirs {
        let e = halves[side.ix()].at(*idx);
        summary.remove_dir += 1;
        groups[5].push((
            Action { op: Op::RemoveDir, side: *side, rel: e.rel.clone(), from: None, sig: Sig::default(), old: None, conflict: false },
            vec![key_of(&e.rel, fold)],
        ));
    }

    let mut actions = Vec::new();
    let mut affected = Vec::new();
    for mut group in groups {
        group.sort_by(|a, b| a.0.rel.cmp(&b.0.rel));
        for (a, keys) in group {
            if a.side == Side::Source {
                summary.to_source += 1;
            }
            actions.push(a);
            affected.push(keys);
        }
    }

    if two_way && !blocked.is_empty() {
        for (k, v) in &base_ref.files {
            if is_blocked(k) {
                after.files.insert(k.clone(), *v);
            }
        }
        for k in &base_ref.dirs {
            if is_blocked(k) {
                after.dirs.insert(k.clone());
            }
        }
    }

    // Set a guard if the plan removes more files than the limit.
    let totals = [source.files, copy.files];
    let mut guard = None;
    for side in Side::BOTH {
        let n = removals[side.ix()];
        if n == 0 {
            continue;
        }
        let total = totals[side.ix()];
        if totals[side.other().ix()] == 0 {
            guard = Some(Guard::EmptySide { side: side.other(), other_files: total });
            break;
        }
        if opts.guard_percent > 0 && n >= 10 && n * 100 > total * opts.guard_percent as u64 {
            guard = Some(Guard::ManyRemovals { side, removals: n, total });
            break;
        }
    }

    Plan {
        mode: opts.mode,
        fold,
        actions,
        summary,
        guard,
        notes: notes.list,
        notes_total: notes.total,
        source: TreeStats { files: source.files, dirs: source.dirs, bytes: source.bytes },
        copy: TreeStats { files: copy.files, dirs: copy.dirs, bytes: copy.bytes },
        stale_parts: [source.stale_parts.clone(), copy.stale_parts.clone()],
        after: two_way.then_some(after),
        affected: if two_way { affected } else { Vec::new() },
        base: if two_way { base } else { None },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: i64 = 1_700_000_000 * SECOND_NS;

    fn tree(items: &[(&str, u64, i64)]) -> Tree {
        let mut t = Tree::default();
        let mut dirs: Vec<String> = Vec::new();
        for (rel, size, mtime) in items {
            let mut at = 0;
            while let Some(i) = rel[at..].find('/') {
                let d = &rel[..at + i];
                if !dirs.iter().any(|x| x == d) {
                    dirs.push(d.to_owned());
                    t.entries.push(Entry { rel: d.to_owned(), kind: Kind::Dir, size: 0, mtime: 0 });
                    t.dirs += 1;
                }
                at += i + 1;
            }
            if rel.ends_with('/') {
                continue;
            }
            t.entries.push(Entry { rel: rel.to_string(), kind: Kind::File, size: *size, mtime: T + *mtime * SECOND_NS });
            t.files += 1;
            t.bytes += size;
        }
        t
    }

    fn opts(mode: Mode) -> PlanOptions {
        PlanOptions { mode, fold: false, compare: CompareOptions::default(), detect_moves: true, guard_percent: 30 }
    }

    fn plan(s: &Tree, c: &Tree, mode: Mode) -> Plan {
        build_plan(s, c, None, &opts(mode), &mut |_, _, _| true)
    }

    fn ops(p: &Plan) -> Vec<String> {
        p.actions
            .iter()
            .map(|a| match (&a.op, &a.from) {
                (Op::Move, Some(f)) => format!("move {f} -> {}", a.rel),
                (op, _) => format!("{} {}", format!("{op:?}").to_lowercase(), a.rel),
            })
            .collect()
    }

    #[test]
    fn identical_trees_need_nothing() {
        let t = tree(&[("a/1.jpg", 10, 0), ("a/2.jpg", 20, 5)]);
        let p = plan(&t, &tree(&[("a/1.jpg", 10, 0), ("a/2.jpg", 20, 5)]), Mode::Mirror);
        assert!(p.is_empty());
        assert_eq!(p.summary.changes(), 0);
    }

    #[test]
    fn times_within_two_seconds_are_the_same_file() {
        let p = plan(&tree(&[("a.jpg", 10, 1)]), &tree(&[("a.jpg", 10, 3)]), Mode::Mirror);
        assert!(p.is_empty());
        let p = plan(&tree(&[("a.jpg", 10, 0)]), &tree(&[("a.jpg", 10, 3)]), Mode::Mirror);
        assert_eq!(ops(&p), ["update a.jpg"]);
    }

    #[test]
    fn one_hour_shift_is_ignored_only_when_asked() {
        let (s, c) = (tree(&[("a.jpg", 10, 0)]), tree(&[("a.jpg", 10, 3600)]));
        assert_eq!(plan(&s, &c, Mode::Mirror).actions.len(), 1);
        let mut o = opts(Mode::Mirror);
        o.compare.dst_shift = true;
        assert!(build_plan(&s, &c, None, &o, &mut |_, _, _| true).is_empty());
    }

    #[test]
    fn mirror_copies_updates_and_removes() {
        let s = tree(&[("new.jpg", 5, 0), ("same.jpg", 7, 0), ("changed.jpg", 9, 50)]);
        let c = tree(&[("same.jpg", 7, 0), ("changed.jpg", 8, 0), ("extra.jpg", 3, 0)]);
        let p = plan(&s, &c, Mode::Mirror);
        assert_eq!(ops(&p), ["copy new.jpg", "update changed.jpg", "remove extra.jpg"]);
        assert_eq!(p.summary.bytes_to_write(), 14);
        assert_eq!(p.actions[1].old, Some(Sig { size: 8, mtime: T }));
    }

    #[test]
    fn add_only_never_removes_and_keeps_newer_copies() {
        let s = tree(&[("new.jpg", 5, 0), ("older.jpg", 9, 0), ("newer.jpg", 9, 100)]);
        let c = tree(&[("older.jpg", 8, 100), ("newer.jpg", 8, 0), ("extra/x.jpg", 3, 0)]);
        let p = plan(&s, &c, Mode::AddOnly);
        assert_eq!(ops(&p), ["copy new.jpg", "update newer.jpg"]);
        assert_eq!(p.notes.len(), 1);
        assert_eq!(p.notes[0].kind, NoteKind::NewerInCopy);
    }

    #[test]
    fn a_reorganized_folder_is_moved_not_recopied() {
        let s = tree(&[("2023/Japan/a.jpg", 100, 1), ("2023/Japan/b.jpg", 200, 2), ("2023/Japan/new.jpg", 300, 9)]);
        let c = tree(&[("Trips/Japan 2023/a.jpg", 100, 1), ("Trips/Japan 2023/b.jpg", 200, 2)]);
        let p = plan(&s, &c, Mode::Mirror);
        assert_eq!(
            ops(&p),
            [
                "copy 2023/Japan/new.jpg",
                "move Trips/Japan 2023/a.jpg -> 2023/Japan/a.jpg",
                "move Trips/Japan 2023/b.jpg -> 2023/Japan/b.jpg",
                "makedir 2023",
                "makedir 2023/Japan",
                "removedir Trips",
                "removedir Trips/Japan 2023",
            ]
        );
        assert_eq!(p.summary.copy.bytes, 300);
        assert_eq!(p.summary.remove.n, 0);
    }

    #[test]
    fn moves_prefer_the_same_file_name_among_lookalikes() {
        let s = tree(&[("x/a.raw", 50, 0), ("y/b.raw", 50, 0)]);
        let c = tree(&[("old/b.raw", 50, 0), ("old/a.raw", 50, 0)]);
        let p = plan(&s, &c, Mode::Mirror);
        let moves: Vec<String> = ops(&p).into_iter().filter(|o| o.starts_with("move")).collect();
        assert_eq!(moves, ["move old/a.raw -> x/a.raw", "move old/b.raw -> y/b.raw"]);
    }

    #[test]
    fn a_move_the_contents_disagree_with_becomes_copy_and_remove() {
        let s = tree(&[("new/a.jpg", 100, 1)]);
        let c = tree(&[("old/a.jpg", 100, 1)]);
        let p = build_plan(&s, &c, None, &opts(Mode::Mirror), &mut |_, _, _| false);
        assert_eq!(ops(&p)[..2], ["copy new/a.jpg".to_string(), "remove old/a.jpg".to_string()]);
    }

    #[test]
    fn empty_files_are_never_treated_as_moved() {
        let p = plan(&tree(&[("b/.keep", 0, 0)]), &tree(&[("a/.keep", 0, 0)]), Mode::Mirror);
        assert!(ops(&p).contains(&"copy b/.keep".to_string()));
        assert!(ops(&p).contains(&"remove a/.keep".to_string()));
    }

    #[test]
    fn a_moved_file_can_replace_a_changed_one() {
        let s = tree(&[("b.jpg", 100, 1)]);
        let c = tree(&[("a.jpg", 100, 1), ("b.jpg", 70, 0)]);
        let p = plan(&s, &c, Mode::Mirror);
        assert_eq!(ops(&p), ["move a.jpg -> b.jpg"]);
        assert_eq!(p.actions[0].old, Some(Sig { size: 70, mtime: T }));
    }

    #[test]
    fn swapping_two_folder_names_is_all_renames() {
        let s = tree(&[("Edits old/a.jpg", 10, 1), ("Edits old/b.jpg", 11, 2), ("Edits/a.jpg", 20, 5), ("Edits/b.jpg", 21, 6)]);
        let c = tree(&[("Edits/a.jpg", 10, 1), ("Edits/b.jpg", 11, 2), ("Edits new/a.jpg", 20, 5), ("Edits new/b.jpg", 21, 6)]);
        let p = plan(&s, &c, Mode::Mirror);
        assert_eq!(
            ops(&p),
            [
                "move Edits/a.jpg -> Edits old/a.jpg",
                "move Edits/b.jpg -> Edits old/b.jpg",
                "move Edits new/a.jpg -> Edits/a.jpg",
                "move Edits new/b.jpg -> Edits/b.jpg",
                "makedir Edits old",
                "removedir Edits new",
            ]
        );
        assert_eq!(p.summary.bytes_to_write(), 0);
    }

    #[test]
    fn two_files_trading_names_are_renamed_not_copied() {
        let s = tree(&[("a.jpg", 20, 5), ("b.jpg", 10, 1)]);
        let c = tree(&[("a.jpg", 10, 1), ("b.jpg", 20, 5)]);
        assert_eq!(ops(&plan(&s, &c, Mode::Mirror)), ["move b.jpg -> a.jpg", "move a.jpg -> b.jpg"]);
    }

    #[test]
    fn empty_folders_are_mirrored() {
        let p = plan(&tree(&[("keep/", 0, 0)]), &tree(&[("gone/", 0, 0)]), Mode::Mirror);
        assert_eq!(ops(&p), ["makedir keep", "removedir gone"]);
        assert_eq!(p.summary.changes(), 2);
    }

    #[test]
    fn a_file_replaced_by_a_folder_and_back() {
        let s = tree(&[("x/inner.jpg", 10, 0), ("y", 5, 0)]);
        let c = tree(&[("x", 7, 0), ("y/inner.jpg", 9, 0)]);
        let p = plan(&s, &c, Mode::Mirror);
        assert_eq!(
            ops(&p),
            ["copy x/inner.jpg", "copy y", "remove x", "remove y/inner.jpg", "makedir x", "removedir y"]
        );
    }

    #[test]
    fn case_only_differences_fold_when_a_drive_ignores_case() {
        let s = tree(&[("Trips/IMG_1.JPG", 10, 0)]);
        let c = tree(&[("trips/img_1.jpg", 10, 0)]);
        assert_eq!(ops(&plan(&s, &c, Mode::Mirror)), ["move trips/img_1.jpg -> Trips/IMG_1.JPG", "makedir Trips", "removedir trips"]);
        let mut o = opts(Mode::Mirror);
        o.fold = true;
        assert!(build_plan(&s, &c, None, &o, &mut |_, _, _| true).is_empty());
    }

    #[test]
    fn new_files_use_the_copys_spelling_of_existing_folders() {
        let s = tree(&[("Trips/Japan/a.jpg", 10, 0), ("Trips/Japan/New/b.jpg", 10, 0)]);
        let c = tree(&[("trips/JAPAN/", 0, 0)]);
        let mut o = opts(Mode::Mirror);
        o.fold = true;
        let p = build_plan(&s, &c, None, &o, &mut |_, _, _| true);
        assert_eq!(ops(&p), ["copy trips/JAPAN/New/b.jpg", "copy trips/JAPAN/a.jpg", "makedir trips/JAPAN/New"]);
    }

    #[test]
    fn names_that_collide_on_the_other_drive_are_reported_not_fought_over() {
        let s = tree(&[("a.jpg", 10, 0), ("A.JPG", 11, 0)]);
        let mut o = opts(Mode::Mirror);
        o.fold = true;
        let p = build_plan(&s, &Tree::default(), None, &o, &mut |_, _, _| true);
        assert_eq!(p.actions.len(), 1);
        assert_eq!(p.notes[0].kind, NoteKind::NameClash);
    }

    #[test]
    fn unreadable_folders_are_left_alone_on_both_sides() {
        let mut s = tree(&[("ok.jpg", 1, 0)]);
        s.unreadable.push("locked".into());
        let c = tree(&[("locked/a.jpg", 5, 0), ("locked/deep/b.jpg", 5, 0), ("lockedish.jpg", 5, 0)]);
        let p = plan(&s, &c, Mode::Mirror);
        assert_eq!(ops(&p), ["copy ok.jpg", "remove lockedish.jpg"]);
    }

    #[test]
    fn a_folder_holding_skipped_files_is_not_removed() {
        let mut c = tree(&[("old/cache/", 0, 0), ("gone/", 0, 0)]);
        c.pinned.extend(["old/cache".to_string(), "old".to_string()]);
        let p = plan(&Tree::default(), &c, Mode::Mirror);
        assert_eq!(ops(&p), ["removedir gone"]);
    }

    #[test]
    fn an_empty_source_trips_the_guard() {
        let c = tree(&[("a.jpg", 1, 0), ("b.jpg", 1, 0)]);
        let p = plan(&Tree::default(), &c, Mode::Mirror);
        assert_eq!(p.guard, Some(Guard::EmptySide { side: Side::Source, other_files: 2 }));
        assert_eq!(plan(&c, &Tree::default(), Mode::Mirror).guard, None);
        assert_eq!(plan(&Tree::default(), &c, Mode::AddOnly).guard, None);
    }

    #[test]
    fn many_removals_trip_the_guard_but_a_reorganization_does_not() {
        let names: Vec<String> = (0..40).map(|i| format!("old/{i}.jpg")).collect();
        let all: Vec<(&str, u64, i64)> = names.iter().enumerate().map(|(i, n)| (n.as_str(), 100 + i as u64, 0)).collect();
        let c = tree(&all);
        let p = plan(&tree(&all[..20]), &c, Mode::Mirror);
        assert_eq!(p.guard, Some(Guard::ManyRemovals { side: Side::Copy, removals: 20, total: 40 }));
        assert_eq!(plan(&tree(&all[..30]), &c, Mode::Mirror).guard, None);

        let moved: Vec<String> = (0..40).map(|i| format!("new/{i}.jpg")).collect();
        let s: Vec<(&str, u64, i64)> = moved.iter().enumerate().map(|(i, n)| (n.as_str(), 100 + i as u64, 0)).collect();
        let p = plan(&tree(&s), &c, Mode::Mirror);
        assert_eq!(p.guard, None);
        assert_eq!(p.summary.mv.n, 40);
    }

    fn snap(items: &[(&str, u64, i64)]) -> Snapshot {
        let mut s = Snapshot::default();
        for (rel, size, mtime) in items {
            let mut at = 0;
            while let Some(i) = rel[at..].find('/') {
                s.dirs.insert(rel[..at + i].to_owned());
                at += i + 1;
            }
            let sig = Sig { size: *size, mtime: T + *mtime * SECOND_NS };
            s.files.insert(rel.to_string(), [sig, sig]);
        }
        s
    }

    fn two_way(s: &Tree, c: &Tree, base: Option<Snapshot>) -> Plan {
        build_plan(s, c, base, &opts(Mode::TwoWay), &mut |_, _, _| true)
    }

    fn sided(p: &Plan) -> Vec<String> {
        p.actions
            .iter()
            .map(|a| format!("{}:{} {}", if a.side == Side::Source { "S" } else { "C" }, format!("{:?}", a.op).to_lowercase(), a.rel))
            .collect()
    }

    #[test]
    fn first_two_way_sync_merges_and_deletes_nothing() {
        let s = tree(&[("a.jpg", 1, 0), ("both.jpg", 5, 100)]);
        let c = tree(&[("b.jpg", 2, 0), ("both.jpg", 4, 0)]);
        let p = two_way(&s, &c, None);
        assert_eq!(sided(&p), ["C:copy a.jpg", "S:copy b.jpg", "C:update both.jpg"]);
        assert_eq!(p.summary.conflicts, 1);
        assert_eq!(p.summary.remove.n, 0);
    }

    #[test]
    fn two_way_carries_edits_and_deletions_in_both_directions() {
        let base = snap(&[("keep.jpg", 1, 0), ("edit_s.jpg", 2, 0), ("edit_c.jpg", 3, 0), ("del_s.jpg", 4, 0), ("del_c.jpg", 5, 0)]);
        let s = tree(&[("keep.jpg", 1, 0), ("edit_s.jpg", 22, 50), ("edit_c.jpg", 3, 0), ("del_c.jpg", 5, 0), ("new_s.jpg", 6, 0)]);
        let c = tree(&[("keep.jpg", 1, 0), ("edit_s.jpg", 2, 0), ("edit_c.jpg", 33, 50), ("del_s.jpg", 4, 0), ("new_c.jpg", 7, 0)]);
        let p = two_way(&s, &c, Some(base));
        assert_eq!(
            sided(&p),
            ["S:copy new_c.jpg", "C:copy new_s.jpg", "S:update edit_c.jpg", "C:update edit_s.jpg", "S:remove del_c.jpg", "C:remove del_s.jpg"]
        );
        assert_eq!(p.summary.conflicts, 0);
        assert_eq!(p.summary.to_source, 3);
    }

    #[test]
    fn two_way_conflicts_keep_the_newer_and_an_edit_beats_a_delete() {
        let base = snap(&[("both.jpg", 1, 0), ("edited_then_deleted.jpg", 2, 0)]);
        let s = tree(&[("both.jpg", 10, 50), ("edited_then_deleted.jpg", 20, 50)]);
        let c = tree(&[("both.jpg", 11, 90)]);
        let p = two_way(&s, &c, Some(base));
        assert_eq!(sided(&p), ["C:copy edited_then_deleted.jpg", "S:update both.jpg"]);
        assert_eq!(p.summary.conflicts, 2);
    }

    #[test]
    fn two_way_follows_moves_made_on_either_side() {
        let base = snap(&[("old/a.jpg", 100, 1), ("x/b.jpg", 200, 2)]);
        let s = tree(&[("new/a.jpg", 100, 1), ("x/b.jpg", 200, 2)]);
        let c = tree(&[("old/a.jpg", 100, 1), ("y/b.jpg", 200, 2)]);
        let p = two_way(&s, &c, Some(base));
        let got = p
            .actions
            .iter()
            .filter(|a| a.op == Op::Move)
            .map(|a| format!("{:?} {} -> {}", a.side, a.from.as_deref().unwrap(), a.rel))
            .collect::<Vec<_>>();
        assert_eq!(got, ["Copy old/a.jpg -> new/a.jpg", "Source x/b.jpg -> y/b.jpg"]);
        assert_eq!(p.summary.copy.n + p.summary.remove.n, 0);
    }

    #[test]
    fn snapshot_after_reflects_only_what_completed() {
        let base = snap(&[("gone.jpg", 4, 0), ("same.jpg", 1, 0)]);
        let s = tree(&[("same.jpg", 1, 0), ("new.jpg", 6, 0)]);
        let c = tree(&[("same.jpg", 1, 0), ("gone.jpg", 4, 0)]);
        let p = two_way(&s, &c, Some(base));
        assert_eq!(sided(&p), ["C:copy new.jpg", "C:remove gone.jpg"]);

        let all = p.snapshot_after(&|_| true).unwrap();
        assert!(all.files.contains_key("new.jpg") && all.files.contains_key("same.jpg"));
        assert!(!all.files.contains_key("gone.jpg"));

        let none = p.snapshot_after(&|_| false).unwrap();
        assert!(!none.files.contains_key("new.jpg"));
        assert!(none.files.contains_key("gone.jpg"));
        assert_eq!(sided(&two_way(&s, &c, Some(none))), sided(&p));
    }

    #[test]
    fn two_way_empty_side_trips_the_guard() {
        let base = snap(&[("a.jpg", 1, 0), ("b.jpg", 2, 0)]);
        let c = tree(&[("a.jpg", 1, 0), ("b.jpg", 2, 0)]);
        let p = two_way(&Tree::default(), &c, Some(base));
        assert_eq!(p.guard, Some(Guard::EmptySide { side: Side::Source, other_files: 2 }));
    }
}
