//! Size baseline: same shape as `size_probe_vault` without the vault (release-size delta measurement).
use std::path::PathBuf;

fn main() {
    let p = PathBuf::from(std::env::args().nth(1).unwrap_or_default());
    println!("{}", p.display());
}
