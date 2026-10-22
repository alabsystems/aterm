// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The front door's windowing grammar and routing policy: `aterm new-tab`, `aterm new-window`,
//! `aterm split-pane`, and whether a request starts a process or is handed to a running aterm
//! over the control socket's `spawn` verb, in the style of Windows Terminal's `wt`.
//!
//! A policy, not a single-instance mutex: several instances may co-exist (an agent may run its
//! own on its own `--control-sock`). Everything here is pure, testable without a socket, window
//! or config file.

/// Where a windowing request should be served; the result of [`route_launch`].
/// "Forward, else start one" is a fallback the caller applies after a forward
/// errors, not a route, so a real failure is not hidden.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum WindowRoute {
    /// Start a new aterm window in this process.
    Spawn,
    /// Hand the request to the running instance over the control socket and exit.
    Forward,
}

/// The `windowing_behavior` config key: what a plain launch (and an explicit
/// `new-tab` / `split-pane`) should do when an aterm is already running.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum WindowingBehavior {
    /// Every launch is its own window and process. The default.
    #[default]
    NewWindow,
    /// A launch joins the running instance (a new tab there) and exits; with no
    /// instance reachable it starts one, so the first launch still works.
    Attach,
}

impl WindowingBehavior {
    /// Parse one config value, case-insensitively and trimmed. `None` for an
    /// unrecognized spelling so the caller can report it. Windows Terminal's
    /// `useNew` / `useExisting` are accepted as aliases.
    #[must_use]
    pub fn parse(raw: &str) -> Option<WindowingBehavior> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "new_window" | "new-window" | "newwindow" | "usenew" | "new" => {
                Some(WindowingBehavior::NewWindow)
            }
            "attach" | "use_existing" | "use-existing" | "useexisting" | "existing" => {
                Some(WindowingBehavior::Attach)
            }
            _ => None,
        }
    }
}

/// What kind of terminal the invocation asked for: three explicit verbs, or a
/// plain `aterm` launch.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum LaunchIntent {
    /// `aterm new-tab` — a tab, wherever the policy says tabs go.
    NewTab,
    /// `aterm new-window`: a window, unconditionally, so `attach` always has an
    /// escape (the same word Windows Terminal uses).
    NewWindow,
    /// `aterm split-pane` — a pane beside the focused one.
    SplitPane,
    /// A bare window launch with nothing else asked of it (no `-e`,
    /// `--headless`, diagnostics). The only policy-eligible plain shape; see
    /// [`plain_launch_is_policy_eligible`].
    Plain,
}

/// The routing decision. Pure: policy + what was asked + whether an instance
/// is reachable.
///
/// * `NewWindow` is never forwarded: a verb naming the outcome outranks the
///   preference.
/// * `NewTab` / `SplitPane` / `Plain` forward only under `attach` and only when
///   an instance answered; under `new_window` they spawn.
/// * Nothing reachable always means spawn.
///
/// A `SplitPane` that spawns has nothing to split (as `wt split-pane` under
/// `useNew`); the caller says so on stderr.
#[must_use]
pub fn route_launch(
    intent: LaunchIntent,
    behavior: WindowingBehavior,
    instance_reachable: bool,
) -> WindowRoute {
    match intent {
        LaunchIntent::NewWindow => WindowRoute::Spawn,
        LaunchIntent::NewTab | LaunchIntent::SplitPane | LaunchIntent::Plain => {
            if behavior == WindowingBehavior::Attach && instance_reachable {
                WindowRoute::Forward
            } else {
                WindowRoute::Spawn
            }
        }
    }
}

/// Which way a `split-pane` divides the focused pane.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum SplitOrientation {
    /// Side by side — the new pane takes the right half. The default, matching
    /// the shape a terminal window usually has (wider than tall).
    #[default]
    Vertical,
    /// Stacked — the new pane takes the bottom half.
    Horizontal,
}

impl SplitOrientation {
    /// The one-letter token the `spawn` control verb takes (`split=v` / `split=h`).
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            SplitOrientation::Vertical => "v",
            SplitOrientation::Horizontal => "h",
        }
    }
}

/// A parsed windowing request: the verb plus its resolved arguments.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WindowRequest {
    /// What was asked for.
    pub intent: LaunchIntent,
    /// The `-d <dir>` starting directory, already made absolute, since the
    /// serving process may have a different working directory.
    pub dir: Option<String>,
    /// `split-pane`'s orientation; ignored (not rejected) for other verbs.
    pub split: SplitOrientation,
}

impl WindowRequest {
    /// The control-socket request line asking a running instance to serve
    /// this: `spawn` opens a tab in the frontmost window (`spawn split=<v|h>`
    /// splits the focused pane).
    ///
    /// The cwd is quoted (`cwd="…"`) only when it contains whitespace, since the
    /// protocol is whitespace-split; the unquoted form also reaches older
    /// servers, which take the value verbatim, quotes included.
    ///
    /// Returns `Err` for a directory that cannot be framed: a `"` would close
    /// the quoted value and a newline would inject a second authenticated verb.
    pub fn control_request(&self) -> Result<String, String> {
        let mut line = String::from("spawn");
        if self.intent == LaunchIntent::SplitPane {
            line.push_str(" split=");
            line.push_str(self.split.wire());
        }
        if let Some(dir) = &self.dir {
            if dir.contains(['"', '\n', '\r']) {
                let mut msg =
                    String::from("cannot forward a directory containing a quote or newline: ");
                msg.push_str(dir);
                return Err(msg);
            }
            line.push_str(" cwd=");
            let needs_quotes = dir.chars().any(char::is_whitespace);
            if needs_quotes {
                line.push('"');
            }
            line.push_str(dir);
            if needs_quotes {
                line.push('"');
            }
        }
        line.push('\n');
        Ok(line)
    }

    /// The argument list for the spawn route: only the window's own
    /// `-d/--working-directory`, so it matches `aterm --window [-d <dir>]`.
    #[must_use]
    pub fn window_args(&self) -> Vec<std::ffi::OsString> {
        match &self.dir {
            Some(dir) => vec![
                std::ffi::OsString::from("-d"),
                std::ffi::OsString::from(dir),
            ],
            None => Vec::new(),
        }
    }
}

/// The usage line for one windowing verb: `--help` starts with it and an option error ends
/// with it.
#[must_use]
pub fn window_verb_usage(verb: &str) -> String {
    let mut s = String::from("usage: aterm ");
    s.push_str(verb);
    if verb == "split-pane" {
        s.push_str(" [-H|-V]");
    }
    s.push_str(" [-d <dir>]");
    s
}

/// Parse `aterm <verb> [args…]`, the wt-shaped grammar.
///
/// `-d <dir>` / `-d=<dir>` / `--directory` / `--working-directory` /
/// `--startingDirectory` all name the starting directory (aterm's window flag
/// plus Windows Terminal's spelling).
///
/// `resolve_dir` turns the raw argument into the absolute path the request
/// carries (injected so the parse stays pure); it returns `Err(message)` for a
/// value that is not a usable directory.
///
/// Unknown options are refused, not ignored: a dropped `-d` would open in the
/// wrong directory.
pub fn parse_window_request<F>(
    verb: &str,
    args: &[std::ffi::OsString],
    resolve_dir: F,
) -> Result<WindowRequest, String>
where
    F: Fn(&str) -> Result<String, String>,
{
    let intent = match verb {
        "new-tab" => LaunchIntent::NewTab,
        "new-window" => LaunchIntent::NewWindow,
        "split-pane" => LaunchIntent::SplitPane,
        other => {
            let mut msg = String::from("not a windowing verb: ");
            msg.push_str(other);
            return Err(msg);
        }
    };
    let mut dir: Option<String> = None;
    let mut split = SplitOrientation::default();
    let mut it = args.iter();
    while let Some(raw) = it.next() {
        let arg = raw.to_string_lossy().into_owned();
        let value = |it: &mut std::slice::Iter<'_, std::ffi::OsString>| -> Result<String, String> {
            match it.next() {
                Some(v) => Ok(v.to_string_lossy().into_owned()),
                None => {
                    let mut msg = String::from("missing <dir> after ");
                    msg.push_str(&arg);
                    msg.push('\n');
                    msg.push_str(&window_verb_usage(verb));
                    Err(msg)
                }
            }
        };
        match arg.as_str() {
            "-d" | "--directory" | "--working-directory" | "--startingDirectory" => {
                dir = Some(resolve_dir(&value(&mut it)?)?);
            }
            "-H" | "--horizontal" if intent == LaunchIntent::SplitPane => {
                split = SplitOrientation::Horizontal;
            }
            "-V" | "--vertical" if intent == LaunchIntent::SplitPane => {
                split = SplitOrientation::Vertical;
            }
            _ => {
                if let Some(v) = arg
                    .strip_prefix("-d=")
                    .or_else(|| arg.strip_prefix("--directory="))
                    .or_else(|| arg.strip_prefix("--working-directory="))
                    .or_else(|| arg.strip_prefix("--startingDirectory="))
                {
                    dir = Some(resolve_dir(v)?);
                } else {
                    let mut msg = String::from("unknown option ");
                    msg.push_str(&arg);
                    msg.push_str(" for `aterm ");
                    msg.push_str(verb);
                    msg.push_str("`\n");
                    msg.push_str(&window_verb_usage(verb));
                    return Err(msg);
                }
            }
        }
    }
    Ok(WindowRequest { intent, dir, split })
}

/// The ambient facts the plain-launch gate consults, gathered by the caller so
/// [`plain_launch_is_policy_eligible`] stays pure.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct LaunchEnv {
    /// `$ATERM_UPDATED_FROM` is set — this process is an update successor,
    /// re-spawned by the outgoing one with its argv verbatim to finish an apply.
    pub updated_from: bool,
}

/// The `-d <dir>` spellings a plain launch may carry and stay policy-eligible:
/// only the two the window's parser accepts (`crates/aterm-gui/src/cli.rs`). A
/// plain launch is passed through verbatim on the spawn route, so a spelling
/// the window rejects must not be forwardable under `attach` either.
const PLAIN_LAUNCH_DIR_FLAGS: &[&str] = &["-d", "--working-directory"];

/// Whether a plain (verb-less) window launch may be routed by policy.
///
/// Excluded, each for a concrete reason:
///
/// * `-e` / `--command` / `--` carry a child command line `spawn` cannot.
/// * `--headless` wants an engine + socket with no window; forwarding would
///   open a tab somewhere and leave CI waiting.
/// * `--diagnose` is the release gates' probe and must measure this process.
/// * `$ATERM_UPDATED_FROM` marks an update successor re-spawning with its own
///   argv; forwarding it would stop the update from landing.
/// * Anything else unrecognized fails closed to spawning.
///
/// `--window` is eligible (it names the mode, not the destination), as is
/// `-d <dir>` in `PLAIN_LAUNCH_DIR_FLAGS` spellings. Windows'
/// `RegisterApplicationRestart` relaunch registers the `new-window` verb, which
/// is never forwarded, so it never reaches this gate.
#[must_use]
pub fn plain_launch_is_policy_eligible(scan: &[std::ffi::OsString], env: LaunchEnv) -> bool {
    if env.updated_from {
        return false;
    }
    let mut it = scan.iter();
    while let Some(raw) = it.next() {
        let arg = raw.to_string_lossy().into_owned();
        if arg == "--window" {
            continue;
        }
        if PLAIN_LAUNCH_DIR_FLAGS.contains(&arg.as_str()) {
            if it.next().is_none() {
                return false;
            }
            continue;
        }
        return false;
    }
    true
}

/// The raw `-d <dir>` operand of a plain launch, for the caller to resolve.
/// Shares `PLAIN_LAUNCH_DIR_FLAGS` with the gate so the two agree.
#[must_use]
pub fn plain_launch_dir_operand(scan: &[std::ffi::OsString]) -> Option<String> {
    let mut it = scan.iter();
    while let Some(raw) = it.next() {
        let arg = raw.to_string_lossy().into_owned();
        if PLAIN_LAUNCH_DIR_FLAGS.contains(&arg.as_str()) {
            return it.next().map(|v| v.to_string_lossy().into_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    /// The resolver a parse test uses: absolute-looking passthrough, so the
    /// grammar is exercised without touching a filesystem.
    fn echo_dir(raw: &str) -> Result<String, String> {
        Ok(raw.to_string())
    }

    #[test]
    fn the_default_policy_is_new_window() {
        assert_eq!(WindowingBehavior::default(), WindowingBehavior::NewWindow);
        // An unrecognized value must not silently become `attach`: a typo in
        // aterm.toml may not change where terminals open.
        assert_eq!(WindowingBehavior::parse("join-please"), None);
    }

    #[test]
    fn policy_values_and_their_windows_terminal_aliases_parse() {
        for spelling in [
            "attach",
            " Attach ",
            "use_existing",
            "useExisting",
            "existing",
        ] {
            assert_eq!(
                WindowingBehavior::parse(spelling),
                Some(WindowingBehavior::Attach),
                "{spelling}"
            );
        }
        for spelling in ["new_window", "NEW-WINDOW", "useNew", "new"] {
            assert_eq!(
                WindowingBehavior::parse(spelling),
                Some(WindowingBehavior::NewWindow),
                "{spelling}"
            );
        }
    }

    /// Every cell of the routing table.
    #[test]
    fn routing_is_the_whole_table() {
        use LaunchIntent::{NewTab, NewWindow, Plain, SplitPane};
        use WindowRoute::{Forward, Spawn};
        use WindowingBehavior::{Attach, NewWindow as PolicyNewWindow};
        let cases = [
            // (intent, policy, reachable, expected)
            (NewTab, PolicyNewWindow, true, Spawn),
            (NewTab, PolicyNewWindow, false, Spawn),
            (NewTab, Attach, true, Forward),
            (NewTab, Attach, false, Spawn),
            (SplitPane, PolicyNewWindow, true, Spawn),
            (SplitPane, PolicyNewWindow, false, Spawn),
            (SplitPane, Attach, true, Forward),
            (SplitPane, Attach, false, Spawn),
            (Plain, PolicyNewWindow, true, Spawn),
            (Plain, PolicyNewWindow, false, Spawn),
            (Plain, Attach, true, Forward),
            (Plain, Attach, false, Spawn),
            // The escape hatch: never forwarded, under either policy.
            (NewWindow, Attach, true, Spawn),
            (NewWindow, Attach, false, Spawn),
            (NewWindow, PolicyNewWindow, true, Spawn),
            (NewWindow, PolicyNewWindow, false, Spawn),
        ];
        for (intent, behavior, reachable, expected) in cases {
            assert_eq!(
                route_launch(intent, behavior, reachable),
                expected,
                "{intent:?} under {behavior:?} with reachable={reachable}"
            );
        }
    }

    #[test]
    fn the_grammar_accepts_every_directory_spelling() {
        for spelling in [
            &["-d", "/tmp/x"][..],
            &["--directory", "/tmp/x"][..],
            &["--working-directory", "/tmp/x"][..],
            &["--startingDirectory", "/tmp/x"][..],
            &["-d=/tmp/x"][..],
            &["--startingDirectory=/tmp/x"][..],
        ] {
            let req = parse_window_request("new-tab", &args(spelling), echo_dir)
                .unwrap_or_else(|e| panic!("{spelling:?}: {e}"));
            assert_eq!(req.intent, LaunchIntent::NewTab);
            assert_eq!(req.dir.as_deref(), Some("/tmp/x"));
        }
    }

    #[test]
    fn the_grammar_maps_each_verb_to_its_intent_and_refuses_the_rest() {
        assert_eq!(
            parse_window_request("new-window", &[], echo_dir)
                .unwrap()
                .intent,
            LaunchIntent::NewWindow
        );
        assert_eq!(
            parse_window_request("split-pane", &[], echo_dir)
                .unwrap()
                .intent,
            LaunchIntent::SplitPane
        );
        assert!(parse_window_request("ctl", &[], echo_dir).is_err());
    }

    #[test]
    fn split_orientation_is_wt_shaped_and_only_on_split_pane() {
        let v = parse_window_request("split-pane", &args(&["-V"]), echo_dir).unwrap();
        assert_eq!(v.split, SplitOrientation::Vertical);
        let h = parse_window_request("split-pane", &args(&["-H"]), echo_dir).unwrap();
        assert_eq!(h.split, SplitOrientation::Horizontal);
        // Default is side-by-side, the shape a wide terminal window wants.
        let d = parse_window_request("split-pane", &[], echo_dir).unwrap();
        assert_eq!(d.split, SplitOrientation::Vertical);
        // `-H` on `new-tab` is a typo, not a silent no-op.
        assert!(parse_window_request("new-tab", &args(&["-H"]), echo_dir).is_err());
    }

    #[test]
    fn a_missing_or_unknown_option_is_a_loud_error() {
        assert!(parse_window_request("new-tab", &args(&["-d"]), echo_dir).is_err());
        assert!(parse_window_request("new-tab", &args(&["--profile", "x"]), echo_dir).is_err());
        // A resolver rejection (not a directory) surfaces verbatim.
        let bad = parse_window_request("new-tab", &args(&["-d", "nope"]), |_| {
            Err("not a directory: nope".to_string())
        })
        .unwrap_err();
        assert_eq!(bad, "not a directory: nope");
    }

    #[test]
    fn the_control_request_is_the_spawn_verb_with_a_quoted_cwd() {
        let plain = WindowRequest {
            intent: LaunchIntent::NewTab,
            dir: None,
            split: SplitOrientation::Vertical,
        };
        assert_eq!(plain.control_request().unwrap(), "spawn\n");

        // A space-free path uses the unquoted form, which older instances
        // also serve.
        let simple = WindowRequest {
            intent: LaunchIntent::NewTab,
            dir: Some(r"C:\Windows".to_string()),
            split: SplitOrientation::Vertical,
        };
        assert_eq!(simple.control_request().unwrap(), "spawn cwd=C:\\Windows\n");

        // An ordinary Windows path with spaces: unquoted, whitespace-splitting would tear it
        // into three tokens.
        let spaced = WindowRequest {
            intent: LaunchIntent::NewTab,
            dir: Some(r"C:\Program Files\Git".to_string()),
            split: SplitOrientation::Vertical,
        };
        assert_eq!(
            spaced.control_request().unwrap(),
            "spawn cwd=\"C:\\Program Files\\Git\"\n"
        );

        let split = WindowRequest {
            intent: LaunchIntent::SplitPane,
            dir: Some(r"C:\Windows".to_string()),
            split: SplitOrientation::Horizontal,
        };
        assert_eq!(
            split.control_request().unwrap(),
            "spawn split=h cwd=C:\\Windows\n"
        );
    }

    /// A newline (second verb injection) or a quote (early close) in the value
    /// is refused before reaching the socket.
    #[test]
    fn an_unframeable_directory_is_refused_not_encoded() {
        for hostile in ["/tmp/a\nspawn", "/tmp/a\rb", "/tmp/\"quoted\""] {
            let req = WindowRequest {
                intent: LaunchIntent::NewTab,
                dir: Some(hostile.to_string()),
                split: SplitOrientation::Vertical,
            };
            assert!(
                req.control_request().is_err(),
                "{hostile:?} must be refused"
            );
        }
    }

    #[test]
    fn window_args_are_the_window_librarys_own_flag() {
        let none = WindowRequest {
            intent: LaunchIntent::NewWindow,
            dir: None,
            split: SplitOrientation::Vertical,
        };
        assert!(none.window_args().is_empty());
        let some = WindowRequest {
            intent: LaunchIntent::NewWindow,
            dir: Some("/tmp/x".to_string()),
            split: SplitOrientation::Vertical,
        };
        assert_eq!(some.window_args(), args(&["-d", "/tmp/x"]));
    }

    /// The narrow gate. Everything that is not a bare window launch spawns,
    /// under either policy — these are the invocations a forward would corrupt.
    #[test]
    fn only_a_bare_window_launch_is_policy_eligible() {
        let plain = LaunchEnv::default();
        assert!(plain_launch_is_policy_eligible(&[], plain));
        assert!(plain_launch_is_policy_eligible(&args(&["--window"]), plain));
        assert!(plain_launch_is_policy_eligible(
            &args(&["--window", "-d", "/tmp"]),
            plain
        ));
        assert!(plain_launch_is_policy_eligible(
            &args(&["--working-directory", "/tmp"]),
            plain
        ));
        for hazard in [
            &["-e", "vim"][..],
            &["--command", "vim"][..],
            &["--"][..],
            &["--headless"][..],
            &["--window", "-d", "/tmp", "--headless"][..],
            &["--diagnose"][..],
            &["--containment", "user"][..],
            &["-d"][..], // a dangling flag: refuse rather than guess
        ] {
            assert!(
                !plain_launch_is_policy_eligible(&args(hazard), plain),
                "{hazard:?} must never be forwarded"
            );
        }
        // An update successor re-runs its own argv; it is a relaunch, never a
        // request for another terminal.
        let updated = LaunchEnv { updated_from: true };
        assert!(!plain_launch_is_policy_eligible(&[], updated));
        assert!(!plain_launch_is_policy_eligible(
            &args(&["--window"]),
            updated
        ));
    }

    /// A plain launch is eligible only with the window parser's own directory
    /// flags (`-d`, `--working-directory`), so one argv never gets two outcomes.
    /// The explicit verbs still take the wider grammar.
    #[test]
    fn the_plain_gate_only_knows_the_directory_flags_the_window_parses() {
        let plain = LaunchEnv::default();
        for window_spelling in [&["-d", "/tmp"][..], &["--working-directory", "/tmp"][..]] {
            assert!(
                plain_launch_is_policy_eligible(&args(window_spelling), plain),
                "{window_spelling:?}"
            );
            assert_eq!(
                plain_launch_dir_operand(&args(window_spelling)).as_deref(),
                Some("/tmp"),
                "{window_spelling:?}"
            );
        }
        for verb_only_spelling in [
            &["-d=/tmp"][..],
            &["--directory", "/tmp"][..],
            &["--directory=/tmp"][..],
            &["--working-directory=/tmp"][..],
            &["--startingDirectory", "/tmp"][..],
            &["--startingDirectory=/tmp"][..],
        ] {
            assert!(
                !plain_launch_is_policy_eligible(&args(verb_only_spelling), plain),
                "{verb_only_spelling:?} is not a spelling the window understands, so a plain \
                 launch carrying it must take the SAME route under both policies"
            );
            // …and the verb grammar still accepts every one of them.
            assert!(
                parse_window_request("new-tab", &args(verb_only_spelling), echo_dir).is_ok(),
                "{verb_only_spelling:?} must stay legal on the explicit verbs"
            );
        }
        assert_eq!(plain_launch_dir_operand(&args(&["--window"])), None);
    }
}
