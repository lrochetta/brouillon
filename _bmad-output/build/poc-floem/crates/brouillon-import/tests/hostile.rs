//! Hostile-input tests for the EditPlus importer. SYNTHETIC fixtures only: the user's real
//! EditPlus folder is never read. A deterministic fuzzer builds crafted INI files (NULs, huge
//! lines, odd encodings, credentials hidden in values, credential-shaped key names) and checks
//! that no canary survives into any output or into the rendered report, and that nothing panics.

use brouillon_import::editplus::plan_from_folder;
use brouillon_import::editplus_full::plan_import;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::fs;

const CANARY: &str = "CNRY9981xyzQ";

fn shaped() -> String {
    format!("ghp_{}", "Zq8vLm2xPw4rT6yU9iO1aS3dF5gH7jK0lN2b")
}

fn known(id: &str) -> bool {
    id.chars().all(|c| c.is_ascii_lowercase() || c == '_')
}

fn lines(rng: &mut ChaCha8Rng) -> Vec<String> {
    let c = CANARY;
    let s = shaped();
    let pool: Vec<String> = vec![
        format!("Password={c}"),
        format!("Remote Password={c}"),
        format!("Pw={c}"),
        format!("FtpPass={c}"),
        format!("Community={c}"),
        format!("Argument=-p {c}"),
        format!("Argument=--password={c} --user bob"),
        format!("Argument=--token {c}"),
        format!("Command=sshpass -p {c} ssh host"),
        format!("Menu=sshpass -p {c}"),
        format!("Argument=https://bob:{c}@example.org/r.git"),
        format!("Argument=postgres://app:{c}@db/x"),
        format!("Command=mysql -u root -p{c} db"),
        format!("{s}=1"),
        format!("[{s}]"),
        format!("Menu={s}"),
        format!("Argument=Authorization: Bearer {c}{c}{c}"),
        "[Tool1]".into(),
        "[Tool2]".into(),
        "[Tools]".into(),
        "[Keyboard]".into(),
        "[Settings]".into(),
        "Menu=&Build".into(),
        "Menu1=&Lint".into(),
        "Command=make".into(),
        "Command1=cargo".into(),
        "Argument=-j4".into(),
        "Argument1=check".into(),
        "Capture=1".into(),
        "Count=3".into(),
        "=".into(),
        "====".into(),
        "[".into(),
        "[]".into(),
        "\0\0\0=\0".into(),
        format!("[{}]", "A".repeat(10_000)),
        format!("Argument={}", "Y".repeat(20_000)),
        format!("{}=v", "K".repeat(5_000)),
        "Key=\u{202e}evil\u{202c}".into(),
        "Font=Consolas".into(),
        "; comment".into(),
        "include=C:\\Windows\\System32\\drivers\\etc\\hosts".into(),
        "File=..\\..\\..\\secret.txt".into(),
    ];
    let n = rng.gen_range(3..40);
    (0..n).map(|_| pool[rng.gen_range(0..pool.len())].clone()).collect()
}

fn encode(rng: &mut ChaCha8Rng, text: &str) -> Vec<u8> {
    match rng.gen_range(0..4) {
        0 => text.as_bytes().to_vec(),
        1 => {
            let mut b = vec![0xFF, 0xFE];
            text.encode_utf16().for_each(|u| b.extend_from_slice(&u.to_le_bytes()));
            b
        }
        2 => text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect(), // UTF-16 without BOM
        _ => {
            let mut b = text.as_bytes().to_vec();
            if !b.is_empty() {
                let at = rng.gen_range(0..b.len());
                b.truncate(at); // cut mid-way, maybe mid-character
                b.extend((0..rng.gen_range(0..64)).map(|_| rng.gen::<u8>()));
            }
            b
        }
    }
}

#[test]
fn fuzzed_hostile_folders_never_leak_a_canary_or_panic() {
    let mut rng = ChaCha8Rng::seed_from_u64(0xB0_0D_1110);
    for round in 0..150 {
        let d = tempfile::tempdir().unwrap();
        for name in ["tool_u.ini", "editplus_u.ini", "workspace_u.ini", "regex_u.ini", "other.ini", "keys_u.ini"] {
            if rng.gen_bool(0.3) {
                continue;
            }
            let text = lines(&mut rng).join(if rng.gen_bool(0.5) { "\n" } else { "\r\n" });
            fs::write(d.path().join(name), encode(&mut rng, &text)).unwrap();
        }
        // The credential stores must never be opened: a canary inside them can never surface.
        fs::write(d.path().join("reg_u.ini"), format!("Name=me\nCode={CANARY}\n")).unwrap();
        fs::write(d.path().join("ftp_u.ini"), format!("Host=h\nPassword={CANARY}\nUser1=u\nPwd1={CANARY}\n")).unwrap();

        let full = plan_import(d.path(), &known, "").unwrap_or_else(|e| panic!("round {round}: {e}"));
        let mut everything: String = full.outputs.iter().map(|(p, c)| format!("{p}\n{c}\n")).collect();
        everything.push_str(&full.report.render(true, "dest", &[]));
        for (k, v) in [("labels", full.report.items.iter().map(|i| format!("{}|{}", i.name, i.detail)).collect::<String>())] {
            everything.push_str(&format!("{k}{v}"));
        }
        let simple = plan_from_folder(d.path()).unwrap_or_else(|e| panic!("round {round}: {e}"));
        everything.extend(simple.outputs.iter().map(|(p, c)| format!("{p}\n{c}\n")));
        everything.extend(simple.report.skipped_keys.iter().map(|k| format!("{k}\n")));
        everything.extend(simple.report.skipped_files.iter().map(|k| format!("{k}\n")));

        assert!(!everything.contains(CANARY), "round {round}: canary leaked");
        assert!(!everything.contains(&shaped()), "round {round}: credential-shaped key name leaked into the report or output (a token cut short by the mutation is no longer token-shaped and is not asserted)");
        assert!(full.report.unopened.contains(&"reg_u.ini".to_string()) && full.report.unopened.contains(&"ftp_u.ini".to_string()));
        assert!(full.report.items.iter().all(|i| i.name.chars().count() <= 200), "report names are bounded");
    }
}

#[test]
fn oversized_files_are_skipped_not_read() {
    let d = tempfile::tempdir().unwrap();
    let big = vec![b'a'; (brouillon_import::hardening::MAX_FILE_BYTES + 10) as usize];
    fs::write(d.path().join("tool_u.ini"), &big).unwrap();
    fs::write(d.path().join("big.ctl"), &big).unwrap();
    let full = plan_import(d.path(), &known, "").unwrap();
    assert!(full.outputs.iter().all(|(_, c)| c.len() < 1000));
    assert!(full.report.items.iter().any(|i| i.detail.contains("larger than 4 MiB")));
    let simple = plan_from_folder(d.path()).unwrap();
    assert!(simple.report.skipped_files.iter().any(|f| f.contains("larger than 4 MiB")));
}

#[test]
fn a_tool_with_an_embedded_password_is_imported_without_it() {
    let d = tempfile::tempdir().unwrap();
    fs::write(
        d.path().join("tool_u.ini"),
        format!("[Tool1]\nMenu=&Deploy\nCommand=ssh\nArgument=-p {CANARY} host\n[Tool2]\nMenu=&Mirror\nCommand=sshpass\nArgument=-f x\n[Tool3]\nMenu=&Build\nCommand=make\nArgument=-j4\n"),
    )
    .unwrap();
    let p = plan_import(d.path(), &known, "").unwrap();
    let tools = p.outputs.iter().find(|(r, _)| r == "tools.toml").map(|(_, c)| c.clone()).unwrap_or_default();
    assert!(!tools.contains(CANARY));
    assert!(tools.contains("make") && tools.contains("-j4"), "ordinary tools survive: {tools}");
    // `Command=sshpass` (the tool IS sshpass) is dropped: no command, no tool.
    assert!(!tools.contains("sshpass"));
    assert!(p.report.items.iter().any(|i| i.category == "secrets" && i.name.contains("Argument")));
}
