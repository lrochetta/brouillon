//! Strict parsers for everything an FTP server sends that we interpret: MLSx facts, `LIST` lines
//! (Unix and DOS styles), `MDTM`, `PASV`/`EPSV`, `PWD`, `FEAT`. Pure functions, no I/O.
//! Anything that does not match exactly is rejected (the caller counts it as "hidden"), never guessed.

use brouillon_remote_proto::EntryKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub name: String,
    pub kind: EntryKind,
    pub size: Option<u64>,
    pub mtime: Option<i64>,
    pub mode: Option<u32>,
}

// ---------------------------------------------------------------- time

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn valid_date(y: i64, m: i64, d: i64) -> bool {
    if !(1..=12).contains(&m) || d < 1 {
        return false;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let dim = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][(m - 1) as usize];
    d <= dim
}

fn epoch(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64) -> Option<i64> {
    if !valid_date(y, mo, d) || !(0..24).contains(&h) || !(0..60).contains(&mi) || !(0..=60).contains(&s) {
        return None;
    }
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + s)
}

/// `YYYYMMDDHHMMSS[.fff]` (UTC), as in `MDTM` replies and the MLSx `modify` fact.
pub fn parse_mdtm(s: &str) -> Option<i64> {
    let s = s.trim();
    let (main, frac) = match s.split_once('.') {
        Some((a, b)) => (a, Some(b)),
        None => (s, None),
    };
    if main.len() != 14 || !main.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if let Some(f) = frac {
        if f.is_empty() || f.len() > 9 || !f.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
    }
    let n = |a: usize, b: usize| main[a..b].parse::<i64>().ok();
    let y = n(0, 4)?;
    if y < 1970 {
        return None;
    }
    epoch(y, n(4, 6)?, n(6, 8)?, n(8, 10)?, n(10, 12)?, n(12, 14)?)
}

const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

fn month(s: &str) -> Option<i64> {
    let l = s.to_ascii_lowercase();
    MONTHS.iter().position(|m| *m == l).map(|i| i as i64 + 1)
}

/// `Jan 12 14:30` / `Jan 12 2023` (server-local time, treated as UTC: approximate by nature).
fn list_date(mon: &str, day: &str, ty: &str, now: i64) -> Option<i64> {
    let mo = month(mon)?;
    let d: i64 = day.parse().ok().filter(|d| (1..=31).contains(d))?;
    if let Some((h, m)) = ty.split_once(':') {
        let (h, m): (i64, i64) = (h.parse().ok()?, m.parse().ok()?);
        let this_year = 1970 + now.div_euclid(31_556_952);
        let t = epoch(this_year, mo, d, h, m, 0)?;
        // a date more than a day in the future belongs to last year (ls shows times for the last ~6 months)
        if t > now + 86_400 {
            return epoch(this_year - 1, mo, d, h, m, 0);
        }
        Some(t)
    } else if ty.len() == 4 && ty.bytes().all(|b| b.is_ascii_digit()) {
        let y: i64 = ty.parse().ok().filter(|y| *y >= 1970)?;
        epoch(y, mo, d, 0, 0, 0)
    } else {
        None
    }
}

// ---------------------------------------------------------------- MLSx

fn mlsx_facts(facts: &str) -> Option<(Option<EntryKind>, bool, Option<u64>, Option<i64>, Option<u32>)> {
    // (kind, is_cdir_or_pdir, size, mtime, mode)
    let (mut kind, mut skip, mut size, mut mtime, mut mode) = (None, false, None, None, None);
    for f in facts.split(';').filter(|f| !f.is_empty()) {
        let (k, v) = f.split_once('=')?;
        match k.to_ascii_lowercase().as_str() {
            "type" => {
                let l = v.to_ascii_lowercase();
                kind = Some(match l.as_str() {
                    "file" => EntryKind::File,
                    "dir" => EntryKind::Dir,
                    "cdir" | "pdir" => {
                        skip = true;
                        EntryKind::Dir
                    }
                    s if s.starts_with("os.unix=slink") || s.starts_with("os.unix=symlink") => EntryKind::Symlink,
                    _ => EntryKind::Other,
                });
            }
            "size" => size = v.parse().ok(),
            "modify" => mtime = parse_mdtm(v),
            "unix.mode" => {
                mode = u32::from_str_radix(v, 8).ok().filter(|m| *m <= 0o7777);
            }
            _ => {}
        }
    }
    Some((kind, skip, size, mtime, mode))
}

/// One `MLSD` line or the fact line of an `MLST` reply (`facts SP name`). `Ok(None)` = skip (`.`/`..`).
pub fn parse_mlsx(line: &str) -> Result<Option<Parsed>, ()> {
    let (facts, name) = line.split_once(' ').ok_or(())?;
    if name.is_empty() {
        return Err(());
    }
    let (kind, skip, size, mtime, mode) = mlsx_facts(facts).ok_or(())?;
    if skip || name == "." || name == ".." {
        return Ok(None);
    }
    Ok(Some(Parsed { name: name.to_string(), kind: kind.ok_or(())?, size, mtime, mode }))
}

// ---------------------------------------------------------------- LIST

#[derive(Debug, PartialEq, Eq)]
pub enum ListLine {
    /// `total N`, blank line, `.`/`..`: not an entry and not an error.
    Skip,
    Entry(Parsed),
    Bad,
}

fn token(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start_matches([' ', '\t']);
    if s.is_empty() {
        return None;
    }
    let end = s.find([' ', '\t']).unwrap_or(s.len());
    Some((&s[..end], &s[end..]))
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit())
}

fn unix_mode(p: &str) -> Option<u32> {
    let b = p.as_bytes();
    if b.len() < 10 {
        return None;
    }
    let mut m = 0u32;
    for (i, c) in b[1..10].iter().enumerate() {
        let bit = 8 - i as u32; // r=8 .. x=0 within 9 bits, i = 0..8
        let (set, special) = match (i % 3, *c) {
            (_, b'-') => (false, false),
            (0, b'r') | (1, b'w') | (2, b'x') => (true, false),
            (2, b's') | (2, b't') => (true, true),
            (2, b'S') | (2, b'T') => (false, true),
            _ => return None,
        };
        if set {
            m |= 1 << bit;
        }
        if special {
            m |= match i {
                2 => 0o4000,
                5 => 0o2000,
                _ => 0o1000,
            };
        }
    }
    Some(m)
}

pub fn parse_list_line(line: &str, now: i64) -> ListLine {
    let t = line.trim_end_matches(['\r', '\n']);
    if t.trim().is_empty() {
        return ListLine::Skip;
    }
    if t.to_ascii_lowercase().starts_with("total ") {
        return ListLine::Skip;
    }
    let first = t.as_bytes()[0];
    if matches!(first, b'-' | b'd' | b'l' | b'c' | b'b' | b'p' | b's') {
        return unix_line(t, now);
    }
    if first.is_ascii_digit() {
        return dos_line(t, now);
    }
    ListLine::Bad
}

fn unix_line(t: &str, now: i64) -> ListLine {
    let Some((perms, rest)) = token(t) else { return ListLine::Bad };
    let pb = perms.as_bytes();
    if !(pb.len() == 10 || (pb.len() == 11 && matches!(pb[10], b'+' | b'@' | b'.'))) {
        return ListLine::Bad;
    }
    let Some(mode) = unix_mode(perms) else { return ListLine::Bad };
    let Some((links, rest)) = token(rest) else { return ListLine::Bad };
    let Some((_owner, rest)) = token(rest) else { return ListLine::Bad };
    if !is_digits(links) {
        return ListLine::Bad;
    }
    // five more tokens a..e; layouts: [group size mon day y/t] or [size mon day y/t name...]
    let mut tk: Vec<(&str, &str)> = Vec::with_capacity(5);
    let mut r = rest;
    for _ in 0..5 {
        let Some((x, rr)) = token(r) else { break };
        tk.push((x, rr));
        r = rr;
    }
    let (size, mon, day, ty, name_rest) = if tk.len() == 5 && is_digits(tk[1].0) && month(tk[2].0).is_some() {
        (tk[1].0, tk[2].0, tk[3].0, tk[4].0, tk[4].1)
    } else if tk.len() >= 4 && is_digits(tk[0].0) && month(tk[1].0).is_some() {
        (tk[0].0, tk[1].0, tk[2].0, tk[3].0, tk[3].1)
    } else {
        return ListLine::Bad;
    };
    let size: u64 = match size.parse() {
        Ok(s) => s,
        Err(_) => return ListLine::Bad,
    };
    let Some(mtime) = list_date(mon, day, ty, now) else { return ListLine::Bad };
    // the name is the rest of the line; a leading blank run is a separator, not part of it
    let name = name_rest.trim_start_matches([' ', '\t']);
    if name.is_empty() {
        return ListLine::Bad;
    }
    let is_link = perms.as_bytes()[0] == b'l';
    let name = if is_link { name.split_once(" -> ").map(|(a, _)| a).unwrap_or(name) } else { name };
    if name == "." || name == ".." {
        return ListLine::Skip;
    }
    let kind = match perms.as_bytes()[0] {
        b'-' => EntryKind::File,
        b'd' => EntryKind::Dir,
        b'l' => EntryKind::Symlink,
        _ => EntryKind::Other,
    };
    ListLine::Entry(Parsed { name: name.to_string(), kind, size: Some(size), mtime: Some(mtime), mode: Some(mode) })
}

fn dos_line(t: &str, now: i64) -> ListLine {
    let _ = now;
    let Some((date, rest)) = token(t) else { return ListLine::Bad };
    let Some((time, rest)) = token(rest) else { return ListLine::Bad };
    let Some((dir_or_size, rest)) = token(rest) else { return ListLine::Bad };
    let d: Vec<&str> = date.split('-').collect();
    if d.len() != 3 {
        return ListLine::Bad;
    }
    let (Ok(mo), Ok(dd), Ok(mut yy)) = (d[0].parse::<i64>(), d[1].parse::<i64>(), d[2].parse::<i64>()) else { return ListLine::Bad };
    if d[2].len() == 2 {
        yy += if yy >= 70 { 1900 } else { 2000 };
    } else if d[2].len() != 4 {
        return ListLine::Bad;
    }
    let tl = time.to_ascii_uppercase();
    let (hm, pm) = if let Some(x) = tl.strip_suffix("PM") {
        (x.to_string(), true)
    } else if let Some(x) = tl.strip_suffix("AM") {
        (x.to_string(), false)
    } else {
        (tl.clone(), false)
    };
    let Some((h, m)) = hm.split_once(':') else { return ListLine::Bad };
    let (Ok(mut h), Ok(m)) = (h.parse::<i64>(), m.parse::<i64>()) else { return ListLine::Bad };
    if tl.ends_with("M") {
        if !(1..=12).contains(&h) {
            return ListLine::Bad;
        }
        h = h % 12 + if pm { 12 } else { 0 };
    }
    let Some(mtime) = epoch(yy, mo, dd, h, m, 0) else { return ListLine::Bad };
    let (kind, size) = if dir_or_size.eq_ignore_ascii_case("<dir>") {
        (EntryKind::Dir, None)
    } else if is_digits(dir_or_size) {
        match dir_or_size.parse() {
            Ok(s) => (EntryKind::File, Some(s)),
            Err(_) => return ListLine::Bad,
        }
    } else {
        return ListLine::Bad;
    };
    let name = rest.trim_start_matches([' ', '\t']);
    if name.is_empty() {
        return ListLine::Bad;
    }
    if name == "." || name == ".." {
        return ListLine::Skip;
    }
    ListLine::Entry(Parsed { name: name.to_string(), kind, size, mtime: Some(mtime), mode: None })
}

// ---------------------------------------------------------------- control replies

/// The data port of a `227` (`h1,h2,h3,h4,p1,p2`) reply. The host part is parsed for validity and
/// then DISCARDED by every caller: data connections always go to the control connection's address.
pub fn parse_pasv_port(text: &str) -> Option<u16> {
    let start = text.find(|c: char| c.is_ascii_digit())?;
    let mut nums: Vec<u32> = Vec::new();
    for part in text[start..].split(',') {
        let digits: String = part.trim().chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() || digits.len() > 3 {
            return None;
        }
        nums.push(digits.parse().ok()?);
        if nums.len() == 6 {
            break;
        }
    }
    if nums.len() != 6 || nums.iter().any(|n| *n > 255) {
        return None;
    }
    let port = nums[4] * 256 + nums[5];
    (port != 0).then_some(port as u16)
}

/// The port of a `229` (`(|||port|)`) reply. Any delimiter byte is allowed, address fields must be empty.
pub fn parse_epsv_port(text: &str) -> Option<u16> {
    let open = text.find('(')?;
    let inner = &text[open + 1..];
    let close = inner.find(')')?;
    let inner = &inner[..close];
    let mut ch = inner.chars();
    let d = ch.next()?;
    if d.is_ascii_digit() || d == ' ' || !d.is_ascii_graphic() {
        return None;
    }
    let parts: Vec<&str> = inner.split(d).collect();
    // "" "" "" port ""
    if parts.len() != 5 || !parts[0].is_empty() || !parts[1].is_empty() || !parts[2].is_empty() || !parts[4].is_empty() {
        return None;
    }
    let p: u32 = parts[3].parse().ok()?;
    (1..=65535).contains(&p).then_some(p as u16)
}

/// `257 "/dir" is the current directory` (a `""` inside the quotes is a literal quote).
pub fn parse_pwd(text: &str) -> Option<String> {
    let s = text.strip_prefix('"').or_else(|| text.find('"').map(|i| &text[i + 1..]))?;
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '"' {
            if it.peek() == Some(&'"') {
                it.next();
                out.push('"');
            } else {
                return Some(out);
            }
        } else {
            out.push(c);
        }
    }
    None
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Feat {
    pub mlst: bool,
    pub mdtm: bool,
    pub size: bool,
    pub utf8: bool,
    pub rest_stream: bool,
    pub hash: Vec<String>,
}

/// `FEAT` reply lines (without the first and last line).
pub fn parse_feat(lines: &[String]) -> Feat {
    let mut f = Feat::default();
    for l in lines {
        let l = l.trim();
        let (cmd, arg) = l.split_once(' ').unwrap_or((l, ""));
        match cmd.to_ascii_uppercase().as_str() {
            "MLST" => f.mlst = true,
            "MDTM" => f.mdtm = true,
            "SIZE" => f.size = true,
            "UTF8" => f.utf8 = true,
            "REST" => f.rest_stream = arg.to_ascii_uppercase().contains("STREAM"),
            "HASH" => {
                for a in arg.split(';').map(|a| a.trim().trim_end_matches('*')).filter(|a| !a.is_empty()) {
                    f.hash.push(a.to_string());
                }
            }
            c @ ("XCRC" | "XMD5" | "XSHA" | "XSHA1" | "XSHA256" | "XSHA512") => f.hash.push(c.to_string()),
            _ => {}
        }
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    const NOW: i64 = 1_800_000_000; // 2027-01-15

    #[test]
    fn mdtm() {
        assert_eq!(parse_mdtm("20240229120000"), Some(1_709_208_000));
        assert_eq!(parse_mdtm("20240229120000.123"), Some(1_709_208_000));
        assert_eq!(parse_mdtm("20230229120000"), None); // not a leap year
        assert_eq!(parse_mdtm("2024022912000"), None);
        assert_eq!(parse_mdtm("20241301120000"), None);
        assert_eq!(parse_mdtm("1969010100000a"), None);
    }

    #[test]
    fn mlsx() {
        let p = parse_mlsx("type=file;size=12;modify=20240229120000;UNIX.mode=0644; a b.txt").unwrap().unwrap();
        assert_eq!((p.name.as_str(), p.kind, p.size, p.mtime, p.mode), ("a b.txt", EntryKind::File, Some(12), Some(1_709_208_000), Some(0o644)));
        assert_eq!(parse_mlsx("type=cdir; .").unwrap(), None);
        assert_eq!(parse_mlsx("type=pdir; ..").unwrap(), None);
        assert_eq!(parse_mlsx("type=dir; ..").unwrap(), None);
        let l = parse_mlsx("type=OS.unix=slink:/etc;size=4; ln").unwrap().unwrap();
        assert_eq!(l.kind, EntryKind::Symlink);
        assert!(parse_mlsx("nofacts").is_err());
        assert!(parse_mlsx("type=file;broken; x").is_err());
        assert!(parse_mlsx("size=3; x").is_err(), "no type fact");
        assert_eq!(parse_mlsx("type=file;unix.mode=99999; x").unwrap().unwrap().mode, None);
    }

    fn entry(l: &str) -> Parsed {
        match parse_list_line(l, NOW) {
            ListLine::Entry(p) => p,
            o => panic!("{l:?} -> {o:?}"),
        }
    }

    #[test]
    fn unix_list() {
        let p = entry("-rw-r--r--   1 ftp      ftp          1234 Jan 12  2023 my file.txt");
        assert_eq!((p.name.as_str(), p.kind, p.size, p.mode), ("my file.txt", EntryKind::File, Some(1234), Some(0o644)));
        let p = entry("drwxr-xr-x   2 ftp      ftp          4096 Dec 25 10:30 sub");
        assert_eq!((p.kind, p.mode), (EntryKind::Dir, Some(0o755)));
        assert!(p.mtime.unwrap() < NOW, "Dec of last year, not the future");
        let p = entry("lrwxrwxrwx   1 ftp      ftp             9 Jan 12  2023 ln -> /etc/passwd");
        assert_eq!((p.name.as_str(), p.kind), ("ln", EntryKind::Symlink));
        let p = entry("-rwsr-xr-t 1 root wheel 5 Feb 3 2020 x");
        assert_eq!(p.mode, Some(0o5755));
        // no group column
        let p = entry("-rw-r--r-- 1 owner 77 Jan 12 2023 nogroup.txt");
        assert_eq!((p.name.as_str(), p.size), ("nogroup.txt", Some(77)));
        assert_eq!(parse_list_line("total 12", NOW), ListLine::Skip);
        assert_eq!(parse_list_line("", NOW), ListLine::Skip);
        assert_eq!(parse_list_line("drwxr-xr-x 2 a b 4096 Jan 1 2020 ..", NOW), ListLine::Skip);
    }

    #[test]
    fn malformed_list_lines_are_rejected() {
        for l in [
            "garbage",
            "-rw-r--r-- x ftp ftp 12 Jan 12 2023 f",
            "-rw-r--r-- 1 ftp ftp twelve Jan 12 2023 f",
            "-rw-r--r-- 1 ftp ftp 12 Foo 12 2023 f",
            "-rw-r--r-- 1 ftp ftp 12 Jan 99 2023 f",
            "-rw-r--r-- 1 ftp ftp 12 Jan 12 2023",
            "-rwxrwxrwxx 1 ftp ftp 12 Jan 12 2023 f",
            "-rw-r--r-- 1 ftp ftp 99999999999999999999999 Jan 12 2023 f",
            "-rw-r--r-- 1 ftp",
            "-qw-r--r-- 1 ftp ftp 12 Jan 12 2023 f",
        ] {
            assert_eq!(parse_list_line(l, NOW), ListLine::Bad, "{l:?}");
        }
    }

    #[test]
    fn hostile_names_survive_parsing_but_are_visible_to_the_name_filter() {
        // the parser does not judge names; RemoteName::new does (tested in ops tests)
        assert_eq!(entry("-rw-r--r-- 1 a b 1 Jan 12 2023 ../x").name, "../x");
        assert_eq!(entry("-rw-r--r-- 1 a b 1 Jan 12 2023 /etc/passwd").name, "/etc/passwd");
        assert_eq!(entry("-rw-r--r-- 1 a b 1 Jan 12 2023 a\u{1b}[2Jb").name, "a\u{1b}[2Jb");
    }

    #[test]
    fn dos_list() {
        let p = entry("01-15-24  03:45PM       <DIR>          Docs");
        assert_eq!((p.name.as_str(), p.kind, p.size), ("Docs", EntryKind::Dir, None));
        let p = entry("12-31-99  12:00AM                 1234 old file.txt");
        assert_eq!((p.name.as_str(), p.kind, p.size), ("old file.txt", EntryKind::File, Some(1234)));
        assert_eq!(parse_list_line("13-45-24  03:45PM <DIR> x", NOW), ListLine::Bad);
        assert_eq!(parse_list_line("01-15-24  25:45 12 x", NOW), ListLine::Bad);
    }

    #[test]
    fn pasv_and_epsv() {
        assert_eq!(parse_pasv_port("Entering Passive Mode (10,0,0,5,195,80)."), Some(50000));
        assert_eq!(parse_pasv_port("Entering Passive Mode (10,0,0,5,0,0)."), None);
        assert_eq!(parse_pasv_port("Entering Passive Mode (10,0,0,5,256,1)."), None);
        assert_eq!(parse_pasv_port("Entering Passive Mode (10,0,0,5,1)."), None);
        assert_eq!(parse_pasv_port("no numbers"), None);
        assert_eq!(parse_epsv_port("Entering Extended Passive Mode (|||6446|)"), Some(6446));
        assert_eq!(parse_epsv_port("Entering Extended Passive Mode (!!!6446!)"), Some(6446));
        assert_eq!(parse_epsv_port("(|1|10.0.0.1|6446|)"), None, "an address in EPSV is refused outright");
        assert_eq!(parse_epsv_port("(|||0|)"), None);
        assert_eq!(parse_epsv_port("(|||70000|)"), None);
    }

    #[test]
    fn pwd_and_feat() {
        assert_eq!(parse_pwd("\"/home/u\" is the current directory").as_deref(), Some("/home/u"));
        assert_eq!(parse_pwd("\"/a\"\"b\" ok").as_deref(), Some("/a\"b"));
        assert_eq!(parse_pwd("\"/unterminated"), None);
        let f = parse_feat(&[" MLST type*;size*;modify*;".into(), " UTF8".into(), " SIZE".into(), " MDTM".into(), " REST STREAM".into(), " HASH SHA-256*;SHA-1".into(), " XCRC".into()]);
        assert!(f.mlst && f.utf8 && f.size && f.mdtm && f.rest_stream);
        assert_eq!(f.hash, ["SHA-256", "SHA-1", "XCRC"]);
        assert!(parse_feat(&[]).hash.is_empty());
    }
}
