//! Empirical semantics of a memory-mapped vs handle-open base file: truncate, replace by rename, delete,
//! external in-place write, external append. Prints one verdict per probe (Windows is the interesting case).
use memmap2::Mmap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;

fn show<T: std::fmt::Debug>(what: &str, r: std::io::Result<T>) {
    match r {
        Ok(v) => println!("  {what:<58} OK   {v:?}"),
        Err(e) => println!("  {what:<58} FAIL {:?} (os error {:?}) {e}", e.kind(), e.raw_os_error()),
    }
}

fn run(mapped: bool) {
    let dir = std::env::temp_dir().join(format!("tsspike_sem_{}", if mapped { "map" } else { "handle" }));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let f = dir.join("base.txt");
    fs::write(&f, vec![b'a'; 1 << 20]).unwrap();
    let file = File::open(&f).unwrap();
    let map = if mapped { Some(unsafe { Mmap::map(&file).unwrap() }) } else { None };
    println!("== base file {} ==", if mapped { "MEMORY-MAPPED (+ handle open)" } else { "open by handle only (pread/ReadFile)" });

    show("truncate to 0 via another handle (OpenOptions.truncate)", OpenOptions::new().write(true).truncate(true).open(&f).map(|_| ()));
    fs::write(&f, vec![b'a'; 1 << 20]).ok();
    show("shrink with set_len(10) via another handle", OpenOptions::new().write(true).open(&f).and_then(|h| h.set_len(10)));
    fs::write(&f, vec![b'a'; 1 << 20]).ok();
    show("grow with set_len(2 MiB) via another handle", OpenOptions::new().write(true).open(&f).and_then(|h| h.set_len(2 << 20)));
    fs::write(&f, vec![b'a'; 1 << 20]).ok();
    show("overwrite in place bytes 0..6 via another handle", OpenOptions::new().write(true).open(&f).and_then(|mut h| h.write_all(b"ZZZZZZ").map(|_| ())));
    if let Some(m) = &map {
        println!("  (mapped view now reads byte 0 = {:?}: external in-place writes ARE visible, silently)", m[0] as char);
    }
    let tmp = dir.join("tmp.new");
    fs::write(&tmp, b"new content").unwrap();
    show("rename tmp over base (atomic save, replace)", fs::rename(&tmp, &f));
    show("   -> path now holds (len)", fs::read(&f).map(|v| v.len()));
    if let Some(m) = &map {
        println!("  (old mapped view len {} first byte {:?})", m.len(), m[0] as char);
    } else {
        use std::os::windows::fs::FileExt;
        let mut s = [0u8; 4];
        show("   -> old handle still reads old bytes (seek_read 4)", file.seek_read(&mut s, 100).map(|n| (n, s)));
    }
    let f2 = dir.join("base2.txt");
    fs::write(&f2, vec![b'b'; 1 << 20]).unwrap();
    let file2 = File::open(&f2).unwrap();
    let map2 = if mapped { Some(unsafe { Mmap::map(&file2).unwrap() }) } else { None };
    show("rename base2 -> base2.retired (move the name away)", fs::rename(&f2, dir.join("base2.retired")));
    show("delete base2.retired while open/mapped", fs::remove_file(dir.join("base2.retired")));
    drop(map2);
    drop(file2);
    show("delete base2.retired after unmapping/closing", fs::remove_file(dir.join("base2.retired")));
    drop(map);
    drop(file);
    let _ = fs::remove_dir_all(&dir);
    println!();
}

fn main() {
    println!("platform: {}", std::env::consts::OS);
    run(false);
    run(true);
}
