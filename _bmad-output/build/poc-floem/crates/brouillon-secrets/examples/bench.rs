//! `cargo run --release -p brouillon-secrets --example bench`: time scanning and masking 1 MB.
use std::time::Instant;

fn main() {
    let unit = "fn main() { let x = compute(42, \"hello\"); // comment: nothing here\n    config.set(\"theme\", \"dark\"); token_count += 1;\n}\n";
    let mut code = String::new();
    while code.len() < 1_000_000 {
        code.push_str(unit);
    }
    let mut dense = String::new();
    let mut i = 0;
    while dense.len() < 1_000_000 {
        dense.push_str(&format!("KEY_{i}=value{i}\npassword = \"Tr0ub4dor&3x{i}\"\nurl=https://u:Zq8vLm2x{i}@h/p\n"));
        i += 1;
    }
    for (name, t) in [("code-like 1 MB", &code), ("secret-dense 1 MB", &dense)] {
        let t0 = Instant::now();
        let s = brouillon_secrets::scan(t);
        let t1 = t0.elapsed();
        let t2 = Instant::now();
        let m = brouillon_secrets::mask(t, &s.findings, brouillon_secrets::Severity::Medium);
        println!("{name}: scan {:?}, mask {:?}, findings {}, redactions {}", t1, t2.elapsed(), s.findings.len(), m.redactions);
    }
}
