//! Clipboard management and keystroke injection for paste-at-cursor.

use objc2_app_kit::NSPasteboard;
use objc2_foundation::NSString;

/// Backup the current clipboard contents, set new text, paste via Cmd+V,
/// then restore the original clipboard after a short delay.
pub fn paste_text(text: &str) {
    let pb = unsafe { NSPasteboard::generalPasteboard() };

    // Backup current clipboard
    let backup = unsafe {
        pb.stringForType(&NSString::from_str("public.utf8-plain-text"))
    };

    // Set clipboard to transcription
    unsafe {
        pb.clearContents();
    }
    let ns_text = NSString::from_str(text);
    let text_type = NSString::from_str("public.utf8-plain-text");
    unsafe {
        pb.setString_forType(&ns_text, &text_type);
    }

    // Small delay to let clipboard settle
    std::thread::sleep(std::time::Duration::from_millis(50));

    // Send Cmd+V keystroke via CGEvent
    send_cmd_v();

    // Restore clipboard after paste completes
    std::thread::sleep(std::time::Duration::from_millis(200));
    if let Some(original) = backup {
        unsafe {
            pb.clearContents();
            pb.setString_forType(&original, &text_type);
        }
    }
}

/// Send Cmd+V keystroke via Core Graphics events.
fn send_cmd_v() {
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
    const CMD_FLAG: u64 = 0x100000; // kCGEventFlagMaskCommand

    unsafe {
        // Key down
        let down = CGEventCreateKeyboardEvent(std::ptr::null_mut(), V_KEYCODE, true);
        if !down.is_null() {
            CGEventSetFlags(down, CMD_FLAG);
            CGEventPost(0, down); // kCGHIDEventTap = 0
            CFRelease(down);
        }

        std::thread::sleep(std::time::Duration::from_millis(20));

        // Key up
        let up = CGEventCreateKeyboardEvent(std::ptr::null_mut(), V_KEYCODE, false);
        if !up.is_null() {
            CGEventSetFlags(up, CMD_FLAG);
            CGEventPost(0, up);
            CFRelease(up);
        }
    }
}
