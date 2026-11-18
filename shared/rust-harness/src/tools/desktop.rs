//! Desktop tools without PyObjC: Quartz window list, CGEvent input, NSWorkspace
//! focus, through direct framework FFI. The OS calls sit behind the `Desktop` trait
//! so the scope checks are testable without posting real input.
use super::{err, ToolError, Tools, R};
use serde_json::{json, Map, Value as Json};
use std::ffi::{c_void, CString};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub owner: Option<String>,
    pub process_id: i64,
    pub name: Option<String>,
    pub on_screen: bool,
    pub layer: i64,
    pub bounds: (f64, f64, f64, f64),
}

impl Window {
    pub fn to_json(&self) -> Json {
        json!({"owner": self.owner, "process_id": self.process_id, "name": self.name, "on_screen": self.on_screen,
               "bounds": {"X": self.bounds.0, "Y": self.bounds.1, "Width": self.bounds.2, "Height": self.bounds.3}})
    }
}

pub trait Desktop {
    fn windows(&self) -> Vec<Window>;
    fn accessibility(&self) -> bool;
    fn screen_recording(&self) -> bool;
    fn display_size(&self) -> (f64, f64);
    fn click(&self, x: f64, y: f64);
    fn type_text(&self, text: &str);
    fn key(&self, code: u16, command: bool);
    fn frontmost_pid(&self) -> Option<i64>;
    /// Err when the process is not a running application.
    fn activate(&self, pid: i64) -> Result<bool, String>;
}

const KEYS: [(&str, u16); 6] = [
    ("Return", 36),
    ("Tab", 48),
    ("Escape", 53),
    ("cmd+s", 1),
    ("cmd+a", 0),
    ("cmd+n", 45),
];

impl Tools {
    /// Layer-0 named windows, plus the named target on any layer; at most 80.
    fn native_windows(&self) -> Vec<Window> {
        let title = self.desktop_window_title.as_deref();
        let mut w: Vec<Window> = self
            .desktop
            .windows()
            .into_iter()
            .filter(|w| {
                (w.layer == 0 && w.name.as_deref().is_some_and(|n| !n.is_empty()))
                    || (title.is_some() && w.name.as_deref() == title)
            })
            .collect();
        w.truncate(80);
        w
    }

    pub fn native_target(&self, visible: bool) -> R<Option<Window>> {
        let Some(title) = self.desktop_window_title.as_deref() else {
            return Ok(None);
        };
        let m: Vec<Window> = self
            .native_windows()
            .into_iter()
            .filter(|w| w.name.as_deref() == Some(title) && (w.on_screen || !visible))
            .collect();
        if m.len() != 1 {
            return err(
                "PermissionError",
                "The named desktop target is not uniquely visible; focus it before input",
            );
        }
        Ok(m.into_iter().next())
    }

    fn input_permission(&self) -> R<()> {
        if !self.desktop.accessibility() {
            return err(
                "PermissionError",
                "Enable Accessibility for the terminal/agent host in macOS Privacy & Security",
            );
        }
        Ok(())
    }

    fn focused_target(&self) -> R<()> {
        if let Some(t) = self.native_target(true)? {
            if self.desktop.frontmost_pid() != Some(t.process_id) {
                return err("PermissionError", "Named desktop target is not focused; click its observed input or focus its application first");
            }
        }
        Ok(())
    }

    pub fn desktop_tool(&mut self, name: &str, a: &Map<String, Json>) -> R<Json> {
        let num = |k: &str| a.get(k).and_then(Json::as_f64).unwrap_or(0.0);
        match name {
            "desktop_observe" => {
                Ok(self.desktop_observe(a.get("query").and_then(Json::as_str).unwrap_or("")))
            }
            "desktop_focus" => {
                self.desktop_focus(a.get("process_id").and_then(Json::as_f64).unwrap_or(0.0) as i64)
            }
            "desktop_screenshot" => self.desktop_screenshot(),
            "desktop_click" => self.desktop_click(
                num("x"),
                num("y"),
                a.get("coordinate_space")
                    .and_then(Json::as_str)
                    .unwrap_or("screen_points"),
            ),
            "desktop_type" => {
                self.desktop_type(a.get("text").and_then(Json::as_str).unwrap_or_default())
            }
            "desktop_key" => {
                self.desktop_key(a.get("key").and_then(Json::as_str).unwrap_or_default())
            }
            _ => err("ValueError", format!("Unknown tool: {name}")),
        }
    }

    pub fn desktop_observe(&mut self, query: &str) -> Json {
        let mut w = self.native_windows();
        if let Some(t) = self.desktop_window_title.as_deref() {
            w.retain(|x| x.name.as_deref() == Some(t));
        }
        if !query.is_empty() {
            let q = query.to_lowercase();
            w.retain(|x| {
                format!(
                    "{} {}",
                    x.owner.as_deref().unwrap_or(""),
                    x.name.as_deref().unwrap_or("")
                )
                .to_lowercase()
                .contains(&q)
            });
        }
        let count = w.len();
        w.sort_by_key(|x| !x.on_screen); // stable: on-screen first
        w.truncate(12);
        self.observed_desktop_pids = w.iter().map(|x| x.process_id).collect();
        let windows: Vec<Json> = w
            .iter()
            .map(|x| {
                let mut j = x.to_json();
                j["name"] = json!(x
                    .name
                    .as_deref()
                    .map(|n| n.chars().take(200).collect::<String>()));
                j
            })
            .collect();
        json!({"accessibility": self.desktop.accessibility(), "screen_recording": self.desktop.screen_recording(),
               "windows": windows, "matched_windows": count, "truncated": count > w.len()})
    }

    pub fn desktop_focus(&mut self, pid: i64) -> R<Json> {
        self.input_permission()?;
        if !self.observed_desktop_pids.contains(&pid) {
            return err(
                "ValueError",
                "Observe the application process before focusing it",
            );
        }
        if let Some(t) = self.native_target(false)? {
            if t.process_id != pid {
                return err(
                    "PermissionError",
                    "Application is outside the named desktop target",
                );
            }
        }
        let activated = self
            .desktop
            .activate(pid)
            .map_err(|e| ToolError(format!("ValueError: {e}")))?;
        std::thread::sleep(Duration::from_millis(300));
        Ok(
            json!({"activation_requested": activated, "process_id": pid, "frontmost_matches": self.desktop.frontmost_pid() == Some(pid),
                  "next": "Take a fresh screenshot before input."}),
        )
    }

    fn desktop_screenshot(&mut self) -> R<Json> {
        if !self.desktop.screen_recording() {
            return err(
                "PermissionError",
                "Enable Screen Recording for the terminal/agent host in macOS Privacy & Security",
            );
        }
        let p = self
            .run_dir
            .join(format!("desktop-{}.png", self.events.len()));
        let mut cmd = Command::new("/usr/sbin/screencapture");
        cmd.args(["-x", "-m"])
            .arg(&p)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let child = cmd
            .spawn()
            .or_else(|e| super::io_err(e, Path::new("/usr/sbin/screencapture")))?;
        let (code, timed_out, _, _) = self.wait_cancellable(child, Duration::from_secs(10))?;
        if timed_out || code != Some(0) || !p.is_file() {
            return err("CalledProcessError", "screencapture failed");
        }
        let (w, h) = self.desktop.display_size();
        // Retina captures are in pixels; scale to screen points so clicks line up.
        if png_size(&p) != Some((w as u32, h as u32)) {
            let ok = Command::new("/usr/bin/sips")
                .args(["-z", &(h as u32).to_string(), &(w as u32).to_string()])
                .arg(&p)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                return err(
                    "OSError",
                    "could not resize the screenshot to screen points",
                );
            }
        }
        Ok(
            json!({"image_path": p.to_string_lossy(), "screen_points": {"width": w, "height": h},
                  "coordinates": "Origin is upper-left. Qwen should use desktop_click coordinate_space=normalized_1000 with x,y on a 0–1000 grid across this full screenshot. Explicit screen_points also supported."}),
        )
    }

    pub fn desktop_click(&mut self, mut x: f64, mut y: f64, space: &str) -> R<Json> {
        self.input_permission()?;
        let (w, h) = self.desktop.display_size();
        if space == "normalized_1000" {
            if !(0.0..=1000.0).contains(&x) || !(0.0..=1000.0).contains(&y) {
                return err("ValueError", "Normalized coordinates must be 0–1000");
            }
            x = x * w / 1000.0;
            y = y * h / 1000.0;
        }
        if x >= w || y >= h {
            return err("ValueError", "Click is outside the main display");
        }
        if let Some(t) = self.native_target(true)? {
            let (bx, by, bw, bh) = t.bounds;
            if !(bx <= x && x < bx + bw && by <= y && y < by + bh) {
                return err(
                    "PermissionError",
                    "Click is outside the named desktop target",
                );
            }
        }
        self.desktop.click(x, y);
        Ok(json!({"clicked": [x, y]}))
    }

    fn desktop_type(&mut self, text: &str) -> R<Json> {
        self.focused_target()?;
        self.input_permission()?;
        if text.chars().count() > 4000 {
            return err("ValueError", "Text exceeds native input limit");
        }
        self.desktop.type_text(text);
        Ok(json!({"typed_characters": text.chars().count()}))
    }

    fn desktop_key(&mut self, key: &str) -> R<Json> {
        self.focused_target()?;
        self.input_permission()?;
        let code = KEYS
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, c)| *c)
            .ok_or_else(|| ToolError(format!("KeyError: '{key}'")))?;
        self.desktop.key(code, key.starts_with("cmd+"));
        Ok(json!({"key": key}))
    }
}

/// Width and height from a PNG's IHDR chunk.
pub fn png_size(p: &Path) -> Option<(u32, u32)> {
    let mut b = [0u8; 24];
    use std::io::Read;
    std::fs::File::open(p).ok()?.read_exact(&mut b).ok()?;
    (&b[..8] == b"\x89PNG\r\n\x1a\n" && &b[12..16] == b"IHDR").then(|| {
        (
            u32::from_be_bytes(b[16..20].try_into().unwrap()),
            u32::from_be_bytes(b[20..24].try_into().unwrap()),
        )
    })
}

// ---------- macOS FFI ----------

type CFTypeRef = *const c_void;
type Id = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGPoint,
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFArrayGetCount(a: CFTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(a: CFTypeRef, i: isize) -> CFTypeRef;
    fn CFDictionaryGetValue(d: CFTypeRef, k: CFTypeRef) -> CFTypeRef;
    fn CFStringCreateWithCString(alloc: CFTypeRef, s: *const libc::c_char, enc: u32) -> CFTypeRef;
    fn CFStringGetLength(s: CFTypeRef) -> isize;
    fn CFStringGetMaximumSizeForEncoding(len: isize, enc: u32) -> isize;
    fn CFStringGetCString(s: CFTypeRef, buf: *mut libc::c_char, size: isize, enc: u32) -> bool;
    fn CFNumberGetValue(n: CFTypeRef, kind: isize, out: *mut c_void) -> bool;
    fn CFBooleanGetValue(b: CFTypeRef) -> bool;
    fn CFGetTypeID(v: CFTypeRef) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFBooleanGetTypeID() -> usize;
    fn CFRelease(v: CFTypeRef);
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGWindowListCopyWindowInfo(option: u32, relative: u32) -> CFTypeRef;
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGMainDisplayID() -> u32;
    fn CGDisplayBounds(display: u32) -> CGRect;
    fn CGEventCreateMouseEvent(source: CFTypeRef, kind: u32, at: CGPoint, button: u32)
        -> CFTypeRef;
    fn CGEventCreateKeyboardEvent(source: CFTypeRef, key: u16, down: bool) -> CFTypeRef;
    fn CGEventKeyboardSetUnicodeString(e: CFTypeRef, len: usize, s: *const u16);
    fn CGEventSetFlags(e: CFTypeRef, flags: u64);
    fn CGEventPost(tap: u32, e: CFTypeRef);
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

#[link(name = "AppKit", kind = "framework")]
extern "C" {}

#[link(name = "objc")]
extern "C" {
    fn objc_getClass(name: *const libc::c_char) -> Id;
    fn sel_registerName(name: *const libc::c_char) -> Id;
    fn objc_msgSend();
    fn objc_autoreleasePoolPush() -> *mut c_void;
    fn objc_autoreleasePoolPop(p: *mut c_void);
}

const UTF8: u32 = 0x0800_0100;

fn cfstr(s: &str) -> CFTypeRef {
    let c = CString::new(s).unwrap();
    unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), UTF8) }
}

unsafe fn get(d: CFTypeRef, key: &str) -> CFTypeRef {
    let k = cfstr(key);
    let v = CFDictionaryGetValue(d, k);
    CFRelease(k);
    v
}

unsafe fn to_string(v: CFTypeRef) -> Option<String> {
    if v.is_null() || CFGetTypeID(v) != CFStringGetTypeID() {
        return None;
    }
    let size = CFStringGetMaximumSizeForEncoding(CFStringGetLength(v), UTF8) + 1;
    let mut buf = vec![0 as libc::c_char; size as usize];
    CFStringGetCString(v, buf.as_mut_ptr(), size, UTF8).then(|| {
        std::ffi::CStr::from_ptr(buf.as_ptr())
            .to_string_lossy()
            .into_owned()
    })
}

unsafe fn to_f64(v: CFTypeRef) -> Option<f64> {
    if v.is_null() {
        return None;
    }
    if CFGetTypeID(v) == CFBooleanGetTypeID() {
        return Some(if CFBooleanGetValue(v) { 1.0 } else { 0.0 });
    }
    if CFGetTypeID(v) != CFNumberGetTypeID() {
        return None;
    }
    let mut out = 0f64;
    CFNumberGetValue(v, 13, &mut out as *mut f64 as *mut c_void).then_some(out) // kCFNumberDoubleType
}

fn sel(name: &str) -> Id {
    let c = CString::new(name).unwrap();
    unsafe { sel_registerName(c.as_ptr()) }
}

fn class(name: &str) -> Id {
    let c = CString::new(name).unwrap();
    unsafe { objc_getClass(c.as_ptr()) }
}

unsafe fn send_id(obj: Id, s: &str) -> Id {
    let f: unsafe extern "C" fn(Id, Id) -> Id =
        std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
    if obj.is_null() {
        obj
    } else {
        f(obj, sel(s))
    }
}

/// The real macOS implementation.
pub struct Native;

impl Desktop for Native {
    fn windows(&self) -> Vec<Window> {
        unsafe {
            // kCGWindowListOptionAll | kCGWindowListExcludeDesktopElements, kCGNullWindowID
            let list = CGWindowListCopyWindowInfo(1 << 4, 0);
            if list.is_null() {
                return vec![];
            }
            let mut out = vec![];
            for i in 0..CFArrayGetCount(list) {
                let d = CFArrayGetValueAtIndex(list, i);
                let b = get(d, "kCGWindowBounds");
                let n = |k: &str| {
                    if b.is_null() {
                        0.0
                    } else {
                        to_f64(get(b, k)).unwrap_or(0.0)
                    }
                };
                out.push(Window {
                    owner: to_string(get(d, "kCGWindowOwnerName")),
                    process_id: to_f64(get(d, "kCGWindowOwnerPID")).unwrap_or(0.0) as i64,
                    name: to_string(get(d, "kCGWindowName")),
                    on_screen: to_f64(get(d, "kCGWindowIsOnscreen")).unwrap_or(0.0) != 0.0,
                    layer: to_f64(get(d, "kCGWindowLayer")).unwrap_or(-1.0) as i64,
                    bounds: (n("X"), n("Y"), n("Width"), n("Height")),
                });
            }
            CFRelease(list);
            out
        }
    }

    fn accessibility(&self) -> bool {
        unsafe { AXIsProcessTrusted() }
    }

    fn screen_recording(&self) -> bool {
        unsafe { CGPreflightScreenCaptureAccess() }
    }

    fn display_size(&self) -> (f64, f64) {
        let r = unsafe { CGDisplayBounds(CGMainDisplayID()) };
        (r.size.x, r.size.y)
    }

    fn click(&self, x: f64, y: f64) {
        unsafe {
            for kind in [1u32, 2] {
                // kCGEventLeftMouseDown, kCGEventLeftMouseUp
                let e = CGEventCreateMouseEvent(std::ptr::null(), kind, CGPoint { x, y }, 0);
                if !e.is_null() {
                    CGEventPost(0, e); // kCGHIDEventTap
                    CFRelease(e);
                }
            }
        }
    }

    fn type_text(&self, text: &str) {
        // One keyboard event carries at most 20 UTF-16 units; split without breaking surrogate pairs.
        let units: Vec<u16> = text.encode_utf16().collect();
        let mut i = 0;
        while i < units.len() {
            let mut end = (i + 20).min(units.len());
            if end < units.len() && (0xD800..0xDC00).contains(&units[end - 1]) {
                end -= 1;
            }
            for down in [true, false] {
                unsafe {
                    let e = CGEventCreateKeyboardEvent(std::ptr::null(), 0, down);
                    if !e.is_null() {
                        CGEventKeyboardSetUnicodeString(e, end - i, units[i..end].as_ptr());
                        CGEventPost(0, e);
                        CFRelease(e);
                    }
                }
            }
            i = end;
        }
    }

    fn key(&self, code: u16, command: bool) {
        for down in [true, false] {
            unsafe {
                let e = CGEventCreateKeyboardEvent(std::ptr::null(), code, down);
                if !e.is_null() {
                    if command {
                        CGEventSetFlags(e, 1 << 20); // kCGEventFlagMaskCommand
                    }
                    CGEventPost(0, e);
                    CFRelease(e);
                }
            }
        }
    }

    fn frontmost_pid(&self) -> Option<i64> {
        unsafe {
            let pool = objc_autoreleasePoolPush();
            let app = send_id(
                send_id(class("NSWorkspace"), "sharedWorkspace"),
                "frontmostApplication",
            );
            let pid = (!app.is_null()).then(|| {
                let f: unsafe extern "C" fn(Id, Id) -> i32 =
                    std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
                f(app, sel("processIdentifier")) as i64
            });
            objc_autoreleasePoolPop(pool);
            pid
        }
    }

    fn activate(&self, pid: i64) -> Result<bool, String> {
        unsafe {
            let pool = objc_autoreleasePoolPush();
            let by_pid: unsafe extern "C" fn(Id, Id, i32) -> Id =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let app = by_pid(
                class("NSRunningApplication"),
                sel("runningApplicationWithProcessIdentifier:"),
                pid as i32,
            );
            let r = if app.is_null() {
                Err("Observed application is no longer running".to_string())
            } else {
                let act: unsafe extern "C" fn(Id, Id, usize) -> bool =
                    std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
                Ok(act(app, sel("activateWithOptions:"), 1 | 2)) // AllWindows | IgnoringOtherApps
            };
            objc_autoreleasePoolPop(pool);
            r
        }
    }
}
