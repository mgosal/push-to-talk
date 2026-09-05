//! Clipboard management and keystroke injection for paste-at-cursor.

use objc2_app_kit::NSPasteboard;
use objc2_foundation::NSString;

/// Set the clipboard to the given text and paste via Cmd+V.
/// The transcription remains on the clipboard afterward so clipboard
/// history tools (e.g. Better Clipboard) can observe it.
pub fn paste_text(text: &str) {
    let pb = { NSPasteboard::generalPasteboard() };

    // Set clipboard to transcription
    {
        pb.clearContents();
    }
    let ns_text = NSString::from_str(text);
    let text_type = NSString::from_str("public.utf8-plain-text");
    {
        pb.setString_forType(&ns_text, &text_type);
    }

    // Small delay to let clipboard settle
    std::thread::sleep(std::time::Duration::from_millis(50));

    // Send Cmd+V keystroke via CGEvent
    send_cmd_v();
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
    fn CGEventPost(tap: u32, event: *mut c_void);
    fn CFRelease(cf: *mut c_void);
}

const V_KEYCODE: u16 = 9;
/// Left Command. Posted as a real key event, not just a flag — see [`send_cmd_v`].
const CMD_KEYCODE: u16 = 55;
const CMD_FLAG: u64 = 0x100000; // kCGEventFlagMaskCommand
const HID_EVENT_TAP: u32 = 0; // kCGHIDEventTap

/// Post one keyboard event with an explicit modifier mask.
fn post_key(keycode: u16, key_down: bool, flags: u64) {
    unsafe {
        let event = CGEventCreateKeyboardEvent(std::ptr::null_mut(), keycode, key_down);
        if event.is_null() {
            return;
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

/// Send Cmd+V as a physical key sequence.
///
/// The Command key is pressed and released as a real key event rather than
/// only setting `kCGEventFlagMaskCommand` on the V event. Native apps read the
/// flag off the keystroke and either approach works, but apps that forward
/// keyboard input elsewhere — Chrome Remote Desktop, VNC clients, VMs — track
/// modifier state from the Command key's own down/up events. With flags alone
/// they never saw Command go down, so they forwarded a bare "v" and the remote
/// text box received the letter V instead of a paste.
fn send_cmd_v() {
    // ⌘ down — this is the event modifier-tracking apps were missing.
    post_key(CMD_KEYCODE, true, CMD_FLAG);
    pause(15);

    // V down, V up, both while Command is held.
    post_key(V_KEYCODE, true, CMD_FLAG);
    pause(20);
    post_key(V_KEYCODE, false, CMD_FLAG);
    pause(15);

    // ⌘ up, with the mask cleared so the modifier is released cleanly. Leaving
    // it set here can strand apps believing Command is still down.
    post_key(CMD_KEYCODE, false, 0);
}
