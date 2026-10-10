//! Fabric package rosters without the app: status of an environment against a roster, or the
//! install itself, on an existing runtime folder.
//!
//!   cargo run -p cobalt-runtime --example roster -- <runtime-root> status  <profile|sail> [roster]
//!   cargo run -p cobalt-runtime --example roster -- <runtime-root> install <profile|sail> [roster]
//!
//! `roster` defaults to the profile name (for `sail`: fabric-2.0).

use cobalt_runtime::install::{self, Context, Progress};
use cobalt_runtime::{Manifest, RuntimeDirs, RuntimeStatus};
use std::sync::atomic::AtomicBool;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        eprintln!("usage: roster <runtime-root> status|install <profile|sail> [roster]");
        std::process::exit(2);
    }
    let dirs = RuntimeDirs::new(&args[0]);
    let env = args[2].clone();
    let roster = args.get(3).cloned().unwrap_or_else(|| if env == cobalt_runtime::SAIL_ENV { "fabric-2.0".into() } else { env.clone() });
    let manifest = Manifest::embedded();
    println!("local-spark-mcp pin {} · rosters: {}", manifest.local_spark_mcp.version, manifest.engine_rosters(if env == cobalt_runtime::SAIL_ENV { cobalt_runtime::Engine::Sail } else { cobalt_runtime::Engine::PySpark }).iter().map(|(n, r)| format!("{n} ({})", r.packages.len())).collect::<Vec<_>>().join(", "));
    let show = |label: &str| {
        let st = if env == cobalt_runtime::SAIL_ENV { RuntimeStatus::inspect_sail(&dirs, &manifest, &roster) } else { RuntimeStatus::inspect(&dirs, &manifest, &env, None, true) };
        println!("{label}: env={:?} package={:?}", st.env, st.package_version);
        match st.roster {
            Some(r) => println!("  roster {}: {} of {} at Fabric's version · missing {:?} · other version {:?} · platform fallback {:?} · failed {:?} · complete {}", r.profile, r.installed, r.total, r.missing, r.mismatched, r.variants, r.failed, r.complete()),
            None => println!("  no roster status"),
        }
    };
    show("before");
    if args[1] == "install" {
        let cancel = AtomicBool::new(false);
        let started = std::time::Instant::now();
        let progress = |p: Progress| match p {
            Progress::Step { step, label } => println!("[{:>5.0}s] == {:?}: {label}", started.elapsed().as_secs_f32(), step),
            Progress::Bytes { .. } => {}
            Progress::Log(s) => println!("[{:>5.0}s] {s}", started.elapsed().as_secs_f32()),
        };
        let cx = Context { dirs: &dirs, manifest: &manifest, progress: &progress, cancel: &cancel };
        match install::install_roster(&cx, &env, &roster) {
            Ok(s) => println!("OK: {s}"),
            Err(e) => {
                println!("FAILED: {e}");
                std::process::exit(1);
            }
        }
        show("after");
    }
}
