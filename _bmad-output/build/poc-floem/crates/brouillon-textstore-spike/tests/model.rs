//! Property tests: random edits, undo/redo, saves and reads on the lazy store (tiny block/chunk/piece-block sizes
//! so every boundary case is hit) and on the rope baseline must match a reference `String` model exactly.
use brouillon_textstore_spike::lazy::{Config, LazyStore, Mode, Utf8};
use brouillon_textstore_spike::rope_store::RopeStore;
use brouillon_textstore_spike::store::{count_matches, hash_all, StoreError, TextStore};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

const POOL: &[&str] = &["a", "b", "xyz", " ", "é", "→", "😀", "\n", "\n", "ñandú", "{", "}", "ZQ", "\t", "\r\n"];
const POOL_LF: &[&str] = &["a", "b", "xyz", " ", "é", "→", "😀", "\n", "\n", "ñandú", "{", "}", "ZQ", "\t"];

fn rand_text(rng: &mut ChaCha8Rng, pool: &[&str], n: usize) -> String {
    (0..n).map(|_| pool[rng.gen_range(0..pool.len())]).collect()
}
fn boundaries(s: &str) -> Vec<usize> {
    s.char_indices().map(|(i, _)| i).chain(std::iter::once(s.len())).collect()
}
fn model_line_start(s: &str, n: usize) -> Option<usize> {
    if n == 0 {
        return Some(0);
    }
    s.match_indices('\n').nth(n - 1).map(|(i, _)| i + 1)
}

fn check_reads(rng: &mut ChaCha8Rng, st: &dyn TextStore, model: &str, rope_too: bool) {
    assert_eq!(st.byte_len(), model.len());
    let bs = boundaries(model);
    for _ in 0..6 {
        let a = bs[rng.gen_range(0..bs.len())];
        let b = bs[rng.gen_range(0..bs.len())];
        let (a, b) = (a.min(b), a.max(b));
        assert_eq!(st.slice_string(a..b), model[a..b], "slice {a}..{b}");
        assert!(st.is_char_boundary(a));
        if rope_too {
            assert_eq!(st.line_of_byte(a), model[..a].matches('\n').count(), "line_of_byte({a})");
        }
    }
    // non-boundary probes
    for _ in 0..4 {
        let p = rng.gen_range(0..=model.len());
        assert_eq!(st.is_char_boundary(p), model.is_char_boundary(p), "is_char_boundary({p})");
    }
    let nl = model.matches('\n').count();
    for _ in 0..4 {
        let n = rng.gen_range(0..nl + 3);
        let want = if n <= nl { model_line_start(model, n) } else { None };
        assert_eq!(st.byte_of_line(n), want, "byte_of_line({n})");
    }
    assert_eq!(st.line_count(), nl + 1);
}

fn run_script(store: &mut dyn TextStore, initial: &str, seed: u64, steps: usize, pool: &[&str], rope_too: bool, dir: &std::path::Path) {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut model = initial.to_string();
    let mut undo: Vec<String> = vec![];
    let mut redo: Vec<String> = vec![];
    let path = dir.join(format!("doc_{seed}.txt"));
    for step in 0..steps {
        match rng.gen_range(0..100) {
            0..=69 => {
                let grouped = rng.gen_ratio(1, 5);
                let before = model.clone();
                if grouped {
                    store.begin_group();
                }
                for _ in 0..if grouped { rng.gen_range(1..4) } else { 1 } {
                    let bs = boundaries(&model);
                    let a = bs[rng.gen_range(0..bs.len())];
                    let b = if rng.gen_bool(0.5) { a } else { bs[rng.gen_range(0..bs.len())].max(a).min(a + 200) };
                    let b = *bs.iter().filter(|&&x| x >= b).next().unwrap_or(&model.len());
                    let tl = rng.gen_range(1..8);
                    let text = if rng.gen_ratio(1, 4) { String::new() } else { rand_text(&mut rng, pool, tl) };
                    store.replace_range(a..b, &text).unwrap();
                    model.replace_range(a..b, &text);
                }
                if grouped {
                    store.end_group();
                }
                undo.push(before);
                redo.clear();
            }
            70..=79 => {
                let ok = store.undo();
                assert_eq!(ok, !undo.is_empty(), "undo availability");
                if ok {
                    redo.push(model.clone());
                    model = undo.pop().unwrap();
                }
            }
            80..=86 => {
                let ok = store.redo();
                assert_eq!(ok, !redo.is_empty());
                if ok {
                    undo.push(model.clone());
                    model = redo.pop().unwrap();
                }
            }
            87..=89 => {
                // edit at a non-boundary must be refused, and must not change anything
                let nb: Vec<usize> = (0..=model.len()).filter(|&i| !model.is_char_boundary(i)).collect();
                if !nb.is_empty() {
                    let p = nb[rng.gen_range(0..nb.len())];
                    assert!(matches!(store.replace_range(p..p, "q"), Err(StoreError::NotCharBoundary(_))));
                }
                assert!(matches!(store.replace_range(0..model.len() + 1, ""), Err(StoreError::OutOfRange)));
            }
            90..=93 => {
                store.save(&path).unwrap();
                assert_eq!(std::fs::read(&path).unwrap(), model.as_bytes(), "saved bytes (step {step})");
                assert!(!store.is_dirty());
            }
            _ => {}
        }
        check_reads(&mut rng, store, &model, rope_too);
        if step % 25 == 0 {
            assert_eq!(hash_all(store), {
                let mut h = 0xcbf29ce484222325u64;
                for &b in model.as_bytes() {
                    h ^= b as u64;
                    h = h.wrapping_mul(0x100000001b3);
                }
                h
            });
            for needle in ["ZQ", "é\n", "a\r\n", "xyz"] {
                assert_eq!(count_matches(store, needle.as_bytes()), model.matches(needle).count(), "count {needle:?}");
            }
        }
    }
    // unwind everything: the original must come back exactly
    while store.undo() {
        model = undo.pop().unwrap();
    }
    assert_eq!(model, initial);
    assert_eq!(store.slice_string(0..store.byte_len()), initial);
}

fn tiny(mode: Mode, bg: bool) -> Config {
    Config { mode, block: 16, chunk: 24, lru_chunks: 3, max_pieces_per_block: 4, background_index: bg, validate_before_edit: true }
}

#[test]
fn lazy_matches_reference_model() {
    let dir = tempfile::tempdir().unwrap();
    let mut rng = ChaCha8Rng::seed_from_u64(7);
    for (i, (mode, bg)) in [(Mode::Chunked, true), (Mode::Chunked, false), (Mode::Mmap, true), (Mode::Mmap, false)].into_iter().enumerate() {
        for crlf in [false, true] {
            let pool = if crlf { POOL } else { POOL_LF };
            let initial = rand_text(&mut rng, pool, 900);
            let f = dir.path().join(format!("base_{i}_{crlf}.txt"));
            std::fs::write(&f, &initial).unwrap();
            let mut st = LazyStore::open(&f, tiny(mode, bg)).unwrap();
            if i == 3 {
                // before the index is built the line API is already exact (lazy path)
                let nl = initial.matches('\n').count();
                assert_eq!(st.byte_of_line(nl / 2), model_line_start(&initial, nl / 2));
            }
            run_script(&mut st, &initial, 100 + i as u64 * 2 + crlf as u64, 600, pool, !crlf, dir.path());
        }
    }
}

#[test]
fn rope_baseline_matches_reference_model() {
    let dir = tempfile::tempdir().unwrap();
    let mut rng = ChaCha8Rng::seed_from_u64(11);
    let initial = rand_text(&mut rng, POOL_LF, 900);
    let f = dir.path().join("base.txt");
    std::fs::write(&f, &initial).unwrap();
    let mut st = RopeStore::open(&f).unwrap();
    run_script(&mut st, &initial, 300, 600, POOL_LF, true, dir.path());
}

#[test]
fn lazy_default_config_big_enough_to_use_many_blocks() {
    // 3 MB with the real defaults (64 KB checkpoints, 256 pieces per block): many blocks, many pieces.
    let dir = tempfile::tempdir().unwrap();
    let mut rng = ChaCha8Rng::seed_from_u64(5);
    let initial = rand_text(&mut rng, POOL_LF, 600_000);
    let f = dir.path().join("big.txt");
    std::fs::write(&f, &initial).unwrap();
    for mode in [Mode::Chunked, Mode::Mmap] {
        let mut st = LazyStore::open(&f, Config { mode, ..Config::default() }).unwrap();
        let mut model = initial.clone();
        for _ in 0..3000 {
            let bs: Vec<usize> = (0..6).map(|_| rng.gen_range(0..model.len())).map(|p| (p..).find(|&q| model.is_char_boundary(q)).unwrap()).collect();
            let a = bs[0];
            let b = (a + rng.gen_range(0..40)..).find(|&q| q >= model.len() || model.is_char_boundary(q)).unwrap().min(model.len());
            let tl = rng.gen_range(0..6);
            let t = rand_text(&mut rng, POOL_LF, tl);
            st.replace_range(a..b, &t).unwrap();
            model.replace_range(a..b, &t);
        }
        assert!(st.stats().blocks > 4, "expected a multi-block piece list, got {:?}", st.stats());
        let mut r2 = ChaCha8Rng::seed_from_u64(9);
        for _ in 0..200 {
            check_reads(&mut r2, &st, &model, true);
        }
        assert_eq!(st.slice_string(0..st.byte_len()), model);
    }
}

#[test]
fn invalid_utf8_policy_read_only_and_byte_exact_save() {
    let dir = tempfile::tempdir().unwrap();
    let mut bytes = "héllo wörld\n".repeat(50).into_bytes();
    bytes.splice(300..300, [0xFF, 0xFE, b'x']); // not valid UTF-8
    let f = dir.path().join("bad.txt");
    std::fs::write(&f, &bytes).unwrap();
    let mut st = LazyStore::open(&f, tiny(Mode::Chunked, true)).unwrap();
    st.wait_index();
    assert_eq!(st.utf8_status(), Utf8::Invalid(300));
    assert!(matches!(st.replace_range(0..0, "x"), Err(StoreError::InvalidUtf8(300))));
    // lossy view works, raw bytes are intact, save is byte-exact
    assert!(st.slice_string(290..320).contains('\u{FFFD}'));
    let out = dir.path().join("bad_out.txt");
    st.save(&out).unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), bytes);
    // a dangling incomplete sequence at EOF is invalid too
    let f2 = dir.path().join("cut.txt");
    std::fs::write(&f2, [b'a', b'\n', 0xE2, 0x82]).unwrap();
    let st = LazyStore::open(&f2, tiny(Mode::Mmap, false)).unwrap();
    st.wait_index();
    assert_eq!(st.utf8_status(), Utf8::Invalid(2));
}

#[test]
fn bom_empty_and_exact_block_multiple_files() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("bom.txt");
    let mut b = vec![0xEF, 0xBB, 0xBF];
    b.extend_from_slice("ligne1\nligne2\n".as_bytes());
    std::fs::write(&f, &b).unwrap();
    let mut st = LazyStore::open(&f, tiny(Mode::Chunked, true)).unwrap();
    assert_eq!(st.slice_string(0..6), "ligne1");
    st.insert(0, "# ").unwrap();
    st.save(&f).unwrap();
    assert_eq!(std::fs::read(&f).unwrap(), [&[0xEF, 0xBB, 0xBF][..], "# ligne1\nligne2\n".as_bytes()].concat());
    // empty file
    let e = dir.path().join("empty.txt");
    std::fs::write(&e, b"").unwrap();
    for mode in [Mode::Chunked, Mode::Mmap] {
        let mut st = LazyStore::open(&e, tiny(mode, true)).unwrap();
        assert_eq!((st.byte_len(), st.line_count(), st.byte_of_line(0), st.byte_of_line(1)), (0, 1, Some(0), None));
        st.insert(0, "x\ny").unwrap();
        assert_eq!((st.byte_len(), st.line_count(), st.byte_of_line(1)), (3, 2, Some(2)));
        assert!(st.undo());
        assert_eq!(st.byte_len(), 0);
    }
    // size exactly 3 blocks (16 B) with a '\n' on the last byte
    let m = dir.path().join("mult.txt");
    let txt = format!("{}\n{}\n{}\n", "a".repeat(14), "b".repeat(17), "c".repeat(14));
    assert_eq!(txt.len(), 48);
    std::fs::write(&m, &txt).unwrap();
    let st = LazyStore::open(&m, tiny(Mode::Chunked, false)).unwrap();
    assert_eq!(st.line_count(), 4);
    assert_eq!(st.byte_of_line(3), Some(48));
    assert_eq!(st.line_of_byte(48), 3);
}

#[test]
fn external_change_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("ext.txt");
    std::fs::write(&f, "hello\n").unwrap();
    let st = LazyStore::open(&f, tiny(Mode::Chunked, false)).unwrap();
    assert!(!st.externally_changed().unwrap());
    std::thread::sleep(std::time::Duration::from_millis(30));
    std::fs::write(&f, "hello world, longer\n").unwrap();
    assert!(st.externally_changed().unwrap());
}
