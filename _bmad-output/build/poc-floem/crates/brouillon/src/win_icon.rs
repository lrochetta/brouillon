//! Windows taskbar / Alt+Tab icon.
//!
//! floem 0.2.0's `WindowConfig::window_icon` only sets the small (title-bar) icon; winit's
//! separate "taskbar icon" is not exposed, so the taskbar button stayed generic. This sets
//! `ICON_BIG` from the icon embedded in the exe by build.rs (resource id 1). No extra crates:
//! three user32/kernel32 calls declared by hand.
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
    fn SendMessageW(h: HWND, msg: u32, w: usize, l: isize) -> isize;
    fn LoadImageW(inst: *mut c_void, name: *const u16, ty: u32, cx: i32, cy: i32, flags: u32) -> *mut c_void;
}
#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
}

const WM_SETICON: u32 = 0x0080;
const ICON_BIG: usize = 1;
const IMAGE_ICON: u32 = 1;
const LR_DEFAULTSIZE: u32 = 0x0040;
const GW_OWNER: u32 = 4;

unsafe extern "system" fn visit(h: HWND, out: LPARAM) -> i32 {
    let mut pid = 0u32;
    GetWindowThreadProcessId(h, &mut pid);
    if pid == std::process::id() && IsWindowVisible(h) != 0 && GetWindow(h, GW_OWNER).is_null() {
        *(out as *mut HWND) = h;
        return 0; // stop
    }
    1
}

/// Polls (max ~10 s) for this process's first top-level window, then gives it the big icon.
pub fn spawn_taskbar_icon_setter() {
    std::thread::spawn(|| {
        for _ in 0..200 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            unsafe {
                let mut hwnd: HWND = std::ptr::null_mut();
                EnumWindows(visit, &mut hwnd as *mut HWND as LPARAM);
                if hwnd.is_null() {
                    continue;
                }
                // MAKEINTRESOURCE(1): the first icon group written by winresource.
                let icon = LoadImageW(GetModuleHandleW(std::ptr::null()), 1usize as *const u16, IMAGE_ICON, 0, 0, LR_DEFAULTSIZE);
                if !icon.is_null() {
                    SendMessageW(hwnd, WM_SETICON, ICON_BIG, icon as isize);
                }
                return;
            }
        }
    });
}
