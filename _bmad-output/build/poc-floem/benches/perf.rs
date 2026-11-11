//! Criterion harness for the two perf budgets we can measure without a
//! windowed event loop:
//!
//!   * `bench_load_100k` — read a 100k-line file from memory into the rope
//!     plus the per-line Vec the renderer consumes. Stand-in for "cold load".
//!   * `bench_scroll_step` — pull `viewport_height` consecutive lines from
//!     an arbitrary offset. Stand-in for "one mouse-wheel notch".
//!
//! Interpretation: `bench_scroll_step` must stay well under 16.6ms (one
//! frame at 60Hz) on the dev machine to leave headroom for paint + GPU.

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use brouillon::buffer::Buffer;
use brouillon::sample;

const SAMPLE_LINES: usize = 100_000;
const SEED: u64 = 0xED17_2026;
const VIEWPORT_HEIGHT: usize = 80;

fn bench_load_100k(c: &mut Criterion) {
    let text = sample::generate(SAMPLE_LINES, SEED);
    c.bench_function("load_100k_rope_and_lines", |b| {
        b.iter(|| {
            let buf = Buffer::from_str(black_box(&text));
            // Mimic the prep cost the renderer pays: collect lines into a Vec.
            let lines: Vec<String> = text
                .split_inclusive('\n')
                .map(|s: &str| s.trim_end_matches('\n').to_string())
                .collect();
            black_box((buf.byte_len(), lines.len()))
        })
    });
}

fn bench_scroll_step(c: &mut Criterion) {
    let text = sample::generate(SAMPLE_LINES, SEED);
    let buf = Buffer::from_str(&text);

    c.bench_function("scroll_step_viewport_80lines", |b| {
        // Bounce through different offsets so we don't always hit the same
        // rope nodes (caches were warm otherwise).
        let mut offset = 0usize;
        b.iter(|| {
            offset = (offset + 137) % (SAMPLE_LINES - VIEWPORT_HEIGHT);
            let lines = buf.visible_lines(offset, offset + VIEWPORT_HEIGHT);
            black_box(lines.len())
        })
    });
}

criterion_group!(benches, bench_load_100k, bench_scroll_step);
criterion_main!(benches);
