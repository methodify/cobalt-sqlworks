//! Manual end-to-end check: `cargo run -p cobalt-runtime --example provision -- <root> [profile] [microsoft|temurin] [steps...]`
//! Provisions the runtime under <root> and runs the smoke test.
use cobalt_runtime::install::{provision, Context, Plan, Progress, Step};
use cobalt_runtime::{JdkVendor, Manifest, RuntimeDirs, RuntimeStatus};
use std::sync::atomic::AtomicBool;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = args.first().expect("root dir");
    let profile = args.get(1).cloned().unwrap_or_else(|| "fabric-2.0".into());
    let vendor = match args.get(2).map(String::as_str) {
        Some("temurin") => JdkVendor::Temurin,
        _ => JdkVendor::Microsoft,
    };
    let steps: Vec<Step> = if args.len() > 3 {
        args[3..]
            .iter()
            .map(|s| match s.as_str() {
                "uv" => Step::Uv,
                "python" => Step::Python,
                "env" => Step::Env,
                "jdk" => Step::Jdk,
                "warm" => Step::Warm,
                other => panic!("unknown step {other}"),
            })
            .collect()
    } else {
        Step::ALL.to_vec()
    };
    let dirs = RuntimeDirs::new(root);
    let manifest = Manifest::embedded();
    let status = RuntimeStatus::inspect(&dirs, &manifest, &profile, None);
    println!("before: uv={:?}\n python={:?}\n env={:?}\n jdk={:?}\n candidates={:?}", status.uv, status.python, status.env, status.jdk, status.jdk_candidates.iter().map(|c| c.label()).collect::<Vec<_>>());
    let cancel = AtomicBool::new(false);
    let started = std::time::Instant::now();
    let progress = |p: Progress| match p {
        Progress::Step { step, label } => println!("[{:>5.0}s] == {:?}: {label}", started.elapsed().as_secs_f32(), step),
        Progress::Bytes { done, total } => {
            if done % (8 << 20) < (256 << 10) {
                println!("[{:>5.0}s]    {} / {}", started.elapsed().as_secs_f32(), cobalt_runtime::fmt_bytes(done), total.map(cobalt_runtime::fmt_bytes).unwrap_or_else(|| "?".into()));
            }
        }
        Progress::Log(s) => println!("[{:>5.0}s] {s}", started.elapsed().as_secs_f32()),
    };
    let cx = Context { dirs: &dirs, manifest: &manifest, progress: &progress, cancel: &cancel };
    let plan = Plan { engine: cobalt_runtime::Engine::PySpark, profile: profile.clone(), jdk_vendor: vendor, adopt_jdk: None, steps, driver_memory: "2g".into() };
    match provision(&cx, &plan) {
        Ok(rec) => println!("OK: {}", serde_json::to_string_pretty(&rec).unwrap()),
        Err(e) => {
            println!("FAILED: {e}");
            std::process::exit(1);
        }
    }
    let status = RuntimeStatus::inspect(&dirs, &manifest, &profile, None);
    println!("after: ready={} warm={} spark={:?} disk={}", status.is_ready(), status.warm, status.spark_version, cobalt_runtime::fmt_bytes(status.disk_bytes));
}
