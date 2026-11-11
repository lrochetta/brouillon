//! Crash-at-every-step loop, fault classes and a random operation sequence.
//! Invariants asserted after every injected fault:
//!  * the live file is the complete old or complete new content (or, between the two
//!    renames of a 3-step swap before recovery, missing);
//!  * the local buffer is never lost (server has it, or the journal snapshot has it);
//!  * after recovery no orphan temp/old remains and the target is old or new;
//!  * recovery never uploads.

use brouillon_safesave::fake::{FakeFs, Fault, FaultKind};
use brouillon_safesave::*;
use tempfile::TempDir;

const OLD: &[u8] = b"old content\r\nline two\r\n";
const NEW: &[u8] = b"\xEF\xBB\xBFnew content\r\nline two changed\r\nno final newline";
const T: &str = "/w/a.txt";

struct Env {
    _d: TempDir,
    journal: Journal,
    backups: BackupStore,
}
fn env() -> Env {
    let d = tempfile::tempdir().unwrap();
    let journal = Journal::new(d.path().join("journal"));
    let backups = BackupStore::new(d.path().join("backups"));
    Env { _d: d, journal, backups }
}
fn caps(posix: bool, hash: bool) -> Caps {
    Caps { posix_rename: posix, hash, set_mode: true }
}
fn only_target(fs: &FakeFs, real: &str) -> bool {
    fs.paths().iter().all(|p| p == real || p == T)
}

#[derive(Clone, Copy, Debug)]
enum Scenario {
    Replace,
    Create,
    Symlink,
}

fn run_loop(posix: bool, hash: bool, kind: &dyn Fn() -> FaultKind, sc: Scenario) -> usize {
    let mut steps = 0;
    for n in 0..80 {
        let e = env();
        let mut fs = FakeFs::new(caps(posix, hash));
        let real = if matches!(sc, Scenario::Symlink) { "/w/real.txt" } else { T };
        let mut doc = match sc {
            Scenario::Create => RemoteDoc::new_file("p", T),
            Scenario::Symlink => {
                fs.put_file(real, OLD, 0o640);
                fs.put_symlink(T, real);
                RemoteDoc::open(&mut fs, "p", T).unwrap().1
            }
            Scenario::Replace => {
                fs.put_file(T, OLD, 0o640);
                RemoteDoc::open(&mut fs, "p", T).unwrap().1
            }
        };
        let old: Option<&[u8]> = if matches!(sc, Scenario::Create) { None } else { Some(OLD) };
        fs.set_fault(Some(Fault { at: n, kind: kind() }));
        let out = SafeSaver::new(&mut fs, &e.journal, &e.backups).save(&mut doc, NEW, SaveOptions::default());
        if !fs.triggered() {
            assert!(matches!(out, SaveOutcome::Saved { .. }), "{sc:?} {out:?}");
            assert_eq!(fs.get(real).as_deref(), Some(NEW));
            assert!(only_target(&fs, real));
            if matches!(sc, Scenario::Symlink) {
                assert!(fs.is_symlink(T), "link must stay a link");
            }
            return steps;
        }
        steps += 1;
        let crashed = matches!(out, SaveOutcome::Failed(Fail::Crashed));
        let tag = format!("{sc:?} posix={posix} hash={hash} n={n} out={out:?}");
        // 1. live file is old/new/(missing mid-swap on non-posix crash)
        let t = fs.get(real);
        let ok = t.as_deref() == Some(NEW) || t.as_deref() == old || (t.is_none() && (crashed || old.is_none()));
        assert!(ok, "partial/mixed live file: {tag}");
        if t.is_none() && old.is_some() {
            assert!(!posix, "missing target only allowed in 3-step swap: {tag}");
        }
        // 2. buffer never lost
        let entry = e.journal.get(&doc.id).unwrap();
        if let Some(en) = &entry {
            assert_eq!(e.journal.snapshot(&en.id).unwrap(), NEW, "{tag}");
        } else {
            assert_eq!(fs.get(real).as_deref(), Some(NEW), "no journal => already on server: {tag}");
        }
        // 3. non-crash faults: orphans are all known to the journal
        if !crashed {
            for p in fs.paths() {
                if p != real && p != T {
                    let en = entry.as_ref().expect(&tag);
                    assert!(Some(&p) == en.temp.as_ref() || Some(&p) == en.old.as_ref() || en.orphans.contains(&p), "unrecorded orphan {p}: {tag}");
                }
            }
        }
        // 4. next launch
        fs.revive();
        let rep = SafeSaver::new(&mut fs, &e.journal, &e.backups).recover_all();
        for (_, r) in &rep {
            assert!(r.is_ok(), "{tag}");
        }
        assert!(only_target(&fs, real), "orphan after recovery: {:?} {tag}", fs.paths());
        let t = fs.get(real);
        assert!(t.as_deref() == Some(NEW) || t.as_deref() == old, "after recovery: {tag}");
        match e.journal.get(&doc.id).unwrap() {
            None => assert_eq!(t.as_deref(), Some(NEW), "{tag}"),
            Some(en) => {
                assert_eq!(en.state, SyncState::Recovered, "{tag}");
                assert_eq!(e.journal.snapshot(&en.id).unwrap(), NEW, "{tag}");
            }
        }
        if matches!(sc, Scenario::Replace) && t.as_deref() == Some(NEW) {
            assert_eq!(fs.mode(real), Some(0o640), "mode preserved: {tag}");
        }
    }
    panic!("save never completed");
}

#[test]
fn crash_loop_every_step_every_fault_every_capability() {
    let mut total = 0;
    for posix in [true, false] {
        for hash in [true, false] {
            for sc in [Scenario::Replace, Scenario::Create, Scenario::Symlink] {
                let kinds: [(&str, Box<dyn Fn() -> FaultKind>); 7] = [
                    ("before", Box::new(|| FaultKind::CrashBefore)),
                    ("after", Box::new(|| FaultKind::CrashAfter)),
                    ("mid", Box::new(|| FaultKind::CrashMid)),
                    ("transport", Box::new(|| FaultKind::Fail(FsError::Transport("reset".into())))),
                    ("nospace", Box::new(|| FaultKind::Fail(FsError::NoSpace))),
                    ("denied", Box::new(|| FaultKind::Fail(FsError::PermissionDenied))),
                    ("other", Box::new(|| FaultKind::Fail(FsError::Other("boom".into())))),
                ];
                for (_, k) in kinds.iter() {
                    total += run_loop(posix, hash, k.as_ref(), sc);
                }
            }
        }
    }
    println!("fault points exercised: {total}");
    assert!(total > 500, "loop too shallow: {total}");
}

#[test]
fn concurrent_reader_never_sees_partial_content() {
    for posix in [true, false] {
        let e = env();
        let mut fs = FakeFs::new(caps(posix, false));
        fs.put_file(T, OLD, 0o644);
        let (_, mut doc) = RemoteDoc::open(&mut fs, "p", T).unwrap();
        let reader = fs.clone();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let s2 = stop.clone();
        let h = std::thread::spawn(move || {
            let mut seen_new = false;
            while !s2.load(std::sync::atomic::Ordering::Relaxed) {
                match reader.get(T) {
                    Some(b) if b == OLD => {}
                    Some(b) if b == NEW => seen_new = true,
                    None => assert!(!posix, "file vanished with posix rename"),
                    Some(b) => panic!("mixed/truncated content: {} bytes", b.len()),
                }
            }
            seen_new
        });
        let out = SafeSaver::new(&mut fs, &e.journal, &e.backups).save(&mut doc, NEW, SaveOptions::default());
        assert!(matches!(out, SaveOutcome::Saved { non_atomic, .. } if non_atomic == !posix));
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        h.join().unwrap();
    }
}

#[test]
fn transport_failure_queues_then_retry_succeeds() {
    let e = env();
    let mut fs = FakeFs::new(caps(true, false));
    fs.put_file(T, OLD, 0o644);
    let (_, mut doc) = RemoteDoc::open(&mut fs, "p", T).unwrap();
    fs.set_fault(Some(Fault { at: 0, kind: FaultKind::Fail(FsError::Transport("down".into())) }));
    let out = SafeSaver::new(&mut fs, &e.journal, &e.backups).save(&mut doc, NEW, SaveOptions::default());
    assert!(matches!(out, SaveOutcome::Queued { .. }));
    assert_eq!(doc.state, SyncState::Queued);
    assert_eq!(fs.get(T).as_deref(), Some(OLD));
    let out = SafeSaver::new(&mut fs, &e.journal, &e.backups).retry(&mut doc, SaveOptions::default());
    assert!(matches!(out, SaveOutcome::Saved { .. }));
    assert_eq!(fs.get(T).as_deref(), Some(NEW));
    assert!(e.journal.list().is_empty());
}

/// Deterministic pseudo-random sequence of saves, external writes and faults:
/// the live file only ever holds a complete content that some party wrote.
#[test]
fn random_sequences_keep_server_consistent() {
    let mut seed = 0x2545F4914F6CDD1Du64;
    let mut rnd = move |m: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % m
    };
    for round in 0..60 {
        let e = env();
        let posix = rnd(2) == 0;
        let mut fs = FakeFs::new(caps(posix, rnd(2) == 0));
        fs.put_file(T, b"v0", 0o600);
        let mut known: Vec<Vec<u8>> = vec![b"v0".to_vec()];
        let (_, mut doc) = RemoteDoc::open(&mut fs, "p", T).unwrap();
        for step in 0..12 {
            match rnd(4) {
                0 => {
                    let b = format!("ext-{round}-{step}").into_bytes();
                    fs.external_write(T, &b);
                    known.push(b);
                }
                _ => {
                    let b = format!("local-{round}-{step}-{}", "x".repeat(rnd(50) as usize)).into_bytes();
                    known.push(b.clone());
                    if rnd(2) == 0 {
                        fs.set_fault(Some(Fault { at: rnd(14) as usize, kind: match rnd(4) {
                            0 => FaultKind::CrashBefore,
                            1 => FaultKind::CrashAfter,
                            2 => FaultKind::CrashMid,
                            _ => FaultKind::Fail(FsError::Transport("x".into())),
                        } }));
                    } else {
                        fs.set_fault(None);
                    }
                    let out = SafeSaver::new(&mut fs, &e.journal, &e.backups).save(&mut doc, &b, SaveOptions::default());
                    let crashed = matches!(out, SaveOutcome::Failed(Fail::Crashed));
                    fs.revive(); // also clears any pending fault
                    if crashed {
                        SafeSaver::new(&mut fs, &e.journal, &e.backups).recover_all();
                        let (_, d) = RemoteDoc::open(&mut fs, "p", T).unwrap();
                        doc = d;
                    } else if let SaveOutcome::Conflict { .. } | SaveOutcome::Stale { .. } = out {
                        let r = SafeSaver::new(&mut fs, &e.journal, &e.backups)
                            .resolve_conflict(&mut doc, &b, ConflictChoice::Overwrite);
                        assert!(r.is_ok());
                    }
                    fs.set_fault(None);
                }
            }
            if let Some(cur) = fs.get(T) {
                assert!(known.contains(&cur), "round {round} step {step}: unknown content {:?}", String::from_utf8_lossy(&cur));
            } else {
                panic!("live file missing outside a crash window");
            }
            assert!(fs.paths().iter().all(|p| p == T || e.journal.list().iter().any(|en| en.temp.as_ref() == Some(p) || en.old.as_ref() == Some(p) || en.orphans.contains(p))), "unrecorded orphan {:?}", fs.paths());
        }
    }
}
