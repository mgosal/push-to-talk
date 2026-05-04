//! History & Corrections window — native Cocoa table + editor.
//!
//! Displays recent dictations in an NSTableView with an editable NSTextView
//! for corrections.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::*;
use objc2_foundation::*;

use crate::db;

// ── Send-safe pointer wrapper ─────────────────────────────────────────

/// Wrapper for raw Cocoa object pointers stored in statics.
/// These are only ever accessed from the main thread.
struct Ptr(*mut c_void);
unsafe impl Send for Ptr {}

impl Ptr {
    fn null() -> Self { Ptr(std::ptr::null_mut()) }
    fn get(&self) -> Option<*mut c_void> {
        if self.0.is_null() { None } else { Some(self.0) }
    }
}

// ── Shared data for the history window ────────────────────────────────

struct HistoryData {
    rows: Vec<db::DictationRow>,
    selected_index: Option<usize>,
    db_path: PathBuf,
}

static HISTORY_DATA: Mutex<Option<HistoryData>> = Mutex::new(None);
static HISTORY_WINDOW: Mutex<Ptr> = Mutex::new(Ptr(std::ptr::null_mut()));
static HISTORY_TABLE: Mutex<Ptr> = Mutex::new(Ptr(std::ptr::null_mut()));
static HISTORY_TEXT_VIEW: Mutex<Ptr> = Mutex::new(Ptr(std::ptr::null_mut()));
static HISTORY_STATUS_LABEL: Mutex<Ptr> = Mutex::new(Ptr(std::ptr::null_mut()));

// ── History delegate (data source only) ───────────────────────────────
// NSTableViewDelegate requires NSControlTextEditingDelegate, which adds
// complexity. Instead, we set the dataSource and handle selection changes
// via msg_send! to avoid the full protocol chain.

struct HistoryDelegateIvars;

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PTTHistoryDelegate"]
    #[ivars = HistoryDelegateIvars]
    struct HistoryDelegate;

    unsafe impl NSObjectProtocol for HistoryDelegate {}

    unsafe impl NSTableViewDataSource for HistoryDelegate {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table_view: &NSTableView) -> isize {
            HISTORY_DATA
                .lock()
                .ok()
                .and_then(|d| d.as_ref().map(|h| h.rows.len() as isize))
                .unwrap_or(0)
        }

        #[unsafe(method(tableView:objectValueForTableColumn:row:))]
        fn object_value(
            &self,
            _table_view: &NSTableView,
            column: &NSTableColumn,
            row: isize,
        ) -> *mut AnyObject {
            let data = HISTORY_DATA.lock().ok();
            let data = match data.as_ref().and_then(|d| d.as_ref()) {
                Some(d) => d,
                None => return std::ptr::null_mut(),
            };

            let idx = row as usize;
            if idx >= data.rows.len() {
                return std::ptr::null_mut();
            }

            let row_data = &data.rows[idx];
            let col_id = unsafe { column.identifier() };
            let col_str = col_id.to_string();

            let text = match col_str.as_str() {
                "status" => match row_data.status.as_str() {
                    "success" => "✓".to_string(),
                    "hallucination" => "⚠".to_string(),
                    "error" => "✗".to_string(),
                    _ => "?".to_string(),
                },
                "time" => {
                    if row_data.timestamp.len() > 11 {
                        row_data.timestamp[..row_data.timestamp.len().min(16)].to_string()
                    } else {
                        row_data.timestamp.clone()
                    }
                }
                "latency" => row_data
                    .latency_s
                    .map(|l| format!("{l:.1}s"))
                    .unwrap_or_else(|| "—".to_string()),
                "transcript" => {
                    let t = row_data
                        .transcript
                        .as_deref()
                        .or(row_data.error_detail.as_deref())
                        .unwrap_or("");
                    t.chars().take(50).collect()
                }
                _ => String::new(),
            };

            let ns_str = NSString::from_str(&text);
            let retained = Retained::into_raw(ns_str);
            retained as *mut AnyObject
        }
    }

    // Handle selection changes and save corrections via NSApplicationDelegate
    // (a convenient protocol to attach ad-hoc action methods).
    unsafe impl NSApplicationDelegate for HistoryDelegate {
        #[unsafe(method(tableViewSelectionDidChange:))]
        fn selection_changed(&self, notification: &NSNotification) {
            let table: &NSTableView = unsafe {
                let obj = notification.object().unwrap();
                &*(Retained::as_ptr(&obj) as *const NSTableView)
            };
            let selected = table.selectedRow();
            if selected < 0 {
                return;
            }

            let idx = selected as usize;

            let (text, status_text) = {
                let mut data = match HISTORY_DATA.lock().ok() {
                    Some(d) => d,
                    None => return,
                };
                let data = match data.as_mut() {
                    Some(d) => d,
                    None => return,
                };
                data.selected_index = Some(idx);

                if idx >= data.rows.len() {
                    return;
                }

                let row = &data.rows[idx];
                let transcript = row.corrected.as_deref()
                    .or(row.transcript.as_deref())
                    .unwrap_or("");

                let status = format!(
                    "Status: {} | Duration: {}s | WPS: {}{}",
                    row.status,
                    row.duration_s.map(|d| format!("{d:.1}")).unwrap_or_else(|| "—".into()),
                    row.wps.map(|w| format!("{w:.1}")).unwrap_or_else(|| "—".into()),
                    if row.corrected.is_some() { " | Corrected" } else { "" },
                );

                (transcript.to_string(), status)
            };

            // Update text view
            if let Some(ptr) = HISTORY_TEXT_VIEW.lock().ok().and_then(|p| p.get()) {
                let text_view: &NSTextView = unsafe { &*(ptr as *const NSTextView) };
                unsafe { text_view.setString(&NSString::from_str(&text)); }
            }

            // Update status label
            if let Some(ptr) = HISTORY_STATUS_LABEL.lock().ok().and_then(|p| p.get()) {
                let label: &NSTextField = unsafe { &*(ptr as *const NSTextField) };
                label.setStringValue(&NSString::from_str(&status_text));
            }
        }

        #[unsafe(method(saveCorrection:))]
        fn save_correction(&self, _sender: &NSObject) {
            let corrected_text = HISTORY_TEXT_VIEW
                .lock()
                .ok()
                .and_then(|p| p.get())
                .map(|ptr| {
                    let text_view: &NSTextView = unsafe { &*(ptr as *const NSTextView) };
                    unsafe { text_view.string().to_string() }
                });

            let corrected_text = match corrected_text {
                Some(t) => t,
                None => return,
            };

            if let Ok(mut data) = HISTORY_DATA.lock() {
                if let Some(ref mut data) = *data {
                    if let Some(idx) = data.selected_index {
                        if idx < data.rows.len() {
                            let row_id = data.rows[idx].id;
                            db::save_correction(&data.db_path, row_id, &corrected_text);
                            data.rows[idx].corrected = Some(corrected_text);
                            eprintln!("[ptt] Correction saved for row {row_id}");

                            if let Some(ptr) = HISTORY_STATUS_LABEL.lock().ok().and_then(|p| p.get()) {
                                let label: &NSTextField = unsafe { &*(ptr as *const NSTextField) };
                                let row = &data.rows[idx];
                                let status = format!(
                                    "Status: {} | Duration: {}s | WPS: {} | Corrected ✓",
                                    row.status,
                                    row.duration_s.map(|d| format!("{d:.1}")).unwrap_or_else(|| "—".into()),
                                    row.wps.map(|w| format!("{w:.1}")).unwrap_or_else(|| "—".into()),
                                );
                                label.setStringValue(&NSString::from_str(&status));
                            }
                        }
                    }
                }
            }

            // Reload table
            if let Some(ptr) = HISTORY_TABLE.lock().ok().and_then(|p| p.get()) {
                let table: &NSTableView = unsafe { &*(ptr as *const NSTableView) };
                table.reloadData();
            }
        }
    }
);

impl HistoryDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = mtm.alloc::<Self>();
        let this = this.set_ivars(HistoryDelegateIvars);
        unsafe { msg_send![super(this), init] }
    }
}

// ── Public API ────────────────────────────────────────────────────────

/// Open the History & Corrections window, or bring it to front if already open.
pub fn show(mtm: MainThreadMarker, db_path: &std::path::Path) {
    // Check if window already exists
    if let Ok(guard) = HISTORY_WINDOW.lock() {
        if let Some(ptr) = guard.get() {
            let window: &NSWindow = unsafe { &*(ptr as *const NSWindow) };
            window.makeKeyAndOrderFront(None);
            unsafe {
                NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
            }
            reload_data(db_path);
            if let Some(ptr) = HISTORY_TABLE.lock().ok().and_then(|p| p.get()) {
                let table: &NSTableView = unsafe { &*(ptr as *const NSTableView) };
                table.reloadData();
            }
            return;
        }
    }

    create_window(mtm, db_path);
}

fn reload_data(db_path: &std::path::Path) {
    let rows = db::recent(db_path, 50);
    *HISTORY_DATA.lock().unwrap() = Some(HistoryData {
        rows,
        selected_index: None,
        db_path: db_path.to_path_buf(),
    });
}

fn create_window(mtm: MainThreadMarker, db_path: &std::path::Path) {
    reload_data(db_path);

    let delegate = HistoryDelegate::new(mtm);

    // ── Window ────────────────────────────────────────────────────
    let frame = NSRect::new(NSPoint::new(200.0, 200.0), NSSize::new(720.0, 560.0));
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Resizable
        | NSWindowStyleMask::Miniaturizable;

    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc(),
            frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setTitle(&NSString::from_str("History & Corrections"));
    window.setMinSize(NSSize::new(500.0, 400.0));

    let content_view = window.contentView().unwrap();
    let content_bounds = content_view.bounds();
    let w = content_bounds.size.width;
    let h = content_bounds.size.height;

    // ── Table columns ─────────────────────────────────────────────
    let col_status = unsafe {
        NSTableColumn::initWithIdentifier(mtm.alloc(), &NSString::from_str("status"))
    };
    col_status.setWidth(30.0);
    unsafe { col_status.headerCell().setStringValue(&NSString::from_str("⚑")); }

    let col_time = unsafe {
        NSTableColumn::initWithIdentifier(mtm.alloc(), &NSString::from_str("time"))
    };
    col_time.setWidth(140.0);
    unsafe { col_time.headerCell().setStringValue(&NSString::from_str("Time")); }

    let col_latency = unsafe {
        NSTableColumn::initWithIdentifier(mtm.alloc(), &NSString::from_str("latency"))
    };
    col_latency.setWidth(60.0);
    unsafe { col_latency.headerCell().setStringValue(&NSString::from_str("Latency")); }

    let col_transcript = unsafe {
        NSTableColumn::initWithIdentifier(mtm.alloc(), &NSString::from_str("transcript"))
    };
    col_transcript.setWidth(w - 250.0);
    unsafe { col_transcript.headerCell().setStringValue(&NSString::from_str("Transcript")); }

    // ── Table view ────────────────────────────────────────────────
    let table_frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h * 0.45));
    let table = unsafe { NSTableView::initWithFrame(mtm.alloc(), table_frame) };

    table.addTableColumn(&col_status);
    table.addTableColumn(&col_time);
    table.addTableColumn(&col_latency);
    table.addTableColumn(&col_transcript);

    // Set data source via msg_send (typed protocol casts are tricky with define_class)
    unsafe {
        let delegate_ref = &*delegate as &AnyObject;
        let _: () = msg_send![&table, setDataSource: delegate_ref];
        let _: () = msg_send![&table, setDelegate: delegate_ref];
    }

    // Scroll view for table (top half)
    let scroll_frame = NSRect::new(NSPoint::new(0.0, h * 0.55), NSSize::new(w, h * 0.45));
    let table_scroll = unsafe { NSScrollView::initWithFrame(mtm.alloc(), scroll_frame) };
    table_scroll.setDocumentView(Some(&table));
    table_scroll.setHasVerticalScroller(true);
    unsafe {
        table_scroll.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
    }
    content_view.addSubview(&table_scroll);

    // ── Separator label ───────────────────────────────────────────
    let sep_frame = NSRect::new(NSPoint::new(10.0, h * 0.52), NSSize::new(w - 20.0, 20.0));
    let sep_label = unsafe { NSTextField::initWithFrame(mtm.alloc(), sep_frame) };
    sep_label.setStringValue(&NSString::from_str("Select a dictation to view or correct:"));
    sep_label.setBezeled(false);
    sep_label.setDrawsBackground(false);
    sep_label.setEditable(false);
    sep_label.setSelectable(false);
    unsafe {
        sep_label.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewMinYMargin,
        );
    }
    content_view.addSubview(&sep_label);

    // ── Text view (bottom half) ───────────────────────────────────
    let text_frame = NSRect::new(NSPoint::new(0.0, 40.0), NSSize::new(w, h * 0.50 - 40.0));
    let text_view = unsafe { NSTextView::initWithFrame(mtm.alloc(), text_frame) };
    text_view.setEditable(true);
    text_view.setRichText(false);
    unsafe {
        text_view.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(
            12.0,
            NSFontWeightRegular,
        )));
    }

    let text_scroll = unsafe { NSScrollView::initWithFrame(mtm.alloc(), text_frame) };
    text_scroll.setDocumentView(Some(&text_view));
    text_scroll.setHasVerticalScroller(true);
    unsafe {
        text_scroll.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
    }
    content_view.addSubview(&text_scroll);

    // ── Bottom bar: status label + save button ────────────────────
    let status_frame = NSRect::new(NSPoint::new(10.0, 10.0), NSSize::new(w - 160.0, 22.0));
    let status_label = unsafe { NSTextField::initWithFrame(mtm.alloc(), status_frame) };
    status_label.setStringValue(&NSString::from_str("Select a dictation above"));
    status_label.setBezeled(false);
    status_label.setDrawsBackground(false);
    status_label.setEditable(false);
    status_label.setSelectable(false);
    unsafe {
        status_label.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewMaxYMargin,
        );
    }
    content_view.addSubview(&status_label);

    let btn_frame = NSRect::new(NSPoint::new(w - 140.0, 6.0), NSSize::new(130.0, 28.0));
    let save_btn = unsafe { NSButton::initWithFrame(mtm.alloc(), btn_frame) };
    save_btn.setTitle(&NSString::from_str("Save Correction"));
    save_btn.setBezelStyle(NSBezelStyle::Rounded);
    unsafe {
        save_btn.setTarget(Some(&delegate));
        save_btn.setAction(Some(sel!(saveCorrection:)));
    }
    unsafe {
        save_btn.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewMinXMargin
                | NSAutoresizingMaskOptions::ViewMaxYMargin,
        );
    }
    content_view.addSubview(&save_btn);

    // ── Store references ──────────────────────────────────────────
    *HISTORY_TABLE.lock().unwrap() = Ptr(&*table as *const NSTableView as *mut c_void);
    *HISTORY_TEXT_VIEW.lock().unwrap() = Ptr(&*text_view as *const NSTextView as *mut c_void);
    *HISTORY_STATUS_LABEL.lock().unwrap() = Ptr(&*status_label as *const NSTextField as *mut c_void);
    *HISTORY_WINDOW.lock().unwrap() = Ptr(&*window as *const NSWindow as *mut c_void);

    // Show window
    window.makeKeyAndOrderFront(None);
    unsafe {
        NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
    }

    // Leak objects — they live for the app lifetime
    std::mem::forget(delegate);
    std::mem::forget(window);
    std::mem::forget(table);
    std::mem::forget(text_view);
    std::mem::forget(table_scroll);
    std::mem::forget(text_scroll);
    std::mem::forget(status_label);
    std::mem::forget(save_btn);
    std::mem::forget(sep_label);
    std::mem::forget(col_status);
    std::mem::forget(col_time);
    std::mem::forget(col_latency);
    std::mem::forget(col_transcript);
}
