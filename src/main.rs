//! Push-to-talk — voice typing for macOS
//!
//! Hold right Option to talk, release to transcribe and paste at cursor.
//! Supports locked dictation (right Opt + left arrow), history & corrections,
//! IPC control (--toggle, --status), and transcript file saving.

mod audio;
mod calibrate;
mod config;
mod db;
mod history;
mod hotkey;
mod ipc;
mod notification;
mod paste;
mod transcript;
mod transcribe;

use std::cell::{Cell, OnceCell, RefCell};
use std::path::PathBuf;
use std::process::Child;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType,
    NSBezelStyle, NSButton, NSMenu, NSMenuItem, NSPasteboard, NSPopUpButton, NSSecureTextField,
    NSStatusBar, NSStatusItem, NSTextField, NSVariableStatusItemLength, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{NSNotification, NSPoint, NSRect, NSSize, NSString, NSTimer};

// ── App State ─────────────────────────────────────────────────────────
#[derive(Clone, Copy, PartialEq, Debug)]
enum AppMode {
    Idle,
    Recording,
    /// Locked recording — hands-free, right Opt stops.
    Locked,
    Transcribing,
}

struct DelegateIvars {
    status_item: OnceCell<Retained<NSStatusItem>>,
    status_menu_item: OnceCell<Retained<NSMenuItem>>,
    toggle_menu_item: OnceCell<Retained<NSMenuItem>>,
    history_menu_item: OnceCell<Retained<NSMenuItem>>,
    stats_menu_item: OnceCell<Retained<NSMenuItem>>,
    hotkey_state: OnceCell<Arc<Mutex<hotkey::HotkeyState>>>,
    ipc_state: OnceCell<Arc<Mutex<ipc::IpcState>>>,
    onboarding_window: RefCell<Option<Retained<NSWindow>>>,
    api_key_field: RefCell<Option<Retained<NSSecureTextField>>>,
    provider_popup: RefCell<Option<Retained<NSPopUpButton>>>,
    onboarding_status_label: RefCell<Option<Retained<NSTextField>>>,
    mode: Cell<AppMode>,
    recording_child: RefCell<Option<Child>>,
    recording_path: RefCell<Option<PathBuf>>,
    recording_start: Cell<Option<Instant>>,
    transcription_count: Cell<u32>,
    total_latency: Cell<f64>,
    /// Full status text for copy-to-clipboard (may be longer than menu display).
    last_status: RefCell<Option<String>>,
    /// Frame counter for icon pulse animation.
    pulse_frame: Cell<u32>,
}

// ── App Delegate ──────────────────────────────────────────────────────
define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PTTAppDelegate"]
    #[ivars = DelegateIvars]
    struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            let mtm = MainThreadMarker::from(self);

            // Load config and store globally
            let cfg = config::load();
            let db = cfg.db_path();
            db::init(&db);
            *CONFIG.lock().unwrap() = Some(cfg);

            self.setup_menubar(mtm);

            self.start_hotkey_monitor_if_allowed();

            // Start IPC server
            let socket_path = with_config(|c| c.socket_path()).unwrap_or_else(|| PathBuf::from("/tmp/ptt.sock"));
            let ipc = ipc::start_server(&socket_path);
            self.ivars().ipc_state.set(ipc).ok();

            // Write PID file (kept alive for app lifetime)
            let pid_path = with_config(|c| c.pid_path()).unwrap_or_else(|| PathBuf::from("/tmp/ptt.pid"));
            let _pid_guard = ipc::PidGuard::create(&pid_path);
            // Leak the guard so it lives for the process lifetime — cleanup happens on drop
            std::mem::forget(_pid_guard);

            self.start_poll_timer(mtm);

            if self.should_show_onboarding() {
                self.show_onboarding_window(mtm);
            }

            eprintln!("[ptt] Ready — hold right Option to dictate");
        }

        #[unsafe(method(pollTick:))]
        fn poll_tick(&self, _timer: &NSTimer) {
            self.process_hotkey_events();
            self.process_ipc_commands();
            self.check_transcription_result();
            self.check_onboarding_result();
            self.check_calibration_gen_result();
            self.check_learn_result();
            self.check_microphone_result();
            self.animate_pulse();
        }

        /// Copy the last status/error message to the clipboard.
        #[unsafe(method(copyStatus:))]
        fn copy_status(&self, _sender: &NSObject) {
            if let Some(ref text) = *self.ivars().last_status.borrow() {
                let pb = NSPasteboard::generalPasteboard();
                pb.clearContents();
                let text_type = NSString::from_str("public.utf8-plain-text");
                pb.setString_forType(&NSString::from_str(text), &text_type);
                eprintln!("[ptt] Copied to clipboard: {}", &text[..text.len().min(80)]);
            }
        }

        /// Toggle recording via menu item.
        #[unsafe(method(toggleRecording:))]
        fn toggle_recording(&self, _sender: &NSObject) {
            let mode = self.ivars().mode.get();
            match mode {
                AppMode::Idle => self.on_push_down(),
                AppMode::Recording | AppMode::Locked => self.on_push_up(),
                AppMode::Transcribing => {} // Can't toggle during transcription
            }
        }

        /// Open the History & Corrections window.
        #[unsafe(method(openHistory:))]
        fn open_history(&self, _sender: &NSObject) {
            let mtm = MainThreadMarker::from(self);
            if let Some(db_path) = with_config(|c| c.db_path()) {
                history::show(mtm, &db_path);
            }
        }

        #[unsafe(method(setupProfile:))]
        fn setup_profile(&self, _sender: &NSObject) {
            // If no profile exists, open a file picker for user-provided context.
            // If profile exists, start calibration.
            if calibrate::profile_exists() {
                self.start_calibration();
            } else {
                self.start_onboarding();
            }
        }

        #[unsafe(method(learnFromCorrections:))]
        fn learn_from_corrections(&self, _sender: &NSObject) {
            self.run_correction_learning();
        }

        #[unsafe(method(openSetup:))]
        fn open_setup(&self, _sender: &NSObject) {
            let mtm = MainThreadMarker::from(self);
            self.show_onboarding_window(mtm);
        }

        #[unsafe(method(saveApiKey:))]
        fn save_api_key(&self, _sender: &NSObject) {
            self.save_api_key_from_setup();
        }

        #[unsafe(method(requestShortcutAccess:))]
        fn request_shortcut_access(&self, _sender: &NSObject) {
            self.request_shortcut_access_from_setup();
        }

        #[unsafe(method(openAccessibilitySettings:))]
        fn open_accessibility_settings(&self, _sender: &NSObject) {
            self.open_settings_url("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility");
        }

        #[unsafe(method(openInputMonitoringSettings:))]
        fn open_input_monitoring_settings(&self, _sender: &NSObject) {
            self.open_settings_url("x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent");
        }

        #[unsafe(method(requestMicrophoneAccess:))]
        fn request_microphone_access(&self, _sender: &NSObject) {
            self.request_microphone_access_from_setup();
        }

        #[unsafe(method(openMicrophoneSettings:))]
        fn open_microphone_settings(&self, _sender: &NSObject) {
            self.open_settings_url("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone");
        }
    }
);

// Global config (loaded once at startup, read from background threads)
use std::sync::LazyLock;
static CONFIG: LazyLock<Mutex<Option<config::Config>>> = LazyLock::new(|| Mutex::new(None));

fn with_config<T>(f: impl FnOnce(&config::Config) -> T) -> Option<T> {
    CONFIG.lock().ok().and_then(|c| c.as_ref().map(|cfg| f(cfg)))
}

impl AppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = mtm.alloc::<Self>();
        let this = this.set_ivars(DelegateIvars {
            status_item: OnceCell::new(),
            status_menu_item: OnceCell::new(),
            toggle_menu_item: OnceCell::new(),
            history_menu_item: OnceCell::new(),
            stats_menu_item: OnceCell::new(),
            hotkey_state: OnceCell::new(),
            ipc_state: OnceCell::new(),
            onboarding_window: RefCell::new(None),
            api_key_field: RefCell::new(None),
            provider_popup: RefCell::new(None),
            onboarding_status_label: RefCell::new(None),
            mode: Cell::new(AppMode::Idle),
            recording_child: RefCell::new(None),
            recording_path: RefCell::new(None),
            recording_start: Cell::new(None),
            transcription_count: Cell::new(0),
            total_latency: Cell::new(0.0),
            last_status: RefCell::new(None),
            pulse_frame: Cell::new(0),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn reload_config(&self) {
        let cfg = config::load();
        db::init(&cfg.db_path());
        *CONFIG.lock().unwrap() = Some(cfg);
    }

    fn should_show_onboarding(&self) -> bool {
        let missing_key = with_config(|c| c.api_key().is_none()).unwrap_or(true);
        missing_key || !hotkey::is_accessibility_trusted() || !config::microphone_was_checked()
    }

    fn start_hotkey_monitor_if_allowed(&self) {
        if self.ivars().hotkey_state.get().is_some() {
            return;
        }
        if hotkey::is_accessibility_trusted() {
            let hk = hotkey::start_monitor();
            self.ivars().hotkey_state.set(hk).ok();
        } else {
            eprintln!("[ptt] Shortcut access not granted yet; open Setup to enable it.");
        }
    }

    fn show_onboarding_window(&self, mtm: MainThreadMarker) {
        if let Some(window) = self.ivars().onboarding_window.borrow().as_ref() {
            window.makeKeyAndOrderFront(None);
            unsafe {
                NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
            }
            self.update_onboarding_status();
            return;
        }

        let frame = NSRect::new(NSPoint::new(260.0, 260.0), NSSize::new(560.0, 410.0));
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
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
        window.setTitle(&NSString::from_str("Push to Talk Setup"));

        let content = window.contentView().unwrap();
        let target: &AnyObject = unsafe { &*(self as *const Self as *const AnyObject) };

        let title = Self::label(
            mtm,
            "Set up Push to Talk",
            24.0,
            360.0,
            500.0,
            24.0,
        );
        content.addSubview(&title);

        let api_label = Self::label(
            mtm,
            "API provider and key",
            24.0,
            320.0,
            180.0,
            22.0,
        );
        content.addSubview(&api_label);

        let provider_popup = unsafe {
            NSPopUpButton::initWithFrame_pullsDown(
                mtm.alloc(),
                NSRect::new(NSPoint::new(210.0, 318.0), NSSize::new(300.0, 26.0)),
                false,
            )
        };
        provider_popup.addItemWithTitle(&NSString::from_str("OpenRouter"));
        provider_popup.addItemWithTitle(&NSString::from_str("OpenAI"));
        if with_config(|c| c.api.endpoint.contains("api.openai.com")).unwrap_or(false) {
            provider_popup.selectItemWithTitle(&NSString::from_str("OpenAI"));
        }
        content.addSubview(&provider_popup);

        let key_field = unsafe {
            NSSecureTextField::initWithFrame(
                mtm.alloc(),
                NSRect::new(NSPoint::new(210.0, 284.0), NSSize::new(300.0, 26.0)),
            )
        };
        key_field.setPlaceholderString(Some(&NSString::from_str("Paste API key")));
        content.addSubview(&key_field);

        let save_key = Self::button(
            mtm,
            "Save API Key",
            390.0,
            248.0,
            120.0,
            30.0,
            target,
            sel!(saveApiKey:),
        );
        content.addSubview(&save_key);

        let permissions_label = Self::label(
            mtm,
            "Permissions",
            24.0,
            205.0,
            180.0,
            22.0,
        );
        content.addSubview(&permissions_label);

        let shortcut_btn = Self::button(
            mtm,
            "Enable Shortcut Access",
            24.0,
            166.0,
            170.0,
            30.0,
            target,
            sel!(requestShortcutAccess:),
        );
        content.addSubview(&shortcut_btn);

        let accessibility_settings = Self::button(
            mtm,
            "Open Accessibility",
            205.0,
            166.0,
            145.0,
            30.0,
            target,
            sel!(openAccessibilitySettings:),
        );
        content.addSubview(&accessibility_settings);

        let input_settings = Self::button(
            mtm,
            "Open Input Monitoring",
            360.0,
            166.0,
            170.0,
            30.0,
            target,
            sel!(openInputMonitoringSettings:),
        );
        content.addSubview(&input_settings);

        let mic_btn = Self::button(
            mtm,
            "Enable Microphone",
            24.0,
            126.0,
            170.0,
            30.0,
            target,
            sel!(requestMicrophoneAccess:),
        );
        content.addSubview(&mic_btn);

        let mic_settings = Self::button(
            mtm,
            "Open Microphone",
            205.0,
            126.0,
            145.0,
            30.0,
            target,
            sel!(openMicrophoneSettings:),
        );
        content.addSubview(&mic_settings);

        let help_shortcut = Self::label(
            mtm,
            "Shortcut access enables right Option detection and paste insertion.",
            24.0,
            92.0,
            506.0,
            22.0,
        );
        content.addSubview(&help_shortcut);

        let help_mic = Self::label(
            mtm,
            "Microphone access is requested only when you click Enable Microphone.",
            24.0,
            70.0,
            506.0,
            22.0,
        );
        content.addSubview(&help_mic);

        let status = Self::label(mtm, "", 24.0, 26.0, 506.0, 40.0);
        content.addSubview(&status);

        *self.ivars().provider_popup.borrow_mut() = Some(provider_popup);
        *self.ivars().api_key_field.borrow_mut() = Some(key_field);
        *self.ivars().onboarding_status_label.borrow_mut() = Some(status);
        *self.ivars().onboarding_window.borrow_mut() = Some(window);

        self.update_onboarding_status();
        if let Some(window) = self.ivars().onboarding_window.borrow().as_ref() {
            window.makeKeyAndOrderFront(None);
            unsafe {
                NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
            }
        }
    }

    fn label(
        mtm: MainThreadMarker,
        text: &str,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    ) -> Retained<NSTextField> {
        let label = unsafe {
            NSTextField::initWithFrame(
                mtm.alloc(),
                NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)),
            )
        };
        label.setStringValue(&NSString::from_str(text));
        label.setBezeled(false);
        label.setDrawsBackground(false);
        label.setEditable(false);
        label.setSelectable(false);
        label
    }

    fn button(
        mtm: MainThreadMarker,
        title: &str,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        target: &AnyObject,
        action: objc2::runtime::Sel,
    ) -> Retained<NSButton> {
        let button = unsafe {
            NSButton::initWithFrame(
                mtm.alloc(),
                NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)),
            )
        };
        button.setTitle(&NSString::from_str(title));
        button.setBezelStyle(NSBezelStyle::Rounded);
        unsafe {
            button.setTarget(Some(target));
            button.setAction(Some(action));
        }
        button
    }

    fn update_onboarding_status(&self) {
        let key_status = if with_config(|c| c.api_key().is_some()).unwrap_or(false) {
            "API key saved"
        } else {
            "API key missing"
        };
        let shortcut_status = if hotkey::is_accessibility_trusted() {
            "shortcut access granted"
        } else {
            "shortcut access needed"
        };
        let mic_status = if config::microphone_was_checked() {
            "microphone checked"
        } else {
            "microphone not checked"
        };
        let text = format!("{key_status} | {shortcut_status} | {mic_status}");
        if let Some(label) = self.ivars().onboarding_status_label.borrow().as_ref() {
            label.setStringValue(&NSString::from_str(&text));
        }
    }

    fn save_api_key_from_setup(&self) {
        let key = match self.ivars().api_key_field.borrow().as_ref() {
            Some(field) => field.stringValue().to_string(),
            None => return,
        };
        let provider = self.ivars().provider_popup.borrow().as_ref()
            .and_then(|popup| popup.titleOfSelectedItem())
            .map(|title| config::Provider::from_label(&title.to_string()))
            .unwrap_or(config::Provider::OpenRouter);

        match config::save_api_key(provider, &key) {
            Ok(()) => {
                self.reload_config();
                if let Some(field) = self.ivars().api_key_field.borrow().as_ref() {
                    field.setStringValue(&NSString::from_str(""));
                }
                self.update_ui("⚪", "✓ API key saved");
            }
            Err(e) => self.update_ui("⚪", &format!("✗ {e}")),
        }
        self.update_onboarding_status();
    }

    fn request_shortcut_access_from_setup(&self) {
        if hotkey::request_accessibility() || hotkey::is_accessibility_trusted() {
            self.start_hotkey_monitor_if_allowed();
            self.update_ui("⚪", "✓ Shortcut access enabled");
        } else {
            self.update_ui("⚪", "Shortcut access needs approval in System Settings");
        }
        self.update_onboarding_status();
    }

    fn request_microphone_access_from_setup(&self) {
        self.update_ui("🟡", "Requesting microphone access…");
        let ffmpeg = with_config(|c| c.ffmpeg()).unwrap_or_else(|| "ffmpeg".into());
        let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
        std::thread::Builder::new()
            .name("mic-permission".into())
            .spawn(move || {
                let result = audio::request_microphone_access(&ffmpeg);
                let _ = tx.send(result);
            })
            .ok();
        *MIC_RX.lock().unwrap() = Some(rx);
    }

    fn check_microphone_result(&self) {
        let result = {
            let rx_guard = MIC_RX.lock().ok();
            rx_guard.and_then(|rx| rx.as_ref().and_then(|r| r.try_recv().ok()))
        };

        if let Some(result) = result {
            *MIC_RX.lock().unwrap() = None;
            match result {
                Ok(()) => {
                    config::mark_microphone_checked();
                    self.update_ui("⚪", "✓ Microphone access enabled");
                }
                Err(e) => {
                    self.update_ui("⚪", &format!("✗ Microphone check failed: {e}"));
                }
            }
            self.update_onboarding_status();
        }
    }

    fn open_settings_url(&self, url: &str) {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }

    fn setup_menubar(&self, mtm: MainThreadMarker) {
        let status_bar = NSStatusBar::systemStatusBar();
        let status_item = status_bar.statusItemWithLength(NSVariableStatusItemLength);

        if let Some(button) = status_item.button(mtm) {
            button.setTitle(&NSString::from_str("⚪"));
        }

        let menu = NSMenu::new(mtm);

        // Status line (click to copy)
        let status_label = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc(),
                &NSString::from_str("Idle — ready to dictate"),
                Some(sel!(copyStatus:)),
                &NSString::from_str(""),
            )
        };
        status_label.setEnabled(false);
        menu.addItem(&status_label);

        menu.addItem(&NSMenuItem::separatorItem(mtm));

        // Toggle Recording
        let toggle_item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc(),
                &NSString::from_str("Toggle Recording"),
                Some(sel!(toggleRecording:)),
                &NSString::from_str(""),
            )
        };
        menu.addItem(&toggle_item);

        // History & Corrections
        let history_item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc(),
                &NSString::from_str("History & Corrections"),
                Some(sel!(openHistory:)),
                &NSString::from_str(""),
            )
        };
        menu.addItem(&history_item);

        menu.addItem(&NSMenuItem::separatorItem(mtm));

        // First-run setup / API key / permissions
        let setup_item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc(),
                &NSString::from_str("Setup…"),
                Some(sel!(openSetup:)),
                &NSString::from_str(""),
            )
        };
        menu.addItem(&setup_item);

        // Speaker Profile / Calibration submenu
        let profile_label = if calibrate::profile_exists() {
            "Calibrate Voice"
        } else {
            "Set Up Speaker Profile…"
        };
        let profile_item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc(),
                &NSString::from_str(profile_label),
                Some(sel!(setupProfile:)),
                &NSString::from_str(""),
            )
        };
        menu.addItem(&profile_item);

        // Learn from Corrections
        let learn_item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc(),
                &NSString::from_str("Learn from Corrections"),
                Some(sel!(learnFromCorrections:)),
                &NSString::from_str(""),
            )
        };
        menu.addItem(&learn_item);

        menu.addItem(&NSMenuItem::separatorItem(mtm));

        // Stats line
        let stats_item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc(),
                &NSString::from_str("No transcriptions yet"),
                None,
                &NSString::from_str(""),
            )
        };
        stats_item.setEnabled(false);
        menu.addItem(&stats_item);

        menu.addItem(&NSMenuItem::separatorItem(mtm));

        // Quit
        let quit_item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc(),
                &NSString::from_str("Quit Dictate"),
                Some(sel!(terminate:)),
                &NSString::from_str("q"),
            )
        };
        menu.addItem(&quit_item);

        status_item.setMenu(Some(&menu));

        self.ivars().status_item.set(status_item).ok();
        self.ivars().status_menu_item.set(status_label).ok();
        self.ivars().toggle_menu_item.set(toggle_item).ok();
        self.ivars().history_menu_item.set(history_item).ok();
        self.ivars().stats_menu_item.set(stats_item).ok();
    }

    fn start_poll_timer(&self, _mtm: MainThreadMarker) {
        let target: &objc2::runtime::AnyObject =
            unsafe { &*(self as *const Self as *const objc2::runtime::AnyObject) };
        unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                0.1,
                target,
                sel!(pollTick:),
                None,
                true,
            );
        }
    }

    fn process_hotkey_events(&self) {
        let hk = match self.ivars().hotkey_state.get() {
            Some(h) => h,
            None => return,
        };
        let events = match hk.lock() {
            Ok(mut s) => s.drain_events(),
            Err(_) => return,
        };
        for event in events {
            match event {
                hotkey::HotkeyEvent::PushDown => {
                    let mode = self.ivars().mode.get();
                    if mode == AppMode::Locked {
                        // Locked mode: right Opt pressed again → stop recording
                        eprintln!("[ptt] Locked mode ended");
                        self.on_push_up();
                    } else {
                        self.on_push_down();
                    }
                }
                hotkey::HotkeyEvent::PushUp => {
                    let mode = self.ivars().mode.get();
                    if mode == AppMode::Locked {
                        // Locked mode: ignore release, recording continues
                        eprintln!("[ptt] Right Option released — locked, recording continues");
                    } else {
                        self.on_push_up();
                    }
                }
                hotkey::HotkeyEvent::LeftArrowDown => {
                    let mode = self.ivars().mode.get();
                    if mode == AppMode::Recording {
                        // Enter locked mode
                        self.ivars().mode.set(AppMode::Locked);
                        audio::play_tone(audio::Tone::Lock);
                        self.update_ui("🔒", "🔒 Locked — press right ⌥ to stop");
                        self.update_toggle_title("Stop Recording (locked)");
                        eprintln!("[ptt] Locked dictation mode engaged");
                    }
                }
            }
        }
    }

    fn process_ipc_commands(&self) {
        let ipc = match self.ivars().ipc_state.get() {
            Some(i) => i,
            None => return,
        };
        let commands = match ipc.lock() {
            Ok(mut s) => s.drain_commands(),
            Err(_) => return,
        };
        for cmd in commands {
            match cmd {
                ipc::IpcCommand::Toggle => {
                    let mode = self.ivars().mode.get();
                    match mode {
                        AppMode::Idle => self.on_push_down(),
                        AppMode::Recording | AppMode::Locked => self.on_push_up(),
                        AppMode::Transcribing => {}
                    }
                }
                ipc::IpcCommand::Status => {
                    // Status is returned inline by the IPC server thread
                    // for now — the main thread just processes toggle commands
                }
            }
        }
    }

    fn update_ui(&self, icon: &str, status: &str) {
        let mtm = MainThreadMarker::from(self);
        if let Some(si) = self.ivars().status_item.get() {
            if let Some(btn) = si.button(mtm) {
                btn.setTitle(&NSString::from_str(icon));
            }
        }
        if let Some(mi) = self.ivars().status_menu_item.get() {
            // Truncate for menu display, store full text for copy
            let display = if status.len() > 60 {
                format!("{}… (click to copy)", &status[..57])
            } else {
                status.to_string()
            };
            mi.setTitle(&NSString::from_str(&display));
            // Enable clicking when there's a result to copy
            let has_result = status.starts_with('✓') || status.starts_with('✗') || status.starts_with('⚠');
            mi.setEnabled(has_result);
            if has_result {
                *self.ivars().last_status.borrow_mut() = Some(status.to_string());
            }
        }
    }

    fn update_toggle_title(&self, title: &str) {
        if let Some(mi) = self.ivars().toggle_menu_item.get() {
            mi.setTitle(&NSString::from_str(title));
        }
    }

    fn update_stats(&self) {
        let n = self.ivars().transcription_count.get();
        if let Some(mi) = self.ivars().stats_menu_item.get() {
            if n == 0 {
                mi.setTitle(&NSString::from_str("No transcriptions yet"));
            } else {
                let avg = self.ivars().total_latency.get() / n as f64;
                let text = format!("{n} transcription{} · avg {avg:.1}s latency",
                    if n == 1 { "" } else { "s" });
                mi.setTitle(&NSString::from_str(&text));
            }
        }
    }

    /// Animate the menubar icon pulse during transcription.
    fn animate_pulse(&self) {
        if self.ivars().mode.get() != AppMode::Transcribing {
            return;
        }

        let frame = self.ivars().pulse_frame.get() + 1;
        self.ivars().pulse_frame.set(frame);

        // Toggle every 5 ticks (500ms at 100ms poll interval)
        if frame % 5 == 0 {
            let mtm = MainThreadMarker::from(self);
            if let Some(si) = self.ivars().status_item.get() {
                if let Some(btn) = si.button(mtm) {
                    let icon = if (frame / 5) % 2 == 0 { "🟡" } else { "⚪" };
                    btn.setTitle(&NSString::from_str(icon));
                }
            }
        }
    }

    fn check_transcription_result(&self) {
        let result = {
            let rx_guard = TRANSCRIBE_RX.lock().ok();
            rx_guard.and_then(|rx| rx.as_ref().and_then(|r| r.try_recv().ok()))
        };

        if let Some(result) = result {
            *TRANSCRIBE_RX.lock().unwrap() = None;
            self.ivars().pulse_frame.set(0);

            if let Some(ref text) = result.text {
                audio::play_tone(audio::Tone::Done);
                let n = self.ivars().transcription_count.get() + 1;
                self.ivars().transcription_count.set(n);
                self.ivars().total_latency.set(self.ivars().total_latency.get() + result.latency);
                let avg = self.ivars().total_latency.get() / n as f64;
                let preview: String = text.chars().take(40).collect();
                self.update_ui("⚪", &format!("✓ {preview}… ({:.1}s, avg {:.1}s)", result.latency, avg));
                self.update_stats();

                // If calibration is active, feed the transcript to the comparator
                let is_calibrating = calibrate::CALIBRATION.lock()
                    .map(|s| s.active)
                    .unwrap_or(false);
                if is_calibrating {
                    self.process_calibration_transcription(text);
                }

                // macOS notification
                let short_preview: String = text.chars().take(60).collect();
                notification::notify_success(&short_preview, result.latency);
            } else if result.hallucination {
                audio::play_tone(audio::Tone::Discard);
                let err = result.error.as_deref().unwrap_or("unknown");
                self.update_ui("⚪", &format!("⚠ {err}"));
                notification::notify_hallucination(err);
            } else {
                audio::play_tone(audio::Tone::Discard);
                let err = result.error.as_deref().unwrap_or("unknown");
                self.update_ui("⚪", &format!("✗ {err}"));
                notification::notify_error(err);
            }

            self.update_toggle_title("Toggle Recording");
            self.ivars().mode.set(AppMode::Idle);
        }
    }

    fn on_push_down(&self) {
        if self.ivars().mode.get() != AppMode::Idle {
            return;
        }
        self.ivars().mode.set(AppMode::Recording);
        audio::play_tone(audio::Tone::Start);

        let (ffmpeg, audio_dir) = match with_config(|c| (c.ffmpeg(), c.audio_dir())) {
            Some(v) => v,
            None => return,
        };

        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let path = audio_dir.join(format!("ptt-{ts}.wav"));

        if let Some(child) = audio::start_recording(&ffmpeg, &path) {
            *self.ivars().recording_child.borrow_mut() = Some(child);
            *self.ivars().recording_path.borrow_mut() = Some(path);
            self.ivars().recording_start.set(Some(Instant::now()));
            self.update_ui("🔴", "Recording...");
            self.update_toggle_title("Stop Recording");
        } else {
            self.ivars().mode.set(AppMode::Idle);
            audio::play_tone(audio::Tone::Discard);
            self.update_ui("⚪", "Recording failed — check ffmpeg");
        }
    }

    fn on_push_up(&self) {
        let mode = self.ivars().mode.get();
        if mode != AppMode::Recording && mode != AppMode::Locked {
            return;
        }

        if let Some(mut child) = self.ivars().recording_child.borrow_mut().take() {
            audio::stop_recording(&mut child);
        }

        let path = self.ivars().recording_path.borrow().clone();
        let rec_duration = self.ivars().recording_start.get()
            .map(|s| s.elapsed().as_secs_f64())
            .unwrap_or(0.0);

        let min_dur = with_config(|c| c.audio.min_duration_s).unwrap_or(0.4);
        if rec_duration < min_dur {
            eprintln!("[ptt] Recording too short ({rec_duration:.1}s), discarding");
            self.ivars().mode.set(AppMode::Idle);
            audio::play_tone(audio::Tone::Discard);
            self.update_ui("⚪", "Too short — discarded");
            self.update_toggle_title("Toggle Recording");
            return;
        }

        self.ivars().mode.set(AppMode::Transcribing);
        self.ivars().pulse_frame.set(0);
        audio::play_tone(audio::Tone::Processing);
        self.update_ui("🟡", "Transcribing...");
        self.update_toggle_title("Transcribing...");

        let audio_path = path.unwrap_or_default();
        let (tx, rx) = std::sync::mpsc::channel::<TranscribeResult>();

        std::thread::Builder::new()
            .name("transcribe".into())
            .spawn(move || {
                let result = do_transcription(&audio_path);
                let _ = tx.send(result);
            })
            .ok();

        *TRANSCRIBE_RX.lock().unwrap() = Some(rx);
    }

    // ── Calibration flows ────────────────────────────────────────────

    fn start_onboarding(&self) {
        use objc2_app_kit::NSOpenPanel;

        let mtm = MainThreadMarker::from(self);
        self.update_ui("⚪", "Setting up speaker profile…");

        // Show file picker for a user-provided context document.
        let panel = NSOpenPanel::openPanel(mtm);
        panel.setCanChooseFiles(true);
        panel.setCanChooseDirectories(false);
        panel.setAllowsMultipleSelection(false);
        panel.setMessage(Some(&NSString::from_str(
            "Select a text file with vocabulary, project names, and writing preferences."
        )));
        panel.setPrompt(Some(&NSString::from_str("Use This File")));

        let response = unsafe { panel.runModal() };

        // NSModalResponseOK = 1
        if response != objc2_app_kit::NSModalResponseOK {
            self.update_ui("⚪", "Profile setup cancelled");
            return;
        }

        let url = match panel.URL() {
            Some(u) => u,
            None => {
                self.update_ui("⚪", "No file selected");
                return;
            }
        };
        let file_path = match url.path() {
            Some(p) => p.to_string(),
            None => {
                self.update_ui("⚪", "Invalid file path");
                return;
            }
        };

        let background_text = match std::fs::read_to_string(&file_path) {
            Ok(t) => t,
            Err(e) => {
                self.update_ui("⚪", &format!("Failed to read file: {e}"));
                return;
            }
        };

        self.update_ui("🟡", "Generating speaker profile…");
        notification::notify_success("Generating your speaker profile from the uploaded document…", 0.0);

        let (endpoint, model, api_key) = match with_config(|c| {
            (c.api.endpoint.clone(), c.api.model.clone(), c.api_key())
        }) {
            Some((e, m, Some(k))) => (e, m, k),
            _ => {
                self.update_ui("⚪", "✗ No API key configured");
                return;
            }
        };

        // Dispatch to background thread
        let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
        std::thread::Builder::new()
            .name("onboarding".into())
            .spawn(move || {
                let result = calibrate::generate_profile(&endpoint, &model, &api_key, &background_text);
                let _ = tx.send(result);
            })
            .ok();

        *ONBOARDING_RX.lock().unwrap() = Some(rx);
    }

    fn check_onboarding_result(&self) {
        let result = {
            let rx_guard = ONBOARDING_RX.lock().ok();
            rx_guard.and_then(|rx| rx.as_ref().and_then(|r| r.try_recv().ok()))
        };

        if let Some(result) = result {
            *ONBOARDING_RX.lock().unwrap() = None;
            match result {
                Ok(profile_text) => {
                    match calibrate::save_profile(&profile_text) {
                        Ok(path) => {
                            self.update_ui("⚪", "✓ Speaker profile created");
                            notification::notify_success(
                                &format!("Profile saved to {}", path.display()), 0.0
                            );
                            eprintln!("[ptt] Speaker profile saved to {}", path.display());
                            // Open in default editor
                            let _ = std::process::Command::new("open")
                                .arg(&path)
                                .spawn();
                        }
                        Err(e) => {
                            self.update_ui("⚪", &format!("✗ {e}"));
                            notification::notify_error(&e);
                        }
                    }
                }
                Err(e) => {
                    self.update_ui("⚪", &format!("✗ {e}"));
                    notification::notify_error(&e);
                }
            }
        }
    }

    fn start_calibration(&self) {
        self.update_ui("🟡", "Generating calibration sentences…");

        let (endpoint, model, api_key) = match with_config(|c| {
            (c.api.endpoint.clone(), c.api.model.clone(), c.api_key())
        }) {
            Some((e, m, Some(k))) => (e, m, k),
            _ => {
                self.update_ui("⚪", "✗ No API key configured");
                return;
            }
        };

        let profile = calibrate::read_profile().unwrap_or_default();
        if profile.is_empty() {
            self.update_ui("⚪", "No speaker profile — set one up first");
            return;
        }

        let (tx, rx) = std::sync::mpsc::channel::<Result<Vec<calibrate::CalibrationSentence>, String>>();
        std::thread::Builder::new()
            .name("calibrate-gen".into())
            .spawn(move || {
                let result = calibrate::generate_calibration_sentences(&endpoint, &model, &api_key, &profile);
                let _ = tx.send(result);
            })
            .ok();

        *CALIBRATE_GEN_RX.lock().unwrap() = Some(rx);
    }

    fn check_calibration_gen_result(&self) {
        let result = {
            let rx_guard = CALIBRATE_GEN_RX.lock().ok();
            rx_guard.and_then(|rx| rx.as_ref().and_then(|r| r.try_recv().ok()))
        };

        if let Some(result) = result {
            *CALIBRATE_GEN_RX.lock().unwrap() = None;
            match result {
                Ok(sentences) => {
                    let n = sentences.len();
                    let _ = calibrate::save_calibration_script(&sentences);

                    if let Ok(mut state) = calibrate::CALIBRATION.lock() {
                        state.sentences = sentences;
                        state.current_index = 0;
                        state.results.clear();
                        state.active = true;
                    }

                    let progress = calibrate::CALIBRATION.lock()
                        .map(|s| s.progress_text())
                        .unwrap_or_default();

                    self.update_ui("⚪", &format!("Calibration ready ({n} sentences)"));
                    notification::notify_success(
                        &format!("Read the sentence and use push-to-talk to record:\n{progress}"), 0.0
                    );
                    eprintln!("[ptt] Calibration: {progress}");
                }
                Err(e) => {
                    self.update_ui("⚪", &format!("✗ {e}"));
                    notification::notify_error(&e);
                }
            }
        }
    }

    /// After a transcription completes during calibration, compare against expected.
    fn process_calibration_transcription(&self, transcript: &str) {
        let (sentence, current_idx) = {
            let state = match calibrate::CALIBRATION.lock() {
                Ok(s) if s.active => s,
                _ => return,
            };
            match state.current_sentence() {
                Some(s) => (s.clone(), state.current_index),
                None => return,
            }
        };

        let subs = calibrate::compare_transcription(&sentence.text, transcript);
        let n_errors = subs.len();

        if let Ok(mut state) = calibrate::CALIBRATION.lock() {
            state.results.push(calibrate::CalibrationResult {
                sentence_num: sentence.number,
                group: sentence.group.to_string(),
                reference: sentence.text.clone(),
                hypothesis: transcript.to_string(),
                substitutions: subs,
            });

            if state.advance() {
                // Next sentence
                let progress = state.progress_text();
                eprintln!("[ptt] Calibration {}: {n_errors} errors. Next: {progress}", current_idx + 1);
                notification::notify_success(
                    &format!("{n_errors} errors. Next:\n{progress}"), 0.0
                );
            } else {
                // Calibration complete — finalise
                state.active = false;
                eprintln!("[ptt] Calibration recording complete. Analysing…");
                drop(state);
                self.finalise_calibration();
            }
        }
    }

    fn finalise_calibration(&self) {
        self.update_ui("🟡", "Analysing calibration results…");

        let (endpoint, model, api_key) = match with_config(|c| {
            (c.api.endpoint.clone(), c.api.model.clone(), c.api_key())
        }) {
            Some((e, m, Some(k))) => (e, m, k),
            _ => return,
        };

        let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
        std::thread::Builder::new()
            .name("calibrate-fin".into())
            .spawn(move || {
                let result = calibrate::finalise_calibration(&endpoint, &model, &api_key);
                let _ = tx.send(result);
            })
            .ok();

        *LEARN_RX.lock().unwrap() = Some(rx);
    }

    fn run_correction_learning(&self) {
        self.update_ui("🟡", "Analysing corrections…");

        let (endpoint, model, api_key, db_path) = match with_config(|c| {
            (c.api.endpoint.clone(), c.api.model.clone(), c.api_key(), c.db_path())
        }) {
            Some((e, m, Some(k), d)) => (e, m, k, d),
            _ => {
                self.update_ui("⚪", "✗ No API key configured");
                return;
            }
        };

        let pairs = calibrate::get_correction_pairs(&db_path);
        if pairs.is_empty() {
            self.update_ui("⚪", "No corrections to learn from yet");
            return;
        }

        let n = pairs.len();
        let profile = calibrate::read_profile().unwrap_or_default();

        let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
        std::thread::Builder::new()
            .name("learn".into())
            .spawn(move || {
                let result = calibrate::analyse_corrections(&endpoint, &model, &api_key, &pairs, &profile);
                match result {
                    Ok(new_sections) => {
                        match calibrate::append_to_profile(&new_sections) {
                            Ok(_) => {
                                let _ = tx.send(Ok(format!("Learned from {n} corrections — profile updated")));
                            }
                            Err(e) => { let _ = tx.send(Err(e)); }
                        }
                    }
                    Err(e) => { let _ = tx.send(Err(e)); }
                }
            })
            .ok();

        *LEARN_RX.lock().unwrap() = Some(rx);
    }

    fn check_learn_result(&self) {
        let result = {
            let rx_guard = LEARN_RX.lock().ok();
            rx_guard.and_then(|rx| rx.as_ref().and_then(|r| r.try_recv().ok()))
        };

        if let Some(result) = result {
            *LEARN_RX.lock().unwrap() = None;
            match result {
                Ok(msg) => {
                    self.update_ui("⚪", &format!("✓ {msg}"));
                    notification::notify_success(&msg, 0.0);
                }
                Err(e) => {
                    self.update_ui("⚪", &format!("✗ {e}"));
                    notification::notify_error(&e);
                }
            }
        }
    }
}

// ── Transcription pipeline ────────────────────────────────────────────
static TRANSCRIBE_RX: LazyLock<Mutex<Option<std::sync::mpsc::Receiver<TranscribeResult>>>> =
    LazyLock::new(|| Mutex::new(None));

static ONBOARDING_RX: LazyLock<Mutex<Option<std::sync::mpsc::Receiver<Result<String, String>>>>> =
    LazyLock::new(|| Mutex::new(None));

static CALIBRATE_GEN_RX: LazyLock<Mutex<Option<std::sync::mpsc::Receiver<Result<Vec<calibrate::CalibrationSentence>, String>>>>> =
    LazyLock::new(|| Mutex::new(None));

static LEARN_RX: LazyLock<Mutex<Option<std::sync::mpsc::Receiver<Result<String, String>>>>> =
    LazyLock::new(|| Mutex::new(None));

static MIC_RX: LazyLock<Mutex<Option<std::sync::mpsc::Receiver<Result<(), String>>>>> =
    LazyLock::new(|| Mutex::new(None));

struct TranscribeResult {
    text: Option<String>,
    latency: f64,
    error: Option<String>,
    hallucination: bool,
}

fn do_transcription(audio_path: &PathBuf) -> TranscribeResult {
    let (endpoint, model, api_key, profile, ffprobe, max_wps, db, max_retries, retry_backoff, transcripts_dir) =
        match with_config(|c| {
            (
                c.api.endpoint.clone(),
                c.api.model.clone(),
                c.api_key(),
                c.speaker_profile(),
                c.ffprobe(),
                c.transcription.max_wps,
                c.db_path(),
                c.api.max_retries,
                c.api.retry_backoff.clone(),
                c.transcripts_dir(),
            )
        }) {
            Some((endpoint, model, Some(key), profile, ffprobe, max_wps, db, retries, backoff, tdir)) => {
                (endpoint, model, key, profile, ffprobe, max_wps, db, retries, backoff, tdir)
            }
            Some((_, _, None, _, _, _, db, _, _, _)) => {
                let err = "No API key configured".to_string();
                db::record(&db, audio_path.to_str(), None, "error", Some(&err), None, None, None);
                return TranscribeResult { text: None, latency: 0.0, error: Some(err), hallucination: false };
            }
            None => {
                return TranscribeResult { text: None, latency: 0.0, error: Some("Config unavailable".into()), hallucination: false };
            }
        };

    match transcribe::transcribe(&endpoint, &model, &api_key, &profile, &ffprobe, audio_path, max_retries, &retry_backoff) {
        Ok(result) => {
            let hallucination = transcribe::is_hallucination(&result.text, result.duration_s, max_wps);

            if hallucination {
                let reason = transcribe::is_format_hallucination(&result.text)
                    .unwrap_or("WPS exceeded");
                eprintln!("[ptt] Hallucination detected: {reason} (WPS={:.1})", result.wps);
                db::record(
                    &db, audio_path.to_str(), Some(&result.text), "hallucination",
                    Some(reason), Some(result.latency_s), Some(result.wps), Some(result.duration_s),
                );

                // Save hallucination transcript file
                if let Some(ref tdir) = transcripts_dir {
                    transcript::save(
                        tdir, audio_path, &result.text, &model,
                        result.latency_s, result.wps, "dictate", true,
                    );
                }

                return TranscribeResult {
                    text: None, latency: result.latency_s,
                    error: Some(format!("Hallucination ({reason})")),
                    hallucination: true,
                };
            }

            paste::paste_text(&result.text);

            db::record(
                &db, audio_path.to_str(), Some(&result.text), "success",
                None, Some(result.latency_s), Some(result.wps), Some(result.duration_s),
            );

            // Save transcript file
            if let Some(ref tdir) = transcripts_dir {
                transcript::save(
                    tdir, audio_path, &result.text, &model,
                    result.latency_s, result.wps, "dictate", false,
                );
            }

            TranscribeResult {
                text: Some(result.text), latency: result.latency_s,
                error: None, hallucination: false,
            }
        }
        Err(e) => {
            eprintln!("[ptt] Transcription error: {e}");
            db::record(&db, audio_path.to_str(), None, "error", Some(&e), None, None, None);
            TranscribeResult { text: None, latency: 0.0, error: Some(e), hallucination: false }
        }
    }
}

// ── Entry Point ───────────────────────────────────────────────────────
fn main() {
    // Parse CLI arguments before anything else
    match ipc::parse_args() {
        ipc::CliAction::Toggle => {
            let socket_path = config::load().socket_path();
            let pid_path = config::load().pid_path();
            if !ipc::is_running(&pid_path) {
                eprintln!("push-to-talk is not running");
                std::process::exit(1);
            }
            match ipc::send_command(&socket_path, "toggle") {
                Ok(resp) => {
                    println!("{resp}");
                    std::process::exit(0);
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                }
            }
        }
        ipc::CliAction::Status => {
            let socket_path = config::load().socket_path();
            let pid_path = config::load().pid_path();
            if !ipc::is_running(&pid_path) {
                println!("{{\"state\": \"not_running\"}}");
                std::process::exit(0);
            }
            match ipc::send_command(&socket_path, "status") {
                Ok(resp) => {
                    println!("{resp}");
                    std::process::exit(0);
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                }
            }
        }
        ipc::CliAction::RunApp => {
            // Continue to run the menubar app
        }
    }

    let mtm = MainThreadMarker::new()
        .expect("must run on main thread");

    // Ensure config dir exists
    let cfg_dir = config::config_dir();
    let _ = std::fs::create_dir_all(&cfg_dir);

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let delegate = AppDelegate::new(mtm);
    let proto_ref: &ProtocolObject<dyn NSApplicationDelegate> =
        ProtocolObject::from_ref(&*delegate);
    app.setDelegate(Some(proto_ref));

    std::mem::forget(delegate);

    eprintln!("[ptt] Config dir: {}", cfg_dir.display());
    app.run();
}
