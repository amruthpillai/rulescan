use clap::Parser;
use rulescan::{config::Config, engine::Engine, paths::Locations};
use std::{
    fs,
    sync::{Arc, atomic::AtomicBool},
    time::Instant,
};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value_t = 100_000)]
    entries: usize,
    #[arg(long, default_value_t = 1_000)]
    candidates: usize,
}
fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    anyhow::ensure!(
        args.candidates <= args.entries && args.entries <= 1_000_000,
        "requires candidates <= entries <= 1000000"
    );
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("corpus");
    fs::create_dir(&root)?;
    for n in 0..args.entries {
        let path = if n < args.candidates {
            root.join(format!("candidate-{n}/AGENTS.md"))
        } else {
            root.join(format!("group-{}/file-{n}.txt", n % 100))
        };
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(path, b"Harmless benchmark data.\n")?;
    }
    let engine = Engine::open(
        Locations::resolve(Some(&temp.path().join("private")))?,
        Config::default(),
    )?;
    let mut results = Vec::new();
    for pass in 1..=2 {
        let start = Instant::now();
        let result = engine.scan(
            std::slice::from_ref(&root),
            Arc::new(AtomicBool::new(false)),
            None,
        )?;
        results.push(serde_json::json!({"pass":pass,"elapsed_ms":start.elapsed().as_millis(),"entries_visited":result.coverage.entries_visited,"artifacts":result.artifacts.len(),"coverage_complete":result.coverage.complete()}));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"version":env!("CARGO_PKG_VERSION"),"cache_state":"OS caches not controlled; second pass is warm","generated_files":args.entries,"expected_candidates":args.candidates,"results":results})
        )?
    );
    Ok(())
}
