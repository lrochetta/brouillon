//! Brouillon image tooling.
//!   cargo run --release --manifest-path tools/images/Cargo.toml -- <social|process|check|all> [--root DIR] [--release-version X]
mod check;
mod pipeline;
mod social;
mod text;
mod util;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("all");
    let flag = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
    let root: PathBuf = flag("--root").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap());
    let assets = root.join("assets");
    let fonts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fonts");
    let version = flag("--release-version").unwrap_or_else(|| {
        std::fs::read_to_string(root.join("site/public/downloads/build-info.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| v["version"].as_str().map(String::from))
            .unwrap_or_else(|| "alpha".into())
    });
    let mut ok = true;
    if cmd == "social" || cmd == "all" {
        println!("social images (release {version})");
        social::run(&assets, &fonts, &version);
    }
    if cmd == "process" || cmd == "all" {
        println!("pipeline");
        let seed_path = flag("--seed").map(PathBuf::from).unwrap_or_else(|| root.join("assets/seed.json"));
        pipeline::run(&assets, &pipeline::load_seed(&seed_path));
    }
    if cmd == "check" || cmd == "all" {
        println!("check");
        ok = check::run(&assets);
    }
    if !matches!(cmd, "social" | "process" | "check" | "all") {
        eprintln!("usage: images <social|process|check|all> [--root DIR] [--release-version X]");
        std::process::exit(2);
    }
    if !ok {
        std::process::exit(1);
    }
}
