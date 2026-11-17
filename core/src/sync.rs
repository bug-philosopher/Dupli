use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use serde::Serialize;

use crate::config::PairConfig;
use crate::copy;
use crate::exec::{self, ExecOptions, Progress, Report};
use crate::history;
use crate::names;
use crate::plan::{build_plan, CompareOptions, Plan, PlanOptions, Snapshot};
use crate::scan::{probe_case_insensitive, scan, Excludes, ScanError, ScanOptions, ScanProgress};
use crate::volumes;
use crate::{Mode, Side};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SyncError {
    Missing { side: Side },
    Overlap,
    Cancelled,
    Failed { side: Side, message: String },
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::Missing { side: Side::Source } => write!(f, "The source folder is not reachable."),
            SyncError::Missing { side: Side::Copy } => write!(f, "The copy folder is not reachable."),
            SyncError::Overlap => write!(f, "The source and the copy overlap. Choose two separate folders."),
            SyncError::Cancelled => write!(f, "Stopped."),
            SyncError::Failed { message, .. } => write!(f, "{message}"),
        }
    }
}

pub fn overlap(a: &Path, b: &Path) -> bool {
    let (Ok(a), Ok(b)) = (std::fs::canonicalize(a), std::fs::canonicalize(b)) else { return false };
    a.starts_with(&b) || b.starts_with(&a)
}

fn compare_options(pair: &PairConfig) -> CompareOptions {
    let fat = [&pair.source, &pair.copy]
        .iter()
        .any(|p| volumes::volume_info(Path::new(p)).is_some_and(|v| volumes::is_fat(&v.fs)));
    CompareOptions { dst_shift: fat, ..Default::default() }
}

pub fn compare(
    pair: &PairConfig,
    base: Option<Snapshot>,
    cancel: &AtomicBool,
    progress: &(dyn Fn(Side, &ScanProgress) + Sync),
) -> Result<Plan, SyncError> {
    let roots = [Path::new(&pair.source), Path::new(&pair.copy)];
    for side in Side::BOTH {
        if !roots[side.ix()].is_dir() {
            return Err(SyncError::Missing { side });
        }
    }
    if overlap(roots[0], roots[1]) {
        return Err(SyncError::Overlap);
    }
    let (excludes, _unparsed) = Excludes::new(&pair.excludes);
    let opts = ScanOptions { excludes, skip_hidden: pair.skip_hidden };

    // Read the two folders at the same time.
    let (source, copy) = std::thread::scope(|scope| {
        let other = scope.spawn(|| scan(roots[1], &opts, cancel, &mut |p| progress(Side::Copy, p)));
        let source = scan(roots[0], &opts, cancel, &mut |p| progress(Side::Source, p));
        (source, other.join())
    });
    let lift = |side: Side, e: ScanError| match e {
        ScanError::Missing => SyncError::Missing { side },
        ScanError::Cancelled => SyncError::Cancelled,
        ScanError::Io(message) => SyncError::Failed { side, message },
    };
    let source = source.map_err(|e| lift(Side::Source, e))?;
    let copy = copy
        .map_err(|_| SyncError::Failed { side: Side::Copy, message: "The compare stopped unexpectedly.".into() })?
        .map_err(|e| lift(Side::Copy, e))?;

    // If one drive does not use letter case, compare the names without letter case.
    let usual = cfg!(any(target_os = "macos", windows));
    let fold = probe_case_insensitive(roots[0], &source).unwrap_or(usual)
        || probe_case_insensitive(roots[1], &copy).unwrap_or(usual);

    let opts = PlanOptions {
        mode: pair.mode,
        fold,
        compare: compare_options(pair),
        detect_moves: pair.detect_moves && pair.mode != Mode::AddOnly,
        guard_percent: pair.guard_percent,
    };
    let base = if pair.mode == Mode::TwoWay { base.and_then(|b| b.for_fold(fold)) } else { None };

    let mut prints: HashMap<(Side, String), Option<[u8; 32]>> = HashMap::new();
    let mut print = |side: Side, rel: &str| {
        *prints
            .entry((side, rel.to_owned()))
            .or_insert_with(|| copy::fingerprint(&names::join(roots[side.ix()], rel)).ok())
    };
    let mut confirm = |side: Side, wanted: &str, candidate: &str| {
        matches!((print(side.other(), wanted), print(side, candidate)), (Some(a), Some(b)) if a == b)
    };
    Ok(build_plan(&source, &copy, base, &opts, &mut confirm))
}

pub struct Applied {
    pub report: Report,
    pub snapshot: Option<Snapshot>,
}

pub fn apply(pair: &PairConfig, plan: &Plan, cancel: &AtomicBool, progress: &mut dyn FnMut(&Progress)) -> Applied {
    let roots = [Path::new(&pair.source), Path::new(&pair.copy)];
    let opts = ExecOptions {
        keep_history: pair.keep_history,
        verify: pair.verify,
        check_contents: pair.check_contents,
        compare: compare_options(pair),
    };
    let out = exec::execute(plan, roots, &opts, cancel, progress);
    tidy_history(pair);
    let snapshot = plan.snapshot_after(&|i| out.status[i].completed());
    Applied { report: out.report, snapshot }
}

pub fn tidy_history(pair: &PairConfig) -> history::Pruned {
    let mut total = history::Pruned::default();
    if !pair.keep_history {
        return total;
    }
    for &side in history_sides(pair.mode) {
        let root = Path::new(if side == Side::Source { &pair.source } else { &pair.copy });
        if !history::history_dir(root).is_dir() {
            continue;
        }
        let p = history::prune(root, pair.history_days, pair.history_max_gb as u64 * 1_000_000_000, crate::now_ms());
        total.runs += p.runs;
        total.files += p.files;
        total.bytes += p.bytes;
    }
    total
}

pub fn history_sides(mode: Mode) -> &'static [Side] {
    match mode {
        Mode::TwoWay => &[Side::Copy, Side::Source],
        _ => &[Side::Copy],
    }
}
