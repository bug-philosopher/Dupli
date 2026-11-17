use std::sync::atomic::AtomicBool;

use dupli_core::config::PairConfig;
use dupli_core::plan::Op;
use dupli_core::sync::{apply, compare};
use dupli_core::Mode;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let plain: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let [verb, source, copy] = plain[..] else {
        eprintln!("usage: dupli-cli compare|sync <source> <copy> [--add-only] [--yes]");
        std::process::exit(2);
    };
    let pair = PairConfig {
        id: "cli".into(),
        name: "cli".into(),
        source: source.clone(),
        copy: copy.clone(),
        mode: if flag("--add-only") { Mode::AddOnly } else { Mode::Mirror },
        ..Default::default()
    };
    let cancel = AtomicBool::new(false);
    let plan = match compare(&pair, None, &cancel, &|_, _| {}) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let s = plan.summary;
    println!(
        "source: {} files   copy: {} files\nto copy {}   to replace {}   to move {}   to remove {}   folders +{} -{}",
        plan.source.files, plan.copy.files, s.copy.n, s.update.n, s.mv.n, s.remove.n, s.make_dir, s.remove_dir
    );
    for a in plan.actions.iter().filter(|a| !matches!(a.op, Op::MakeDir | Op::RemoveDir)).take(40) {
        match (&a.op, &a.from) {
            (Op::Move, Some(from)) => println!("  move     {from}  ->  {}", a.rel),
            (op, _) => println!("  {:<8} {}", format!("{op:?}").to_lowercase(), a.rel),
        }
    }
    if s.file_changes() > 40 {
        println!("  ... and {} more", s.file_changes() - 40);
    }
    if plan.notes_total > 0 {
        println!("{} left alone (links, unreadable folders, clashing names)", plan.notes_total);
    }
    if verb.as_str() != "sync" {
        return;
    }
    if let Some(guard) = plan.guard {
        if !flag("--yes") {
            eprintln!("Not syncing: {guard:?}. Look at the list above, then pass --yes to go ahead.");
            std::process::exit(3);
        }
    }
    let done = apply(&pair, &plan, &cancel, &mut |_| {});
    let r = done.report;
    println!(
        "copied {}   replaced {}   moved {}   removed {}   re-dated {}   kept in history {}   {} bytes in {} ms",
        r.copied, r.updated, r.moved, r.removed, r.touched, r.kept, r.bytes, r.elapsed_ms
    );
    for p in &r.problems {
        println!("  ! {}: {}", p.rel, p.what);
    }
    if let Some(why) = r.stopped {
        println!("stopped: {why}");
    }
}
