// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Where the OS light/dark appearance comes from, read independently of any window's chrome.

use aterm_types::Appearance;
use aterm_window::event_loop::EventLoopProxy;

use crate::Wake;

/// Where `App` reads the OS appearance from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OsAppearanceSource {
    /// No OS read, no observer, and window theme events are ignored.
    Inert,
    /// A development build's launch reading, from `$ATERM_OS_APPEARANCE` (`dark`|`light`, [`aterm_types::dev_seam!`]).
    Injected(Appearance),
    /// Platform appearance reader (`NSApp.effectiveAppearance`, XDG portal/gsettings, or window theme).
    Platform,
}

const LAUNCH_SEAM: &str = "ATERM_OS_APPEARANCE";

impl OsAppearanceSource {
    /// The source a launch uses.
    #[must_use]
    pub(crate) fn for_launch(headless: bool) -> Self {
        let injected = aterm_types::dev_seam!(LAUNCH_SEAM)
            .and_then(|v| v.to_str().and_then(parse_appearance_word));
        Self::choose(headless, injected)
    }

    /// [`Self::for_launch`] with the seam already read: an injected reading
    /// wins, then headless is inert.
    #[must_use]
    const fn choose(headless: bool, injected: Option<Appearance>) -> Self {
        match injected {
            Some(appearance) => Self::Injected(appearance),
            None if headless => Self::Inert,
            None => Self::Platform,
        }
    }

    /// Read the OS appearance before any window, backend, or session is created.
    #[must_use]
    pub(crate) fn initial(self) -> Option<Appearance> {
        match self {
            Self::Inert => None,
            Self::Injected(appearance) => Some(appearance),
            Self::Platform => platform_initial(),
        }
    }

    /// The OS appearance at window attach.
    #[must_use]
    pub(crate) fn at_attach(
        self,
        window_theme: impl FnOnce() -> Option<aterm_window::window::Theme>,
    ) -> Option<Appearance> {
        if self != Self::Platform {
            return None;
        }
        #[cfg(target_os = "macos")]
        {
            let _ = window_theme;
            mac::app_appearance()
        }
        #[cfg(windows)]
        {
            Some(crate::app_colorscheme::theme_to_appearance(window_theme()))
        }
        #[cfg(all(not(target_os = "macos"), not(windows)))]
        {
            let _ = window_theme;
            None
        }
    }

    /// Whether winit's per-window theme events (`WindowEvent::ThemeChanged`) report the OS appearance.
    #[must_use]
    pub(crate) fn reads_window_theme(self) -> bool {
        self == Self::Platform && cfg!(windows)
    }

    /// Start observing live OS flips, each posted as `Wake::OsAppearance`.
    pub(crate) fn observe(self, proxy: &EventLoopProxy<Wake>) -> Option<OsAppearanceObserver> {
        match self {
            Self::Inert | Self::Injected(_) => None,
            Self::Platform => platform_observe(proxy),
        }
    }

    /// The `source=` word the `appearance` control verb prints.
    #[must_use]
    pub(crate) const fn wire_name(self) -> &'static str {
        match self {
            Self::Inert => "inert",
            Self::Injected(_) => "injected",
            Self::Platform if cfg!(target_os = "macos") => "nsapp",
            Self::Platform if cfg!(windows) => "window",
            Self::Platform => "desktop",
        }
    }
}

/// The wire word for an appearance (`dark`/`light`), as the control verbs print and parse it.
#[must_use]
pub(crate) const fn appearance_word(appearance: Appearance) -> &'static str {
    match appearance {
        Appearance::Dark => "dark",
        Appearance::Light => "light",
    }
}

/// [`appearance_word`]'s inverse.
#[must_use]
pub(crate) fn parse_appearance_word(word: &str) -> Option<Appearance> {
    match word {
        "dark" => Some(Appearance::Dark),
        "light" => Some(Appearance::Light),
        _ => None,
    }
}

const INJECTION_ENABLED: bool = cfg!(any(debug_assertions, feature = "dev-seams"));

/// The `appearance [dark|light]` control verb, on a control worker.
pub(crate) fn cmd_appearance(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let set = match appearance_request(rest, INJECTION_ENABLED) {
        Ok(set) => set,
        Err(refusal) => return format!("ERR {refusal}\n"),
    };
    match crate::control::control_media::call_main(proxy, |reply| Wake::ControlAppearance {
        set,
        reply,
    }) {
        Ok(line) => format!("OK {line}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

fn appearance_request(rest: &str, injection: bool) -> Result<Option<Appearance>, &'static str> {
    let mut words = rest.split_whitespace();
    let set = match (words.next(), words.next()) {
        (None, _) => None,
        (Some(word), None) => {
            Some(parse_appearance_word(word).ok_or("usage: appearance [dark|light]")?)
        }
        (Some(_), Some(_)) => return Err("usage: appearance [dark|light]"),
    };
    if set.is_some() && !injection {
        return Err("appearance dark|light needs a development build (dev-seams)");
    }
    Ok(set)
}

/// What [`OsAppearanceSource::observe`] hands back, kept alive by `App`.
#[cfg_attr(
    windows,
    allow(
        dead_code,
        reason = "Windows observes through the window's ThemeChanged and never builds one"
    )
)]
pub(crate) struct OsAppearanceObserver {
    #[cfg(target_os = "macos")]
    _target: aterm_objc::Retained<mac::OsAppearanceTarget>,
}

#[cfg(target_os = "macos")]
fn platform_initial() -> Option<Appearance> {
    mac::defaults_appearance()
}

#[cfg(target_os = "macos")]
fn platform_observe(proxy: &EventLoopProxy<Wake>) -> Option<OsAppearanceObserver> {
    mac::observe(proxy).map(|target| OsAppearanceObserver { _target: target })
}

#[cfg(windows)]
fn platform_initial() -> Option<Appearance> {
    None
}

#[cfg(windows)]
fn platform_observe(_proxy: &EventLoopProxy<Wake>) -> Option<OsAppearanceObserver> {
    None
}

#[cfg(all(not(target_os = "macos"), not(windows)))]
fn platform_initial() -> Option<Appearance> {
    desktop::read_now()
}

#[cfg(all(not(target_os = "macos"), not(windows)))]
fn platform_observe(proxy: &EventLoopProxy<Wake>) -> Option<OsAppearanceObserver> {
    desktop::spawn_watch(proxy.clone()).then_some(OsAppearanceObserver {})
}

/// The Linux/BSD desktop reader: the XDG portal, else GNOME's gsettings key.
#[cfg(any(all(not(target_os = "macos"), not(windows)), test))]
#[cfg_attr(
    not(all(not(target_os = "macos"), not(windows))),
    allow(
        dead_code,
        reason = "the parsers and the tether are tested on every host"
    )
)]
mod desktop {
    use aterm_types::Appearance;

    const PORTAL_DEST: &str = "org.freedesktop.portal.Desktop";
    const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
    const APPEARANCE_NAMESPACE: &str = "org.freedesktop.appearance";
    const COLOR_SCHEME_KEY: &str = "color-scheme";
    const GNOME_SCHEMA: &str = "org.gnome.desktop.interface";

    const READ_BOUND: std::time::Duration = std::time::Duration::from_millis(200);
    const WATCH_READ_BOUND: std::time::Duration = std::time::Duration::from_secs(1);

    /// The portal's `color-scheme` value.
    #[must_use]
    pub(super) fn portal_color_scheme(value: u32) -> Option<Appearance> {
        match value {
            1 => Some(Appearance::Dark),
            0 | 2 => Some(Appearance::Light),
            _ => None,
        }
    }

    /// The `uint32` a `gdbus call … Settings.Read` printed.
    #[must_use]
    pub(super) fn parse_portal_read(stdout: &str) -> Option<u32> {
        let (_, tail) = stdout.rsplit_once("uint32 ")?;
        let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    }

    /// The `color-scheme` value in one `gdbus monitor` line, when the line is the portal's `SettingChanged` for that key.
    #[must_use]
    pub(super) fn parse_portal_signal(line: &str) -> Option<u32> {
        let (_, args) = line.split_once("SettingChanged")?;
        let quoted = |s: &str| format!("'{s}'");
        if !args.contains(&quoted(APPEARANCE_NAMESPACE))
            || !args.contains(&quoted(COLOR_SCHEME_KEY))
        {
            return None;
        }
        parse_portal_read(args)
    }

    /// GNOME's `color-scheme` as `gsettings get` prints it (`'prefer-dark'`), or a `gsettings monitor` line (`color-scheme: 'prefer-dark'`).
    #[must_use]
    pub(super) fn parse_gsettings(text: &str) -> Option<Appearance> {
        let value = text.trim();
        let value = value
            .strip_prefix(COLOR_SCHEME_KEY)
            .and_then(|v| v.strip_prefix(':'))
            .unwrap_or(value)
            .trim()
            .trim_matches('\'');
        match value {
            "prefer-dark" => Some(Appearance::Dark),
            "prefer-light" | "default" => Some(Appearance::Light),
            _ => None,
        }
    }

    fn portal_read_args() -> [&'static str; 11] {
        [
            "call",
            "--session",
            "--dest",
            PORTAL_DEST,
            "--object-path",
            PORTAL_PATH,
            "--method",
            "org.freedesktop.portal.Settings.Read",
            APPEARANCE_NAMESPACE,
            COLOR_SCHEME_KEY,
            "--timeout=1",
        ]
    }

    fn read_portal(bound: std::time::Duration) -> Option<Appearance> {
        crate::platform::read_bounded_stdout("gdbus", &portal_read_args(), bound)
            .as_deref()
            .and_then(parse_portal_read)
            .and_then(portal_color_scheme)
    }

    fn read_gsettings() -> Option<Appearance> {
        crate::platform::read_bounded_stdout(
            "gsettings",
            &["get", GNOME_SCHEMA, COLOR_SCHEME_KEY],
            READ_BOUND,
        )
        .as_deref()
        .and_then(parse_gsettings)
    }

    /// The desktop's appearance now, the launch read.
    pub(super) fn read_now() -> Option<Appearance> {
        read_running_portal().or_else(read_gsettings)
    }

    /// The portal's reading if it is running, asked in-process
    /// (`portal_settings`, which never starts it): no `gdbus` child, and no
    /// wait (200 ms of a Linux window's launch) for a portal that cannot start.
    #[cfg(target_os = "linux")]
    fn read_running_portal() -> Option<Appearance> {
        crate::portal_settings::read(APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY, READ_BOUND)
            .as_ref()
            .and_then(aterm_dbus::Value::as_u32)
            .and_then(portal_color_scheme)
    }

    /// The other unixes have no in-process portal reader (aterm-dbus is a
    /// Linux dependency): the bounded `gdbus` read.
    #[cfg(not(target_os = "linux"))]
    fn read_running_portal() -> Option<Appearance> {
        read_portal(READ_BOUND)
    }

    const MAX_WATCH_LINE_BYTES: usize = 64 << 10;

    pub(super) struct BoundedLines(std::io::BufReader<std::process::ChildStdout>);

    impl Iterator for BoundedLines {
        type Item = std::io::Result<String>;

        fn next(&mut self) -> Option<Self::Item> {
            use std::io::{BufRead as _, Read as _};
            let mut buf = Vec::new();
            match (&mut self.0)
                .take((MAX_WATCH_LINE_BYTES as u64) + 1)
                .read_until(b'\n', &mut buf)
            {
                Ok(0) => None,
                Ok(_) => {
                    if !buf.ends_with(b"\n") && buf.len() > MAX_WATCH_LINE_BYTES {
                        while let Ok(chunk) = self.0.fill_buf() {
                            if chunk.is_empty() {
                                break;
                            }
                            let nl = chunk.iter().position(|&b| b == b'\n');
                            let len = chunk.len();
                            if let Some(nl) = nl {
                                self.0.consume(nl + 1);
                                break;
                            }
                            self.0.consume(len);
                        }
                    }
                    if buf.ends_with(b"\n") {
                        buf.pop();
                        if buf.ends_with(b"\r") {
                            buf.pop();
                        }
                    }
                    Some(
                        String::from_utf8(buf)
                            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
                    )
                }
                Err(e) => Some(Err(e)),
            }
        }
    }

    /// A long-running watcher (`gdbus monitor`, `gsettings monitor`) tethered to this process.
    pub(super) struct Tethered {
        child: aterm_uds::spawnfd::Child,
        /// The tether: dropping it ends the watcher.
        tether: Option<std::process::ChildStdin>,
        pub(super) lines: BoundedLines,
    }

    impl Tethered {
        /// Close the tether.
        pub(super) fn release(&mut self) {
            drop(self.tether.take());
        }
    }

    const TETHER_SCRIPT: &str =
        r#""$@" & pid=$!; exec >/dev/null; read -r _line || :; kill "$pid" 2>/dev/null; wait"#;

    pub(super) fn tethered(program: &str, args: &[&str]) -> Option<Tethered> {
        let mut child = aterm_uds::spawnfd::command("/bin/sh")
            .arg("-c")
            .arg(TETHER_SCRIPT)
            .arg("sh")
            .arg(program)
            .args(args)
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .ok()?;
        let tether = child.stdin.take();
        let stdout = child.stdout.take()?;
        Some(Tethered {
            child,
            tether,
            lines: BoundedLines(std::io::BufReader::new(stdout)),
        })
    }

    impl Drop for Tethered {
        fn drop(&mut self) {
            self.release();
            let _ = self.child.wait();
        }
    }

    /// Start the worker that posts every desktop appearance it reads.
    #[cfg(all(not(target_os = "macos"), not(windows)))]
    pub(super) fn spawn_watch(
        proxy: aterm_window::event_loop::EventLoopProxy<crate::Wake>,
    ) -> bool {
        std::thread::Builder::new()
            .name("aterm-os-appearance".into())
            .spawn(move || {
                crate::qos::set_self(crate::qos::Role::Background);
                let post = |appearance| {
                    let _ = proxy.send_event(crate::Wake::OsAppearance(appearance));
                };
                watch(&post);
            })
            .is_ok()
    }

    /// The worker body: watch the portal, else gsettings, posting the current
    /// value first and then every change.
    #[cfg(all(not(target_os = "macos"), not(windows)))]
    fn watch(post: &dyn Fn(Appearance)) {
        let portal_monitor = [
            "monitor",
            "--session",
            "--dest",
            PORTAL_DEST,
            "--object-path",
            PORTAL_PATH,
        ];
        if let Some(watcher) = tethered("gdbus", &portal_monitor)
            && let Some(now) = read_portal(WATCH_READ_BOUND)
        {
            follow(watcher, now, post, "the XDG desktop portal", |line| {
                parse_portal_signal(line).and_then(portal_color_scheme)
            });
            return;
        }
        if let Some(watcher) = tethered("gsettings", &["monitor", GNOME_SCHEMA, COLOR_SCHEME_KEY])
            && let Some(now) = read_gsettings()
        {
            follow(
                watcher,
                now,
                post,
                "gsettings color-scheme",
                parse_gsettings,
            );
            return;
        }
        aterm_log::info!(
            "os appearance: no XDG portal or gsettings color-scheme answered; a split theme \
             keeps its dark side"
        );
    }

    /// Post `now`, then every reading `parse` finds in the watcher's lines,
    /// until the watcher ends.
    #[cfg(all(not(target_os = "macos"), not(windows)))]
    fn follow(
        mut watcher: Tethered,
        now: Appearance,
        post: &dyn Fn(Appearance),
        what: &str,
        parse: impl Fn(&str) -> Option<Appearance>,
    ) {
        post(now);
        aterm_log::info!("os appearance: following {what}");
        for line in watcher.lines.by_ref().map_while(Result::ok) {
            if let Some(appearance) = parse(&line) {
                post(appearance);
            }
        }
        aterm_log::info!("os appearance: the {what} watcher ended");
    }
}

/// macOS: the application-level appearance.
#[cfg(target_os = "macos")]
mod mac {
    use aterm_objc::{Id, class, sel};
    use aterm_types::Appearance;
    use aterm_window::event_loop::EventLoopProxy;

    use crate::Wake;
    use crate::appkit::{self, MainThread};

    const KEY_PATH: &str = "effectiveAppearance";

    /// Map a `-bestMatchFromAppearancesWithNames:` answer to an [`Appearance`].
    ///
    /// # Safety
    /// `best` must be nil or a live `NSString`.
    unsafe fn appearance_of_best_match(best: Id) -> Option<Appearance> {
        if best.is_null() {
            return None;
        }
        // SAFETY: `-isEqualToString:` is `B@:@` on `NSString`; `best` is a live
        // NSString (caller) and the name a live AppKit global.
        let dark = unsafe {
            appkit::send_bool_id(best, sel!(isEqualToString:), appkit::appearance_name(true))
        };
        Some(if dark {
            Appearance::Dark
        } else {
            Appearance::Light
        })
    }

    /// The light/dark side of `appearance`, an `NSAppearance`.
    /// # Safety
    /// `appearance` must be nil or a live `NSAppearance`.
    unsafe fn appearance_of(appearance: Id) -> Option<Appearance> {
        if appearance.is_null() {
            return None;
        }
        let names = [
            appkit::appearance_name(false),
            appkit::appearance_name(true),
        ];
        // SAFETY: `+arrayWithObjects:count:` is `@@:r^@Q` and reads `count`
        // live, non-nil objects (two AppKit globals); the array is
        // autoreleased and borrowed inside the caller's pool.
        // `-bestMatchFromAppearancesWithNames:` is `@@:@` on `NSAppearance`
        // and answers a borrowed `NSString` or nil.
        unsafe {
            let array = appkit::send_id_idptr_usize(
                class(c"NSArray").as_id(),
                sel!(arrayWithObjects:count:),
                names.as_ptr(),
                names.len(),
            );
            if array.is_null() {
                return None;
            }
            let best =
                appkit::send_id_id(appearance, sel!(bestMatchFromAppearancesWithNames:), array);
            appearance_of_best_match(best)
        }
    }

    /// `NSApp.effectiveAppearance`, the application-level appearance a window's forced `appearance` does not change.
    pub(super) fn app_appearance() -> Option<Appearance> {
        let main_thread = MainThread::new()?;
        let app = appkit::existing_application(main_thread)?;
        aterm_objc::autoreleasepool(|_| {
            // SAFETY: `app` is the existing main-thread AppKit application;
            // `-effectiveAppearance` is `@@:` on
            // `NSApplication` (10.14+) and answers a borrowed `NSAppearance`.
            unsafe { appearance_of(appkit::send_id(app, sel!(effectiveAppearance))) }
        })
    }

    /// The `AppleInterfaceStyle` user default.
    pub(super) fn defaults_appearance() -> Option<Appearance> {
        aterm_objc::autoreleasepool(|_| {
            let key = appkit::nsstring("AppleInterfaceStyle")?;
            // SAFETY: `+standardUserDefaults` is `-(id)`, the process
            // singleton; `-stringForKey:` is `@@:@` and answers a borrowed
            // `NSString` or nil, read into a Rust string at once.
            let style = unsafe {
                let defaults =
                    appkit::send_id(class(c"NSUserDefaults").as_id(), sel!(standardUserDefaults));
                if defaults.is_null() {
                    return None;
                }
                appkit::nsstring_to_rust(appkit::send_id_id(
                    defaults,
                    sel!(stringForKey:),
                    key.id(),
                ))
            };
            Some(style_appearance(&style))
        })
    }

    /// `AppleInterfaceStyle`'s value.
    pub(super) const fn style_appearance(style: &str) -> Appearance {
        if style.eq_ignore_ascii_case("dark") {
            Appearance::Dark
        } else {
            Appearance::Light
        }
    }

    /// The KVO target's ivars.
    pub(crate) struct OsAppearanceIvars {
        relay: Box<dyn Fn()>,
    }

    aterm_objc::declare_class! {
        /// The observer of `NSApp.effectiveAppearance`.
        pub(crate) struct OsAppearanceTarget: NSObject {
            const NAME: &str = "ATermOsAppearanceTarget";
            type Ivars = OsAppearanceIvars;

            @sel(observeValueForKeyPath:ofObject:change:context:)
            fn observe_value(
                &self,
                _key_path: aterm_objc::Id,
                _object: aterm_objc::Id,
                _change: aterm_objc::Id,
                _context: *mut core::ffi::c_void,
            ) {
                (self.ivars().relay)();
            }
        }
    }

    pub(super) fn new_target(
        mtm: MainThread,
        relay: Box<dyn Fn()>,
    ) -> Option<aterm_objc::Retained<OsAppearanceTarget>> {
        OsAppearanceTarget::alloc_init(mtm, OsAppearanceIvars { relay })
    }

    /// Observe `NSApp.effectiveAppearance` for the process's life.
    pub(super) fn observe(
        proxy: &EventLoopProxy<Wake>,
    ) -> Option<aterm_objc::Retained<OsAppearanceTarget>> {
        let main_thread = MainThread::new()?;
        let app = appkit::existing_application(main_thread)?;
        let proxy = proxy.clone();
        let target = new_target(
            main_thread,
            Box::new(move || {
                if let Some(appearance) = app_appearance() {
                    let _ = proxy.send_event(Wake::OsAppearance(appearance));
                }
            }),
        )?;
        let registered = aterm_objc::autoreleasepool(|_| {
            let Some(key) = appkit::nsstring(KEY_PATH) else {
                return false;
            };
            // SAFETY: `app` is the existing main-thread AppKit application;
            // `-addObserver:forKeyPath:options:context:` is `v@:@@Q^v` on
            // every `NSObject` — `target` lives for the process (the caller
            // keeps it), `key` is a live +1 NSString KVO copies, options 0,
            // and a NULL context it never dereferences.
            unsafe {
                appkit::send_v_id_id_usize_ptr(
                    app,
                    sel!(addObserver:forKeyPath:options:context:),
                    target.as_id(),
                    key.id(),
                    0,
                    core::ptr::null_mut(),
                );
            }
            true
        });
        registered.then_some(target)
    }

    #[cfg(test)]
    mod tests;
}

#[cfg(test)]
#[path = "os_appearance_tests.rs"]
mod tests;
