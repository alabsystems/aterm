// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The front door's windowing grammar and routing policy.

/// Where a windowing request should be served; the result of [`route_launch`].
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum WindowRoute {
    /// Open a new aterm window from this invocation (on Windows, through the windowed image).
    Spawn,
    /// Hand the request to the running instance over the control socket and exit.
    Forward,
}

/// The `windowing_behavior` config key.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum WindowingBehavior {
    /// Every launch is its own window and process.
    #[default]
    NewWindow,
    /// A launch joins the running instance (a new tab there) and exits.
    Attach,
}

impl WindowingBehavior {
    /// Parse one config value, case-insensitively and trimmed.
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

/// What kind of terminal the invocation asked for: three explicit verbs, or a plain `aterm` launch.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum LaunchIntent {
    /// `aterm new-tab` — a tab, wherever the policy says tabs go.
    NewTab,
    /// `aterm new-window`: a window, unconditionally, so `attach` always has an escape.
    NewWindow,
    /// `aterm split-pane` — a pane beside the focused one.
    SplitPane,
    /// A bare window launch with nothing else asked of it (no `-e`, `--headless`, diagnostics).
    Plain,
}

/// The routing decision.
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
    /// Side by side — the new pane takes the right half.
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
    /// The `-d <dir>` starting directory.
    pub dir: Option<String>,
    /// `split-pane`'s orientation; other verbs keep the default and never send it.
    pub split: SplitOrientation,
}

impl WindowRequest {
    /// The control-socket request line asking a running instance to serve this.
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

    /// The argument list for the spawn route.
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

/// The usage line for one windowing verb: `--help` starts with it and an option error ends with it.
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

/// The ambient facts the plain-launch gate consults.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct LaunchEnv {
    /// `$ATERM_UPDATED_FROM` is set.
    pub updated_from: bool,
}

const PLAIN_LAUNCH_DIR_FLAGS: &[&str] = &["-d", "--working-directory"];

/// Whether a plain (verb-less) window launch may be routed by policy.
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

/// The raw `-d <dir>` operand of a plain launch (the last one when repeated, matching
/// [`parse_window_request`]), for the caller to resolve.
#[must_use]
pub fn plain_launch_dir_operand(scan: &[std::ffi::OsString]) -> Option<String> {
    let mut it = scan.iter();
    let mut last = None;
    while let Some(raw) = it.next() {
        let arg = raw.to_string_lossy();
        if PLAIN_LAUNCH_DIR_FLAGS.contains(&arg.as_ref()) {
            last = Some(it.next()?.to_string_lossy().into_owned());
        }
    }
    last
}

/// Incremental UTF-16 code-unit to UTF-8 transcoder, emitting one U+FFFD per unpaired surrogate.
#[cfg(any(windows, test))]
pub(crate) fn push_utf16_unit(unit: u16, pending: &mut Option<u16>, out: &mut Vec<u8>) {
    fn push_char(c: char, out: &mut Vec<u8>) {
        let mut buf = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }
    if let Some(high) = pending.take() {
        if (0xDC00..=0xDFFF).contains(&unit) {
            let c = 0x10000 + ((u32::from(high) - 0xD800) << 10) + (u32::from(unit) - 0xDC00);
            push_char(
                char::from_u32(c).unwrap_or(char::REPLACEMENT_CHARACTER),
                out,
            );
            return;
        }
        push_char(char::REPLACEMENT_CHARACTER, out);
    }
    match unit {
        0xD800..=0xDBFF => *pending = Some(unit),
        0xDC00..=0xDFFF => push_char(char::REPLACEMENT_CHARACTER, out),
        u => push_char(
            char::from_u32(u32::from(u)).unwrap_or(char::REPLACEMENT_CHARACTER),
            out,
        ),
    }
}

#[cfg(test)]
#[path = "windowing_tests.rs"]
mod tests;
