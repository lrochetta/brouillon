//! Equivalence tests: a rope-backed `Buffer` and a lazy-store-backed `Buffer` must behave identically under random
//! edit / undo / redo / cursor-read sequences, checked against a `String` model. The lazy store runs with tiny
//! block / chunk / piece-block sizes so that every boundary is crossed constantly.
use brouillon_core::contract::{EditSet, TextEdit};
use brouillon_editor_engine::buffer::Buffer;
use brouillon_textstore_spike::lazy::{Config, LazyStore, Mode};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::path::Path;

const POOL_LF: &[&str] = &["a", "b", "xyz", " ", "é", "→", "😀", "\n", "\n", "ñandú", "{", "}", "ZQ", "\t"];
const POOL_CRLF: &[&str] = &["a", "b", "xyz", " ", "é", "→", "😀", "\r\n", "\r\n", "ñandú", "{", "}", "ZQ", "\t", "\n"];

fn tiny(mode: Mode, bg: bool) -> Config {
    Config { mode, block: 16, chunk: 24, lru_chunks: 3, max_pieces_per_block: 4, background_index: bg, validate_before_edit: true }
}

fn rand_text(rng: &mut ChaCha8Rng, pool: &[&str], n: usize) -> String {
    (0..n).map(|_| pool[rng.gen_range(0..pool.len())]).collect()
}

fn boundaries(s: &str) -> Vec<usize> {
    s.char_indices().map(|(i, _)| i).chain(std::iter::once(s.len())).collect()
}

fn lazy_buffer(path: &Path, cfg: Config) -> Buffer {
    Buffer::from_lazy(LazyStore::open(path, cfg).unwrap())
}

/// Every read of `lazy` must equal `rope` (and the model).
fn check_reads(rng: &mut ChaCha8Rng, rope: &Buffer, lazy: &Buffer, model: &str) {
    assert_eq!(lazy.byte_len(), model.len());
    assert_eq!(lazy.text(), model);
    assert_eq!(lazy.line_count_exact(), rope.line_count());
    assert_eq!(lazy.version(), rope.version(), "version");
    assert_eq!(lazy.is_dirty(), rope.is_dirty(), "dirty");
    assert_eq!(lazy.can_undo(), rope.can_undo());
    assert_eq!(lazy.can_redo(), rope.can_redo());
    let bs = boundaries(model);
    for _ in 0..6 {
        let a = bs[rng.gen_range(0..bs.len())];
        let b = bs[rng.gen_range(0..bs.len())];
        let (a, b) = (a.min(b), a.max(b));
        assert_eq!(lazy.slice(a..b), model[a..b], "slice {a}..{b}");
        assert_eq!(lazy.line_of_byte(a), rope.line_of_byte(a), "line_of_byte({a})");
        assert!(lazy.is_char_boundary(a));
        let mut pieces = String::new();
        lazy.visit_str(a..b, &mut |p| {
            pieces.push_str(p);
            true
        });
        assert_eq!(pieces, model[a..b], "visit_str {a}..{b}");
    }
    for _ in 0..4 {
        let p = rng.gen_range(0..=model.len());
        assert_eq!(lazy.is_char_boundary(p), rope.is_char_boundary(p), "is_char_boundary({p})");
    }
    let n = rope.line_count();
    for _ in 0..5 {
        let i = rng.gen_range(0..n + 2);
        assert_eq!(lazy.nth_line(i), rope.nth_line(i), "nth_line({i})");
        assert_eq!(lazy.line_byte_range(i), rope.line_byte_range(i), "line_byte_range({i})");
        assert_eq!(lazy.byte_of_line(i), rope.byte_of_line(i), "byte_of_line({i})");
    }
    assert_eq!(lazy.visible_lines(0, 4), rope.visible_lines(0, 4));
    if rng.gen_ratio(1, 6) {
        assert_eq!(lazy.hash(), rope.hash());
    }
}

struct Pair {
    rope: Buffer,
    lazy: Buffer,
    model: String,
}

fn run_script(initial: &str, seed: u64, steps: usize, pool: &[&str], cfg: Config) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("doc.txt");
    std::fs::write(&path, initial).unwrap();
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut p = Pair { rope: Buffer::from_str(initial), lazy: lazy_buffer(&path, cfg), model: initial.to_string() };
    let a_rope = p.rope.add_anchor(initial.len() / 2, brouillon_editor_engine::edit_apply::Bias::Right);
    let a_lazy = p.lazy.add_anchor(initial.len() / 2, brouillon_editor_engine::edit_apply::Bias::Right);
    let mut clock = 0u64;
    for step in 0..steps {
        clock += 100;
        let before = p.model.clone();
        let mut edited = true;
        match rng.gen_range(0..100) {
            0..=14 => {
                let bs = boundaries(&p.model);
                let at = bs[rng.gen_range(0..bs.len())];
                let tl = rng.gen_range(1..6);
                let t = rand_text(&mut rng, pool, tl);
                p.rope.insert_at(at, &t);
                p.lazy.insert_at(at, &t);
                p.model.insert_str(at, &t);
            }
            15..=29 => {
                let bs = boundaries(&p.model);
                let a = bs[rng.gen_range(0..bs.len())];
                let b = *bs.iter().find(|&&x| x >= (a + rng.gen_range(0..40)).min(p.model.len())).unwrap_or(&p.model.len());
                p.rope.remove_range(a..b);
                p.lazy.remove_range(a..b);
                p.model.replace_range(a..b, "");
                edited = a < b;
            }
            30..=44 => {
                let bs = boundaries(&p.model);
                let a = bs[rng.gen_range(0..bs.len())];
                let b = *bs.iter().find(|&&x| x >= (a + rng.gen_range(0..30)).min(p.model.len())).unwrap_or(&p.model.len());
                let tl = rng.gen_range(0..5);
                let t = rand_text(&mut rng, pool, tl);
                p.rope.replace_range(a..b, &t);
                p.lazy.replace_range(a..b, &t);
                p.model.replace_range(a..b, &t);
            }
            45..=56 => {
                // an EditSet of 1..3 ordered, non-overlapping edits as ONE group
                let bs = boundaries(&p.model);
                let mut cuts: Vec<usize> = (0..rng.gen_range(2..7)).map(|_| bs[rng.gen_range(0..bs.len())]).collect();
                cuts.sort_unstable();
                let mut edits = Vec::new();
                for w in cuts.chunks(2) {
                    if w.len() == 2 {
                        let tl = rng.gen_range(0..4);
                        edits.push(TextEdit::new(w[0], w[1], rand_text(&mut rng, pool, tl)));
                    }
                }
                let set = EditSet::from_unsorted(edits).unwrap();
                let want = set.apply_to_str(&p.model).unwrap();
                p.rope.apply_edits(&set).unwrap();
                p.lazy.apply_edits(&set).unwrap();
                edited = !set.is_empty();
                p.model = want;
            }
            57..=64 => {
                // a transaction of two edits (one undo step)
                let bs = boundaries(&p.model);
                let a = bs[rng.gen_range(0..bs.len())];
                let t1 = rand_text(&mut rng, pool, 2);
                let t2 = rand_text(&mut rng, pool, 3);
                for b in [&mut p.rope, &mut p.lazy] {
                    b.transaction(|b| {
                        b.insert_at(a, &t1);
                        b.insert_at(0, &t2);
                    });
                }
                p.model.insert_str(a, &t1);
                p.model.insert_str(0, &t2);
            }
            65..=72 => {
                // typing run: a few coalesced keystrokes at the end of the previous one
                let bs = boundaries(&p.model);
                let mut at = bs[rng.gen_range(0..bs.len())];
                p.rope.break_typing();
                p.lazy.break_typing();
                for _ in 0..rng.gen_range(1..5) {
                    let t = rand_text(&mut rng, &["a", "b", "é", "x", "😀"], 1);
                    clock += 50;
                    p.rope.typing_insert(at, &t, clock);
                    p.lazy.typing_insert(at, &t, clock);
                    p.model.insert_str(at, &t);
                    at += t.len();
                }
            }
            73..=78 if !p.model.is_empty() => {
                // backspaces (coalesced)
                let bs = boundaries(&p.model);
                let mut end = bs[rng.gen_range(1..bs.len())];
                p.rope.break_typing();
                p.lazy.break_typing();
                for _ in 0..rng.gen_range(1..4) {
                    if end == 0 {
                        break;
                    }
                    let start = p.model[..end].char_indices().last().map(|(i, _)| i).unwrap();
                    clock += 50;
                    p.rope.typing_delete(start..end, false, clock);
                    p.lazy.typing_delete(start..end, false, clock);
                    p.model.replace_range(start..end, "");
                    end = start;
                }
            }
            79..=89 => {
                edited = false;
                let r = p.rope.undo();
                let l = p.lazy.undo();
                assert_eq!(r, l, "undo result at step {step}");
                if r {
                    p.model = p.rope.text(); // the rope buffer is the oracle for history
                }
            }
            90..=96 => {
                edited = false;
                let r = p.rope.redo();
                let l = p.lazy.redo();
                assert_eq!(r, l, "redo result at step {step}");
                if r {
                    p.model = p.rope.text();
                }
            }
            _ => {
                edited = false;
                p.rope.mark_saved();
                p.lazy.mark_saved();
            }
        }
        let _ = (edited, before);
        assert_eq!(p.lazy.text(), p.rope.text(), "text diverged at step {step}");
        assert_eq!(p.lazy.text(), p.model.as_str(), "model diverged at step {step}");
        assert_eq!(p.lazy.anchor_pos(a_lazy), p.rope.anchor_pos(a_rope), "anchor at step {step}");
        if step % 3 == 0 {
            check_reads(&mut rng, &p.rope, &p.lazy, &p.model);
        }
    }
    check_reads(&mut rng, &p.rope, &p.lazy, &p.model);
    // undo everything: both back to the initial text
    let nr = p.rope.undo_all();
    let nl = p.lazy.undo_all();
    assert_eq!(nr, nl, "undo_all count");
    assert_eq!(p.lazy.text(), initial);
    assert_eq!(p.rope.text(), initial);
    assert!(!p.lazy.can_undo());
    // redo is available and replays to the same final state in both
    while p.rope.redo() {
        assert!(p.lazy.redo());
        assert_eq!(p.lazy.text(), p.rope.text());
    }
    assert!(!p.lazy.redo());
    // save + re-open: the file equals the text
    let out = dir.path().join("out.txt");
    p.lazy.save_to_path(&out).unwrap();
    assert_eq!(std::fs::read_to_string(&out).unwrap(), p.lazy.text());
    assert!(!p.lazy.is_dirty());
}

fn initial_doc(rng: &mut ChaCha8Rng, pool: &[&str], n: usize) -> String {
    rand_text(rng, pool, n)
}

#[test]
fn lazy_matches_rope_chunked_lf() {
    for seed in 0..6 {
        let mut rng = ChaCha8Rng::seed_from_u64(1000 + seed);
        let init = initial_doc(&mut rng, POOL_LF, 120);
        run_script(&init, seed, 160, POOL_LF, tiny(Mode::Chunked, true));
    }
}

#[test]
fn lazy_matches_rope_mmap_crlf() {
    for seed in 0..5 {
        let mut rng = ChaCha8Rng::seed_from_u64(2000 + seed);
        let init = initial_doc(&mut rng, POOL_CRLF, 150);
        run_script(&init, 50 + seed, 160, POOL_CRLF, tiny(Mode::Mmap, true));
    }
}

#[test]
fn lazy_matches_rope_without_background_scan() {
    // The index is driven on demand by the queries themselves.
    for seed in 0..4 {
        let mut rng = ChaCha8Rng::seed_from_u64(3000 + seed);
        let init = initial_doc(&mut rng, POOL_CRLF, 90);
        run_script(&init, 100 + seed, 140, POOL_CRLF, tiny(Mode::Chunked, false));
    }
}

#[test]
fn empty_and_tiny_documents() {
    for init in ["", "\n", "a", "a\n", "é", "\r\n"] {
        run_script(init, 7, 60, POOL_CRLF, tiny(Mode::Chunked, true));
    }
}

#[test]
fn real_default_sizes_many_blocks() {
    let mut rng = ChaCha8Rng::seed_from_u64(42);
    let init = rand_text(&mut rng, POOL_LF, 90_000); // ~400 KB: many 64 KB blocks
    run_script(&init, 9, 120, POOL_LF, Config::default());
}

#[test]
fn utf8_boundary_edits_are_refused_without_effect() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("d.txt");
    std::fs::write(&path, "aé→😀z").unwrap();
    let mut b = lazy_buffer(&path, tiny(Mode::Chunked, true));
    let v = b.version();
    b.insert_at(2, "X"); // inside 'é'
    b.remove_range(4..5); // inside '→'
    assert_eq!(b.text(), "aé→😀z");
    assert_eq!(b.version(), v);
    assert!(!b.is_dirty());
    let set = EditSet::from_unsorted(vec![TextEdit::insert(2, "q")]).unwrap();
    assert!(b.apply_edits(&set).is_err());
    assert_eq!(b.text(), "aé→😀z");
}

#[test]
fn invalid_utf8_is_read_only_and_saves_byte_exact_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.txt");
    let mut raw = b"hello \xFF world\nline two\n".to_vec();
    raw.extend_from_slice("é".as_bytes());
    std::fs::write(&path, &raw).unwrap();
    let mut b = lazy_buffer(&path, tiny(Mode::Chunked, true));
    b.lazy_wait_index();
    assert!(b.is_read_only());
    let v = b.version();
    b.insert_at(0, "X");
    b.remove_range(0..3);
    b.replace_range(0..1, "q");
    b.typing_insert(0, "t", 1);
    assert_eq!(b.version(), v, "no mutation on a read-only document");
    assert!(!b.is_dirty());
    assert!(b.apply_edits(&EditSet::from_unsorted(vec![TextEdit::insert(0, "x")]).unwrap()).is_err());
    // reading still works (lossy replacement character)
    assert!(b.text().contains('\u{FFFD}'));
    assert_eq!(b.nth_line(1).as_deref(), Some("line two"));
    // refusing to save: the file on disk is untouched
    assert!(b.save_to_path(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), raw);
}

#[test]
fn bom_is_kept_out_of_the_text_and_back_in_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bom.txt");
    std::fs::write(&path, b"\xEF\xBB\xBFhello\nworld").unwrap();
    let mut b = lazy_buffer(&path, tiny(Mode::Chunked, true));
    assert!(b.lazy_has_bom());
    assert_eq!(b.text(), "hello\nworld");
    b.insert_at(5, "!");
    b.save_to_path(&path).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"\xEF\xBB\xBFhello!\nworld");
}

#[test]
fn undo_survives_a_save_for_a_small_document() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.txt");
    std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
    let mut b = lazy_buffer(&path, tiny(Mode::Chunked, true));
    b.insert_at(3, " 1");
    b.remove_range(0..1);
    b.save_to_path(&path).unwrap();
    assert!(!b.is_dirty());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "ne 1\ntwo\nthree\n");
    b.insert_at(0, "Z");
    assert!(b.is_dirty());
    assert!(b.undo()); // Z
    assert!(!b.is_dirty());
    assert!(b.undo()); // across the save: the remove
    assert_eq!(b.text(), "one 1\ntwo\nthree\n");
    assert!(b.undo());
    assert_eq!(b.text(), "one\ntwo\nthree\n");
    assert!(b.redo() && b.redo());
    assert_eq!(b.text(), "ne 1\ntwo\nthree\n");
    // save again onto the same path after more history: no dangling base generation
    b.insert_at(0, "A");
    b.save_to_path(&path).unwrap();
    assert!(b.undo_all() >= 1);
    assert_eq!(b.text(), "one\ntwo\nthree\n");
}

#[test]
fn history_cap_compacts_the_add_buffer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.txt");
    std::fs::write(&path, "start\n".repeat(1000)).unwrap();
    let mut b = lazy_buffer(&path, Config::default());
    b.set_undo_limits(3, 1 << 30); // three groups only
    let big = "0123456789abcdef".repeat(8192); // 128 KB per insert
    let mut model = "start\n".repeat(1000);
    for i in 0..120 {
        let at = (i * 37) % (model.len() / 2);
        let at = (0..=at).rev().find(|&x| model.is_char_boundary(x)).unwrap();
        b.insert_at(at, &big);
        model.insert_str(at, &big);
        // replace the previous big insert so the live data stays small
        let end = at + big.len();
        b.remove_range(at..end);
        model.replace_range(at..end, "");
    }
    assert_eq!(b.text(), model);
    let (add, _pieces, _undo) = b.lazy_stats().unwrap();
    // 120 inserts of 128 KB would be 15 MB if the add buffer never shrank
    assert!(add < 6 << 20, "add buffer should have been compacted, is {add} bytes");
    // the surviving history still undoes/redoes correctly after the remap
    let t = b.text();
    assert!(b.undo());
    assert!(b.redo());
    assert_eq!(b.text(), t);
}

#[test]
fn rope_is_never_the_lazy_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("r.txt");
    std::fs::write(&path, "abc\n").unwrap();
    let b = lazy_buffer(&path, tiny(Mode::Chunked, true));
    let before = brouillon_editor_engine::buffer::lazy_rope_misuse();
    assert_eq!(b.rope().byte_len(), 0, "an empty rope, never the real text");
    assert!(brouillon_editor_engine::buffer::lazy_rope_misuse() > before);
}

#[test]
fn snapshots_are_stable_and_thread_safe() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.txt");
    let text = "line of text\n".repeat(5000);
    std::fs::write(&path, &text).unwrap();
    let mut b = lazy_buffer(&path, Config::default());
    let snap = b.snapshot();
    // edits after the snapshot do not show in it
    b.insert_at(0, "CHANGED ");
    b.remove_range(100..200);
    assert_eq!(snap.byte_len(), text.len());
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let s = snap.clone();
            let want = text.clone();
            std::thread::spawn(move || {
                let mut got = String::new();
                s.visit_str(0..s.byte_len(), &mut |p| {
                    got.push_str(p);
                    true
                });
                assert_eq!(got, want, "reader {i}");
                assert_eq!(s.line_count(), 5000);
                assert_eq!(s.slice(13..26), "line of text\n");
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
}
