// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The front door's WINDOWING grammar and routing POLICY — `aterm new-tab`,
//! `aterm new-window`, `aterm split-pane`, and the one rule that decides whether
//! such a request starts a process or is handed to the aterm already running.
//!
//! ## Why this exists
//!
//! On Windows the established convention is Windows Terminal's: `wt` from any
//! shell routes into the running instance, and `wt new-tab -d <dir>` is how a
//! script, a file-manager verb, or a taskbar task opens a terminal *where you
//! already are*. aterm had the capability and none of the front door: the
//! control socket has answered `spawn cwd=<path>` from any shell for a long
//! time (that is exactly what `aterm ctl spawn` does), but the `aterm` command
//! itself only ever knew how to become a brand-new process.
//!
//! ## Policy, not a mutex
//!
//! The obvious Windows shape — `CreateMutexW` at startup, and the loser posts
//! its argv to the winner — was considered and REJECTED. aterm is not a
//! one-window app: `fleet_watch` is a whole sibling-discovery subsystem, the
//! control socket already enumerates every live instance deterministically, and
//! several instances co-existing is a supported, used configuration (an agent
//! launches its own instance on its own `$ATERM_CONTROL_SOCK`). A named mutex
//! would make "there can be only one" a process-lifetime FACT enforced below
//! all of that, instead of a preference the operator states. So the second
//! instance is never prevented from existing — it is simply not *started* when
//! the operator asked for attach behaviour and an instance is reachable.
//!
//! ## The default is `new_window`, deliberately
//!
//! WT itself defaults to `windowingBehavior: useNew`, and — more importantly —
//! `new_window` is byte-for-byte today's behaviour on every platform. Flipping
//! the default would change what happens when an existing user types `aterm`,
//! which is precisely the class of surprise a terminal must not spring. Both
//! policies are implemented; the safe one ships on.
//!
//! Everything in this module is PURE — a parse and a three-input decision — so
//! the grammar and the routing rule are unit-testable without a socket, a
//! window, or a config file.

/// Where a windowing request should be served.
///
/// The whole routing decision is [`route_launch`]; this is its result. Two
/// variants and no third: "forward, and if that fails start one anyway" is a
/// FALLBACK the caller applies after a forward errors, not a route — modelling
/// it here would hide a real failure (an instance that answered `ERR`) behind a
/// silently different outcome.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum WindowRoute {
    /// Start a new aterm window in THIS process (today's behaviour).
    Spawn,
    /// Hand the request to the running instance over the control socket and exit.
    Forward,
}

/// The `windowing_behavior` config key: what a plain launch (and an explicit
/// `new-tab` / `split-pane`) should do when an aterm is already running.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum WindowingBehavior {
    /// Every launch is its own window and its own process. THE DEFAULT — this
    /// is what aterm has always done, so an existing install cannot change
    /// behaviour by upgrading into this feature.
    #[default]
    NewWindow,
    /// A launch joins the running instance (a new tab there) and exits; with no
    /// instance reachable it starts one, so the FIRST launch still works.
    Attach,
}

impl WindowingBehavior {
    /// Parse one config value, case-insensitively and trimmed. `None` for an
    /// unrecognized spelling so the caller can say so out loud rather than
    /// silently picking a behaviour the operator did not ask for.
    ///
    /// The `useNew` / `useExisting` aliases are Windows Terminal's own spellings
    /// for the same two choices. Someone porting a `settings.json` habit should
    /// not have to discover that aterm renamed the values.
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

    /// The behaviour for a possibly-absent, possibly-invalid config value: the
    /// safe default for both. Pure companion to [`WindowingBehavior::parse`] so
    /// the precedence "absent = default, invalid = default" is testable on its
    /// own (the caller adds the warning line for the invalid case).
    #[must_use]
    pub fn resolve(raw: Option<&str>) -> WindowingBehavior {
        raw.and_then(WindowingBehavior::parse)
            .unwrap_or(WindowingBehavior::NewWindow)
    }

    /// The value spelling this behaviour writes/reads in `aterm.toml`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            WindowingBehavior::NewWindow => "new_window",
            WindowingBehavior::Attach => "attach",
        }
    }
}

/// What kind of terminal the invocation asked for. The routing input that is
/// NOT a policy: three of these come from an explicit verb, the fourth is a
/// plain `aterm` launch that carries no other instruction.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum LaunchIntent {
    /// `aterm new-tab` — a tab, wherever the policy says tabs go.
    NewTab,
    /// `aterm new-window` — a window, unconditionally. The escape hatch that
    /// makes `attach` safe to turn on: there is always a way to say "no, a
    /// SEPARATE one", and it is the same word Windows Terminal uses.
    NewWindow,
    /// `aterm split-pane` — a pane beside the focused one.
    SplitPane,
    /// A bare window launch with nothing else asked of it (no `-e`, no
    /// `--headless`, no diagnostics). Only THIS shape is policy-eligible; see
    /// [`plain_launch_is_policy_eligible`] for why the gate is so narrow.
    Plain,
}

/// THE ROUTING DECISION. Pure: policy + what was asked + whether anyone is home.
///
/// * `NewWindow` is never forwarded. A verb that names the outcome outranks a
///   preference about the outcome — otherwise `attach` would be a trap with no
///   way out, and the jump list's "New Window" task would be a lie.
/// * `NewTab` / `SplitPane` / `Plain` forward only under `attach`, and only when
///   an instance actually answered. Under the default `new_window` they spawn,
///   which is exactly what they do today.
/// * Nothing reachable always means spawn — the first launch of the day must
///   work identically under both policies.
///
/// A `SplitPane` that spawns has nothing to split (a brand-new window is one
/// pane). That is not a bug being papered over: it is what `wt split-pane`
/// does under `useNew` too, and the caller says so on stderr rather than
/// pretending a split happened.
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
    /// The `-d <dir>` starting directory, already made ABSOLUTE by the caller's
    /// own cwd. Absolute is not cosmetic: the request may be served by a process
    /// whose working directory is somewhere else entirely, so a relative path
    /// would land in a different place depending on who answered it.
    pub dir: Option<String>,
    /// `split-pane`'s orientation; meaningless for the other verbs and ignored
    /// there rather than rejected (a caller building requests generically should
    /// not have to special-case the field).
    pub split: SplitOrientation,
}

impl WindowRequest {
    /// The control-socket request line that asks a RUNNING instance to serve
    /// this — the `spawn` verb, which opens a tab in the frontmost window
    /// (`spawn split=<v|h>` splits the focused pane instead).
    ///
    /// The cwd is quoted ONLY WHEN IT HAS TO BE — i.e. when it contains
    /// whitespace. The control protocol is newline-delimited and otherwise
    /// whitespace-split, so `cwd=C:\Program Files\Git` would arrive as two
    /// tokens and be rejected as a usage error, which is most of the Windows
    /// paths anyone actually types; `cwd="…"` is the additive form the server
    /// learned alongside this grammar.
    ///
    /// Quoting only on demand is a MIXED-VERSION rule, not tidiness. A new front
    /// door can be talking to an instance that has been running since before the
    /// quoted form existed (an upgrade replaces the binary; it does not restart
    /// the windows). That older server strips `cwd=` and takes the rest
    /// verbatim, quotes included, so an unconditionally-quoted space-free path
    /// would open a tab in a directory literally named `"C:\Windows"`. Emitting
    /// the byte-identical historical form whenever it is legal means the common
    /// case is exactly as correct against an old server as against a new one,
    /// and only the case that ALREADY failed there (a path with a space) depends
    /// on the new parser.
    ///
    /// Returns `Err` for a directory that cannot be framed at all: a `"` (which
    /// no Windows path may contain) would close the quoted value early, and a
    /// newline would inject a SECOND authenticated verb into the stream. Both
    /// are refused loudly here rather than encoded — the alternative, an escape
    /// vocabulary in the wire format, is a protocol change this feature does not
    /// need.
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

    /// The argument list for the SPAWN route — the window library's own
    /// `-d/--working-directory` flag, so the started window opens where the
    /// forwarded tab would have. Deliberately nothing else: the spawn arm must
    /// stay byte-identical to `aterm --window [-d <dir>]`, the path every one of
    /// these verbs already had before the grammar existed.
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

/// Usage text for one windowing verb, printed on a grammar error.
#[must_use]
pub fn window_verb_usage(verb: &str) -> String {
    let mut s = String::from("usage: aterm ");
    s.push_str(verb);
    s.push_str(" [-d <dir>]");
    if verb == "split-pane" {
        s.push_str(" [-H|-V]");
    }
    s
}

/// Parse `aterm <verb> [args…]` — the wt-shaped grammar.
///
/// `-d <dir>` / `-d=<dir>` / `--directory` / `--working-directory` /
/// `--startingDirectory` all name the starting directory. The long spellings
/// are aterm's own (`--working-directory` is the window library's flag, so the
/// front door and the window agree) plus Windows Terminal's
/// (`--startingDirectory`), because the whole point of this grammar is that a
/// `wt` habit works here.
///
/// `resolve_dir` turns the raw argument into the absolute path the request will
/// carry — injected rather than called so the parse stays pure and testable
/// with no filesystem. It returns `Err(message)` for a value that is not a
/// usable directory.
///
/// Unknown options are REFUSED, not ignored. A silently dropped `-d` opens a
/// terminal in the wrong directory, which looks like the verb worked.
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

/// The AMBIENT facts the plain-launch gate consults, gathered by the caller so
/// [`plain_launch_is_policy_eligible`] itself stays a pure function of its
/// inputs. A struct rather than a row of bare booleans because each one is a
/// separate hazard with its own reason, and a call site reading
/// `(scan, true, false)` says nothing about which is which.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct LaunchEnv {
    /// `$ATERM_UPDATED_FROM` is set — this process is an update SUCCESSOR,
    /// re-spawned by the outgoing one with its argv verbatim to finish an apply.
    pub updated_from: bool,
    /// `$ATERM_HEADLESS` is PRESENT (presence, not truthiness — the same test
    /// the router's own mode fork applies, so the two agree about which launches
    /// are headless-shaped).
    pub headless: bool,
}

/// The `-d <dir>` spellings a PLAIN launch may carry and still be policy-eligible.
///
/// Deliberately just the two the window's own parser accepts (`crates/aterm-gui/
/// src/cli.rs`), and NOT the wider set the explicit verbs take (`--directory`,
/// `--startingDirectory`, and the `=`-joined forms). The verbs can afford the
/// wider grammar because the front door resolves the value itself and hands the
/// spawned window a canonical `-d <dir>`; a plain launch is passed through
/// VERBATIM on the spawn route, so a spelling the window rejects must not be
/// eligible either — otherwise the same argv opens a terminal under `attach` and
/// dies with "unknown option" under the default, which is the one thing a
/// routing policy must never do.
const PLAIN_LAUNCH_DIR_FLAGS: &[&str] = &["-d", "--working-directory"];

/// Whether a plain (verb-less) window launch may be routed by policy at all.
///
/// The gate is deliberately paranoid, and every excluded token is a real hazard
/// rather than a hypothetical one:
///
/// * `-e` / `--command` / `--` open a payload boundary — a whole child command
///   line. `spawn` cannot carry one, so forwarding would silently drop the
///   command the operator asked to run.
/// * `--headless` / `$ATERM_HEADLESS` ask for an engine + control socket with no
///   window. Forwarding would answer with a TAB in someone's window — the exact
///   opposite — and CI would hang waiting for a process that already exited. The
///   ENV form is [`LaunchEnv::headless`]: the router's mode fork makes a merely
///   PRESENT `$ATERM_HEADLESS` windowish, so without this the variable alone
///   (no flag) walked straight through the gate and the documented exclusion was
///   a comment, not a rule.
/// * `--diagnose` is the release gates' probe; it must measure THIS process.
/// * `$ATERM_UPDATED_FROM` marks an update SUCCESSOR: the outgoing process
///   re-spawns itself with its own argv verbatim to complete an apply. Under
///   `attach` that successor would forward into the very instance that is
///   tearing itself down (or, worse, into a sibling) and the update would never
///   land. A relaunch of a window is not a request for a terminal.
/// * Anything else unrecognized (`--containment`, a future flag) fails CLOSED to
///   spawning, because spawning is what it does today.
///
/// `--window` IS eligible: it names the MODE (window rather than session), not
/// the destination, and it is the documented way to ask for the window from a
/// script. `-d <dir>` is eligible because the forward carries it exactly — but
/// only in the spellings the window itself understands; see
/// `PLAIN_LAUNCH_DIR_FLAGS`.
///
/// The OTHER OS-driven relaunch — `RegisterApplicationRestart`, which Windows
/// fires after a Restart-Manager reboot *and* after a crash or hang — needs no
/// entry here: it registers the `new-window` VERB, which [`route_launch`] never
/// forwards under any policy, so it never reaches this gate at all.
#[must_use]
pub fn plain_launch_is_policy_eligible(scan: &[std::ffi::OsString], env: LaunchEnv) -> bool {
    if env.updated_from || env.headless {
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

/// The `-d <dir>` operand of a plain launch, in the spellings
/// [`plain_launch_is_policy_eligible`] accepts — the RAW value, for the caller to
/// resolve against its own working directory.
///
/// Shares `PLAIN_LAUNCH_DIR_FLAGS` with the gate on purpose: the two questions
/// ("may this launch be routed" and "where does it want to open") must never
/// disagree about what a directory flag looks like.
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
    fn the_default_policy_is_todays_behaviour() {
        assert_eq!(WindowingBehavior::default(), WindowingBehavior::NewWindow);
        assert_eq!(
            WindowingBehavior::resolve(None),
            WindowingBehavior::NewWindow
        );
        // An unrecognized value must NOT silently become `attach`: a typo in
        // aterm.toml may not change where terminals open.
        assert_eq!(
            WindowingBehavior::resolve(Some("join-please")),
            WindowingBehavior::NewWindow
        );
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
        assert_eq!(WindowingBehavior::NewWindow.name(), "new_window");
        assert_eq!(WindowingBehavior::Attach.name(), "attach");
    }

    /// THE ROUTING TABLE, every cell of it. Twelve combinations, no ambiguity.
    #[test]
    fn routing_is_the_whole_two_by_four_table() {
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
            (SplitPane, Attach, true, Forward),
            (SplitPane, Attach, false, Spawn),
            (Plain, PolicyNewWindow, true, Spawn),
            (Plain, Attach, true, Forward),
            (Plain, Attach, false, Spawn),
            // The escape hatch: NEVER forwarded, under either policy.
            (NewWindow, Attach, true, Spawn),
            (NewWindow, PolicyNewWindow, true, Spawn),
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
    fn the_default_policy_never_forwards_anything() {
        for intent in [
            LaunchIntent::NewTab,
            LaunchIntent::NewWindow,
            LaunchIntent::SplitPane,
            LaunchIntent::Plain,
        ] {
            for reachable in [true, false] {
                assert_eq!(
                    route_launch(intent, WindowingBehavior::default(), reachable),
                    WindowRoute::Spawn,
                    "the shipped default must be byte-identical to today for {intent:?}"
                );
            }
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
        let missing = parse_window_request("new-tab", &args(&["-d"]), echo_dir).unwrap_err();
        assert!(missing.contains("missing <dir>"), "{missing}");
        let unknown =
            parse_window_request("new-tab", &args(&["--profile", "x"]), echo_dir).unwrap_err();
        assert!(unknown.contains("unknown option --profile"), "{unknown}");
        assert!(unknown.contains("usage: aterm new-tab"), "{unknown}");
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

        // A space-free path is emitted in the HISTORICAL, unquoted form, so an
        // instance older than this feature serves it correctly (see the method's
        // mixed-version note).
        let simple = WindowRequest {
            intent: LaunchIntent::NewTab,
            dir: Some(r"C:\Windows".to_string()),
            split: SplitOrientation::Vertical,
        };
        assert_eq!(simple.control_request().unwrap(), "spawn cwd=C:\\Windows\n");

        // The whole reason the value CAN be quoted: this is an ordinary Windows
        // path, and whitespace-splitting would tear it into three tokens.
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

    /// A newline in the value would inject a second authenticated verb into a
    /// newline-delimited protocol; a quote would close the value early. Both are
    /// refused before a byte reaches the socket.
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
    /// under EITHER policy — these are the invocations a forward would corrupt.
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
        let updated = LaunchEnv {
            updated_from: true,
            ..LaunchEnv::default()
        };
        assert!(!plain_launch_is_policy_eligible(&[], updated));
        assert!(!plain_launch_is_policy_eligible(
            &args(&["--window"]),
            updated
        ));
    }

    /// `$ATERM_HEADLESS` alone — no `--headless` flag anywhere in argv — is the
    /// shape the router's mode fork makes windowish by PRESENCE. The gate's own
    /// documentation has always excluded it; this pins that it actually does,
    /// because a forwarded headless launch answers with a tab in someone's window
    /// and leaves CI waiting for a control socket that will never exist.
    #[test]
    fn a_headless_environment_is_never_policy_eligible() {
        let headless = LaunchEnv {
            headless: true,
            ..LaunchEnv::default()
        };
        for argv in [&[][..], &["--window"][..], &["--window", "-d", "/tmp"][..]] {
            assert!(
                plain_launch_is_policy_eligible(&args(argv), LaunchEnv::default()),
                "{argv:?} is the eligible shape without the variable"
            );
            assert!(
                !plain_launch_is_policy_eligible(&args(argv), headless),
                "{argv:?} must never be forwarded under $ATERM_HEADLESS"
            );
        }
    }

    /// The gate and the window's OWN parser must agree about what a directory
    /// flag looks like, or one argv takes two different outcomes: forwarded
    /// under `attach`, "unknown option" under the default. `crates/aterm-gui/
    /// src/cli.rs` knows `-d` and `--working-directory` and nothing else — no
    /// `=`-joined form, no wt spelling — so those are the only two a PLAIN launch
    /// may carry through the gate. (The explicit verbs still take the wider
    /// grammar: they resolve the value themselves and hand the window `-d`.)
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
