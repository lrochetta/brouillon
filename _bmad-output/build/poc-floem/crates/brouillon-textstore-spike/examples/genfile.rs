//! cargo run --release -p brouillon-textstore-spike --example genfile -- <out> <size-mb|size-bytes> <code|prose> <lf|crlf>
use brouillon_textstore_spike::gen::{generate, Eol, Kind};
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let n: u64 = a[1].parse().unwrap();
    let size = if n < 100_000 { n * 1024 * 1024 } else { n };
    let kind = if a[2] == "code" { Kind::Code } else { Kind::Prose };
    let eol = if a[3] == "lf" { Eol::Lf } else { Eol::Crlf };
    let w = generate(std::path::Path::new(&a[0]), size, kind, eol, 1234).unwrap();
    println!("{} bytes", w);
}
