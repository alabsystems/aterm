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
    /// This platform's reader (see the module doc).
    Platform,
}

/// The development seam naming an injected launch appearance.
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
    fn choose(headless: bool, injected: Option<Appearance>) -> Self {
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
    pub(crate) fn wire_name(self) -> &'static str {
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
pub(crate) fn appearance_word(appearance: Appearance) -> &'static str {
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

/// Whether this build accepts an injected flip (`appearance dark|light`): a
/// development build only, like every `dev_seam!`.
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

/// The `appearance` verb's arguments: none (read), or one `dark`|`light` to
/// inject, which only a build with `injection` admits.
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

    /// The portal's settings interface, on the session bus.
    const PORTAL_DEST: &str = "org.freedesktop.portal.Desktop";
    const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
    const APPEARANCE_NAMESPACE: &str = "org.freedesktop.appearance";
    const COLOR_SCHEME_KEY: &str = "color-scheme";
    /// GNOME's own key, read where no portal answers.
    const GNOME_SCHEMA: &str = "org.gnome.desktop.interface";

    /// How long one read may take. The launch read runs on the main thread
    /// before the first window, so it is bounded well under a visible delay;
    /// a slower desktop answers "unknown" and the engine default stands until
    /// the watcher's own read lands.
    const READ_BOUND: std::time::Duration = std::time::Duration::from_millis(200);

    /// How long the watcher's portal read may take, on its own thread: `gdbus`'s
    /// own call timeout (`--timeout=1`, [`portal_read_args`]).
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

    /// One portal read within `bound`.
    fn read_portal(bound: std::time::Duration) -> Option<Appearance> {
        crate::platform::read_bounded_stdout("gdbus", &portal_read_args(), bound)
            .as_deref()
            .and_then(parse_portal_read)
            .and_then(portal_color_scheme)
    }

    /// One bounded gsettings read.
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
    /// wait for a portal that cannot start (200 ms of a Linux window's launch
    /// before, 2026-10-20).
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

    /// A long-running watcher (`gdbus monitor`, `gsettings monitor`) TETHERED to this process.
    pub(super) struct Tethered {
        child: aterm_uds::spawnfd::Child,
        /// The tether: dropping it ends the watcher.
        tether: Option<std::process::ChildStdin>,
        pub(super) lines: std::io::Lines<std::io::BufReader<std::process::ChildStdout>>,
    }

    impl Tethered {
        /// Close the tether.
        pub(super) fn release(&mut self) {
            drop(self.tether.take());
        }
    }

    /// The shell around a tethered watcher: start it, drop this shell's own
    /// stdout, wait for the tether to close, then end it.
    const TETHER_SCRIPT: &str =
        r#""$@" & pid=$!; exec >/dev/null; read -r _line || :; kill "$pid" 2>/dev/null; wait"#;

    pub(super) fn tethered(program: &str, args: &[&str]) -> Option<Tethered> {
        use std::io::BufRead as _;
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
            lines: std::io::BufReader::new(stdout).lines(),
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

    /// The key path observed on `NSApp`.
    const KEY_PATH: &str = "effectiveAppearance";

    /// The appearance a `-bestMatchFromAppearancesWithNames:` answer names:
    /// dark for `NSAppearanceNameDarkAqua`, light for anything else it picked
    /// (only Aqua is offered), `None` for no match.
    ///
    /// # Safety
    /// `best` must be nil or a live `NSString`.
    unsafe fn appearance_of_best_match(best: Id) -> Option<Appearance> {
        if best.is_null() {
            return None;
        }
        // SAFETY: `-isEqualToString:` is `B@:@` on `NSString`.
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
        // SAFETY: `+arrayWithObjects:count:` is `@@:r^@Q` and reads `count` live, non-nil objects (two AppKit globals).
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
            // SAFETY: `app` is the existing main-thread AppKit application.
            unsafe { appearance_of(appkit::send_id(app, sel!(effectiveAppearance))) }
        })
    }

    /// The `AppleInterfaceStyle` user default.
    pub(super) fn defaults_appearance() -> Option<Appearance> {
        aterm_objc::autoreleasepool(|_| {
            let key = appkit::nsstring("AppleInterfaceStyle")?;
            // SAFETY: `+standardUserDefaults` is `-(id)`, the process singleton.
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
    pub(super) fn style_appearance(style: &str) -> Appearance {
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

            /// `v@:@@@^v`. Registered for one key path on one object, so
            /// every call is an appearance change; options are 0, so `change`
            /// is not read.
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
            // SAFETY: `app` is the existing main-thread AppKit application.
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
    mod tests {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use aterm_types::Appearance;

        /// The KVO callback the runtime dispatches reaches the relay: the
        /// class is registered under its name, and a real
        /// `observeValueForKeyPath:ofObject:change:context:` send runs the
        /// Rust body.
        #[test]
        fn the_kvo_callback_runs_the_relay() {
            use aterm_objc::{ClassType, class};
            let hits = Arc::new(AtomicUsize::new(0));
            let seen = Arc::clone(&hits);
            let target = super::new_target(
                crate::appkit::test_witness(),
                Box::new(move || {
                    seen.fetch_add(1, Ordering::SeqCst);
                }),
            )
            .expect("an OsAppearanceTarget");
            assert_eq!(
                class(c"ATermOsAppearanceTarget"),
                super::OsAppearanceTarget::class()
            );
            // SAFETY: the declared selector's exact prototype, on a live instance.
            unsafe {
                let send: unsafe extern "C-unwind" fn(
                    aterm_objc::Id,
                    aterm_objc::Sel,
                    aterm_objc::Id,
                    aterm_objc::Id,
                    aterm_objc::Id,
                    *mut core::ffi::c_void,
                ) = aterm_objc::checked_msg();
                send(
                    target.as_id(),
                    aterm_objc::sel!(observeValueForKeyPath:ofObject:change:context:),
                    aterm_objc::Id::NIL,
                    aterm_objc::Id::NIL,
                    aterm_objc::Id::NIL,
                    core::ptr::null_mut(),
                );
            }
            assert_eq!(hits.load(Ordering::SeqCst), 1);
        }

        /// The best-match answer maps DarkAqua to dark and Aqua to light;
        /// nil is no reading.
        #[test]
        fn best_match_names_map_to_appearances() {
            // SAFETY: AppKit's own name globals, and nil.
            unsafe {
                assert_eq!(
                    super::appearance_of_best_match(crate::appkit::appearance_name(true)),
                    Some(Appearance::Dark)
                );
                assert_eq!(
                    super::appearance_of_best_match(crate::appkit::appearance_name(false)),
                    Some(Appearance::Light)
                );
                assert_eq!(super::appearance_of_best_match(aterm_objc::Id::NIL), None);
            }
        }

        /// `AppleInterfaceStyle = Dark` is dark; absent (empty) is light.
        #[test]
        fn the_interface_style_default_maps() {
            assert_eq!(super::style_appearance("Dark"), Appearance::Dark);
            assert_eq!(super::style_appearance(""), Appearance::Light);
            assert_eq!(super::style_appearance("Light"), Appearance::Light);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::desktop::{
        parse_gsettings, parse_portal_read, parse_portal_signal, portal_color_scheme,
    };
    use super::{OsAppearanceSource, parse_appearance_word};
    use aterm_types::Appearance;

    /// The portal's three values: prefer-dark, prefer-light and no preference
    /// (light, as GNOME draws it); an unknown value is no reading.
    #[test]
    fn the_portal_mapping_covers_prefer_dark_prefer_light_and_none() {
        assert_eq!(portal_color_scheme(1), Some(Appearance::Dark));
        assert_eq!(portal_color_scheme(2), Some(Appearance::Light));
        assert_eq!(portal_color_scheme(0), Some(Appearance::Light));
        assert_eq!(
            portal_color_scheme(3),
            None,
            "negative control: not a portal value"
        );
    }

    /// `gdbus call` prints Read's double variant and ReadOne's single one;
    /// an error prints neither.
    #[test]
    fn a_portal_read_parses_both_variant_depths() {
        assert_eq!(parse_portal_read("(<<uint32 1>>,)\n"), Some(1));
        assert_eq!(parse_portal_read("(<uint32 2>,)\n"), Some(2));
        assert_eq!(
            parse_portal_read(
                "Error: GDBus.Error:org.freedesktop.portal.Error.NotFound: Requested setting not found\n"
            ),
            None
        );
    }

    /// Only the appearance key's `SettingChanged` is a reading; another
    /// setting's signal on the same interface is not.
    #[test]
    fn a_portal_signal_is_read_only_for_the_color_scheme_key() {
        let line = "/org/freedesktop/portal/desktop: org.freedesktop.portal.Settings.SettingChanged \
                    ('org.freedesktop.appearance', 'color-scheme', <uint32 1>)";
        assert_eq!(
            parse_portal_signal(line).and_then(portal_color_scheme),
            Some(Appearance::Dark)
        );
        let other = "/org/freedesktop/portal/desktop: org.freedesktop.portal.Settings.SettingChanged \
                     ('org.freedesktop.appearance', 'accent-color', <uint32 1>)";
        assert_eq!(
            parse_portal_signal(other),
            None,
            "negative control: another key"
        );
        assert_eq!(parse_portal_signal("(<<uint32 1>>,)"), None, "not a signal");
    }

    /// GNOME's key, as `get` and `monitor` print it.
    #[test]
    fn the_gsettings_fallback_maps_gnome_color_scheme() {
        assert_eq!(parse_gsettings("'prefer-dark'\n"), Some(Appearance::Dark));
        assert_eq!(parse_gsettings("'prefer-light'\n"), Some(Appearance::Light));
        assert_eq!(parse_gsettings("'default'\n"), Some(Appearance::Light));
        assert_eq!(
            parse_gsettings("color-scheme: 'prefer-dark'"),
            Some(Appearance::Dark)
        );
        assert_eq!(parse_gsettings("No such key “color-scheme”\n"), None);
    }

    /// Headless and test Apps read nothing and follow no window theme.
    #[test]
    fn the_inert_source_reads_nothing() {
        let inert = OsAppearanceSource::Inert;
        assert_eq!(inert.initial(), None);
        assert_eq!(
            inert.at_attach(|| Some(aterm_window::window::Theme::Light)),
            None
        );
        assert!(!inert.reads_window_theme());
        assert_eq!(OsAppearanceSource::choose(true, None), inert);
        assert_eq!(
            OsAppearanceSource::choose(false, None),
            OsAppearanceSource::Platform
        );
    }

    /// A development build's injected launch reading is what launch builds
    /// for, on any lane, and nothing is observed after it.
    #[test]
    fn an_injected_launch_reading_wins_and_observes_nothing() {
        for headless in [true, false] {
            let source = OsAppearanceSource::choose(headless, Some(Appearance::Light));
            assert_eq!(source, OsAppearanceSource::Injected(Appearance::Light));
            assert_eq!(source.initial(), Some(Appearance::Light));
            assert_eq!(
                source.at_attach(|| Some(aterm_window::window::Theme::Dark)),
                None
            );
            assert!(!source.reads_window_theme());
        }
    }

    /// The verb reads bare, injects one word, and refuses anything else; a
    /// shipped build (no injection) refuses the injection but still reads.
    #[test]
    fn the_appearance_verb_parses_and_gates_injection() {
        use super::appearance_request;
        assert_eq!(appearance_request("", true), Ok(None));
        assert_eq!(
            appearance_request(" light ", true),
            Ok(Some(Appearance::Light))
        );
        assert_eq!(appearance_request("dark", true), Ok(Some(Appearance::Dark)));
        assert!(appearance_request("auto", true).is_err());
        assert!(appearance_request("dark light", true).is_err());
        assert_eq!(appearance_request("", false), Ok(None));
        assert!(
            appearance_request("light", false).is_err(),
            "negative control: a shipped build cannot inject"
        );
    }

    #[test]
    fn appearance_words_round_trip() {
        for a in [Appearance::Dark, Appearance::Light] {
            assert_eq!(parse_appearance_word(super::appearance_word(a)), Some(a));
        }
        assert_eq!(parse_appearance_word("auto"), None);
    }

    /// The tether law: closing the tether ends the watched program, so a
    /// watcher never outlives the process holding it.
    #[cfg(unix)]
    #[test]
    fn closing_the_tether_ends_the_watcher() {
        let mut watcher =
            super::desktop::tethered("/bin/sh", &["-c", "while :; do echo tick; sleep 0.1; done"])
                .expect("spawn a tethered watcher");
        for _ in 0..11 {
            let line = watcher.lines.next().expect("a line").expect("utf-8");
            assert_eq!(line, "tick", "the watcher runs while tethered");
        }
        watcher.release();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in watcher.lines.by_ref() {
                if line.is_err() {
                    break;
                }
            }
            let _ = tx.send(());
            drop(watcher);
        });
        rx.recv_timeout(std::time::Duration::from_secs(60))
            .expect("the watcher ended when its tether closed");
    }
}
