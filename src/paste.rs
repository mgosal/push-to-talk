//! Clipboard management and keystroke injection for paste-at-cursor.

use objc2_app_kit::NSPasteboard;
use objc2_foundation::NSString;

/// Deliver `text` to the focused application.
///
/// The clipboard is always set, whichever delivery method is used, so
/// clipboard history tools (e.g. Better Clipboard) still observe the
/// transcript and the user can paste it again by hand.
pub fn deliver(text: &str, method: crate::config::PasteMethod) {
    set_clipboard(text);
    // Let the clipboard settle before anything reads it.
    pause(50);
    match method {
        crate::config::PasteMethod::Shortcut => send_cmd_v(),
        crate::config::PasteMethod::Type => type_text(text),
        crate::config::PasteMethod::AppleScript => send_cmd_v_via_system_events(),
        // Nothing to inject: the text is on the clipboard and the user pastes.
        crate::config::PasteMethod::Clipboard => {}
    }
}

/// Ask System Events to press ⌘V.
///
/// This goes through the Accessibility API instead of synthesising the events
/// here. Some applications that ignore a Core Graphics ⌘V honour this one,
/// because the modifier is established by the system rather than by flags we
/// set ourselves. Costs an `osascript` process, which is why it is not the
/// default.
fn send_cmd_v_via_system_events() {
    let status = std::process::Command::new("osascript")
        .args([
            "-e",
            "tell application \"System Events\" to keystroke \"v\" using command down",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output();
    match status {
        Ok(out) if !out.status.success() => {
            let err = String::from_utf8_lossy(&out.stderr);
            eprintln!("[ptt] AppleScript paste failed: {}", err.trim());
        }
        Err(e) => eprintln!("[ptt] Could not run osascript: {e}"),
        _ => {}
    }
}

/// Type `text` directly as Unicode key events, using no modifier at all.
///
/// Remote desktop sessions relay keystrokes to another machine and reconstruct
/// modifier state as they go; a synthetic ⌘ is the fragile part of a paste.
/// Typing sidesteps the modifier entirely — there is nothing to reconstruct.
/// It is slower and arrives character by character, so it is opt-in.
fn type_text(text: &str) {
    // Post in small batches: a single event carrying a long string is
    // truncated by some applications.
    const BATCH: usize = 16;
    let utf16: Vec<u16> = text.encode_utf16().collect();

    for chunk in utf16.chunks(BATCH) {
        for down in [true, false] {
            unsafe {
                // Keycode 0 with an attached Unicode string: the character is
                // taken from the string, not from a physical key, so this is
                // independent of the user's keyboard layout.
                let event = CGEventCreateKeyboardEvent(std::ptr::null_mut(), 0, down);
                if event.is_null() {
                    return;
                }
                CGEventKeyboardSetUnicodeString(event, chunk.len(), chunk.as_ptr());
                CGEventPost(HID_EVENT_TAP, event);
                CFRelease(event);
            }
        }
        // Give the receiver time to keep up; a remote session especially.
        pause(4);
    }
}

/// Set the clipboard to the given text and paste via Cmd+V.
/// The transcription remains on the clipboard afterward so clipboard
/// history tools (e.g. Better Clipboard) can observe it.
/// Set the clipboard without delivering anything. Used by `--last`.
pub fn set_clipboard_public(text: &str) {
    set_clipboard(text);
}

fn set_clipboard(text: &str) {
    let pb = { NSPasteboard::generalPasteboard() };
    {
        pb.clearContents();
    }
    let ns_text = NSString::from_str(text);
    let text_type = NSString::from_str("public.utf8-plain-text");
    {
        pb.setString_forType(&ns_text, &text_type);
    }
}

use std::ffi::c_void;

// CGEvent FFI for keystroke injection
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventCreateKeyboardEvent(
        source: *mut c_void,
        keycode: u16,
        key_down: bool,
    ) -> *mut c_void;
    fn CGEventSetFlags(event: *mut c_void, flags: u64);
    fn CGEventSetType(event: *mut c_void, event_type: u32);
    fn CGEventKeyboardSetUnicodeString(
        event: *mut c_void,
        string_length: usize,
        unicode_string: *const u16,
    );
    fn CGEventPost(tap: u32, event: *mut c_void);
    fn CFRelease(cf: *mut c_void);
}

const V_KEYCODE: u16 = 9;
/// Left Command. Posted as a real key event, not just a flag — see [`send_cmd_v`].
const CMD_KEYCODE: u16 = 55;
const CMD_FLAG: u64 = 0x100000; // kCGEventFlagMaskCommand
const HID_EVENT_TAP: u32 = 0; // kCGHIDEventTap
/// kCGEventFlagsChanged. macOS reports a modifier key press with this event
/// type, never as a keyDown — the app's own hotkey monitor relies on the same
/// fact to detect right Option.
const EVENT_FLAGS_CHANGED: u32 = 12;

/// Post one keyboard event with an explicit modifier mask.
fn post_key(keycode: u16, key_down: bool, flags: u64) {
    post_event(keycode, key_down, flags, false);
}

/// Post a modifier key press or release the way the window server does it.
///
/// A modifier is not a keyDown. Creating the event with
/// `CGEventCreateKeyboardEvent` and posting it as-is produces a keyDown
/// carrying the Command keycode, which is a shape macOS never emits and which
/// modifier-tracking code ignores. Retyping it to `kCGEventFlagsChanged` is
/// what makes it look like a real ⌘ press.
fn post_modifier(keycode: u16, key_down: bool, flags: u64) {
    post_event(keycode, key_down, flags, true);
}

fn post_event(keycode: u16, key_down: bool, flags: u64, as_flags_changed: bool) {
    unsafe {
        let event = CGEventCreateKeyboardEvent(std::ptr::null_mut(), keycode, key_down);
        if event.is_null() {
            return;
        }
        if as_flags_changed {
            CGEventSetType(event, EVENT_FLAGS_CHANGED);
        }
        // Set flags explicitly rather than inheriting whatever the user happens
        // to be holding.
        CGEventSetFlags(event, flags);
        CGEventPost(HID_EVENT_TAP, event);
        CFRelease(event);
    }
}

fn pause(ms: u64) {
    std::thread::sleep(std::time::Duration::from_millis(ms));
}

/// Send Cmd+V as the window server would.
///
/// Native apps read `kCGEventFlagMaskCommand` straight off the V keystroke, so
/// for them the flag alone is enough. Apps that *forward* keyboard input —
/// Chrome Remote Desktop, Apple Screen Sharing, VNC clients, VMs — instead
/// track modifier state from the Command key's own `kCGEventFlagsChanged`
/// events, and relay a synthesised keystroke to the far end. Without a
/// flagsChanged they believe no modifier is held and forward a bare "v", which
/// is why the remote text box received the letter V rather than a paste.
///
/// The gaps give a remote client time to relay the modifier change over the
/// network before the V arrives; they are generous because a paste happens
/// once per dictation and 100ms is not felt.
fn send_cmd_v() {
    // ⌘ down, as a flagsChanged — the event forwarding apps were missing.
    post_modifier(CMD_KEYCODE, true, CMD_FLAG);
    pause(40);

    // V down, V up, both while Command is held.
    post_key(V_KEYCODE, true, CMD_FLAG);
    pause(30);
    post_key(V_KEYCODE, false, CMD_FLAG);
    pause(40);

    // ⌘ up, with the mask cleared so the modifier is released cleanly. Leaving
    // it set here can strand apps believing Command is still down.
    post_modifier(CMD_KEYCODE, false, 0);
}
