//! Global hotkey monitoring via CGEventTap.
//!
//! Monitors right-Option key (push-to-talk) using a Core Graphics event tap.
//! Runs on a dedicated thread with its own CFRunLoop.

use std::ffi::c_void;
use std::sync::{Arc, Mutex};

/// Hotkey events sent from the event tap thread.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HotkeyEvent {
    PushDown,
    PushUp,
    /// Left arrow pressed while right Option is held.
    /// Main thread decides whether to engage locked mode based on app state.
    LeftArrowDown,
}

/// Shared state between event tap callback and app.
pub struct HotkeyState {
    pub events: Vec<HotkeyEvent>,
    pub option_held: bool,
}

impl HotkeyState {
    pub fn new() -> Self {
        Self {
            events: Vec::new(),
            option_held: false,
        }
    }

    pub fn drain_events(&mut self) -> Vec<HotkeyEvent> {
        std::mem::take(&mut self.events)
    }
}

// ── Accessibility check ───────────────────────────────────────────────
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFDictionaryCreate(
        allocator: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        num_values: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> *const c_void;

    static kCFBooleanTrue: *const c_void;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
}

// kAXTrustedCheckOptionPrompt key string
extern "C" {
    static kAXTrustedCheckOptionPrompt: *const c_void;
}

/// Check if the process already has Accessibility access.
pub fn is_accessibility_trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// Prompt macOS for Accessibility access. This should only be called from an
/// explicit setup action so first launch does not stack multiple prompts.
pub fn request_accessibility() -> bool {
    unsafe {
        if AXIsProcessTrusted() {
            eprintln!("[ptt] Accessibility access: granted");
            return true;
        }

        eprintln!("[ptt] Accessibility access: NOT granted — requesting...");

        let keys = [kAXTrustedCheckOptionPrompt];
        let values = [kCFBooleanTrue];

        let options = CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            &kCFTypeDictionaryKeyCallBacks as *const _ as *const c_void,
            &kCFTypeDictionaryValueCallBacks as *const _ as *const c_void,
        );

        let trusted = AXIsProcessTrustedWithOptions(options);

        if !trusted {
            eprintln!(
                "[ptt] ⚠ Accessibility access required.\n\
                 [ptt]   1. System Settings → Privacy & Security → Accessibility\n\
                 [ptt]   2. Toggle Push to Talk ON\n\
                 [ptt]   3. Return to setup and re-check"
            );
        }

        trusted
    }
}

// ── Raw CGEventTap FFI ────────────────────────────────────────────────
type CGEventRef = *mut c_void;
type CFMachPortRef = *mut c_void;

type CGEventTapCallBack = extern "C" fn(
    proxy: *mut c_void,
    event_type: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;

    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetFlags(event: CGEventRef) -> u64;
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFMachPortCreateRunLoopSource(
        allocator: *mut c_void,
        port: CFMachPortRef,
        order: isize,
    ) -> *mut c_void;

    fn CFRunLoopAddSource(rl: *mut c_void, source: *mut c_void, mode: *mut c_void);
    fn CFRunLoopGetCurrent() -> *mut c_void;
    fn CFRunLoopRun();

    static kCFRunLoopDefaultMode: *mut c_void;
}

const K_CG_SESSION_EVENT_TAP: u32 = 1;
const K_CG_HEAD_INSERT_EVENT_TAP: u32 = 0;
const K_CG_EVENT_TAP_OPTION_LISTEN_ONLY: u32 = 1;
const K_CG_EVENT_FLAGS_CHANGED: u32 = 12;
const K_CG_EVENT_KEY_DOWN: u32 = 10;

// CGEventField for keycode extraction
const K_CG_KEYBOARD_EVENT_KEYCODE: u32 = 9;

// Key codes
const LEFT_ARROW_KEYCODE: i64 = 123;

// Right Option key flag (NX_DEVICERALTKEYMASK)
const NX_DEVICERALTKEYMASK: u64 = 0x40;

/// CGEventTap callback — runs on the event tap thread.
extern "C" fn event_callback(
    _proxy: *mut c_void,
    event_type: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    if event.is_null() || user_info.is_null() {
        return event;
    }

    let state = unsafe { &*(user_info as *const Mutex<HotkeyState>) };

    match event_type {
        K_CG_EVENT_FLAGS_CHANGED => {
            let flags = unsafe { CGEventGetFlags(event) };
            let right_opt_down = (flags & NX_DEVICERALTKEYMASK) != 0;

            if let Ok(mut s) = state.lock() {
                if right_opt_down && !s.option_held {
                    s.option_held = true;
                    s.events.push(HotkeyEvent::PushDown);
                    eprintln!("[ptt] ▶ Push down (flags=0x{flags:x})");
                } else if !right_opt_down && s.option_held {
                    s.option_held = false;
                    s.events.push(HotkeyEvent::PushUp);
                    eprintln!("[ptt] ■ Push up (flags=0x{flags:x})");
                }
            }
        }
        K_CG_EVENT_KEY_DOWN => {
            let keycode = unsafe { CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE) };
            if keycode == LEFT_ARROW_KEYCODE {
                if let Ok(s) = state.lock() {
                    if s.option_held {
                        drop(s);
                        if let Ok(mut s) = state.lock() {
                            s.events.push(HotkeyEvent::LeftArrowDown);
                            eprintln!("[ptt] ⇠ Left arrow (opt held)");
                        }
                    }
                }
            }
        }
        _ => {}
    }

    event
}

/// Start the global hotkey monitor on a background thread.
/// Returns the shared state handle.
pub fn start_monitor() -> Arc<Mutex<HotkeyState>> {
    let state = Arc::new(Mutex::new(HotkeyState::new()));
    let state_clone = Arc::clone(&state);

    std::thread::Builder::new()
        .name("hotkey-monitor".into())
        .spawn(move || {
            let event_mask: u64 = (1 << K_CG_EVENT_FLAGS_CHANGED) | (1 << K_CG_EVENT_KEY_DOWN);

            // Leak a reference for the C callback (lives for app lifetime)
            let state_ptr = Arc::into_raw(state_clone) as *mut c_void;

            unsafe {
                let tap = CGEventTapCreate(
                    K_CG_SESSION_EVENT_TAP,
                    K_CG_HEAD_INSERT_EVENT_TAP,
                    K_CG_EVENT_TAP_OPTION_LISTEN_ONLY,
                    event_mask,
                    event_callback,
                    state_ptr,
                );

                if tap.is_null() {
                    eprintln!(
                        "[ptt] FATAL: CGEventTapCreate returned NULL.\n\
                         [ptt]   Accessibility access was not granted.\n\
                         [ptt]   Restart after granting access."
                    );
                    return;
                }

                let source = CFMachPortCreateRunLoopSource(std::ptr::null_mut(), tap, 0);
                let run_loop = CFRunLoopGetCurrent();
                CFRunLoopAddSource(run_loop, source, kCFRunLoopDefaultMode);
                CGEventTapEnable(tap, true);

                eprintln!("[ptt] Hotkey monitor active (right Option = push-to-talk, +left arrow = lock)");
                CFRunLoopRun(); // blocks forever
            }
        })
        .expect("Failed to spawn hotkey thread");

    state
}
