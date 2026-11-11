//! Embeds build metadata: build number (git commit count), short hash, UTC date.
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Days since 1970-01-01 -> (year, month, day) (Hinnant's civil_from_days).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d)
}

fn main() {
    let secs = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or_else(|| SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0));
    let (y, m, d) = civil(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    let date = format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60);
    let build = std::env::var("BROUILLON_BUILD_NUMBER")
        .ok()
        .or_else(|| git(&["rev-list", "--count", "HEAD"]))
        .unwrap_or_else(|| "0".into());
    let commit = git(&["rev-parse", "--short=7", "HEAD"]).unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=BROUILLON_BUILD_NUMBER={build}");
    println!("cargo:rustc-env=BROUILLON_GIT_COMMIT={commit}");
    println!("cargo:rustc-env=BROUILLON_BUILD_DATE={date}");
    println!("cargo:rerun-if-changed=../../../../../.git/HEAD");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    println!("cargo:rerun-if-env-changed=BROUILLON_BUILD_NUMBER");
    #[cfg(windows)]
    embed_windows_resources(&build);
}

/// Icon + VERSIONINFO for the Windows exe. Skipped when targeting another OS.
#[cfg(windows)]
fn embed_windows_resources(build: &str) {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let ico = std::path::Path::new("../../../../../assets/brand/brouillon.ico");
    println!("cargo:rerun-if-changed={}", ico.display());
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let mut nums = version.split(['.', '-']).take(3).map(|p| p.parse::<u64>().unwrap_or(0));
    let (a, b, c) = (nums.next().unwrap_or(0), nums.next().unwrap_or(0), nums.next().unwrap_or(0));
    let packed = (a << 48) | (b << 32) | (c << 16) | build.parse::<u64>().unwrap_or(0).min(0xFFFF);
    let mut res = winresource::WindowsResource::new();
    res.set_icon(&ico.to_string_lossy())
        .set("ProductName", "Brouillon")
        .set("FileDescription", "Brouillon, the fast editor with no telemetry")
        .set("CompanyName", "Laurent Rochetta")
        .set("LegalCopyright", "Copyright (c) 2026 Laurent Rochetta (MIT)")
        .set("OriginalFilename", "brouillon.exe")
        .set("InternalName", "brouillon")
        .set("FileVersion", &format!("{version} (build {build})"))
        .set("ProductVersion", &version)
        .set_version_info(winresource::VersionInfo::FILEVERSION, packed)
        .set_version_info(winresource::VersionInfo::PRODUCTVERSION, packed);
    if let Err(e) = res.compile() {
        // Never block a build over cosmetics (no rc.exe on this machine).
        println!("cargo:warning=icon/version resource not embedded: {e}");
    }
}
