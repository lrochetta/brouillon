//! Generates `sample/sample-100k.txt` (deterministic, 100k lines, ~5MB).
//! Run from repo root: `cargo run --release --example gen-sample -p brouillon`.

fn main() -> std::io::Result<()> {
    let path = "sample/sample-100k.txt";
    brouillon::sample::write_to(path, 100_000, 42)?;
    let bytes = std::fs::metadata(path)?.len();
    println!("wrote {} ({} bytes, {:.2} MB)", path, bytes, bytes as f64 / 1_048_576.0);
    Ok(())
}
