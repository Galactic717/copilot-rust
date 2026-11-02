//! Win32 helpers for the windows egui/winit doesn't fully control: focus-less overlay, capture exclusion,
//! geometry in physical pixels, single instance.

use std::sync::atomic::{AtomicIsize, Ordering};

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{EnumDisplayDevicesW, GetMonitorInfoW, MonitorFromPoint, MonitorFromRect, DISPLAY_DEVICEW, MONITORINFO, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcessId};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const TITLE_MAIN: &str = "Copilot";
pub const TITLE_OVERLAY: &str = "Copilot · overlay";
pub const TITLE_PICKER: &str = "Copilot · picker";

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

fn hwnd(v: isize) -> HWND {
    HWND(v as *mut core::ffi::c_void)
}

struct Find {
    pid: u32,
    title: Vec<u16>,
    found: isize,
}

unsafe extern "system" fn enum_cb(h: HWND, l: LPARAM) -> windows::core::BOOL {
    let f = &mut *(l.0 as *mut Find);
    let mut pid = 0u32;
    GetWindowThreadProcessId(h, Some(&mut pid));
    if pid == f.pid {
        // InternalGetWindowText never sends WM_GETTEXT, so a busy thread of ours can't hang the lookup.
        let mut buf = [0u16; 128];
        let n = InternalGetWindowText(h, &mut buf).max(0) as usize;
        if buf[..n] == f.title[..] {
            f.found = h.0 as isize;
            return false.into();
        }
    }
    true.into()
}

fn find_own(title: &str) -> isize {
    let mut f = Find { pid: unsafe { GetCurrentProcessId() }, title: title.encode_utf16().collect(), found: 0 };
    unsafe {
        let _ = EnumWindows(Some(enum_cb), LPARAM(&mut f as *mut Find as isize));
    }
    f.found
}

/// Cached HWND of one of our windows, looked up by its (fixed, language-neutral) title.
pub struct WinRef {
    title: &'static str,
    h: AtomicIsize,
}

impl WinRef {
    pub const fn new(title: &'static str) -> Self {
        Self { title, h: AtomicIsize::new(0) }
    }

    pub fn get(&self) -> Option<HWND> {
        let mut v = self.h.load(Ordering::Relaxed);
        if v == 0 || unsafe { !IsWindow(Some(hwnd(v))).as_bool() } {
            v = find_own(self.title);
            self.h.store(v, Ordering::Relaxed);
        }
        (v != 0).then(|| hwnd(v))
    }

    pub fn is_visible(&self) -> bool {
        self.get().is_some_and(|h| unsafe { IsWindowVisible(h).as_bool() })
    }

    /// Shown without taking focus: the IDE/meeting app keeps the keyboard.
    pub fn show_no_activate(&self) {
        if let Some(h) = self.get() {
            unsafe {
                let _ = ShowWindowAsync(h, SW_SHOWNOACTIVATE);
            }
        }
    }

    pub fn hide(&self) {
        if let Some(h) = self.get() {
            unsafe {
                let _ = ShowWindowAsync(h, SW_HIDE);
            }
        }
    }

    pub fn show_focused(&self) {
        if let Some(h) = self.get() {
            unsafe {
                let _ = ShowWindowAsync(h, SW_SHOW);
                let _ = SetForegroundWindow(h);
            }
        }
    }

    /// Overlay style: never activated by clicks, no taskbar button.
    pub fn make_tool_noactivate(&self) -> bool {
        let Some(h) = self.get() else { return false };
        unsafe {
            let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
            let want = (ex | WS_EX_NOACTIVATE.0 as isize | WS_EX_TOOLWINDOW.0 as isize) & !(WS_EX_APPWINDOW.0 as isize);
            if want != ex {
                SetWindowLongPtrW(h, GWL_EXSTYLE, want);
            }
        }
        true
    }

    /// WDA_EXCLUDEFROMCAPTURE: honored by most capture APIs, but not a guarantee.
    pub fn set_capture_excluded(&self, on: bool) -> bool {
        let Some(h) = self.get() else { return false };
        unsafe { SetWindowDisplayAffinity(h, if on { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE }).is_ok() }
    }

    pub fn set_topmost(&self, on: bool) {
        if let Some(h) = self.get() {
            unsafe {
                let _ = SetWindowPos(h, Some(if on { HWND_TOPMOST } else { HWND_NOTOPMOST }), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS);
            }
        }
    }

    pub fn rect(&self) -> Option<Rect> {
        let h = self.get()?;
        let mut r = RECT::default();
        unsafe { GetWindowRect(h, &mut r).ok()? };
        Some(Rect { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top })
    }

    pub fn set_rect(&self, r: Rect, topmost: bool, show: bool) {
        if let Some(h) = self.get() {
            let mut flags = SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS;
            if show {
                flags |= SWP_SHOWWINDOW;
            }
            let after = if topmost { Some(HWND_TOPMOST) } else { None };
            if after.is_none() {
                flags |= SWP_NOZORDER;
            }
            // Twice: moving onto a monitor with another DPI makes Windows rescale the first call's size.
            for _ in 0..2 {
                unsafe {
                    let _ = SetWindowPos(h, after, r.x, r.y, r.w, r.h, flags);
                }
            }
        }
    }

    pub fn foreground(&self) {
        if let Some(h) = self.get() {
            unsafe {
                let _ = SetForegroundWindow(h);
            }
        }
    }
}

pub static MAIN: WinRef = WinRef::new(TITLE_MAIN);
pub static OVERLAY: WinRef = WinRef::new(TITLE_OVERLAY);
pub static PICKER: WinRef = WinRef::new(TITLE_PICKER);

/// WDA_EXCLUDEFROMCAPTURE exists since Windows 10 2004 (build 19041).
pub fn display_protection_supported() -> bool {
    windows_build() >= 19041
}

pub fn windows_build() -> u32 {
    use windows::Wdk::System::SystemServices::RtlGetVersion;
    use windows::Win32::System::SystemInformation::OSVERSIONINFOW;
    let mut v = OSVERSIONINFOW { dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32, ..Default::default() };
    unsafe {
        if RtlGetVersion(&mut v).is_ok() {
            return v.dwBuildNumber;
        }
    }
    0
}

fn monitor_info(m: windows::Win32::Graphics::Gdi::HMONITOR) -> Option<(Rect, Rect)> {
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe { GetMonitorInfoW(m, &mut mi).ok().ok()? };
    let c = |r: RECT| Rect { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top };
    Some((c(mi.rcMonitor), c(mi.rcWork)))
}

/// Full and work-area rect (physical px) of the monitor under the mouse cursor.
pub fn monitor_under_cursor() -> Option<(Rect, Rect)> {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
        monitor_info(MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST))
    }
}

pub fn primary_work_area() -> Rect {
    unsafe { monitor_info(MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY)).map(|m| m.1).unwrap_or(Rect { x: 0, y: 0, w: 1920, h: 1080 }) }
}

/// I restore saved bounds only if they are still visible on some connected display.
pub fn bounds_visible(r: Rect) -> bool {
    let rc = RECT { left: r.x + 60, top: r.y, right: r.x + r.w - 60, bottom: r.y + 40 };
    unsafe { !MonitorFromRect(&rc, MONITOR_DEFAULTTONULL).is_invalid() }
}

pub fn has_nvidia() -> bool {
    let mut i = 0;
    loop {
        let mut dd = DISPLAY_DEVICEW { cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
        if unsafe { !EnumDisplayDevicesW(PCWSTR::null(), i, &mut dd, 0).as_bool() } {
            return false;
        }
        let name = String::from_utf16_lossy(&dd.DeviceString).to_lowercase();
        if name.contains("nvidia") {
            return true;
        }
        i += 1;
    }
}

/// Opens an https:// link in the default browser; anything else is refused.
pub fn open_url(url: &str) {
    if !url.starts_with("https://") && !url.starts_with("ms-settings:") {
        return;
    }
    unsafe {
        ShellExecuteW(None, &HSTRING::from("open"), &HSTRING::from(url), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// Returns false if another instance already runs; that instance's window is brought up.
/// `wait` is for a relaunch after an update: the previous version is on its way out and gets a few seconds to go.
pub fn single_instance(wait: bool) -> bool {
    unsafe {
        let name = HSTRING::from("Local\\app.copilot.single-instance");
        let mut h = CreateMutexW(None, true, &name);
        let taken = || windows::Win32::Foundation::GetLastError() == windows::Win32::Foundation::ERROR_ALREADY_EXISTS;
        let mut busy = taken();
        let mut tries = if wait { 40 } else { 0 };
        while busy && tries > 0 {
            if let Ok(handle) = h {
                let _ = windows::Win32::Foundation::CloseHandle(handle);
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
            h = CreateMutexW(None, true, &name);
            busy = taken();
            tries -= 1;
        }
        if busy {
            if let Ok(h) = FindWindowW(PCWSTR::null(), &HSTRING::from(TITLE_MAIN)) {
                let _ = ShowWindow(h, SW_SHOW);
                let _ = ShowWindow(h, SW_RESTORE);
                let _ = SetForegroundWindow(h);
            }
            return false;
        }
    }
    true
}

/// Physical memory in bytes; 0 when unknown (then every model "fits").
pub fn total_ram() -> u64 {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut m = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    unsafe { GlobalMemoryStatusEx(&mut m) }.map(|_| m.ullTotalPhys).unwrap_or(0)
}

/// "uk-UA", "en-US"…: the Windows display language, used once for a first run.
pub fn system_locale() -> String {
    use windows::Win32::Globalization::GetUserDefaultLocaleName;
    let mut buf = [0u16; 85];
    let n = unsafe { GetUserDefaultLocaleName(&mut buf) };
    String::from_utf16_lossy(&buf[..(n.max(1) as usize - 1).min(buf.len())])
}

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE: &str = "Copilot";
/// Started by Windows at login: stay in the tray until asked.
pub const TRAY_ARG: &str = "--tray";

pub fn autostart_enabled() -> bool {
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
    let mut len = 0u32;
    unsafe { RegGetValueW(HKEY_CURRENT_USER, &HSTRING::from(RUN_KEY), &HSTRING::from(RUN_VALUE), RRF_RT_REG_SZ, None, None, Some(&mut len)) }.is_ok()
}

pub fn set_autostart(on: bool) -> Result<(), String> {
    use windows::Win32::System::Registry::{RegCloseKey, RegCreateKeyW, RegDeleteValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, REG_SZ};
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    unsafe {
        let mut key = HKEY::default();
        RegCreateKeyW(HKEY_CURRENT_USER, &HSTRING::from(RUN_KEY), &mut key).ok().map_err(|e| e.to_string())?;
        let r = if on {
            let cmd: Vec<u16> = format!("\"{}\" {TRAY_ARG}\0", exe.display()).encode_utf16().collect();
            let bytes = std::slice::from_raw_parts(cmd.as_ptr() as *const u8, cmd.len() * 2);
            RegSetValueExW(key, &HSTRING::from(RUN_VALUE), None, REG_SZ, Some(bytes)).ok()
        } else {
            let _ = RegDeleteValueW(key, &HSTRING::from(RUN_VALUE));
            Ok(())
        };
        let _ = RegCloseKey(key);
        r.map_err(|e| e.to_string())
    }
}

fn start_menu_link() -> Option<std::path::PathBuf> {
    Some(std::path::PathBuf::from(std::env::var_os("APPDATA")?).join("Microsoft").join("Windows").join("Start Menu").join("Programs").join("Copilot.lnk"))
}

pub fn start_menu_shortcut_exists() -> bool {
    start_menu_link().is_some_and(|p| p.is_file())
}

/// COM needs its own apartment, so this runs on a throwaway thread rather than touching the UI thread's.
pub fn create_start_menu_shortcut() -> Result<(), String> {
    use windows::core::Interface;
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let link = start_menu_link().ok_or_else(|| "APPDATA".to_string())?;
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let r = (|| -> windows::core::Result<()> {
            let l: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            l.SetPath(&HSTRING::from(exe.as_os_str()))?;
            if let Some(d) = exe.parent() {
                l.SetWorkingDirectory(&HSTRING::from(d.as_os_str()))?;
            }
            l.SetDescription(&HSTRING::from("Copilot"))?;
            l.cast::<IPersistFile>()?.Save(&HSTRING::from(link.as_os_str()), true)
        })();
        CoUninitialize();
        r.map_err(|e| e.to_string())
    })
    .join()
    .map_err(|_| "shortcut thread panicked".to_string())?
}

/// Starts the given exe detached from this process, for the relaunch after an update.
pub fn relaunch(exe: &std::path::Path) -> Result<(), String> {
    std::process::Command::new(exe).arg(crate::update::RELAUNCH_ARG).spawn().map(|_| ()).map_err(|e| e.to_string())
}
