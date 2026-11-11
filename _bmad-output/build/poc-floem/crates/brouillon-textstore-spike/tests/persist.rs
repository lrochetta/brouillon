//! Persistence of the lazy store: crash-at-every-step save loop, journal replay vs a String model, journal refusal
//! rules, external-change scenarios (append / rewrite / replace / delete), save behaviours (BOM, backup hook,
//! read-only, Save As, undo after save) and session-restore helpers.
use brouillon_textstore_spike::lazy::persist::*;
use brouillon_textstore_spike::lazy::{Config, LazyStore, Mode};
use brouillon_textstore_spike::store::TextStore;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

fn cfg() -> Config {
    Config { mode: Mode::Chunked, block: 256, chunk: 128, lru_chunks: 4, max_pieces_per_block: 8, background_index: true, validate_before_edit: true }
}

fn text(s: &LazyStore) -> String {
    s.slice_string(0..s.byte_len())
}

fn base_text(lines: usize) -> String {
    (0..lines).map(|i| format!("line {i} é→x\n")).collect()
}

fn temps(dir: &Path) -> Vec<String> {
    fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with('.') && n.ends_with(".tmp")).collect()
}

/// Apply the same edits to a store and to a String model.
fn edit(s: &mut LazyStore, m: &mut String, a: usize, b: usize, ins: &str) {
    let snap = |mut i: usize, m: &str| {
        i = i.min(m.len());
        while !m.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let (a, b) = (snap(a, m), snap(b, m));
    let (a, b) = (a.min(b), a.max(b));
    s.replace_range(a..b, ins).unwrap();
    m.replace_range(a..b, ins);
}

fn set_mtime(p: &Path, t: SystemTime) {
    fs::OpenOptions::new().write(true).open(p).unwrap().set_modified(t).unwrap();
}

// ------------------------------------------------------------------------------------------ crash loop

#[test]
fn save_crash_at_every_step_leaves_old_or_new_and_no_orphan() {
    let steps: Vec<SaveStep> = [SaveStep::CreateTemp, SaveStep::Flush, SaveStep::Fsync, SaveStep::Rename].into_iter().chain((0..40).map(SaveStep::Write)).collect();
    let faults = [FaultKind::Crash, FaultKind::Error(io::ErrorKind::StorageFull), FaultKind::Error(io::ErrorKind::PermissionDenied)];
    let mut injected = 0;
    for step in steps {
        for fk in faults {
            let d = tempfile::tempdir().unwrap();
            let p = d.path().join("doc.txt");
            let old = base_text(200);
            fs::write(&p, &old).unwrap();
            let mut s = LazyStore::open(&p, cfg()).unwrap();
            let mut m = old.clone();
            edit(&mut s, &mut m, 10, 20, "NEW");
            edit(&mut s, &mut m, 500, 500, "inserted\n");
            edit(&mut s, &mut m, 900, 950, "");
            edit(&mut s, &mut m, 1500, 1500, "tail éé");
            let r = s.save_with(&p, SaveOptions { fault: Some((step, fk)), ..Default::default() });
            match r {
                Ok(_) => {
                    // the fault point is past the end of this save (Write(n) beyond the last chunk): new content only
                    assert_eq!(fs::read_to_string(&p).unwrap(), m, "{step:?}");
                    assert!(!s.is_dirty());
                    assert!(temps(d.path()).is_empty());
                    continue;
                }
                Err(e) => {
                    injected += 1;
                    match fk {
                        FaultKind::Crash => assert!(matches!(e, SaveError::Crashed(_)), "{e}"),
                        FaultKind::Error(_) => assert!(matches!(e, SaveError::Write(_)), "{e}"),
                    }
                }
            }
            // old content only, whatever happened
            assert_eq!(fs::read_to_string(&p).unwrap(), old, "file must be untouched after a failure at {step:?}/{fk:?}");
            match fk {
                FaultKind::Error(_) => assert!(temps(d.path()).is_empty(), "a reported failure cleans its temp ({step:?})"),
                FaultKind::Crash => {
                    // a killed process leaves at most one of OUR temp files; the next launch sweeps it
                    assert!(temps(d.path()).len() <= 1);
                    // "recovery": reopening sees the old file, and the sweep removes the orphan
                    let fresh = LazyStore::open(&p, cfg()).unwrap();
                    assert_eq!(text(&fresh), old);
                    sweep_orphan_temps(&p);
                    assert!(temps(d.path()).is_empty(), "no orphan temp after recovery ({step:?})");
                }
            }
            // the live store is intact and a retry succeeds
            assert!(s.is_dirty());
            assert_eq!(text(&s), m);
            s.save_with(&p, SaveOptions::default()).unwrap();
            assert_eq!(fs::read_to_string(&p).unwrap(), m);
            assert!(temps(d.path()).is_empty());
            assert!(!s.is_dirty());
            assert_eq!(text(&s), m);
            // undo still works after the retry, back to the original
            while s.undo() {}
            assert_eq!(text(&s), old);
        }
    }
    assert!(injected >= 3 * 4, "the loop must actually inject failures ({injected})");
}

// ------------------------------------------------------------------------------------------ save behaviours

#[test]
fn save_keeps_bom_crlf_and_rebases_with_undo() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("bom.txt");
    let mut raw = vec![0xEF, 0xBB, 0xBF];
    raw.extend_from_slice("a\r\nb\r\nc\r\n".repeat(100).as_bytes());
    fs::write(&p, &raw).unwrap();
    let mut s = LazyStore::open(&p, cfg()).unwrap();
    let mut m = String::from_utf8(raw[3..].to_vec()).unwrap();
    edit(&mut s, &mut m, 3, 3, "X\r\n");
    s.save_with(&p, SaveOptions::default()).unwrap();
    let mut want = vec![0xEF, 0xBB, 0xBF];
    want.extend_from_slice(m.as_bytes());
    assert_eq!(fs::read(&p).unwrap(), want);
    assert_eq!(text(&s), m);
    // edits after the save + undo across the save
    edit(&mut s, &mut m, 0, 4, "");
    assert!(s.is_dirty());
    s.save_with(&p, SaveOptions::default()).unwrap();
    assert_eq!(s.undo(), true);
    assert_eq!(s.undo(), true);
    assert_eq!(text(&s), String::from_utf8(raw[3..].to_vec()).unwrap());
    assert!(s.redo());
    assert!(s.is_dirty());
}

#[test]
fn backup_hook_sees_old_bytes_and_failure_leaves_file_alone() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("b.txt");
    fs::write(&p, "old\n".repeat(50)).unwrap();
    let mut s = LazyStore::open(&p, cfg()).unwrap();
    s.insert(0, "new ").unwrap();
    let mut seen = String::new();
    let mut hook = |t: &Path| -> io::Result<Option<PathBuf>> {
        seen = fs::read_to_string(t)?;
        let b = t.with_extension("txt.bak");
        fs::copy(t, &b)?;
        Ok(Some(b))
    };
    let rep = s.save_with(&p, SaveOptions { backup: Some(&mut hook), ..Default::default() }).unwrap();
    assert_eq!(seen, "old\n".repeat(50));
    assert_eq!(fs::read_to_string(rep.backup.unwrap()).unwrap(), "old\n".repeat(50));
    assert!(fs::read_to_string(&p).unwrap().starts_with("new old"));
    // a failing backup aborts before anything is written
    s.insert(0, "again ").unwrap();
    let before = fs::read(&p).unwrap();
    let mut bad = |_: &Path| -> io::Result<Option<PathBuf>> { Err(io::Error::other("disk full")) };
    let e = s.save_with(&p, SaveOptions { backup: Some(&mut bad), ..Default::default() }).unwrap_err();
    assert!(matches!(e, SaveError::Backup(_)));
    assert_eq!(fs::read(&p).unwrap(), before);
    assert!(temps(d.path()).is_empty());
}

#[test]
fn read_only_target_is_refused_and_untouched() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("ro.txt");
    fs::write(&p, "locked\n").unwrap();
    let mut perm = fs::metadata(&p).unwrap().permissions();
    perm.set_readonly(true);
    fs::set_permissions(&p, perm).unwrap();
    let mut s = LazyStore::open(&p, cfg()).unwrap();
    s.insert(0, "x").unwrap();
    let e = s.save_with(&p, SaveOptions::default()).unwrap_err();
    assert!(matches!(e, SaveError::ReadOnly(_)), "{e}");
    assert_eq!(fs::read_to_string(&p).unwrap(), "locked\n");
    assert!(temps(d.path()).is_empty());
    assert!(s.is_dirty());
    let mut perm = fs::metadata(&p).unwrap().permissions();
    perm.set_readonly(false);
    fs::set_permissions(&p, perm).unwrap();
}

#[test]
fn save_as_writes_elsewhere_and_rebases() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("a.txt");
    let q = d.path().join("sub_b.txt");
    let old = base_text(100);
    fs::write(&p, &old).unwrap();
    let mut s = LazyStore::open(&p, cfg()).unwrap();
    let mut m = old.clone();
    edit(&mut s, &mut m, 5, 9, "ZZ");
    s.save_with(&q, SaveOptions::default()).unwrap();
    assert_eq!(fs::read_to_string(&q).unwrap(), m);
    assert_eq!(fs::read_to_string(&p).unwrap(), old, "the original is untouched by Save As");
    assert_eq!(s.base_path().unwrap(), q);
    assert!(!s.is_dirty());
    assert!(s.undo());
    assert_eq!(text(&s), old);
    s.redo();
    s.save_with(&q, SaveOptions::default()).unwrap();
    assert_eq!(fs::read_to_string(&q).unwrap(), m);
}

#[test]
fn saving_a_missing_target_creates_it() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("gone.txt");
    fs::write(&p, "abc\n").unwrap();
    let mut s = LazyStore::open(&p, cfg()).unwrap();
    s.insert(0, "x").unwrap();
    fs::remove_file(&p).unwrap();
    assert_eq!(s.check_external().unwrap(), External::Gone);
    s.save_with(&p, SaveOptions::default()).unwrap();
    assert_eq!(fs::read_to_string(&p).unwrap(), "xabc\n");
}

// ------------------------------------------------------------------------------------------ journal

#[test]
fn journal_replay_matches_string_model_through_edits_undo_and_saves() {
    for seed in 0..6u64 {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("m.txt");
        let jf = d.path().join("m.lzj");
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let init = base_text(120);
        fs::write(&p, &init).unwrap();
        let mut s = LazyStore::open(&p, cfg()).unwrap();
        let mut m = init.clone();
        let mut undo: Vec<String> = vec![];
        let mut redo: Vec<String> = vec![];
        let pool = ["a", "xyz", "é", "→", "😀", "\n", "\r\n", "ñandú", "{", "ZQ"];
        let bounds = |m: &str| -> Vec<usize> { m.char_indices().map(|(i, _)| i).chain(std::iter::once(m.len())).collect() };
        for step in 0..90 {
            match rng.gen_range(0..100) {
                0..=54 => {
                    let b = bounds(&m);
                    let (x, y) = (b[rng.gen_range(0..b.len())], b[rng.gen_range(0..b.len())]);
                    let (x, y) = (x.min(y), x.max(y));
                    let y = if rng.gen_bool(0.5) { x } else { y.min(x + 40).max(x) };
                    let y = (x..=y).rev().find(|&i| m.is_char_boundary(i)).unwrap();
                    let ins: String = (0..rng.gen_range(0..4)).map(|_| pool[rng.gen_range(0..pool.len())]).collect();
                    undo.push(m.clone());
                    redo.clear();
                    edit(&mut s, &mut m, x, y, &ins);
                }
                55..=64 => {
                    if let Some(prev) = undo.pop() {
                        redo.push(m.clone());
                        assert!(s.undo());
                        m = prev;
                    }
                }
                65..=69 => {
                    if let Some(next) = redo.pop() {
                        undo.push(m.clone());
                        assert!(s.redo());
                        m = next;
                    }
                }
                70..=76 => {
                    s.save_with(&p, SaveOptions::default()).unwrap();
                    assert_eq!(fs::read_to_string(&p).unwrap(), m);
                }
                _ => {
                    let st = s.journal_write(&jf, "t", &JournalLimits::default()).unwrap();
                    let r = LazyStore::from_journal(&jf, cfg()).unwrap();
                    assert_eq!(text(&r), m, "seed {seed} step {step}: replay != model");
                    assert!(r.is_dirty());
                    assert_eq!(r.byte_len(), m.len());
                    assert_eq!(r.line_count(), m.matches('\n').count() + 1);
                    assert!(st.bytes < 64 << 10);
                    // the recovered store is fully editable and saves correctly
                    let mut r = r;
                    r.insert(0, "R").unwrap();
                    let q = d.path().join("restored.txt");
                    r.save_with(&q, SaveOptions::default()).unwrap();
                    assert_eq!(fs::read_to_string(&q).unwrap(), format!("R{m}"));
                }
            }
            assert_eq!(text(&s), m);
        }
    }
}

#[test]
fn journal_size_is_proportional_to_edits_not_document() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("big.txt");
    fs::write(&p, base_text(50_000)).unwrap(); // ~800 KB
    let mut s = LazyStore::open(&p, Config { background_index: false, ..cfg() }).unwrap();
    s.insert(100, "typed").unwrap();
    s.delete(5000..9000).unwrap();
    let st = s.journal_write(&d.path().join("j.lzj"), "big", &JournalLimits::default()).unwrap();
    assert!(st.bytes < 600, "journal {} bytes for a 800 KB doc with two edits", st.bytes);
    assert_eq!(st.add_bytes, 5);
}

#[test]
fn journal_refuses_changed_base_and_salvages_typed_text() {
    let mk = |tag: &str| {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join(format!("{tag}.txt"));
        let jf = d.path().join("j.lzj");
        fs::write(&p, base_text(100)).unwrap();
        let mut s = LazyStore::open(&p, cfg()).unwrap();
        s.insert(0, "unsaved typing").unwrap();
        s.journal_write(&jf, "t", &JournalLimits::default()).unwrap();
        (d, p, jf)
    };
    // intact base: fine
    let (_d, _p, jf) = mk("ok");
    assert!(LazyStore::from_journal(&jf, cfg()).is_ok());
    // appended
    let (_d, p, jf) = mk("append");
    fs::OpenOptions::new().append(true).open(&p).unwrap().write_all_at_end(b"more\n");
    assert!(matches!(LazyStore::from_journal(&jf, cfg()), Err(JournalError::BaseChanged { reason: "length differs", .. })));
    // same length, different mtime
    let (_d, p, jf) = mk("touch");
    set_mtime(&p, SystemTime::now() + Duration::from_secs(3600));
    assert!(matches!(LazyStore::from_journal(&jf, cfg()), Err(JournalError::BaseChanged { reason: "modification time differs", .. })));
    // same length, same mtime, different head bytes
    let (_d, p, jf) = mk("rewrite");
    let mt = fs::metadata(&p).unwrap().modified().unwrap();
    let mut b = fs::read(&p).unwrap();
    b[0] = b'L';
    fs::write(&p, b).unwrap();
    set_mtime(&p, mt);
    assert!(matches!(LazyStore::from_journal(&jf, cfg()), Err(JournalError::BaseChanged { reason: "content differs", .. })));
    // deleted
    let (_d, p, jf) = mk("deleted");
    fs::remove_file(&p).unwrap();
    assert!(matches!(LazyStore::from_journal(&jf, cfg()), Err(JournalError::BaseGone(_))));
    // the typed text is still recoverable
    let mut out = Vec::new();
    assert_eq!(journal_salvage(&jf, &mut out).unwrap(), "unsaved typing".len() as u64);
    assert_eq!(String::from_utf8(out).unwrap(), "unsaved typing\n");
}

#[test]
fn crash_between_rename_and_journal_clear_is_refused_not_misapplied() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("c.txt");
    fs::write(&p, base_text(100)).unwrap();
    let mut s = LazyStore::open(&p, cfg()).unwrap();
    let jr = LazyJournals::new(d.path().join("rec"));
    let mut a = Autosaver::new(LazyJournals::new(d.path().join("rec")), "doc1");
    s.insert(0, "NEW ").unwrap();
    assert!(matches!(a.tick(&s, "c.txt").unwrap(), AutosaveOutcome::Written(_)));
    assert_eq!(a.tick(&s, "c.txt").unwrap(), AutosaveOutcome::Unchanged);
    // process dies after the rename but before the journal is cleared: emulate with a plain save
    s.save_with(&p, SaveOptions::default()).unwrap();
    assert!(jr.file("doc1").exists());
    assert!(matches!(jr.restore("doc1", cfg()), Err(JournalError::BaseChanged { .. })));
    // the normal path clears it
    s.insert(0, "MORE ").unwrap();
    a.tick(&s, "c.txt").unwrap();
    a.save(&mut s, &p, SaveOptions::default()).unwrap();
    assert!(!jr.file("doc1").exists());
    assert_eq!(a.tick(&s, "c.txt").unwrap(), AutosaveOutcome::Unchanged);
}

#[test]
fn journal_corruption_cap_and_listing() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("l.txt");
    fs::write(&p, base_text(100)).unwrap();
    let mut s = LazyStore::open(&p, cfg()).unwrap();
    s.insert(0, "hello").unwrap();
    let jr = LazyJournals::new(d.path().join("rec"));
    jr.write("one", &s, "l.txt", &JournalLimits::default()).unwrap();
    let listed = jr.list();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].0, "one");
    assert_eq!(listed[0].1.title, "l.txt");
    assert_eq!(listed[0].1.base_path, p);
    assert_eq!(listed[0].1.add_bytes, 5);
    // bit flip -> checksum
    let jf = jr.file("one");
    let mut b = fs::read(&jf).unwrap();
    let mid = b.len() / 2;
    b[mid] ^= 0x40;
    fs::write(&jf, &b).unwrap();
    assert!(matches!(jr.restore("one", cfg()), Err(JournalError::Corrupt(_))));
    // truncated
    fs::write(&jf, &b[..b.len() - 7]).unwrap();
    assert!(matches!(jr.restore("one", cfg()), Err(JournalError::Corrupt(_))));
    // cap: refused, the previous journal is untouched
    jr.write("two", &s, "t", &JournalLimits::default()).unwrap();
    let before = fs::read(jr.file("two")).unwrap();
    let e = jr.write("two", &s, "t", &JournalLimits { max_bytes: 10 }).unwrap_err();
    assert!(matches!(e, JournalError::TooLarge { .. }));
    assert_eq!(fs::read(jr.file("two")).unwrap(), before);
    // orphan journal temp swept
    fs::write(d.path().join("rec").join(".two.lzj.abc123.tmp"), b"half").unwrap();
    assert_eq!(jr.sweep_temps(), 1);
    // ids cannot escape the directory
    assert!(jr.file("../../etc/x").starts_with(d.path().join("rec")));
}

#[test]
fn clean_document_has_no_journal() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("n.txt");
    fs::write(&p, "abc\n").unwrap();
    let mut s = LazyStore::open(&p, cfg()).unwrap();
    let mut a = Autosaver::new(LazyJournals::new(d.path().join("rec")), "n");
    assert_eq!(a.tick(&s, "n").unwrap(), AutosaveOutcome::Unchanged);
    s.insert(0, "x").unwrap();
    assert!(matches!(a.tick(&s, "n").unwrap(), AutosaveOutcome::Written(_)));
    s.undo();
    s.save_with(&p, SaveOptions::default()).unwrap();
    assert_eq!(a.tick(&s, "n").unwrap(), AutosaveOutcome::Cleared);
    assert!(!a.journals.file("n").exists());
    assert_eq!(a.tick(&s, "n").unwrap(), AutosaveOutcome::Unchanged);
}

// ------------------------------------------------------------------------------------------ external change

trait AppendExt {
    fn write_all_at_end(self, b: &[u8]);
}
impl AppendExt for fs::File {
    fn write_all_at_end(mut self, b: &[u8]) {
        use std::io::Write;
        self.write_all(b).unwrap();
    }
}

#[test]
fn append_by_another_process_makes_store_stale_then_follow_keeps_edits() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("log.txt");
    let old = base_text(300);
    fs::write(&p, &old).unwrap();
    let mut s = LazyStore::open(&p, cfg()).unwrap();
    assert_eq!(s.check_external().unwrap(), External::Unchanged);
    let mut m = old.clone();
    edit(&mut s, &mut m, 0, 0, "HEAD ");
    // a log writer appends
    fs::OpenOptions::new().append(true).open(&p).unwrap().write_all_at_end(b"appended line 1\nappended line 2\n");
    set_mtime(&p, SystemTime::now() + Duration::from_secs(5));
    // a physical read now notices (no poll needed): the store refuses instead of returning mixed bytes
    let probe = LazyStore::open(&p, cfg()).unwrap();
    drop(probe);
    let ext = s.check_external().unwrap();
    assert!(matches!(ext, External::Appended { old_len, new_len } if new_len == old_len + 32), "{ext:?}");
    assert!(s.is_stale());
    assert_eq!(s.slice_string(10..30), "", "stale reads are refused, not served");
    assert!(s.refused_reads() >= 20);
    assert!(s.insert(0, "x").is_err(), "no edits while stale");
    assert!(matches!(s.save_with(&p, SaveOptions::default()), Err(SaveError::Stale)));
    assert!(matches!(s.journal_write(&d.path().join("j"), "t", &JournalLimits::default()), Err(JournalError::Stale)));
    // Keep mine = follow the append
    s.follow_append(&mut |_| {}).unwrap();
    assert!(!s.is_stale());
    let want = format!("{m}appended line 1\nappended line 2\n");
    assert_eq!(text(&s), want);
    assert!(s.is_dirty());
    // undo history survived and points at the new generation
    assert!(s.undo());
    assert_eq!(text(&s), format!("{old}appended line 1\nappended line 2\n"));
    s.redo();
    s.save_with(&p, SaveOptions::default()).unwrap();
    assert_eq!(fs::read_to_string(&p).unwrap(), want);
}

#[test]
fn clean_doc_follows_append_and_stays_clean() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("log.txt");
    fs::write(&p, base_text(50)).unwrap();
    let mut s = LazyStore::open(&p, cfg()).unwrap();
    fs::OpenOptions::new().append(true).open(&p).unwrap().write_all_at_end(b"x\n");
    set_mtime(&p, SystemTime::now() + Duration::from_secs(5));
    assert!(matches!(s.check_external().unwrap(), External::Appended { .. }));
    s.follow_append(&mut |_| {}).unwrap();
    assert!(!s.is_dirty());
    assert_eq!(text(&s), format!("{}x\n", base_text(50)));
}

#[test]
fn in_place_rewrite_is_modified_stale_and_reload_salvages_typing() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("r.txt");
    let old = base_text(100);
    fs::write(&p, &old).unwrap();
    let mut s = LazyStore::open(&p, cfg()).unwrap();
    s.insert(3, "my typing").unwrap();
    let mt = fs::metadata(&p).unwrap().modified().unwrap();
    fs::write(&p, old.to_uppercase()).unwrap();
    set_mtime(&p, mt + Duration::from_secs(10));
    assert_eq!(s.check_external().unwrap(), External::Modified);
    assert!(s.is_stale());
    assert_eq!(s.slice_string(20..40), "");
    assert!(matches!(s.save_with(&p, SaveOptions::default()), Err(SaveError::Stale)));
    // Reload: edits dropped, but the typed text is kept in a salvage file
    let sal = d.path().join("salvage");
    let f = s.reload(Some(&sal)).unwrap().expect("dirty doc must be salvaged");
    assert_eq!(fs::read_to_string(f).unwrap(), "my typing\n");
    assert!(!s.is_stale());
    assert_eq!(text(&s), old.to_uppercase());
    assert!(!s.is_dirty());
}

#[test]
fn truncation_is_modified() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("t.txt");
    fs::write(&p, base_text(100)).unwrap();
    let s = LazyStore::open(&p, cfg()).unwrap();
    fs::OpenOptions::new().write(true).open(&p).unwrap().set_len(100).unwrap();
    assert_eq!(s.check_external().unwrap(), External::Modified);
    assert!(s.is_stale());
}

#[test]
fn atomic_replace_by_another_editor_keeps_reads_consistent_and_gates_the_save() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("rep.txt");
    let old = base_text(100);
    fs::write(&p, &old).unwrap();
    let mut s = LazyStore::open(&p, cfg()).unwrap();
    let mut m = old.clone();
    edit(&mut s, &mut m, 0, 0, "mine ");
    // another editor writes a new file and renames it over ours
    let other = d.path().join("other.tmp");
    fs::write(&other, "someone else's content\n").unwrap();
    fs::rename(&other, &p).unwrap();
    assert_eq!(s.check_external().unwrap(), External::Replaced);
    // our open handle still reads the original bytes: nothing inconsistent, nothing refused
    assert!(!s.is_stale());
    assert_eq!(text(&s), m);
    // saving over their version needs an explicit choice
    match s.save_with(&p, SaveOptions::default()) {
        Err(SaveError::ChangedOnDisk(External::Replaced)) => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(fs::read_to_string(&p).unwrap(), "someone else's content\n");
    s.save_with(&p, SaveOptions { overwrite_changed: true, ..Default::default() }).unwrap();
    assert_eq!(fs::read_to_string(&p).unwrap(), m);
    assert_eq!(s.check_external().unwrap(), External::Unchanged);
}

#[test]
fn polling_a_quiet_file_is_unchanged_and_our_own_save_is_not_a_change() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("q.txt");
    fs::write(&p, base_text(80)).unwrap();
    let mut s = LazyStore::open(&p, cfg()).unwrap();
    for _ in 0..3 {
        assert_eq!(s.check_external().unwrap(), External::Unchanged);
    }
    s.insert(0, "z").unwrap();
    s.save_with(&p, SaveOptions::default()).unwrap();
    assert_eq!(s.check_external().unwrap(), External::Unchanged);
    s.insert(0, "y").unwrap();
    s.save_with(&p, SaveOptions::default()).unwrap(); // gate passes after our own save
    assert!(!s.is_stale());
}

// ------------------------------------------------------------------------------------------ session

#[test]
fn session_restore_clamps_cursor_and_scroll_without_loading() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("s.txt");
    let t = base_text(500);
    fs::write(&p, &t).unwrap();
    let s = LazyStore::open(&p, cfg()).unwrap();
    let lines: Vec<&str> = t.split_inclusive('\n').collect();
    let start_of = |n: usize| lines[..n].iter().map(|l| l.len()).sum::<usize>();
    assert_eq!(s.restore_cursor(0, 0), 0);
    assert_eq!(s.restore_cursor(10, 0), start_of(10));
    // columns count chars (é, → are multi-byte): "line 10 é→x": col 9 is after "é"
    let col9 = lines[10].char_indices().nth(9).unwrap().0;
    assert_eq!(s.restore_cursor(10, 9), start_of(10) + col9);
    assert!(t.is_char_boundary(s.restore_cursor(10, 9)));
    // column past the end of the line stays on the line (before the newline)
    assert_eq!(s.restore_cursor(10, 999), start_of(10) + lines[10].len() - 1);
    // line past the end: end of document
    assert_eq!(s.restore_cursor(99_999, 3), t.len());
    assert_eq!(s.restore_scroll_line(100), 100);
    assert!(s.restore_scroll_line(99_999) <= 500);
    // close + reopen: nothing is read up front
    drop(s);
    let again = LazyStore::open(&p, cfg()).unwrap();
    assert_eq!(again.byte_len(), t.len());
}
