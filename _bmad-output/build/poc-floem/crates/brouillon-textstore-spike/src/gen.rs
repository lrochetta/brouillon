//! Deterministic generators for code-like and prose-like text files (LF or CRLF), streamed to disk.
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::io::Write;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Code,
    Prose,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Eol {
    Lf,
    Crlf,
}

const IDENTS: &[&str] = &[
    "buffer", "cursor", "handle_request", "viewport", "line_index", "piece", "offset", "result", "config", "state", "render",
    "update", "parse_token", "Vec", "String", "Option", "self", "count", "start", "end", "len", "data", "chunk", "window",
];
const KEYWORDS: &[&str] = &["fn", "let", "if", "else", "for", "while", "match", "return", "pub", "struct", "impl", "use", "mut"];
const WORDS: &[&str] = &[
    "the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog", "editor", "brouillon", "texte", "fichier", "écriture",
    "mémoire", "rapide", "où", "être", "ça", "naïve", "café", "résumé", "and", "of", "to", "in", "is", "that", "with", "for",
    "un", "une", "des", "les", "dans", "pour", "avec", "sans", "sous", "très", "grand", "petit", "long", "court", "—", "→",
];

fn code_line(rng: &mut ChaCha8Rng, out: &mut String) {
    let indent = rng.gen_range(0..5) * 4;
    for _ in 0..indent {
        out.push(' ');
    }
    match rng.gen_range(0..10) {
        0 => {
            out.push_str("// ");
            for _ in 0..rng.gen_range(3..14) {
                out.push_str(WORDS[rng.gen_range(0..WORDS.len())]);
                out.push(' ');
            }
        }
        1 | 2 => {
            out.push_str(KEYWORDS[rng.gen_range(0..KEYWORDS.len())]);
            out.push(' ');
            out.push_str(IDENTS[rng.gen_range(0..IDENTS.len())]);
            out.push('(');
            out.push_str(IDENTS[rng.gen_range(0..IDENTS.len())]);
            out.push_str(": &str) {");
        }
        3 => out.push('}'),
        _ => {
            let n = rng.gen_range(2..9);
            for i in 0..n {
                if i > 0 {
                    out.push_str(if rng.gen_bool(0.5) { "." } else { " + " });
                }
                out.push_str(IDENTS[rng.gen_range(0..IDENTS.len())]);
                if rng.gen_bool(0.3) {
                    out.push_str(&rng.gen_range(0..100000u32).to_string());
                }
            }
            out.push(';');
        }
    }
}

fn prose_para(rng: &mut ChaCha8Rng, out: &mut String) {
    let n = rng.gen_range(20..120);
    for i in 0..n {
        let w = WORDS[rng.gen_range(0..WORDS.len())];
        if i > 0 {
            out.push(' ');
        }
        out.push_str(w);
        if rng.gen_ratio(1, 14) {
            out.push(',');
        }
        if rng.gen_ratio(1, 30) {
            out.push('.');
        }
    }
    out.push('.');
}

/// Write at least `size` bytes (ends on a line end). Every ~4000 lines a marker token `ZQXJ_MARK` is added (for Find).
pub fn generate(path: &Path, size: u64, kind: Kind, eol: Eol, seed: u64) -> std::io::Result<u64> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut w = std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(path)?);
    let nl = if eol == Eol::Lf { "\n" } else { "\r\n" };
    let mut written = 0u64;
    let mut line = String::with_capacity(1024);
    let mut n = 0u64;
    while written < size {
        line.clear();
        match kind {
            Kind::Code => code_line(&mut rng, &mut line),
            Kind::Prose => {
                if !rng.gen_ratio(1, 6) {
                    prose_para(&mut rng, &mut line)
                }
            }
        }
        n += 1;
        if n % 4000 == 0 {
            line.push_str(" ZQXJ_MARK");
        }
        line.push_str(nl);
        w.write_all(line.as_bytes())?;
        written += line.len() as u64;
    }
    w.flush()?;
    Ok(written)
}
