//! Operations against the in-process fake SFTP server, including hostile-server cases.

use brouillon_remote::ops::{Session, WriteParams};
use brouillon_remote::sftp::{ClientConfig, SftpClient, SftpError};
use brouillon_remote::testkit::*;
use brouillon_remote_proto::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

fn p(s: &str) -> RemotePath {
    RemotePath::parse(s).unwrap()
}

fn cfg(idle_ms: u64) -> ClientConfig {
    ClientConfig { idle_timeout: Duration::from_millis(idle_ms), handshake_timeout: Duration::from_secs(5), window: 8, chunk: 4096, ..ClientConfig::default() }
}

fn session_with(fs: FakeFs, faults: Faults, root: Option<&str>, idle_ms: u64, limits: Limits) -> (Session, FakeHandle) {
    let ((r, w), h) = spawn_fake(fs, faults);
    let client = SftpClient::connect(r, w, cfg(idle_ms)).expect("handshake");
    let (s, _home) = Session::open(client, root.map(p), limits).expect("open");
    (s, h)
}

fn session(fs: FakeFs, faults: Faults) -> (Session, FakeHandle) {
    session_with(fs, faults, None, 3000, Limits::default())
}

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

fn pattern(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i.wrapping_mul(31) % 251) as u8).collect()
}

fn read_all(s: &Session, path: &str) -> Result<Vec<u8>, RemoteError> {
    let mut out = Vec::new();
    let c = no_cancel();
    s.read(&p(path), 0, None, None, false, &c, |off, d| {
        assert_eq!(off as usize, out.len(), "chunks must arrive in order");
        out.extend_from_slice(d);
        Ok(())
    })?;
    Ok(out)
}

fn write_all(s: &Session, path: &str, data: &[u8], create: CreateMode, expect: Option<Expect>) -> Result<WriteResult, RemoteError> {
    let wp = WriteParams { path: p(path), size: data.len() as u64, create, perm: Some(0o640), expect, fsync: true, allow_symlink: false };
    let mut chunks = data.chunks(10_000).map(|c| c.to_vec()).collect::<Vec<_>>().into_iter();
    s.write(&wp, &no_cancel(), || Ok(chunks.next()), |_| {})
}

// ------------------------------------------------------------------ listing

#[test]
fn lists_a_directory_with_kinds_and_without_dot_entries() {
    let mut fs = FakeFs::new();
    fs.dir("/home/u/d").file("/home/u/a.txt", b"hello").link("/home/u/ln", "a.txt");
    let (s, _h) = session(fs, Faults::default());
    let mut all = vec![];
    let r = s.list_dir(&p("/home/u"), &no_cancel(), |e| all.extend(e)).unwrap();
    let names: Vec<_> = all.iter().map(|e| e.name.as_wire_str().to_string()).collect();
    assert_eq!(names, ["a.txt", "d", "ln"]);
    assert_eq!(r.count, 3);
    assert!(!r.truncated && r.hidden_unsafe == 0);
    let kind = |n: &str| all.iter().find(|e| e.name.as_wire_str() == n).unwrap().kind;
    assert_eq!(kind("a.txt"), EntryKind::File);
    assert_eq!(kind("d"), EntryKind::Dir);
    assert_eq!(kind("ln"), EntryKind::Symlink);
    let a = all.iter().find(|e| e.name.as_wire_str() == "a.txt").unwrap();
    assert_eq!((a.size, a.mtime, a.mode), (Some(5), Some(1000), Some(0o644)));
}

#[test]
fn hostile_names_are_hidden_and_counted() {
    let mut fs = FakeFs::new();
    fs.file("/home/u/ok.txt", b"x").file("/home/u/dup", b"x");
    let faults = Faults {
        extra_names: vec![
            b"../etc/passwd".to_vec(),
            b"/etc/passwd".to_vec(),
            b"a/b".to_vec(),
            b"nul\0byte".to_vec(),
            "evil\u{202e}gpj.exe".as_bytes().to_vec(),
            "zero\u{200b}width".as_bytes().to_vec(),
            b"ctl\x07bell".to_vec(),
            vec![b'x'; 300],
            b"..".to_vec(), // dropped silently as a dot entry
            b"".to_vec(),
            vec![0xff, 0xfe, 0xfd], // not UTF-8
            b"dup".to_vec(),        // duplicate of a real entry
            b"con".to_vec(),        // legal remotely, flagged local_unsafe
            b"a:b".to_vec(),
        ],
        ..Faults::default()
    };
    let (s, _h) = session(fs, faults);
    let mut all = vec![];
    let r = s.list_dir(&p("/home/u"), &no_cancel(), |e| all.extend(e)).unwrap();
    let names: Vec<_> = all.iter().map(|e| e.name.as_wire_str().to_string()).collect();
    assert_eq!(names.len(), 4, "{names:?}");
    for good in ["ok.txt", "dup", "con", "a:b"] {
        assert!(names.iter().any(|n| n == good), "{good}");
    }
    assert!(names.iter().all(|n| !n.contains('/') && !n.contains('\0') && n != ".."));
    assert!(all.iter().find(|e| e.name.as_wire_str() == "con").unwrap().local_unsafe);
    assert!(all.iter().find(|e| e.name.as_wire_str() == "a:b").unwrap().local_unsafe);
    assert!(!all.iter().find(|e| e.name.as_wire_str() == "ok.txt").unwrap().local_unsafe);
    // 14 hostile names: ".." is dropped as a dot entry, `con` and `a:b` are shown (flagged),
    // the other 11 are hidden (incl. empty, non-UTF-8, duplicate)
    assert_eq!(r.hidden_unsafe, 11);
}

#[test]
fn huge_listing_is_capped_and_paged() {
    let mut fs = FakeFs::new();
    fs.dir("/huge");
    let faults = Faults { huge_listing: 60_000, ..Faults::default() };
    let (s, _h) = session(fs, faults);
    let mut pages = vec![];
    let r = s.list_dir(&p("/huge"), &no_cancel(), |e| pages.push(e.len())).unwrap();
    assert_eq!(r.count, 50_000);
    assert!(r.truncated);
    assert!(pages.iter().all(|&n| n <= 500) && pages.iter().sum::<usize>() == 50_000);
}

#[test]
fn listing_byte_cap_truncates() {
    let mut fs = FakeFs::new();
    fs.dir("/huge");
    let limits = Limits { max_list_bytes: 10_000, ..Limits::default() };
    let faults = Faults { huge_listing: 5_000, ..Faults::default() };
    let (s, _h) = session_with(fs, faults, None, 3000, limits);
    let r = s.list_dir(&p("/huge"), &no_cancel(), |_| {}).unwrap();
    assert!(r.truncated && r.count < 200);
}

#[test]
fn lying_name_count_is_a_protocol_error_not_an_allocation() {
    let mut fs = FakeFs::new();
    fs.file("/home/u/a", b"1");
    let (s, _h) = session(fs, Faults { lie_name_count: true, ..Faults::default() });
    let e = s.list_dir(&p("/home/u"), &no_cancel(), |_| {}).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Protocol);
}

#[test]
fn listing_a_missing_dir_or_a_file() {
    let mut fs = FakeFs::new();
    fs.file("/home/u/f", b"1");
    let (s, _h) = session(fs, Faults::default());
    assert_eq!(s.list_dir(&p("/nope"), &no_cancel(), |_| {}).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(s.list_dir(&p("/home/u/f"), &no_cancel(), |_| {}).unwrap_err().kind, ErrorKind::Failure);
}

// ------------------------------------------------------------------ read

#[test]
fn reads_pipelined_in_order() {
    let data = pattern(150_000);
    let mut fs = FakeFs::new();
    fs.file("/home/u/big", &data);
    let (s, _h) = session(fs, Faults::default());
    assert_eq!(read_all(&s, "/home/u/big").unwrap(), data);
}

#[test]
fn short_reads_are_completed() {
    let data = pattern(50_000);
    let mut fs = FakeFs::new();
    fs.file("/home/u/f", &data);
    let (s, _h) = session(fs, Faults { max_read_reply: Some(1000), ..Faults::default() });
    assert_eq!(read_all(&s, "/home/u/f").unwrap(), data);
}

#[test]
fn ranged_read_and_eof_flag() {
    let data = pattern(10_000);
    let mut fs = FakeFs::new();
    fs.file("/home/u/f", &data);
    let (s, _h) = session(fs, Faults::default());
    let mut out = vec![];
    let r = s.read(&p("/home/u/f"), 100, Some(500), None, false, &no_cancel(), |_, d| {
        out.extend_from_slice(d);
        Ok(())
    }).unwrap();
    assert_eq!(out, &data[100..600]);
    assert_eq!((r.bytes, r.eof), (500, false));
    let r = s.read(&p("/home/u/f"), 9_000, Some(5_000), None, false, &no_cancel(), |_, _| Ok(())).unwrap();
    assert_eq!((r.bytes, r.eof), (1_000, true));
}

#[test]
fn endless_stream_stops_at_the_cap() {
    let mut fs = FakeFs::new();
    fs.file("/endless", b"x");
    let limits = Limits { max_read_bytes: 100_000, ..Limits::default() };
    let (s, _h) = session_with(fs, Faults { endless: true, ..Faults::default() }, None, 3000, limits);
    let mut total = 0usize;
    let e = s
        .read(&p("/endless"), 0, None, None, false, &no_cancel(), |_, d| {
            total += d.len();
            Ok(())
        })
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::TooLarge);
    assert!(total <= 100_000, "never delivers past the cap, got {total}");
}

#[test]
fn a_lying_stat_size_changes_nothing() {
    let data = pattern(3000);
    let mut fs = FakeFs::new();
    fs.file("/home/u/f", &data);
    let (s, _h) = session(fs, Faults { lie_size: Some(5), ..Faults::default() });
    assert_eq!(s.stat(&p("/home/u/f"), false).unwrap().size, Some(5)); // reported as is...
    assert_eq!(read_all(&s, "/home/u/f").unwrap(), data); // ...but reads count real bytes
    let e = s.read(&p("/home/u/f"), 0, None, Some(1000), false, &no_cancel(), |_, _| Ok(())).unwrap_err();
    assert_eq!(e.kind, ErrorKind::TooLarge);
}

#[test]
fn reply_longer_than_requested_is_a_protocol_error() {
    let mut fs = FakeFs::new();
    fs.file("/home/u/f", &pattern(100));
    let (s, _h) = session(fs, Faults { read_returns_more: true, ..Faults::default() });
    assert_eq!(read_all(&s, "/home/u/f").unwrap_err().kind, ErrorKind::Protocol);
}

#[test]
fn empty_data_reply_cannot_loop_forever() {
    let mut fs = FakeFs::new();
    fs.file("/home/u/f", &pattern(100));
    let (s, _h) = session(fs, Faults { zero_data: true, ..Faults::default() });
    assert_eq!(read_all(&s, "/home/u/f").unwrap_err().kind, ErrorKind::Protocol);
}

#[test]
fn oversized_packet_kills_the_connection() {
    let mut fs = FakeFs::new();
    fs.file("/home/u/f", &pattern(100));
    let (s, _h) = session(fs, Faults { oversize_reply_on_read: true, ..Faults::default() });
    assert_eq!(read_all(&s, "/home/u/f").unwrap_err().kind, ErrorKind::Protocol);
    // the session is poisoned: further calls fail fast, they do not hang
    assert!(s.client.is_dead());
    assert!(s.stat(&p("/home/u"), false).is_err());
}

#[test]
fn reply_with_a_wrong_id_kills_the_connection() {
    let (s, _h) = session(FakeFs::new(), Faults { wrong_id_once: true, ..Faults::default() });
    let e = s.stat(&p("/home/u"), false).unwrap_err();
    assert!(matches!(e.kind, ErrorKind::Protocol | ErrorKind::ConnectionLost), "{e:?}");
    assert!(s.client.is_dead());
}

#[test]
fn unsolicited_reply_is_a_protocol_violation() {
    let ((r, w), _h) = spawn_fake(FakeFs::new(), Faults { unsolicited: true, ..Faults::default() });
    let client = SftpClient::connect(r, w, cfg(2000)).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert!(client.is_dead());
    assert!(matches!(client.dead_reason(), Some(SftpError::Protocol(_))));
}

#[test]
fn wrong_protocol_version_is_refused() {
    let ((r, w), _h) = spawn_fake(FakeFs::new(), Faults { version: 6, ..Faults::default() });
    assert!(matches!(SftpClient::connect(r, w, cfg(2000)), Err(SftpError::Protocol(_))));
}

#[test]
fn oversized_handle_is_refused() {
    let mut fs = FakeFs::new();
    fs.file("/home/u/f", b"x");
    let (s, _h) = session(fs, Faults { handle_len: 1000, ..Faults::default() });
    assert_eq!(read_all(&s, "/home/u/f").unwrap_err().kind, ErrorKind::Protocol);
}

#[test]
fn slow_server_times_out_instead_of_hanging() {
    let mut fs = FakeFs::new();
    fs.file("/home/u/f", b"x");
    let faults = Faults { reply_delay: Duration::from_millis(600), ..Faults::default() };
    let ((r, w), _h) = spawn_fake(fs, faults);
    let client = SftpClient::connect(r, w, cfg(150)).unwrap();
    let (s, _) = match Session::open(client, None, Limits::default()) {
        Ok(x) => x,
        Err(e) => {
            assert_eq!(e.kind, ErrorKind::Timeout);
            return;
        }
    };
    let e = s.stat(&p("/home/u/f"), false).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Timeout);
}

#[test]
fn trickling_server_trips_the_throughput_watchdog() {
    let mut fs = FakeFs::new();
    fs.file("/home/u/f", &pattern(200_000));
    let limits = Limits { min_rate_window_s: 1, min_rate_bytes: 50_000_000, ..Limits::default() };
    let faults = Faults { reply_delay: Duration::from_millis(60), max_read_reply: Some(2000), ..Faults::default() };
    let (s, _h) = session_with(fs, faults, None, 5000, limits);
    let e = s.read(&p("/home/u/f"), 0, None, None, false, &no_cancel(), |_, _| Ok(())).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Timeout);
}

#[test]
fn cancel_flag_stops_a_read() {
    let mut fs = FakeFs::new();
    fs.file("/home/u/f", &pattern(200_000));
    let (s, _h) = session(fs, Faults::default());
    let c = AtomicBool::new(false);
    let mut chunks = 0;
    let e = s
        .read(&p("/home/u/f"), 0, None, None, false, &c, |_, _| {
            chunks += 1;
            if chunks == 3 {
                c.store(true, Ordering::Relaxed);
            }
            Ok(())
        })
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Cancelled);
    assert!(chunks < 10);
}

// ------------------------------------------------------------------ write

#[test]
fn write_new_then_conflicts() {
    let (s, h) = session(FakeFs::new(), Faults::default());
    let data = pattern(70_000);
    let r = write_all(&s, "/home/u/new.txt", &data, CreateMode::New, None).unwrap();
    assert_eq!(r.size, 70_000);
    assert!(r.fsynced);
    assert_eq!(r.mtime, Some(3000));
    assert_eq!(h.fs.lock().unwrap().data("/home/u/new.txt").unwrap(), data);
    // O_EXCL: a second New fails with AlreadyExists
    assert_eq!(write_all(&s, "/home/u/new.txt", b"x", CreateMode::New, None).unwrap_err().kind, ErrorKind::AlreadyExists);
    // expect matches -> ok; expect stale -> Conflict and nothing written
    let st = s.stat(&p("/home/u/new.txt"), false).unwrap();
    let ok = Expect { size: st.size.unwrap(), mtime: st.mtime.unwrap() };
    write_all(&s, "/home/u/new.txt", b"second", CreateMode::Truncate, Some(ok)).unwrap();
    assert_eq!(h.fs.lock().unwrap().data("/home/u/new.txt").unwrap(), b"second");
    let stale = Expect { size: 1, mtime: 1 };
    let before = h.log.lock().unwrap().len();
    assert_eq!(write_all(&s, "/home/u/new.txt", b"third", CreateMode::Truncate, Some(stale)).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(h.log.lock().unwrap().len(), before, "a conflict must not touch the server");
    // expect on a vanished file is a conflict as well
    assert_eq!(write_all(&s, "/home/u/gone", b"x", CreateMode::Truncate, Some(stale)).unwrap_err().kind, ErrorKind::Conflict);
}

#[test]
fn write_size_mismatch_is_refused() {
    let (s, _h) = session(FakeFs::new(), Faults::default());
    let wp = WriteParams { path: p("/home/u/f"), size: 10, create: CreateMode::New, perm: None, expect: None, fsync: false, allow_symlink: false };
    let mut once = Some(vec![1u8; 4]);
    let e = s.write(&wp, &no_cancel(), || Ok(once.take()), |_| {}).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Protocol);
    let wp = WriteParams { size: 2, path: p("/home/u/g"), ..wp };
    let mut once = Some(vec![1u8; 4]);
    let e = s.write(&wp, &no_cancel(), || Ok(once.take()), |_| {}).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Protocol);
}

#[test]
fn write_cap_is_enforced() {
    let limits = Limits { max_write_bytes: 100, ..Limits::default() };
    let (s, _h) = session_with(FakeFs::new(), Faults::default(), None, 3000, limits);
    assert_eq!(write_all(&s, "/home/u/f", &[0; 101], CreateMode::New, None).unwrap_err().kind, ErrorKind::TooLarge);
}

#[test]
fn dropped_connection_mid_write_is_reported_and_leaves_only_the_temp_file() {
    let mut fs = FakeFs::new();
    fs.file("/home/u/live.txt", b"ORIGINAL");
    let (s, h) = session_with(fs, Faults { drop_after_write_bytes: Some(30_000), ..Faults::default() }, None, 3000, Limits::default());
    // a safe save writes to a temp name and never opens the live file for writing
    let e = write_all(&s, "/home/u/.live.txt.brouillon-tmp", &pattern(200_000), CreateMode::New, None).unwrap_err();
    assert!(matches!(e.kind, ErrorKind::ConnectionLost | ErrorKind::Timeout), "{e:?}");
    assert_eq!(h.fs.lock().unwrap().data("/home/u/live.txt").unwrap(), b"ORIGINAL");
    let partial = h.fs.lock().unwrap().data("/home/u/.live.txt.brouillon-tmp").unwrap();
    assert!(partial.len() < 200_000);
    assert!(s.client.is_dead());
    assert!(h.log.lock().unwrap().iter().all(|l| !l.contains("/home/u/live.txt")), "live file untouched");
}

// ------------------------------------------------------------------ namespace

#[test]
fn rename_variants() {
    let mut fs = FakeFs::new();
    fs.file("/home/u/a", b"A").file("/home/u/b", b"B").file("/home/u/c", b"C");
    let (s, h) = session(fs, Faults::default());
    // no overwrite onto an existing target
    assert_eq!(s.rename(&p("/home/u/a"), &p("/home/u/b"), false, false).unwrap_err().kind, ErrorKind::AlreadyExists);
    // atomic overwrite
    assert!(s.rename(&p("/home/u/a"), &p("/home/u/b"), true, false).unwrap().atomic);
    assert_eq!(h.fs.lock().unwrap().data("/home/u/b").unwrap(), b"A");
    assert!(h.log.lock().unwrap().iter().any(|l| l.starts_with("posix-rename")));
    // plain rename to a free name
    assert!(s.rename(&p("/home/u/b"), &p("/home/u/z"), false, false).unwrap().atomic);
    assert_eq!(s.rename(&p("/home/u/missing"), &p("/home/u/q"), false, false).unwrap_err().kind, ErrorKind::NotFound);
}

#[test]
fn overwrite_without_posix_rename_is_refused_unless_allowed() {
    let mut fs = FakeFs::new();
    fs.file("/home/u/a", b"A").file("/home/u/b", b"B");
    let (s, h) = session(fs, Faults { no_posix_rename: true, ..Faults::default() });
    assert_eq!(s.rename(&p("/home/u/a"), &p("/home/u/b"), true, false).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(h.fs.lock().unwrap().data("/home/u/b").unwrap(), b"B", "refusal changes nothing");
    let r = s.rename(&p("/home/u/a"), &p("/home/u/b"), true, true).unwrap();
    assert!(!r.atomic);
    assert_eq!(h.fs.lock().unwrap().data("/home/u/b").unwrap(), b"A");
}

#[test]
fn mkdir_remove_rmdir_chmod_realpath_readlink() {
    let mut fs = FakeFs::new();
    fs.file("/home/u/f", b"x").link("/home/u/l", "../../etc/pass\u{7}wd");
    let (s, h) = session(fs, Faults::default());
    s.mkdir(&p("/home/u/d"), Some(0o700)).unwrap();
    assert_eq!(s.mkdir(&p("/home/u/d"), None).unwrap_err().kind, ErrorKind::AlreadyExists);
    assert!(s.remove(&p("/home/u/d")).is_err(), "remove is for files");
    s.rmdir(&p("/home/u/d")).unwrap();
    s.remove(&p("/home/u/f")).unwrap();
    assert_eq!(s.remove(&p("/home/u/f")).unwrap_err().kind, ErrorKind::NotFound);
    s.mkdir(&p("/home/u/e"), None).unwrap();
    s.chmod(&p("/home/u/e"), 0o40750).unwrap();
    assert!(h.log.lock().unwrap().iter().any(|l| l == "setstat /home/u/e 750"));
    assert_eq!(s.realpath(&p("/home/u/./e/..")).unwrap(), p("/home/u"));
    let t = s.readlink(&p("/home/u/l")).unwrap();
    assert!(t.starts_with("../../etc/pass") && !t.chars().any(|c| c.is_control()), "{t:?}");
}

// ------------------------------------------------------------------ confinement

#[test]
fn root_confinement_and_symlink_gate() {
    let mut fs = FakeFs::new();
    fs.dir("/home/u/site")
        .file("/home/u/site/index.html", b"<html>")
        .file("/etc/passwd", b"root:x")
        .dir("/etc")
        .link("/home/u/site/inside", "index.html")
        .link("/home/u/site/escape", "/etc/passwd")
        .link("/home/u/site/rel-escape", "../../../../etc/passwd");
    let (s, _h) = session_with(fs, Faults::default(), Some("/home/u/site"), 3000, Limits::default());
    // lexical `..` is resolved by RemotePath::parse before it reaches the server
    let up = p("/home/u/site/../../../etc/passwd");
    assert_eq!(up.as_wire_str(), "/etc/passwd");
    assert_eq!(s.stat(&up, false).unwrap_err().kind, ErrorKind::OutsideRoot);
    assert_eq!(s.list_dir(&p("/etc"), &no_cancel(), |_| {}).unwrap_err().kind, ErrorKind::OutsideRoot);
    assert_eq!(write_all(&s, "/home/u/other", b"x", CreateMode::New, None).unwrap_err().kind, ErrorKind::OutsideRoot);
    assert_eq!(s.rename(&p("/home/u/site/index.html"), &p("/tmp/x"), true, false).unwrap_err().kind, ErrorKind::OutsideRoot);
    assert_eq!(s.realpath(&p("/home/u/site/escape")).unwrap_err().kind, ErrorKind::OutsideRoot);
    // symlinks
    assert_eq!(read_all(&s, "/home/u/site/inside").unwrap(), b"<html>");
    assert_eq!(read_all(&s, "/home/u/site/escape").unwrap_err().kind, ErrorKind::SymlinkRefused);
    assert_eq!(read_all(&s, "/home/u/site/rel-escape").unwrap_err().kind, ErrorKind::SymlinkRefused);
    let mut out = vec![];
    s.read(&p("/home/u/site/escape"), 0, None, None, true, &no_cancel(), |_, d| {
        out.extend_from_slice(d);
        Ok(())
    })
    .unwrap();
    assert_eq!(out, b"root:x", "explicit allow_symlink opens it");
    // writing through an escaping link is refused too
    let wp = WriteParams { path: p("/home/u/site/escape"), size: 1, create: CreateMode::Truncate, perm: None, expect: None, fsync: false, allow_symlink: false };
    let mut once = Some(vec![1u8]);
    assert_eq!(s.write(&wp, &no_cancel(), || Ok(once.take()), |_| {}).unwrap_err().kind, ErrorKind::SymlinkRefused);
    // stat reports where a link resolves
    let st = s.stat(&p("/home/u/site/escape"), true).unwrap();
    assert_eq!(st.resolved.unwrap().as_wire_str(), "/etc/passwd");
    assert!(st.outside_root);
    let st = s.stat(&p("/home/u/site/inside"), false).unwrap();
    assert_eq!(st.kind, EntryKind::Symlink);
    assert!(!st.outside_root);
}

#[test]
fn concurrent_requests_share_one_connection() {
    let mut fs = FakeFs::new();
    for i in 0..4 {
        fs.file(&format!("/home/u/f{i}"), &pattern(40_000 + i));
    }
    let (s, _h) = session(fs, Faults::default());
    let s = std::sync::Arc::new(s);
    let hs: Vec<_> = (0..4)
        .map(|i| {
            let s = s.clone();
            std::thread::spawn(move || {
                let d = read_all(&s, &format!("/home/u/f{i}")).unwrap();
                assert_eq!(d, pattern(40_000 + i));
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
}
