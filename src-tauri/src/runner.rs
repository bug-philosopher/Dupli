use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use dupli_core::config::{self, AppSettings, Config, PairConfig, VolumeRef};
use dupli_core::exec::{Phase as Step, Progress, Report};
use dupli_core::history::{self, HistEntry, Pruned, RestoreReport, RunInfo, Usage};
use dupli_core::plan::{CompareOptions, Guard, Note, Op, Plan, Snapshot, Summary, TreeStats};
use dupli_core::scan::{is_junk, is_part_file, ScanProgress};
use dupli_core::sync::{self, SyncError};
use dupli_core::volumes::{self, VolumeInfo, VolumeKind};
use dupli_core::{names, now_ms, Mode, Side};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};

const TICK: Duration = Duration::from_millis(200);
const PROBE_EVERY: Duration = Duration::from_secs(3);
const SETTLE_AFTER_CONNECT: Duration = Duration::from_secs(2);
const QUIET_AFTER_CHANGE: Duration = Duration::from_secs(8);
const LONGEST_WAIT: Duration = Duration::from_secs(300);
const AT_ONCE: usize = 2;
const LOG_LIMIT: usize = 300;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    Manual,
    Startup,
    Connect,
    Change,
    Timer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    Compare,
    Sync { confirmed: bool },
}

#[derive(Clone, Copy, Debug)]
struct Job {
    want: Want,
    trigger: Trigger,
    eject: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Idle,
    Queued,
    Comparing,
    Syncing,
    Restoring,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Synced,
    Partial,
    Cancelled,
    Stopped,
    Held,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunRecord {
    pub pair: String,
    pub at_ms: i64,
    pub trigger: Trigger,
    pub outcome: Outcome,
    pub report: Report,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Unknown,
    UpToDate,
    Changes,
    Held,
    Problem,
    Offline,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProgressView {
    pub step: Step,
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub current: String,
    pub rate: f64,
    pub eta_s: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PairView {
    pub config: PairConfig,
    pub phase: Phase,
    pub health: Health,
    pub online: [Option<bool>; 2],
    pub volumes: [Option<VolumeInfo>; 2],
    pub summary: Option<Summary>,
    pub notes: u64,
    pub guard: Option<Guard>,
    pub stats: Option<[TreeStats; 2]>,
    pub checked_ms: i64,
    pub synced_ms: i64,
    pub scan: [ScanProgress; 2],
    pub progress: Option<ProgressView>,
    pub last: Option<RunRecord>,
    pub error: Option<String>,
    pub history: Option<Usage>,
    pub trigger: Option<Trigger>,
    pub watching: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct AppView {
    pub pairs: Vec<PairView>,
    pub settings: AppSettings,
    pub platform: &'static str,
    pub version: &'static str,
}

pub enum Signal {
    State,
    Notify { title: String, body: String },
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Memory {
    checked_ms: i64,
    synced_ms: i64,
}

#[derive(Default)]
struct Rate {
    samples: VecDeque<(Instant, u64)>,
}

impl Rate {
    fn add(&mut self, bytes: u64) {
        let now = Instant::now();
        if self.samples.back().is_some_and(|(t, _)| now.duration_since(*t) < Duration::from_millis(250)) {
            return;
        }
        self.samples.push_back((now, bytes));
        while self.samples.front().is_some_and(|(t, _)| now.duration_since(*t) > Duration::from_secs(8)) {
            self.samples.pop_front();
        }
    }

    fn per_second(&self) -> f64 {
        match (self.samples.front(), self.samples.back()) {
            (Some((t0, b0)), Some((t1, b1))) if t1 > t0 && b1 > b0 => {
                (*b1 - *b0) as f64 / t1.duration_since(*t0).as_secs_f64()
            }
            _ => 0.0,
        }
    }
}

struct Live {
    phase: Phase,
    job: Option<Job>,
    next: Option<Job>,
    cancel: Arc<AtomicBool>,
    online: [Option<bool>; 2],
    volumes: [Option<VolumeInfo>; 2],
    probing: [bool; 2],
    probed: [Option<Instant>; 2],
    was_ready: bool,
    connected: Option<Instant>,
    greeted: bool,
    hold_first: bool,
    plan: Option<Arc<Plan>>,
    stats: Option<[TreeStats; 2]>,
    settled: bool,
    scan: [ScanProgress; 2],
    progress: Option<Progress>,
    rate: Rate,
    error: Option<String>,
    held: Option<Guard>,
    last: Option<RunRecord>,
    memory: Memory,
    history: Option<Usage>,
    last_attempt: Instant,
    watching: bool,
}

impl Live {
    fn new(memory: Memory) -> Live {
        Live {
            phase: Phase::Idle,
            job: None,
            next: None,
            cancel: Arc::new(AtomicBool::new(false)),
            online: [None; 2],
            volumes: [None, None],
            probing: [false; 2],
            probed: [None; 2],
            was_ready: false,
            connected: None,
            greeted: false,
            hold_first: false,
            plan: None,
            stats: None,
            settled: false,
            scan: [ScanProgress::default(); 2],
            progress: None,
            rate: Rate::default(),
            error: None,
            held: None,
            last: None,
            memory,
            history: None,
            last_attempt: Instant::now(),
            watching: false,
        }
    }

    fn ready(&self) -> bool {
        self.online == [Some(true), Some(true)]
    }

    fn busy(&self) -> bool {
        matches!(self.phase, Phase::Comparing | Phase::Syncing | Phase::Restoring)
    }

    fn health(&self) -> Health {
        if self.online.contains(&Some(false)) {
            Health::Offline
        } else if self.error.is_some() {
            Health::Problem
        } else if self.held.is_some() {
            Health::Held
        } else if let Some(plan) = &self.plan {
            if plan.summary.changes() == 0 {
                Health::UpToDate
            } else {
                Health::Changes
            }
        } else if self.settled {
            Health::UpToDate
        } else if self.last.as_ref().is_some_and(|r| matches!(r.outcome, Outcome::Partial | Outcome::Stopped | Outcome::Failed)) {
            Health::Problem
        } else {
            Health::Unknown
        }
    }
}

struct Inner {
    config: Config,
    live: HashMap<String, Live>,
    log: Vec<RunRecord>,
    running: usize,
}

pub struct Engine {
    dir: PathBuf,
    inner: Mutex<Inner>,
    dirty: Mutex<HashMap<String, (Instant, Instant)>>,
    changed: AtomicBool,
    sink: Box<dyn Fn(Signal) + Send + Sync>,
}

struct Watch {
    roots: Vec<PathBuf>,
    _watcher: RecommendedWatcher,
}

fn not_worth_a_sync(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == history::META_DIR)
        || path.file_name().and_then(|n| n.to_str()).is_some_and(|n| is_junk(n) || is_part_file(n))
}

fn probe(root: &str, expect: &Option<VolumeRef>) -> (bool, Option<VolumeInfo>) {
    let path = Path::new(root);
    if root.is_empty() || !path.is_dir() {
        return (false, None);
    }
    let info = volumes::volume_info(path);
    let right_drive = match (expect, &info) {
        (Some(e), Some(i)) => e.matches(i),
        _ => true,
    };
    (right_drive, if right_drive { info } else { None })
}

fn volume_ref(path: &str) -> Option<VolumeRef> {
    volumes::volume_info(Path::new(path)).map(|v| VolumeRef::of(&v))
}

fn folder_name(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_owned())
}

impl Engine {
    pub fn start(dir: PathBuf, sink: impl Fn(Signal) + Send + Sync + 'static) -> Arc<Engine> {
        let config = Config::load(&dir.join("config.json"));
        let memory: HashMap<String, Memory> = config::read_json(&dir.join("status.json")).unwrap_or_default();
        let log: Vec<RunRecord> = config::read_json(&dir.join("log.json")).unwrap_or_default();
        let mut live = HashMap::new();
        for p in &config.pairs {
            let mut l = Live::new(memory.get(&p.id).copied().unwrap_or_default());
            l.last = log.iter().rev().find(|r| r.pair == p.id).cloned();
            live.insert(p.id.clone(), l);
        }
        let engine = Arc::new(Engine {
            dir,
            inner: Mutex::new(Inner { config, live, log, running: 0 }),
            dirty: Mutex::new(HashMap::new()),
            changed: AtomicBool::new(true),
            sink: Box::new(sink),
        });
        let scheduler = engine.clone();
        thread::Builder::new().name("dupli-scheduler".into()).spawn(move || scheduler.schedule()).expect("scheduler thread");
        engine
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn touch(&self) {
        self.changed.store(true, Ordering::Relaxed);
    }

    // This is the scheduler loop. It finds the connected drives and starts the jobs.
    fn schedule(self: Arc<Self>) {
        let mut watches: HashMap<String, Watch> = HashMap::new();
        loop {
            thread::sleep(TICK);
            let mut start: Vec<(String, Job)> = Vec::new();
            let mut probes: Vec<(String, usize, String, Option<VolumeRef>)> = Vec::new();
            let mut wanted_watches: HashMap<String, Vec<PathBuf>> = HashMap::new();
            {
                let mut inner = self.lock();
                let inner = &mut *inner;
                let auto = !inner.config.settings.auto_paused;
                let mut dirty = self.dirty.lock().unwrap_or_else(|e| e.into_inner());
                for pair in &inner.config.pairs {
                    let Some(live) = inner.live.get_mut(&pair.id) else { continue };

                    if !live.busy() {
                        for side in Side::BOTH {
                            let i = side.ix();
                            if !live.probing[i] && live.probed[i].is_none_or(|t| t.elapsed() >= PROBE_EVERY) {
                                live.probing[i] = true;
                                let (root, expect) = match side {
                                    Side::Source => (pair.source.clone(), pair.source_volume.clone()),
                                    Side::Copy => (pair.copy.clone(), pair.copy_volume.clone()),
                                };
                                probes.push((pair.id.clone(), i, root, expect));
                            }
                        }
                    }

                    let ready = live.ready();
                    if ready && !live.was_ready {
                        live.connected = Some(Instant::now());
                    }
                    if !ready && live.was_ready {
                        live.plan = None;
                        live.settled = false;
                        self.touch();
                    }
                    live.was_ready = ready;

                    let watch = ready && auto && pair.on_change && !pair.paused;
                    if watch {
                        let mut roots = vec![PathBuf::from(&pair.source)];
                        if pair.mode == Mode::TwoWay {
                            roots.push(PathBuf::from(&pair.copy));
                        }
                        wanted_watches.insert(pair.id.clone(), roots);
                    }
                    if !watch {
                        dirty.remove(&pair.id);
                    }

                    if live.phase == Phase::Idle && ready {
                        let mut job = None;
                        if live.connected.is_some_and(|t| t.elapsed() >= SETTLE_AFTER_CONNECT) {
                            live.connected = None;
                            let startup = !live.greeted;
                            live.greeted = true;
                            let hold = std::mem::take(&mut live.hold_first);
                            let trigger = if startup { Trigger::Startup } else { Trigger::Connect };
                            let sync = auto && !pair.paused && !hold && (pair.on_connect || (startup && pair.automatic()));
                            job = Some(Job {
                                want: if sync { Want::Sync { confirmed: false } } else { Want::Compare },
                                trigger,
                                eject: sync && !startup && pair.eject_after,
                            });
                        } else if live.connected.is_none() && auto && !pair.paused {
                            let changed = pair.on_change
                                && dirty.get(&pair.id).is_some_and(|(first, last)| {
                                    last.elapsed() >= QUIET_AFTER_CHANGE || first.elapsed() >= LONGEST_WAIT
                                });
                            let due = pair.every_minutes > 0
                                && live.last_attempt.elapsed() >= Duration::from_secs(pair.every_minutes as u64 * 60);
                            if changed {
                                dirty.remove(&pair.id);
                                job = Some(Job { want: Want::Sync { confirmed: false }, trigger: Trigger::Change, eject: false });
                            } else if due {
                                job = Some(Job { want: Want::Sync { confirmed: false }, trigger: Trigger::Timer, eject: false });
                            }
                        }
                        if let Some(job) = job {
                            live.job = Some(job);
                            live.phase = Phase::Queued;
                            self.touch();
                        }
                    }

                    if live.phase == Phase::Queued && inner.running < AT_ONCE {
                        if let Some(job) = live.job {
                            if ready {
                                inner.running += 1;
                                live.phase = Phase::Comparing;
                                live.cancel = Arc::new(AtomicBool::new(false));
                                live.scan = [ScanProgress::default(); 2];
                                live.progress = None;
                                live.error = None;
                                live.last_attempt = Instant::now();
                                start.push((pair.id.clone(), job));
                            } else {
                                live.phase = Phase::Idle;
                                live.job = None;
                            }
                            self.touch();
                        }
                    }
                }
            }

            // Do not hold a lock when you remove a watcher.
            watches.retain(|id, w| wanted_watches.get(id).is_some_and(|roots| *roots == w.roots));
            let mut now_watching: Vec<(String, bool)> = Vec::new();
            for (id, roots) in wanted_watches {
                if !watches.contains_key(&id) {
                    if let Some(w) = self.watch(&id, &roots) {
                        watches.insert(id.clone(), w);
                    }
                }
                now_watching.push((id.clone(), watches.contains_key(&id)));
            }
            {
                let mut inner = self.lock();
                for (id, live) in inner.live.iter_mut() {
                    let on = now_watching.iter().any(|(i, ok)| i == id && *ok);
                    if live.watching != on {
                        live.watching = on;
                        self.touch();
                    }
                }
            }

            // A network share can be slow. Use a different thread.
            for (id, i, root, expect) in probes {
                let me = self.clone();
                thread::spawn(move || {
                    let (ok, info) = probe(&root, &expect);
                    let mut inner = me.lock();
                    if let Some(live) = inner.live.get_mut(&id) {
                        live.probing[i] = false;
                        live.probed[i] = Some(Instant::now());
                        let moved = |a: &Option<VolumeInfo>, b: &Option<VolumeInfo>| match (a, b) {
                            (Some(a), Some(b)) => a.mount != b.mount || a.name != b.name || a.free.abs_diff(b.free) > 50_000_000,
                            (None, None) => false,
                            _ => true,
                        };
                        if live.online[i] != Some(ok) || moved(&live.volumes[i], &info) {
                            live.online[i] = Some(ok);
                            live.volumes[i] = info;
                            me.touch();
                        }
                    }
                });
            }
            for (id, job) in start {
                let me = self.clone();
                thread::spawn(move || {
                    me.run(&id, job);
                    let mut inner = me.lock();
                    inner.running = inner.running.saturating_sub(1);
                    if let Some(live) = inner.live.get_mut(&id) {
                        live.phase = Phase::Idle;
                        live.job = None;
                        live.progress = None;
                        if let Some(next) = live.next.take() {
                            live.job = Some(next);
                            live.phase = Phase::Queued;
                        }
                    }
                    me.touch();
                });
            }

            if self.changed.swap(false, Ordering::Relaxed) {
                (self.sink)(Signal::State);
            }
        }
    }

    fn watch(self: &Arc<Self>, id: &str, roots: &[PathBuf]) -> Option<Watch> {
        let weak = Arc::downgrade(self);
        let pair = id.to_owned();
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let Ok(event) = event else { return };
            if matches!(event.kind, notify::EventKind::Access(_)) || event.paths.iter().all(|p| not_worth_a_sync(p)) {
                return;
            }
            if let Some(engine) = weak.upgrade() {
                let now = Instant::now();
                let mut dirty = engine.dirty.lock().unwrap_or_else(|e| e.into_inner());
                dirty.entry(pair.clone()).and_modify(|(_, last)| *last = now).or_insert((now, now));
            }
        })
        .ok()?;
        for root in roots {
            watcher.watch(root, RecursiveMode::Recursive).ok()?;
        }
        Some(Watch { roots: roots.to_vec(), _watcher: watcher })
    }

    fn snapshot_path(&self, id: &str) -> PathBuf {
        self.dir.join("snapshots").join(format!("{id}.json"))
    }

    fn run(&self, id: &str, job: Job) {
        let (pair, cancel) = {
            let inner = self.lock();
            let Some(pair) = inner.config.pairs.iter().find(|p| p.id == id).cloned() else { return };
            let Some(live) = inner.live.get(id) else { return };
            (pair, live.cancel.clone())
        };
        let base: Option<Snapshot> =
            if pair.mode == Mode::TwoWay { config::read_json(&self.snapshot_path(id)) } else { None };

        let compared = sync::compare(&pair, base, &cancel, &|side, p| {
            if let Some(live) = self.lock().live.get_mut(id) {
                live.scan[side.ix()] = *p;
            }
            self.touch();
        });
        let plan = match compared {
            Ok(plan) => Arc::new(plan),
            Err(e) => {
                let mut inner = self.lock();
                if let Some(live) = inner.live.get_mut(id) {
                    match &e {
                        SyncError::Missing { side } => {
                            live.online[side.ix()] = Some(false);
                            live.volumes[side.ix()] = None;
                            live.probed[side.ix()] = Some(Instant::now());
                        }
                        SyncError::Cancelled => {}
                        _ => live.error = Some(e.to_string()),
                    }
                }
                return;
            }
        };

        let now = now_ms();
        let has_work = !plan.actions.is_empty() || plan.stale_parts.iter().any(|p| !p.is_empty());
        // An automatic sync stops here if the plan has a guard.
        let go = match job.want {
            Want::Compare => false,
            Want::Sync { confirmed } => has_work && (plan.guard.is_none() || confirmed),
        };
        {
            let mut inner = self.lock();
            let Some(live) = inner.live.get_mut(id) else { return };
            live.stats = Some([plan.source, plan.copy]);
            live.memory.checked_ms = now;
            live.plan = Some(plan.clone());
            live.settled = false;
            live.held = None;
            if !go {
                if plan.summary.changes() == 0 {
                    live.memory.synced_ms = now;
                } else if matches!(job.want, Want::Sync { .. }) {
                    live.held = plan.guard;
                }
            } else {
                live.phase = Phase::Syncing;
                live.rate = Rate::default();
            }
        }
        self.touch();

        if !go {
            if matches!(job.want, Want::Sync { .. }) {
                if let Some(guard) = plan.guard {
                    if job.trigger != Trigger::Manual {
                        self.record(RunRecord {
                            pair: id.to_owned(),
                            at_ms: now,
                            trigger: job.trigger,
                            outcome: Outcome::Held,
                            report: Report::default(),
                            message: Some(guard_text(&guard)),
                        });
                        (self.sink)(Signal::Notify {
                            title: format!("{} is waiting for you", pair.name),
                            body: guard_text(&guard),
                        });
                    }
                } else {
                    sync::tidy_history(&pair);
                    if job.eject {
                        self.eject_copy(&pair, "Nothing needed copying.");
                    }
                }
            }
            self.save_memory();
            return;
        }

        let applied = sync::apply(&pair, &plan, &cancel, &mut |p| {
            if let Some(live) = self.lock().live.get_mut(id) {
                live.rate.add(p.bytes_done);
                live.progress = Some(p.clone());
            }
            self.touch();
        });
        if let Some(snapshot) = &applied.snapshot {
            let _ = config::write_json(&self.snapshot_path(id), snapshot);
        }
        let report = applied.report;
        let outcome = if report.stopped.is_some() {
            Outcome::Stopped
        } else if report.cancelled {
            Outcome::Cancelled
        } else if report.failed + report.skipped > 0 {
            Outcome::Partial
        } else {
            Outcome::Synced
        };
        let usage = usage_of(&pair);
        let record = RunRecord {
            pair: id.to_owned(),
            at_ms: now_ms(),
            trigger: job.trigger,
            outcome,
            message: report.stopped.clone(),
            report,
        };
        {
            let mut inner = self.lock();
            if let Some(live) = inner.live.get_mut(id) {
                live.plan = None;
                live.settled = outcome == Outcome::Synced;
                live.history = Some(usage);
                if live.settled {
                    live.memory.synced_ms = record.at_ms;
                    if let (Some(stats), Mode::Mirror) = (live.stats.as_mut(), pair.mode) {
                        stats[1] = stats[0];
                    }
                }
            }
        }
        self.record(record.clone());
        self.save_memory();
        self.announce(&pair, &record, job);
    }

    fn announce(&self, pair: &PairConfig, record: &RunRecord, job: Job) {
        let r = &record.report;
        match record.outcome {
            Outcome::Synced => {
                let what = sentence(r);
                if job.eject {
                    self.eject_copy(pair, &what);
                } else if job.trigger == Trigger::Connect {
                    (self.sink)(Signal::Notify { title: format!("{} is up to date", pair.name), body: what });
                }
            }
            Outcome::Partial => (self.sink)(Signal::Notify {
                title: format!("{} synced, with exceptions", pair.name),
                body: format!("{} could not be handled. Open Dupli to see which.", count(r.failed + r.skipped, "file")),
            }),
            Outcome::Stopped | Outcome::Failed => (self.sink)(Signal::Notify {
                title: format!("{} stopped before finishing", pair.name),
                body: record.message.clone().unwrap_or_default(),
            }),
            Outcome::Cancelled | Outcome::Held => {}
        }
    }

    fn eject_copy(&self, pair: &PairConfig, what: &str) {
        let Some(info) = volumes::volume_info(Path::new(&pair.copy)) else { return };
        if info.kind == VolumeKind::Internal {
            return;
        }
        match volumes::eject(&info) {
            Ok(()) => (self.sink)(Signal::Notify {
                title: format!("{} is safe to unplug", info.name),
                body: format!("{} is up to date. {what}", pair.name),
            }),
            Err(e) => (self.sink)(Signal::Notify {
                title: format!("{} is up to date", pair.name),
                body: format!("{what} {} could not be ejected: {e}", info.name),
            }),
        }
    }

    fn record(&self, record: RunRecord) {
        let mut inner = self.lock();
        if let Some(live) = inner.live.get_mut(&record.pair) {
            live.last = Some(record.clone());
        }
        inner.log.push(record);
        let extra = inner.log.len().saturating_sub(LOG_LIMIT);
        inner.log.drain(..extra);
        let _ = config::write_json(&self.dir.join("log.json"), &inner.log);
    }

    fn save_memory(&self) {
        let inner = self.lock();
        let memory: HashMap<&String, Memory> = inner.live.iter().map(|(id, l)| (id, l.memory)).collect();
        let _ = config::write_json(&self.dir.join("status.json"), &memory);
    }

    pub fn request(&self, id: &str, want: Want) {
        let mut inner = self.lock();
        let Some(live) = inner.live.get_mut(id) else { return };
        let job = Job { want, trigger: Trigger::Manual, eject: false };
        match live.phase {
            Phase::Idle | Phase::Queued => {
                live.job = Some(job);
                live.phase = Phase::Queued;
                live.connected = None;
                live.greeted = true;
            }
            _ => live.next = Some(job),
        }
        self.touch();
    }

    pub fn stop(&self, id: &str) {
        let mut inner = self.lock();
        if let Some(live) = inner.live.get_mut(id) {
            live.cancel.store(true, Ordering::Relaxed);
            live.next = None;
            if live.phase == Phase::Queued {
                live.phase = Phase::Idle;
                live.job = None;
            }
        }
        self.touch();
    }

    pub fn sync_all(&self) {
        let ids: Vec<String> = {
            let inner = self.lock();
            inner.config.pairs.iter().filter(|p| inner.live.get(&p.id).is_some_and(|l| l.ready())).map(|p| p.id.clone()).collect()
        };
        for id in ids {
            self.request(&id, Want::Sync { confirmed: false });
        }
    }

    pub fn settings(&self) -> AppSettings {
        self.lock().config.settings.clone()
    }

    pub fn set_settings(&self, settings: AppSettings) {
        let mut inner = self.lock();
        inner.config.settings = settings;
        let _ = inner.config.save(&self.dir.join("config.json"));
        self.touch();
    }

    pub fn save_pair(&self, mut pair: PairConfig) -> Result<String, String> {
        pair.name = pair.name.trim().to_owned();
        pair.source = tidy_path(&pair.source);
        pair.copy = tidy_path(&pair.copy);
        if pair.source.is_empty() || pair.copy.is_empty() {
            return Err("Choose both a source folder and a copy folder.".into());
        }
        if pair.name.is_empty() {
            pair.name = folder_name(&pair.source);
        }
        pair.excludes = pair.excludes.iter().map(|p| p.trim().to_owned()).filter(|p| !p.is_empty()).collect();

        let mut inner = self.lock();
        let before = inner.config.pairs.iter().find(|p| p.id == pair.id).cloned();
        let moved = before.as_ref().is_none_or(|b| b.source != pair.source || b.copy != pair.copy);
        if moved {
            for (label, path) in [("source", &pair.source), ("copy", &pair.copy)] {
                if !Path::new(path).is_dir() {
                    return Err(format!("The {label} folder is not reachable right now. Connect its drive, then try again."));
                }
            }
            if sync::overlap(Path::new(&pair.source), Path::new(&pair.copy)) {
                return Err("One folder is inside the other. The source and the copy have to be separate folders.".into());
            }
            pair.source_volume = volume_ref(&pair.source);
            pair.copy_volume = volume_ref(&pair.copy);
        } else if let Some(b) = &before {
            pair.source_volume = b.source_volume.clone();
            pair.copy_volume = b.copy_volume.clone();
        }

        let rethink = moved
            || before.as_ref().is_some_and(|b| {
                b.mode != pair.mode || b.excludes != pair.excludes || b.skip_hidden != pair.skip_hidden || b.detect_moves != pair.detect_moves
            });
        if pair.id.is_empty() {
            pair.id = config::new_id();
            let mut live = Live::new(Memory::default());
            live.hold_first = true;
            inner.live.insert(pair.id.clone(), live);
            inner.config.pairs.push(pair.clone());
        } else {
            let Some(slot) = inner.config.pairs.iter_mut().find(|p| p.id == pair.id) else {
                return Err("That pair no longer exists.".into());
            };
            let mode_changed = slot.mode != pair.mode;
            *slot = pair.clone();
            if moved || mode_changed {
                let _ = std::fs::remove_file(self.snapshot_path(&pair.id));
            }
            if let Some(live) = inner.live.get_mut(&pair.id) {
                if rethink {
                    live.plan = None;
                    live.settled = false;
                    live.held = None;
                    live.error = None;
                    live.cancel.store(true, Ordering::Relaxed);
                }
                if moved {
                    live.online = [None; 2];
                    live.probed = [None; 2];
                    live.was_ready = false;
                    live.hold_first = true;
                    live.stats = None;
                    live.history = None;
                }
            }
        }
        inner.config.save(&self.dir.join("config.json")).map_err(|e| format!("Couldn't save: {e}"))?;
        let id = pair.id.clone();
        let idle_and_ready = inner.live.get(&id).is_some_and(|l| l.ready() && !l.busy());
        drop(inner);
        if rethink && !moved && idle_and_ready {
            self.request(&id, Want::Compare);
        }
        self.touch();
        Ok(id)
    }

    pub fn remove_pair(&self, id: &str) {
        let mut inner = self.lock();
        if let Some(live) = inner.live.remove(id) {
            live.cancel.store(true, Ordering::Relaxed);
        }
        inner.config.pairs.retain(|p| p.id != id);
        inner.log.retain(|r| r.pair != id);
        let _ = inner.config.save(&self.dir.join("config.json"));
        let _ = config::write_json(&self.dir.join("log.json"), &inner.log);
        let _ = std::fs::remove_file(self.snapshot_path(id));
        drop(inner);
        self.save_memory();
        self.touch();
    }

    pub fn reorder(&self, ids: &[String]) {
        let mut inner = self.lock();
        let place = |id: &str| ids.iter().position(|i| i == id).unwrap_or(usize::MAX);
        inner.config.pairs.sort_by_key(|p| place(&p.id));
        let _ = inner.config.save(&self.dir.join("config.json"));
        self.touch();
    }

    pub fn view(&self) -> AppView {
        let inner = self.lock();
        let pairs = inner
            .config
            .pairs
            .iter()
            .filter_map(|p| {
                let l = inner.live.get(&p.id)?;
                let progress = l.progress.as_ref().map(|g| {
                    let rate = l.rate.per_second();
                    ProgressView {
                        step: g.phase,
                        files_done: g.files_done,
                        files_total: g.files_total,
                        bytes_done: g.bytes_done,
                        bytes_total: g.bytes_total,
                        current: g.current.clone(),
                        rate,
                        eta_s: (rate > 1.0 && g.bytes_total > g.bytes_done)
                            .then(|| ((g.bytes_total - g.bytes_done) as f64 / rate).ceil() as u64),
                    }
                });
                Some(PairView {
                    config: p.clone(),
                    phase: l.phase,
                    health: l.health(),
                    online: l.online,
                    volumes: l.volumes.clone(),
                    summary: l.plan.as_ref().map(|p| p.summary),
                    notes: l.plan.as_ref().map_or(0, |p| p.notes_total),
                    guard: l.plan.as_ref().and_then(|p| p.guard),
                    stats: l.stats,
                    checked_ms: l.memory.checked_ms,
                    synced_ms: l.memory.synced_ms,
                    scan: l.scan,
                    progress,
                    last: l.last.clone(),
                    error: l.error.clone(),
                    history: l.history,
                    trigger: l.job.map(|j| j.trigger),
                    watching: l.watching,
                })
            })
            .collect();
        AppView {
            pairs,
            settings: inner.config.settings.clone(),
            platform: std::env::consts::OS,
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    fn pair(&self, id: &str) -> Result<PairConfig, String> {
        self.lock().config.pairs.iter().find(|p| p.id == id).cloned().ok_or_else(|| "That pair no longer exists.".to_string())
    }

    fn plan(&self, id: &str) -> Option<Arc<Plan>> {
        self.lock().live.get(id).and_then(|l| l.plan.clone())
    }

    pub fn plan_groups(&self, id: &str) -> PlanGroups {
        let Some(plan) = self.plan(id) else { return PlanGroups::default() };
        let mut groups: Vec<PlanGroup> = Vec::new();
        let mut at: HashMap<(Op, Side, &str, Option<&str>), usize> = HashMap::new();
        let mut more = 0u64;
        for a in &plan.actions {
            if matches!(a.op, Op::MakeDir | Op::RemoveDir) {
                continue;
            }
            let dir = names::parent(&a.rel);
            let from_dir = (a.op == Op::Move).then(|| names::parent(a.from.as_deref().unwrap_or_default()));
            let slot = match at.get(&(a.op, a.side, dir, from_dir)) {
                Some(&i) => i,
                None if groups.len() >= GROUP_LIMIT => {
                    more += 1;
                    continue;
                }
                None => {
                    at.insert((a.op, a.side, dir, from_dir), groups.len());
                    groups.push(PlanGroup {
                        op: a.op,
                        side: a.side,
                        dir: dir.to_owned(),
                        from_dir: from_dir.map(str::to_owned),
                        count: 0,
                        bytes: 0,
                        conflicts: 0,
                        sample: names::file_name(&a.rel).to_owned(),
                        sample_from: (a.op == Op::Move).then(|| names::file_name(a.from.as_deref().unwrap_or_default()).to_owned()),
                    });
                    groups.len() - 1
                }
            };
            groups[slot].count += 1;
            groups[slot].bytes += a.sig.size;
            groups[slot].conflicts += a.conflict as u64;
        }
        PlanGroups { groups, more, notes: plan.notes.clone(), notes_total: plan.notes_total }
    }

    pub fn plan_files(&self, id: &str, group: &GroupKey, offset: usize, limit: usize) -> Vec<PlanFile> {
        let Some(plan) = self.plan(id) else { return Vec::new() };
        plan.actions
            .iter()
            .filter(|a| {
                a.op == group.op
                    && a.side == group.side
                    && names::parent(&a.rel) == group.dir
                    && (a.op != Op::Move || Some(names::parent(a.from.as_deref().unwrap_or_default())) == group.from_dir.as_deref())
            })
            .skip(offset)
            .take(limit.min(500))
            .map(|a| PlanFile {
                name: names::file_name(&a.rel).to_owned(),
                from_name: (a.op == Op::Move).then(|| names::file_name(a.from.as_deref().unwrap_or_default()).to_owned()),
                size: a.sig.size,
                conflict: a.conflict,
                replaces: a.old.is_some(),
            })
            .collect()
    }

    pub fn activity(&self, id: &str) -> Vec<RunRecord> {
        self.lock().log.iter().rev().filter(|r| r.pair == id).take(60).cloned().collect()
    }

    fn root_of(pair: &PairConfig, side: Side) -> &Path {
        Path::new(if side == Side::Source { &pair.source } else { &pair.copy })
    }

    pub fn history(&self, id: &str) -> Result<HistoryView, String> {
        let pair = self.pair(id)?;
        let mut runs = Vec::new();
        for &side in sync::history_sides(pair.mode) {
            for info in history::runs(Self::root_of(&pair, side)) {
                runs.push(HistoryRun { side, info });
            }
        }
        runs.sort_by(|a, b| b.info.time_ms.cmp(&a.info.time_ms));
        let usage = usage_of(&pair);
        if let Some(live) = self.lock().live.get_mut(id) {
            live.history = Some(usage);
        }
        self.touch();
        Ok(HistoryView { runs, usage })
    }

    pub fn history_entries(&self, id: &str, side: Side, run: &str) -> Result<Vec<HistEntry>, String> {
        let pair = self.pair(id)?;
        Ok(history::entries(Self::root_of(&pair, side), run))
    }

    pub fn history_path(&self, id: &str, side: Side, run: Option<&str>, rel: Option<&str>) -> Result<PathBuf, String> {
        let pair = self.pair(id)?;
        let mut p = history::history_dir(Self::root_of(&pair, side));
        if let Some(run) = run {
            p.push(run);
            if let Some(rel) = rel {
                p = names::join(&p, rel);
            }
        }
        Ok(p)
    }

    pub fn history_restore(&self, id: &str, side: Side, run: &str, rels: &[String]) -> Result<RestoreReport, String> {
        let pair = self.pair(id)?;
        let cancel = {
            let mut inner = self.lock();
            let live = inner.live.get_mut(id).ok_or("That pair no longer exists.")?;
            if live.busy() {
                return Err("Wait for the current sync to finish, then restore.".into());
            }
            if !live.ready() {
                return Err("Both folders have to be connected to restore, so the file can be put back in each.".into());
            }
            live.phase = Phase::Restoring;
            live.job = None;
            live.cancel = Arc::new(AtomicBool::new(false));
            live.cancel.clone()
        };
        self.touch();
        let root = Self::root_of(&pair, side);
        let other = Self::root_of(&pair, side.other());
        let report = history::restore(root, Some(other), run, rels, &CompareOptions::default(), &cancel);
        let usage = usage_of(&pair);
        {
            let mut inner = self.lock();
            if let Some(live) = inner.live.get_mut(id) {
                live.phase = Phase::Idle;
                live.plan = None;
                live.settled = false;
                live.history = Some(usage);
            }
        }
        self.request(id, Want::Compare);
        Ok(report)
    }

    pub fn history_clear(&self, id: &str) -> Result<Pruned, String> {
        let pair = self.pair(id)?;
        if self.lock().live.get(id).is_some_and(|l| l.busy()) {
            return Err("Wait for the current sync to finish first.".into());
        }
        let mut total = Pruned::default();
        for &side in sync::history_sides(pair.mode) {
            let p = history::clear(Self::root_of(&pair, side));
            total.runs += p.runs;
            total.files += p.files;
            total.bytes += p.bytes;
        }
        if let Some(live) = self.lock().live.get_mut(id) {
            live.history = Some(Usage::default());
        }
        self.touch();
        Ok(total)
    }

    pub fn eject(&self, id: &str) -> Result<String, String> {
        let pair = self.pair(id)?;
        if self.lock().live.get(id).is_some_and(|l| l.busy()) {
            return Err("Wait for the current sync to finish first.".into());
        }
        let info = volumes::volume_info(Path::new(&pair.copy)).ok_or("The copy's drive is not connected.")?;
        volumes::eject(&info)?;
        Ok(info.name)
    }

    pub fn tray(&self) -> TraySummary {
        let view = self.view();
        let mut lines = Vec::new();
        let (mut busy, mut alert) = (0, 0);
        for p in &view.pairs {
            let text = match (p.phase, p.health) {
                (Phase::Syncing, _) => match &p.progress {
                    Some(g) if g.bytes_total > 0 => format!("syncing, {}%", g.bytes_done.min(g.bytes_total) * 100 / g.bytes_total),
                    _ => "syncing".to_string(),
                },
                (Phase::Comparing | Phase::Queued, _) => "comparing".to_string(),
                (Phase::Restoring, _) => "restoring".to_string(),
                (_, Health::Offline) => "not connected".to_string(),
                (_, Health::UpToDate) => "up to date".to_string(),
                (_, Health::Changes) => count(p.summary.map_or(0, |s| if s.file_changes() > 0 { s.file_changes() } else { s.changes() }), "change"),
                (_, Health::Held) => "waiting for you".to_string(),
                (_, Health::Problem) => "needs a look".to_string(),
                (_, Health::Unknown) => "not compared yet".to_string(),
            };
            busy += matches!(p.phase, Phase::Syncing | Phase::Comparing | Phase::Restoring) as usize;
            alert += (p.phase == Phase::Idle && matches!(p.health, Health::Held | Health::Problem)) as usize;
            lines.push(TrayLine { id: p.config.id.clone(), text: format!("{}: {text}", p.config.name) });
        }
        let state = if busy > 0 {
            TrayState::Busy
        } else if alert > 0 {
            TrayState::Alert
        } else {
            TrayState::Idle
        };
        TraySummary { lines, state, paused: view.settings.auto_paused }
    }
}

const GROUP_LIMIT: usize = 400;

#[derive(Clone, Debug, Serialize)]
pub struct PlanGroup {
    pub op: Op,
    pub side: Side,
    pub dir: String,
    pub from_dir: Option<String>,
    pub count: u64,
    pub bytes: u64,
    pub conflicts: u64,
    pub sample: String,
    pub sample_from: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct PlanGroups {
    pub groups: Vec<PlanGroup>,
    pub more: u64,
    pub notes: Vec<Note>,
    pub notes_total: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GroupKey {
    pub op: Op,
    pub side: Side,
    pub dir: String,
    pub from_dir: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PlanFile {
    pub name: String,
    pub from_name: Option<String>,
    pub size: u64,
    pub conflict: bool,
    pub replaces: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct HistoryRun {
    pub side: Side,
    #[serde(flatten)]
    pub info: RunInfo,
}

#[derive(Clone, Debug, Serialize)]
pub struct HistoryView {
    pub runs: Vec<HistoryRun>,
    pub usage: Usage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayState {
    Idle,
    Busy,
    Alert,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayLine {
    pub id: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraySummary {
    pub lines: Vec<TrayLine>,
    pub state: TrayState,
    pub paused: bool,
}

fn usage_of(pair: &PairConfig) -> Usage {
    let mut total = Usage::default();
    for &side in sync::history_sides(pair.mode) {
        let u = history::usage(Engine::root_of(pair, side));
        total.runs += u.runs;
        total.files += u.files;
        total.bytes += u.bytes;
    }
    total
}

fn tidy_path(path: &str) -> String {
    let p = path.trim();
    let trimmed = p.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() || trimmed.ends_with(':') {
        p.to_owned()
    } else {
        trimmed.to_owned()
    }
}

pub fn count(n: u64, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{} {noun}s", group_digits(n))
    }
}

fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn sentence(r: &Report) -> String {
    let mut parts = Vec::new();
    for (n, word) in [(r.copied + r.updated, "copied"), (r.moved, "moved"), (r.removed, "removed"), (r.touched, "re-dated")] {
        if n > 0 {
            parts.push(format!("{} {word}", group_digits(n)));
        }
    }
    if parts.is_empty() {
        "Nothing needed copying.".to_string()
    } else {
        format!("{}.", parts.join(", "))
    }
}

pub fn guard_text(guard: &Guard) -> String {
    match guard {
        Guard::EmptySide { side, other_files } => format!(
            "The {} folder is empty. Syncing would remove all {} from the {}, so Dupli is waiting for a go-ahead.",
            if *side == Side::Source { "source" } else { "copy" },
            count(*other_files, "file"),
            if *side == Side::Source { "copy" } else { "source" },
        ),
        Guard::ManyRemovals { side, removals, total } => format!(
            "Syncing would remove {} of {} from the {}. Dupli is waiting for a go-ahead.",
            group_digits(*removals),
            count(*total, "file"),
            if *side == Side::Source { "source" } else { "copy" },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_lose_trailing_separators_but_drive_roots_keep_theirs() {
        assert_eq!(tidy_path(" /Volumes/LaCie/Photos/ "), "/Volumes/LaCie/Photos");
        assert_eq!(tidy_path("E:\\"), "E:\\");
        assert_eq!(tidy_path("/"), "/");
        assert_eq!(tidy_path("C:\\Users\\k\\Pictures\\"), "C:\\Users\\k\\Pictures");
    }

    #[test]
    fn counts_read_naturally() {
        assert_eq!(count(1, "file"), "1 file");
        assert_eq!(count(1234567, "change"), "1,234,567 changes");
        let r = Report { copied: 12, updated: 2, moved: 1203, ..Default::default() };
        assert_eq!(sentence(&r), "14 copied, 1,203 moved.");
        assert_eq!(sentence(&Report::default()), "Nothing needed copying.");
    }

    #[test]
    fn dupli_ignores_its_own_footprints() {
        assert!(not_worth_a_sync(Path::new("/a/.dupli/history/x/y.jpg")));
        assert!(not_worth_a_sync(Path::new("/a/b/.DS_Store")));
        assert!(not_worth_a_sync(Path::new("/a/b/.dupli-1f-2.part")));
        assert!(!not_worth_a_sync(Path::new("/a/b/IMG_1.JPG")));
    }
}
