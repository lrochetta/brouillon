//! Windows side of dark / light mode: the OS "apps use light theme" switch and the
//! immersive dark title bar. No extra crates: three advapi32 / dwmapi / user32 calls
//! declared by hand (same approach as `win_icon`).
#![cfg(windows)]
#![allow(non_snake_case, clippy::upper_case_acronyms)]

use std::ffi::c_void;

type HWND = *mut c_void;
type LPARAM = isize;

#[link(name = "user32")]
extern "system" {
    fn EnumWindows(cb: unsafe extern "system" fn(HWND, LPARAM) -> i32, l: LPARAM) -> i32;
    fn GetWindowThreadProcessId(h: HWND, pid: *mut u32) -> u32;
    fn IsWindowVisible(h: HWND) -> i32;
    fn GetWindow(h: HWND, cmd: u32) -> HWND;
}
#[link(name = "advapi32")]
extern "system" {
    fn RegGetValueW(hkey: isize, sub_key: *const u16, value: *const u16, flags: u32, ty: *mut u32, data: *mut c_void, size: *mut u32) -> i32;
}
#[link(name = "dwmapi")]
extern "system" {
    fn DwmSetWindowAttribute(h: HWND, attr: u32, data: *const c_void, size: u32) -> i32;
}

const HKEY_CURRENT_USER: isize = 0x8000_0001_u32 as i32 as isize;
const RRF_RT_REG_DWORD: u32 = 0x10;
const GW_OWNER: u32 = 4;
const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
const DWMWA_USE_IMMERSIVE_DARK_MODE_PRE_20H1: u32 = 19;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize\AppsUseLightTheme`:
/// `Some(true)` = apps should be light, `None` when the value cannot be read.
pub fn apps_use_light_theme() -> Option<bool> {
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let val = wide("AppsUseLightTheme");
    let (mut data, mut size, mut ty) = (0u32, 4u32, 0u32);
    // SAFETY: valid NUL-terminated UTF-16 strings and correctly sized out-parameters.
    let rc = unsafe { RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), val.as_ptr(), RRF_RT_REG_DWORD, &mut ty, &mut data as *mut u32 as *mut c_void, &mut size) };
    (rc == 0).then_some(data != 0)
}

unsafe extern "system" fn visit(h: HWND, out: LPARAM) -> i32 {
    let mut pid = 0u32;
    GetWindowThreadProcessId(h, &mut pid);
    if pid == std::process::id() && IsWindowVisible(h) != 0 && GetWindow(h, GW_OWNER).is_null() {
        *(out as *mut HWND) = h;
        return 0; // stop
    }
    1
}

/// Switch this process's top-level window between the dark and light title bar.
/// `false` when the window does not exist yet (or DWM refused).
pub fn set_titlebar_dark(dark: bool) -> bool {
    let mut hwnd: HWND = std::ptr::null_mut();
    // SAFETY: `visit` only writes the HWND slot we pass; DwmSetWindowAttribute reads 4 bytes.
    unsafe {
        EnumWindows(visit, &mut hwnd as *mut HWND as LPARAM);
        if hwnd.is_null() {
            return false;
        }
        let v: i32 = dark as i32;
        let p = &v as *const i32 as *const c_void;
        DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, p, 4) == 0
            || DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE_PRE_20H1, p, 4) == 0
    }
}
