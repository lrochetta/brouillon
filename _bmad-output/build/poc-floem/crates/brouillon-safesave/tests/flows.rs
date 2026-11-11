//! Conflict, roundtrip, symlink, backup, journal and in-place behaviours.

use brouillon_safesave::fake::FakeFs;
use brouillon_safesave::*;

const T: &str = "/w/a.txt";

struct Env {
    d: tempfile::TempDir,
    journal: Journal,
    backups: BackupStore,
}
fn env() -> Env {
    let d = tempfile::tempdir().unwrap();
    let journal = Journal::new(d.path().join("journal"));
    let backups = BackupStore::new(d.path().join("backups"));
    Env { d, journal, backups }
}
fn fs_with(data: &[u8]) -> FakeFs {
    let fs = FakeFs::new(Caps { posix_rename: true, hash: false, set_mode: true });
    fs.put_file(T, data, 0o644);
    fs
}

#[test]
fn bytes_roundtrip_exactly() {
    for data in [
        &b"\xEF\xBB\xBFa\r\nb\r\nc"[..],
        &b"\xFF\xFEa\0b\0\r\0\n\0"[..],
        &b"mixed\r\nlf\nnone"[..],
        &b""[..],
        &[0u8, 255, 10, 13, 13, 10][..],
    ] {
        let e = env();
        let mut fs = fs_with(b"x");
        let (_, mut doc) = RemoteDoc::open(&mut fs, "p", T).unwrap();
        let out = SafeSaver::new(&mut fs, &e.journal, &e.backups).save(&mut doc, data, SaveOptions::default());
        assert!(matches!(out, SaveOutcome::Saved { .. }));
        assert_eq!(fs.get(T).unwrap(), data);
        let (back, d2) = RemoteDoc::open(&mut fs, "p", T).unwrap();
        assert_eq!(back, data);
        assert_eq!(d2.meta, FileMeta::detect(data));
    }
    let m = FileMeta::detect(b"\xEF\xBB\xBFa\r\nb\r\nc");
    assert!(m.bom && m.eol == Eol::CrLf && !m.final_newline && !m.mixed_eol);
}

#[test]
fn conflict_writes_nothing_and_every_choice_works() {
    let e = env();
    let mut fs = fs_with(b"base\n");
    let (_, mut doc) = RemoteDoc::open(&mut fs, "p", T).unwrap();
    fs.external_write(T, b"theirs\n");
    fs.clear_log();
    let out = SafeSaver::new(&mut fs, &e.journal, &e.backups).save(&mut doc, b"mine\n", SaveOptions::default());
    assert!(matches!(out, SaveOutcome::Conflict { .. }));
    assert_eq!(doc.state, SyncState::Conflict);
    assert!(fs.mutation_log().is_empty(), "precondition failure must write zero bytes");
    assert_eq!(fs.get(T).unwrap(), b"theirs\n");

    let mut s = SafeSaver::new(&mut fs, &e.journal, &e.backups);
    match s.resolve_conflict(&mut doc, b"mine\n", ConflictChoice::Diff).unwrap() {
        Resolution::Diff { base, local, remote } => {
            assert_eq!((base, local, remote), (b"base\n".to_vec(), b"mine\n".to_vec(), b"theirs\n".to_vec()));
        }
        r => panic!("{r:?}"),
    }
    assert!(matches!(s.resolve_conflict(&mut doc, b"mine\n", ConflictChoice::Cancel).unwrap(), Resolution::Cancelled));
    // SaveAs
    match s.resolve_conflict(&mut doc, b"mine\n", ConflictChoice::SaveAs { path: "/w/b.txt".into() }).unwrap() {
        Resolution::SavedAs(nd, SaveOutcome::Saved { .. }) => assert_eq!(nd.path, "/w/b.txt"),
        r => panic!("{r:?}"),
    }
    assert_eq!(fs.get("/w/b.txt").unwrap(), b"mine\n");
    assert_eq!(fs.get(T).unwrap(), b"theirs\n");

    // Overwrite: the real remote bytes are backed up first
    let mut s = SafeSaver::new(&mut fs, &e.journal, &e.backups);
    assert!(matches!(
        s.resolve_conflict(&mut doc, b"mine\n", ConflictChoice::Overwrite).unwrap(),
        Resolution::Saved(SaveOutcome::Saved { .. })
    ));
    assert_eq!(fs.get(T).unwrap(), b"mine\n");
    let vs = e.backups.list("p", T);
    assert!(vs.iter().any(|v| v.source == "overwrite" && e.backups.read(v).unwrap() == b"theirs\n"));

    // Reload: local buffer lands in discarded/
    fs.external_write(T, b"third\n");
    let out = SafeSaver::new(&mut fs, &e.journal, &e.backups).save(&mut doc, b"mine2\n", SaveOptions::default());
    assert!(matches!(out, SaveOutcome::Conflict { .. }));
    match SafeSaver::new(&mut fs, &e.journal, &e.backups).resolve_conflict(&mut doc, b"mine2\n", ConflictChoice::Reload).unwrap() {
        Resolution::Reloaded { bytes } => assert_eq!(bytes, b"third\n"),
        r => panic!("{r:?}"),
    }
    assert_eq!(doc.state, SyncState::Clean);
    let n = std::fs::read_dir(e.d.path().join("journal/discarded")).unwrap().count();
    assert_eq!(n, 2);
    assert!(e.journal.list().is_empty());
}

#[test]
fn merge_rebases_on_remote_without_uploading() {
    let e = env();
    let mut fs = fs_with(b"base\n");
    let (_, mut doc) = RemoteDoc::open(&mut fs, "p", T).unwrap();
    fs.external_write(T, b"theirs\n");
    fs.clear_log();
    let r = SafeSaver::new(&mut fs, &e.journal, &e.backups)
        .resolve_conflict(&mut doc, b"mine\n", ConflictChoice::Merge { merged: b"both\n".to_vec() })
        .unwrap();
    assert!(matches!(r, Resolution::Rebased { buffer } if buffer == b"both\n"));
    assert!(fs.mutation_log().is_empty());
    let out = SafeSaver::new(&mut fs, &e.journal, &e.backups).save(&mut doc, b"both\n", SaveOptions::default());
    assert!(matches!(out, SaveOutcome::Saved { .. }));
}

#[test]
fn touch_only_change_is_not_a_conflict_and_unchanged_buffer_is_stale() {
    let e = env();
    let mut fs = fs_with(b"same\n");
    let (_, mut doc) = RemoteDoc::open(&mut fs, "p", T).unwrap();
    fs.external_write(T, b"same\n"); // new mtime, identical bytes
    let out = SafeSaver::new(&mut fs, &e.journal, &e.backups).save(&mut doc, b"edited\n", SaveOptions::default());
    assert!(matches!(out, SaveOutcome::Saved { .. }), "{out:?}");

    fs.external_write(T, b"remote newer\n");
    let out = SafeSaver::new(&mut fs, &e.journal, &e.backups).save(&mut doc, b"edited\n", SaveOptions::default());
    assert!(matches!(out, SaveOutcome::Stale { .. }), "buffer == base => stale: {out:?}");
}

#[test]
fn remote_gone_then_recreate_with_base_mode() {
    let e = env();
    let mut fs = fs_with(b"x");
    fs.put_file(T, b"x", 0o640);
    let (_, mut doc) = RemoteDoc::open(&mut fs, "p", T).unwrap();
    fs.external_remove(T);
    let out = SafeSaver::new(&mut fs, &e.journal, &e.backups).save(&mut doc, b"y", SaveOptions::default());
    assert_eq!(out, SaveOutcome::RemoteGone);
    let out = SafeSaver::new(&mut fs, &e.journal, &e.backups).save(&mut doc, b"y", SaveOptions { allow_create: true, ..Default::default() });
    assert!(matches!(out, SaveOutcome::Saved { .. }));
    assert_eq!(fs.mode(T), Some(0o640));
}

#[test]
fn symlink_is_kept_and_dangling_refused() {
    let e = env();
    let mut fs = FakeFs::new(Caps { posix_rename: false, hash: true, set_mode: true });
    fs.put_file("/w/real", b"r", 0o600);
    fs.put_symlink("/w/link", "/w/real");
    let (_, mut doc) = RemoteDoc::open(&mut fs, "p", "/w/link").unwrap();
    let out = SafeSaver::new(&mut fs, &e.journal, &e.backups).save(&mut doc, b"new", SaveOptions::default());
    assert_eq!(out, SaveOutcome::Saved { verified: Verification::Hash, non_atomic: true });
    assert!(fs.is_symlink("/w/link"));
    assert_eq!(fs.get("/w/real").unwrap(), b"new");
    assert_eq!(fs.mode("/w/real"), Some(0o600));
    fs.external_remove("/w/real");
    let out = SafeSaver::new(&mut fs, &e.journal, &e.backups).save(&mut doc, b"again", SaveOptions::default());
    assert_eq!(out, SaveOutcome::Failed(Fail::DanglingLink));
}

#[test]
fn mode_change_on_server_is_reported_not_overwritten() {
    let e = env();
    let mut fs = fs_with(b"x");
    let (_, mut doc) = RemoteDoc::open(&mut fs, "p", T).unwrap();
    // chmod only: same size and mtime
    let mut f2 = fs.clone();
    f2.set_mode(T, 0o600).unwrap();
    let out = SafeSaver::new(&mut fs, &e.journal, &e.backups).save(&mut doc, b"y", SaveOptions::default());
    assert_eq!(out, SaveOutcome::Failed(Fail::PermissionsChanged));
    assert_eq!(fs.get(T).unwrap(), b"x");
}

#[test]
fn forced_in_place_backs_up_first() {
    let e = env();
    let mut fs = fs_with(b"old");
    let (_, mut doc) = RemoteDoc::open(&mut fs, "p", T).unwrap();
    let out = SafeSaver::new(&mut fs, &e.journal, &e.backups).save(&mut doc, b"new", SaveOptions { force_in_place: true, ..Default::default() });
    assert!(matches!(out, SaveOutcome::Saved { non_atomic: true, .. }));
    assert_eq!(fs.get(T).unwrap(), b"new");
    assert!(e.backups.list("p", T).iter().any(|v| e.backups.read(v).unwrap() == b"old"));
    assert!(fs.mutation_log().iter().any(|l| l.starts_with("write_truncate")));
}

#[test]
fn in_place_crash_keeps_backup_and_snapshot() {
    use brouillon_safesave::fake::{Fault, FaultKind};
    let e = env();
    let mut fs = fs_with(b"old content");
    let (_, mut doc) = RemoteDoc::open(&mut fs, "p", T).unwrap();
    // ops: stat, stat(real), read(backup)... find the truncate by trying CrashMid on increasing n
    for n in 0..10 {
        let mut fs = fs_with(b"old content");
        let (_, mut d) = RemoteDoc::open(&mut fs, "p", T).unwrap();
        fs.set_fault(Some(Fault { at: n, kind: FaultKind::CrashMid }));
        SafeSaver::new(&mut fs, &e.journal, &e.backups).save(&mut d, b"brand new content", SaveOptions { force_in_place: true, ..Default::default() });
        if fs.get(T).is_some_and(|b| b != b"old content" && b != b"brand new content") {
            fs.revive();
            let rep = SafeSaver::new(&mut fs, &e.journal, &e.backups).recover(&d.id).unwrap().unwrap();
            assert!(rep.in_place_unverified);
            assert_eq!(e.journal.snapshot(&d.id).unwrap(), b"brand new content");
            assert!(e.backups.list("p", T).iter().any(|v| e.backups.read(v).unwrap() == b"old content"));
            return;
        }
    }
    let _ = &mut doc;
    panic!("never produced a truncated in-place target");
}

#[test]
fn backup_retention_and_rollback_is_a_normal_save() {
    let e = env();
    let mut st = BackupStore::new(e.d.path().join("b2"));
    st.keep_versions = 5;
    let day = 24 * 3600 * 1000u64;
    for i in 0..8u64 {
        st.put_at("prof/ile", T, format!("v{i}").as_bytes(), BackupSource::BeforeSave, 1_000_000 + i).unwrap();
    }
    let l = st.list("prof/ile", T);
    assert_eq!(l.len(), 5);
    assert_eq!(st.read(&l[0]).unwrap(), b"v7");
    // identical content is not duplicated
    st.put_at("prof/ile", T, b"v7", BackupSource::BeforeSave, 1_000_100).unwrap();
    assert_eq!(st.list("prof/ile", T).len(), 5);
    // age: everything old goes except the newest
    st.prune("prof/ile", T, 1_000_000 + 40 * day);
    assert_eq!(st.list("prof/ile", T).len(), 1);

    // rollback through the state machine
    let mut fs = fs_with(b"v0");
    let (_, mut doc) = RemoteDoc::open(&mut fs, "p", T).unwrap();
    let mut s = SafeSaver::new(&mut fs, &e.journal, &e.backups);
    assert!(matches!(s.save(&mut doc, b"v1", SaveOptions::default()), SaveOutcome::Saved { .. }));
    let v = e.backups.list("p", T).remove(0);
    let old = e.backups.read(&v).unwrap();
    assert_eq!(old, b"v0");
    assert!(matches!(s.save(&mut doc, &old, SaveOptions::default()), SaveOutcome::Saved { .. }));
    assert_eq!(fs.get(T).unwrap(), b"v0");
    assert!(e.backups.list("p", T).iter().any(|v| e.backups.read(v).unwrap() == b"v1"));
}

#[test]
fn journal_roundtrip_discard_and_sweep() {
    let e = env();
    let mut fs = fs_with(b"x");
    let (_, mut doc) = RemoteDoc::open(&mut fs, "p", T).unwrap();
    let mut f2 = fs.clone();
    use brouillon_safesave::fake::{Fault, FaultKind};
    f2.set_fault(Some(Fault { at: 0, kind: FaultKind::Fail(FsError::Transport("t".into())) }));
    let out = SafeSaver::new(&mut f2, &e.journal, &e.backups).save(&mut doc, b"unsaved", SaveOptions::default());
    assert!(matches!(out, SaveOutcome::Queued { .. }));
    let en = e.journal.get(&doc.id).unwrap().unwrap();
    assert_eq!(en.state, SyncState::Queued);
    assert_eq!(e.journal.snapshot(&doc.id).unwrap(), b"unsaved");
    assert!(!std::fs::read_to_string(e.d.path().join(format!("journal/{}.json", doc.id))).unwrap().to_lowercase().contains("password"));
    // recovery on a Queued entry: no server work, becomes Recovered, buffer intact, nothing uploaded
    fs.clear_log();
    SafeSaver::new(&mut fs, &e.journal, &e.backups).recover_all();
    assert!(fs.mutation_log().is_empty());
    assert_eq!(e.journal.get(&doc.id).unwrap().unwrap().state, SyncState::Recovered);
    e.journal.discard(&doc.id).unwrap();
    assert!(e.journal.list().is_empty());
    e.journal.sweep_discarded(u64::MAX / 2);
    assert_eq!(std::fs::read_dir(e.d.path().join("journal/discarded")).unwrap().count(), 0);
}
