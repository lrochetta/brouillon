//! Deterministic synthetic-buffer generator for reproducible benchmarks.
//! Used by both the binary (if no CLI arg given) and the criterion bench.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

/// Generate `n_lines` of synthetic Rust-like content with a fixed seed.
/// Lines vary in length 20..120 chars and look superficially like real Rust
/// (enough for tree-sitter to produce a non-trivial parse tree). 100k lines
/// is ~5MB — fits in cache, parseable in well under a second.
pub fn generate(n_lines: usize, seed: u64) -> String {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    // ~3MB scratch — pre-allocate to keep allocator out of the timing.
    let mut out = String::with_capacity(n_lines * 60);

    const KEYWORDS: &[&str] = &[
        "fn", "let", "mut", "pub", "struct", "impl", "match", "if", "else",
        "for", "while", "return", "use", "mod", "trait", "enum",
    ];
    const IDENTS: &[&str] = &[
        "value", "result", "buffer", "node", "tree", "rope", "view", "ctx",
        "state", "config", "frame", "metric", "highlight", "parser", "line",
    ];
    const PUNCT: &[&str] = &[" = ", "(", ")", "; ", " + ", " - ", " * ", " / ", " :: "];

    for i in 0..n_lines {
        let target_len: usize = rng.gen_range(20..=120);
        let mut line = String::with_capacity(target_len + 2);

        // Indent: 0..3 levels of 4 spaces.
        let indent = rng.gen_range(0..=3);
        for _ in 0..indent {
            line.push_str("    ");
        }

        while line.len() < target_len {
            let pick = rng.gen_range(0..4);
            match pick {
                0 => line.push_str(KEYWORDS.choose(&mut rng).copied().unwrap_or("fn")),
                1 => line.push_str(IDENTS.choose(&mut rng).copied().unwrap_or("x")),
                2 => line.push_str(PUNCT.choose(&mut rng).copied().unwrap_or(" ")),
                _ => {
                    let n: u32 = rng.gen_range(0..1024);
                    line.push_str(&n.to_string());
                }
            }
            line.push(' ');
        }

        // Tag every 1000th line so manual scrolling can verify position.
        if i % 1000 == 0 {
            line.push_str(&format!("// line-marker {i}"));
        }
        line.push('\n');
        out.push_str(&line);
    }
    out
}

/// Write the synthetic file to disk. Idempotent — re-writes on each call so
/// the bench can rely on it existing.
pub fn write_to<P: AsRef<Path>>(path: P, n_lines: usize, seed: u64) -> io::Result<()> {
    let text = generate(n_lines, seed);
    let mut f = fs::File::create(path)?;
    f.write_all(text.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_empty() {
        let content = generate(0, 12345);
        assert!(content.is_empty());
    }

    #[test]
    fn generate_line_count() {
        let content = generate(10, 12345);
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 10);
    }

    #[test]
    fn generate_is_deterministic() {
        let a = generate(50, 42);
        let b = generate(50, 42);
        assert_eq!(a, b);

        let c = generate(50, 43);
        assert_ne!(a, c);
    }

    #[test]
    fn generate_line_markers() {
        let content = generate(2001, 12345);
        let lines: Vec<&str> = content.lines().collect();
        assert!(lines[0].contains("// line-marker 0"));
        assert!(lines[1000].contains("// line-marker 1000"));
        assert!(lines[2000].contains("// line-marker 2000"));
    }

    #[test]
    fn write_to_temp_file() {
        let dir = std::env::temp_dir();
        let path = dir.join("brouillon_test_sample.txt");
        write_to(&path, 5, 999).unwrap();
        
        let read_back = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = read_back.lines().collect();
        assert_eq!(lines.len(), 5);

        let _ = fs::remove_file(path);
    }
}
