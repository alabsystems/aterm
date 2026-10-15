// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE DEVELOPER TOOLS LIST (macOS): whether macOS holds each new program aterm starts
//! for its first-run check (owner ruling 2026-10-14: "fix this everywhere and it needs to
//! be in aterm and doctor").
//!
//! Why it matters. macOS assesses every new binary (syspolicyd, with XProtect) before its
//! first exec, one at a time, and the program waits in `_dyld_start` until it is done.
//! Measured on the owner's Mac on 2026-10-14, with aterm not on the list: a `git push`
//! hook's `/usr/bin/env` held about 45 s, freshly built test binaries 1–3 min each, and a
//! compiler `x.py check` took 1 h 26 min; timing-bounded shell tests failed. An app on
//! System Settings ▸ Privacy & Security ▸ Developer Tools has the programs it is
//! responsible for exempted from that assessment of locally built code, and every
//! program started in an aterm tab names aterm as its responsible app. Only the owner adds
//! an app there; aterm reads the list and never writes it.
//!
//! One detector ([`aterm_containment::consent::devtool_grant`], over the switch's row in
//! the system TCC.db and tccd's own answers) and one row ([`row`]), which `aterm doctor`
//! and `aterm pkg doctor` both print. Measured 2026-10-14: switched on at 16:30:37, the
//! next new program started in the RUNNING aterm 13 s later was answered `Allowed (System
//! Set)`, so the change applies to the next program with nothing relaunched.

use aterm_containment::consent::{
    self, DevToolGrant, DevToolList, DevToolListLabel, DevToolListing, DevToolProbe,
    DevToolProbeLabel, DevToolState, ProbeGate,
};

use crate::doctor::{Level, Row};

/// Where the switch is, in words: the consent module's one spelling.
pub const PANE: &str = consent::DEVTOOLS_SETTINGS_WORDS;

/// The pane's address: the legacy pane id, which System Settings maps to the Privacy &
/// Security extension's `Privacy_DevTools` anchor (the window's opener tries the
/// extension's own identifier first and this one second).
pub const PANE_URL: &str = consent::DEVTOOLS_SETTINGS_URL_LEGACY;

/// The command that OPENS the pane and changes nothing: the owner flips the switch.
pub const OPEN_PANE: &str =
    "open \"x-apple.systempreferences:com.apple.preference.security?Privacy_DevTools\"";

/// Apple's command that adds Terminal to the list, which makes the Developer Tools pane
/// appear on a Mac whose list is empty. It lists Terminal; it does not list aterm.
pub const LIST_TERMINAL: &str = "spctl developer-mode enable-terminal";

/// What the doctors read: the app asked about, the switch's row and tccd's answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    /// The running bundle's `CFBundleIdentifier`, when there is one.
    pub subject: Option<String>,
    /// The switch's row in the system TCC.db ([`consent::probe_devtool_list`]).
    pub list: DevToolList,
    /// tccd's answers for the subject in the last ten minutes ([`consent::probe_devtool`]).
    pub log: DevToolProbe,
    /// The display name of the program macOS holds responsible for THIS process, when it
    /// is not the subject's bundle (a doctor run in Terminal, or over ssh): aterm's switch
    /// does not reach this shell.
    pub other_responsible: Option<String>,
}

impl Facts {
    /// Facts that measured nothing, for `list`'s and `log`'s reasons.
    #[must_use]
    pub const fn refused(list: DevToolListLabel, log: DevToolProbeLabel) -> Self {
        Self {
            subject: None,
            list: DevToolList::refused(list),
            log: DevToolProbe::refused(log),
            other_responsible: None,
        }
    }

    /// The joined verdict ([`consent::devtool_grant`]).
    #[must_use]
    pub fn grant(&self) -> DevToolGrant {
        consent::devtool_grant(&self.list, &self.log)
    }
}

/// Read the list and tccd's answers for the running bundle, under the probes' own fences:
/// a binary outside a `.app` (a test binary, `target/release/aterm`) opens and spawns
/// nothing and is `not measured`; one started through `~/.local/bin/aterm` is judged by
/// the bundled image ([`consent::fence_exe`]). The gate is [`ProbeGate::on`]: `[privacy]
/// enabled` and `check` gate the window's background probes, and neither read can raise
/// a dialog. `[privacy] observer = false` refuses both, as it refuses the window's row
/// (§9.1 O11). `None` off macOS, where there is no such list (no row).
#[must_use]
pub fn facts() -> Option<Facts> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let exe = consent::fence_exe();
    if exe.as_deref().is_some_and(consent::path_is_in_app_bundle) && config_observer_off() {
        return Some(Facts::refused(
            DevToolListLabel::RefusedObserverOff,
            DevToolProbeLabel::RefusedObserverOff,
        ));
    }
    let root = exe.as_deref().and_then(consent::app_bundle_root);
    let protected = consent::protected_roots(&[]);
    let subject = root
        .as_deref()
        .and_then(|root| consent::bundle_id_of(root, &protected));
    let id = subject.as_deref().unwrap_or_default();
    let list = consent::probe_devtool_list(ProbeGate::on(), id);
    if list.label.refused() {
        // The fence spoke: the log read would refuse for the same reason.
        return Some(Facts {
            subject,
            list,
            log: DevToolProbe::refused(match list.label {
                DevToolListLabel::RefusedNoSubject => DevToolProbeLabel::RefusedNoSubject,
                DevToolListLabel::RefusedNoExe => DevToolProbeLabel::RefusedNoExe,
                DevToolListLabel::UnsupportedPlatform => DevToolProbeLabel::UnsupportedPlatform,
                _ => DevToolProbeLabel::RefusedOutOfBundle,
            }),
            other_responsible: None,
        });
    }
    let log = consent::probe_devtool(ProbeGate::on(), id);
    // Past the fence: this is a bundled aterm, so naming the responsible app is in bounds.
    let other_responsible = i32::try_from(std::process::id())
        .ok()
        .and_then(consent::responsible_app)
        .filter(|app| {
            let theirs = consent::app_bundle_root(&app.path)
                .and_then(|root| consent::bundle_id_of(&root, &protected));
            theirs.is_none() || theirs != subject
        })
        .map(|app| app.display_name);
    Some(Facts {
        subject,
        list,
        log,
        other_responsible,
    })
}

/// `[privacy] observer = false` in the aterm.toml the window loads
/// (`aterm_types::dirs::aterm_config_path`). A file that is missing, unreadable or not
/// TOML leaves the window on its defaults, under which its row reads the list, so it is
/// not "off" here either.
fn config_observer_off() -> bool {
    aterm_types::dirs::aterm_config_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .is_some_and(|text| observer_off_in(&text))
}

/// The pure half of [`config_observer_off`]: whether `[privacy] observer` is the boolean
/// `false`, parsed with `aterm_toml` as the window parses the file.
fn observer_off_in(text: &str) -> bool {
    text.parse::<aterm_toml::Table>()
        .ok()
        .and_then(|table| table.get("privacy")?.get("observer")?.as_bool())
        == Some(false)
}

/// Why the switch's row was not read, in words.
const fn list_reason(label: DevToolListLabel) -> &'static str {
    match label {
        DevToolListLabel::Pending => "the list has not been read yet",
        DevToolListLabel::Read => "the list was read",
        DevToolListLabel::NoFullDiskAccess => {
            "the system TCC.db that holds the list opens only to an app with Full Disk Access"
        }
        DevToolListLabel::OpenFailed => "the system TCC.db could not be opened",
        DevToolListLabel::ToolFailed => "sqlite3 could not read the system TCC.db",
        DevToolListLabel::FormatUnrecognized => {
            "the system TCC.db answered in a shape aterm does not recognise"
        }
        DevToolListLabel::TimedOut => "reading the system TCC.db did not finish in time",
        DevToolListLabel::SpawnFailed => "sqlite3 could not be started",
        DevToolListLabel::OutputUnreadable => "sqlite3's answer could not be read",
        DevToolListLabel::RefusedDisabled => "the privacy probe is switched off in the config",
        DevToolListLabel::RefusedObserverOff => "observer = false in aterm.toml's [privacy]",
        DevToolListLabel::RefusedOutOfBundle => "running outside the app bundle",
        DevToolListLabel::RefusedNoExe => "this program's own path could not be resolved",
        DevToolListLabel::RefusedNoSubject => "this build has no usable bundle identifier",
        DevToolListLabel::UnsupportedPlatform => "macOS only",
    }
}

/// Why tccd's answers were not read, in words.
const fn log_reason(label: DevToolProbeLabel) -> &'static str {
    match label {
        DevToolProbeLabel::Pending => "tccd's log has not been read yet",
        DevToolProbeLabel::Observed => "tccd's answer was read",
        DevToolProbeLabel::NoRecentRequest => {
            "no new program started under aterm in the last 10 minutes, so tccd gave no answer"
        }
        DevToolProbeLabel::LogUnavailable => "only an administrator account can read tccd's log",
        DevToolProbeLabel::LogFailed
        | DevToolProbeLabel::TimedOut
        | DevToolProbeLabel::SpawnFailed
        | DevToolProbeLabel::OutputUnreadable => "macOS's log could not be read this time",
        DevToolProbeLabel::FormatUnrecognized => {
            "tccd's log answered in a shape aterm does not recognise"
        }
        DevToolProbeLabel::RefusedDisabled => "the privacy probe is switched off in the config",
        DevToolProbeLabel::RefusedObserverOff => "observer = false in aterm.toml's [privacy]",
        DevToolProbeLabel::RefusedOutOfBundle => "running outside the app bundle",
        DevToolProbeLabel::RefusedNoExe => "this program's own path could not be resolved",
        DevToolProbeLabel::RefusedNoSubject => "this build has no usable bundle identifier",
        DevToolProbeLabel::UnsupportedPlatform => "macOS only",
    }
}

/// The `devtools:` row, pure over the facts, or `None` where there is no such list.
///
/// `not_granted` is a WARNING (both doctors count it), never a failure: nothing in it
/// stops a shell from starting, and only the owner can change it. `unknown` is a note
/// that says what could not be read; it is never `ok`.
#[must_use]
pub fn row(facts: &Facts) -> Option<Row> {
    if facts.list.label == DevToolListLabel::UnsupportedPlatform {
        return None;
    }
    let who = match facts.subject.as_deref() {
        Some(id) => format!("aterm ({id})"),
        None => "aterm".to_string(),
    };
    let under = facts
        .other_responsible
        .as_deref()
        .map_or_else(String::new, |app| {
            format!("; this shell runs under {app}, not aterm, so aterm's switch does not reach it")
        });
    // tccd's own count, when its log was read: the cost, measured, not supposed.
    let checked = match (facts.log.label, facts.log.not_allowed) {
        (DevToolProbeLabel::Observed, Some(n)) if n > 0 => format!(
            "; macOS checked {n} new program{} run in aterm in the last {} minutes",
            if n == 1 { "" } else { "s" },
            consent::DEVTOOL_WINDOW_S / 60
        ),
        _ => String::new(),
    };
    let (level, text) = match facts.grant() {
        DevToolGrant::Granted => {
            let how = if facts.list.label == DevToolListLabel::Read
                && facts.list.listing == DevToolListing::On
            {
                "switched on"
            } else {
                "tccd allowed its latest new program"
            };
            (
                Level::Ok,
                format!(
                    "{who} is on {PANE} ({how}), so macOS should not hold the programs it \
                     starts for its first-run check{under}"
                ),
            )
        }
        DevToolGrant::NotGranted => {
            let is = match (facts.list.label, facts.list.listing, facts.log.state) {
                (DevToolListLabel::Read, DevToolListing::Off, _) => {
                    "is switched off on".to_string()
                }
                (DevToolListLabel::Read, DevToolListing::On, _) => {
                    "is switched on, but tccd still checks its new programs, on".to_string()
                }
                (_, _, DevToolState::Denied) => "is not allowed by".to_string(),
                _ => "is not on".to_string(),
            };
            (
                Level::Warn,
                format!(
                    "{who} {is} {PANE} \u{2014} macOS holds every new program it starts for a \
                     first-run check (syspolicyd), one at a time, so builds and tests wait and \
                     timing tests flake{checked} \u{2014} fix: switch aterm on under Developer \
                     Tools (`aterm doctor` opens the pane for you, or `{OPEN_PANE}`; then + \
                     \u{2192} aterm \u{2192} on; no Developer Tools pane? `{LIST_TERMINAL}` makes \
                     it appear, listing only Terminal) \u{2014} it applies to the next program \
                     aterm starts{under}"
                ),
            )
        }
        DevToolGrant::Unknown => {
            let list = list_reason(facts.list.label);
            let log = log_reason(facts.log.label);
            let why = if facts.list.label.refused() {
                list.to_string()
            } else {
                format!("{list}, and {log}")
            };
            (Level::Note, format!("not measured ({why}){under}"))
        }
    };
    Some(Row { level, text })
}

/// What the hand-over to the owner did ([`hand_to_owner`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handled {
    /// aterm is on the list: nothing to do.
    Granted,
    /// Nothing was done, and why: the list could not be read (aterm does not act on a
    /// state it did not measure), or this run does not act (read-only, no window session).
    Left(String),
    /// The pane is open for the owner. `listed_terminal` is set when the list was empty
    /// and [`LIST_TERMINAL`] ran first so the pane exists, with what it answered.
    Opened {
        /// [`LIST_TERMINAL`]'s outcome, when it ran.
        listed_terminal: Option<Result<(), String>>,
    },
    /// The pane could not be opened, in the opener's words.
    OpenFailed {
        /// [`LIST_TERMINAL`]'s outcome, when it ran.
        listed_terminal: Option<Result<(), String>>,
        /// Why the pane did not open.
        why: String,
    },
}

/// The two things aterm may do toward the switch, injected so a test runs nothing. Neither
/// flips it: only the owner does, in System Settings, with macOS's own authentication.
pub struct Reach<'a> {
    /// [`LIST_TERMINAL`] ([`list_terminal`]).
    pub list_terminal: &'a dyn Fn() -> Result<(), String>,
    /// Open the pane ([`open_pane`], or the window's own opener).
    pub open_pane: &'a dyn Fn() -> Result<(), String>,
}

/// Bring the owner to the switch (owner ruling 2026-10-14: "handled automatically,
/// batteries included"). Only for `not_granted`: on a list read whole and EMPTY (no
/// Developer Tools pane yet) [`LIST_TERMINAL`] runs first, then the pane opens. aterm
/// never adds itself, never writes TCC.db, never calls tccutil and never presses anything
/// in System Settings: the switch, and macOS's authentication for it, stay the owner's.
/// `unknown` does nothing (fail-closed: the list may already hold aterm).
#[must_use]
pub fn hand_to_owner(facts: &Facts, reach: &Reach<'_>) -> Handled {
    match facts.grant() {
        DevToolGrant::Granted => Handled::Granted,
        DevToolGrant::Unknown => {
            Handled::Left("the list could not be read, so aterm leaves it alone".to_string())
        }
        DevToolGrant::NotGranted => {
            let empty = facts.list.label == DevToolListLabel::Read && facts.list.rows == Some(0);
            let listed_terminal = empty.then(|| (reach.list_terminal)());
            match (reach.open_pane)() {
                Ok(()) => Handled::Opened { listed_terminal },
                Err(why) => Handled::OpenFailed {
                    listed_terminal,
                    why,
                },
            }
        }
    }
}

/// What [`hand_to_owner`] did, in one clause for a doctor's report.
#[must_use]
pub fn handled_words(handled: &Handled) -> String {
    let terminal = |listed: &Option<Result<(), String>>| match listed {
        None => String::new(),
        Some(Ok(())) => format!("ran `{LIST_TERMINAL}` so the pane appears (it lists Terminal), "),
        Some(Err(why)) => format!("`{LIST_TERMINAL}` failed ({why}), "),
    };
    match handled {
        Handled::Granted => "nothing to do".to_string(),
        Handled::Left(why) => format!("did nothing: {why}"),
        Handled::Opened { listed_terminal } => format!(
            "{}opened {PANE}: press +, choose aterm, switch it on (macOS asks for your password)",
            terminal(listed_terminal)
        ),
        Handled::OpenFailed {
            listed_terminal,
            why,
        } => format!(
            "{}could not open the pane ({why}); run `{OPEN_PANE}`",
            terminal(listed_terminal)
        ),
    }
}

/// Where aterm notes that it opened the pane for the owner: `<state>/devtools-pane-opened`,
/// one line, `<aterm version> <unix seconds>`. The window opens the pane once per version
/// and `aterm doctor` at most once per [`REOPEN_AFTER_S`], so neither puts System Settings
/// in front of the owner again and again.
#[must_use]
pub fn opened_marker() -> Option<std::path::PathBuf> {
    aterm_types::dirs::state_dir().map(|dir| dir.join("devtools-pane-opened"))
}

/// How long `aterm doctor` waits before it opens the pane again (an agent may run it in a
/// loop): ten minutes.
pub const REOPEN_AFTER_S: u64 = 600;

/// The last time aterm opened the pane: `(version, unix seconds)`, or `None`.
#[must_use]
pub fn last_opened() -> Option<(String, u64)> {
    let text = std::fs::read_to_string(opened_marker()?).ok()?;
    parse_opened(&text)
}

/// How many seconds ago aterm opened the pane, when that was under [`REOPEN_AFTER_S`]
/// ago: `aterm doctor` does not open it again then (unless a person is at the terminal).
#[must_use]
pub fn opened_recently() -> Option<u64> {
    let (_, at) = last_opened()?;
    let ago = aterm_time::unix_secs()?.saturating_sub(at);
    (ago < REOPEN_AFTER_S).then_some(ago)
}

/// One marker line, `<version> <unix seconds>`. Pure.
fn parse_opened(text: &str) -> Option<(String, u64)> {
    let mut words = text.split_whitespace();
    let (version, at) = (words.next()?, words.next()?.parse().ok()?);
    words.next().is_none().then(|| (version.to_string(), at))
}

/// Note that aterm opened the pane now, for `version`. Best effort: a marker that cannot
/// be written means only that the pane may open once more.
pub fn note_opened(version: &str) {
    let Some(path) = opened_marker() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Some(now) = aterm_time::unix_secs() else {
        return;
    };
    let _ = std::fs::write(&path, format!("{version} {now}\n"));
}

/// Wait for the switch: read the list every two seconds until it reads `on` for `subject`
/// or `limit` passes. `read` is [`consent::probe_devtool_list`] in production. Returns the
/// last reading.
pub fn await_switch(
    subject: &str,
    limit: std::time::Duration,
    read: impl Fn(&str) -> DevToolList,
) -> DevToolList {
    let until = std::time::Instant::now() + limit;
    loop {
        let list = read(subject);
        if list.listing == DevToolListing::On
            || list.label != DevToolListLabel::Read
            || std::time::Instant::now() >= until
        {
            return list;
        }
        std::thread::sleep(std::time::Duration::from_secs(2).min(limit));
    }
}

/// How long one of the two commands may take.
const REACH_CEILING: std::time::Duration = std::time::Duration::from_secs(30);

/// Run `program args…` to its exit within [`REACH_CEILING`]; its stderr's first line when
/// it fails.
fn run_quietly(program: &str, args: &[&str]) -> Result<(), String> {
    let out = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command(program).args(args),
        std::time::Instant::now() + REACH_CEILING,
    )
    .map_err(|e| format!("{program}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&out.stderr);
    let first = said.lines().next().unwrap_or_default().trim();
    Err(if first.is_empty() {
        format!("{program} exited {:?}", out.status.code())
    } else {
        crate::progress::sanitize_for_tty(first, 160)
    })
}

/// [`LIST_TERMINAL`], absolutely named.
///
/// # Errors
/// The tool's own first line of complaint, or why it could not run.
pub fn list_terminal() -> Result<(), String> {
    run_quietly("/usr/sbin/spctl", &["developer-mode", "enable-terminal"])
}

/// Open the pane with `/usr/bin/open` ([`PANE_URL`]). It changes nothing.
///
/// # Errors
/// `open`'s own first line of complaint, or why it could not run.
pub fn open_pane() -> Result<(), String> {
    run_quietly("/usr/bin/open", &[PANE_URL])
}

#[cfg(test)]
mod tests {
    use super::*;

    const ATERM: &str = "com.aterm.aterm";

    fn facts(list: DevToolList, log: DevToolProbe) -> Facts {
        Facts {
            subject: Some(ATERM.to_string()),
            list,
            log,
            other_responsible: None,
        }
    }

    fn read(listing: DevToolListing, at: i64) -> DevToolList {
        DevToolList {
            listing,
            label: DevToolListLabel::Read,
            modified_at_s: (listing != DevToolListing::Absent).then_some(at),
            rows: Some(1),
        }
    }

    fn tccd(state: DevToolState, not_allowed: u32, at_ms: i64) -> DevToolProbe {
        DevToolProbe {
            state,
            label: DevToolProbeLabel::Observed,
            requests: Some(not_allowed.max(1)),
            not_allowed: Some(not_allowed),
            answered_at_ms: Some(at_ms),
        }
    }

    /// The row a doctor printed for each verdict: `ok` names the switch, `warn` names
    /// the cost, the place, the command that only opens it and the missing-pane note,
    /// and `unknown` says what could not be read and is never `ok`.
    #[test]
    fn the_row_says_the_state_the_cost_and_the_fix() {
        let quiet = DevToolProbe::refused(DevToolProbeLabel::NoRecentRequest);

        let on = row(&facts(read(DevToolListing::On, 100), quiet)).expect("a row");
        assert_eq!(on.level, Level::Ok);
        assert_eq!(
            on.text,
            "aterm (com.aterm.aterm) is on System Settings \u{25b8} Privacy & Security \u{25b8} \
             Developer Tools (switched on), so macOS should not hold the programs it starts for \
             its first-run check"
        );

        // The state measured on 2026-10-07 and again on 2026-10-14: no row, 96 checked.
        let absent = row(&facts(
            read(DevToolListing::Absent, 0),
            tccd(DevToolState::Unlisted, 96, 1),
        ))
        .expect("a row");
        assert_eq!(absent.level, Level::Warn);
        for needle in [
            "aterm (com.aterm.aterm) is not on System Settings \u{25b8} Privacy & Security \
             \u{25b8} Developer Tools",
            "first-run check (syspolicyd)",
            "builds and tests wait and timing tests flake",
            "macOS checked 96 new programs run in aterm in the last 10 minutes",
            "fix: switch aterm on under Developer Tools (`aterm doctor` opens the pane for \
             you, or `open \
             \"x-apple.systempreferences:com.apple.preference.security?Privacy_DevTools\"`; \
             then + \u{2192} aterm \u{2192} on; no Developer Tools pane? `spctl developer-mode \
             enable-terminal` makes it appear, listing only Terminal)",
            "it applies to the next program aterm starts",
        ] {
            assert!(
                absent.text.contains(needle),
                "missing {needle:?}\n{}",
                absent.text
            );
        }
        // No command that changes the list on the owner's behalf.
        for absent_word in ["tccutil", "sqlite3", "sudo"] {
            assert!(!absent.text.contains(absent_word), "{absent_word}");
        }

        let off = row(&facts(read(DevToolListing::Off, 100), quiet)).expect("a row");
        assert_eq!(off.level, Level::Warn);
        assert!(
            off.text.contains("is switched off on System Settings"),
            "{}",
            off.text
        );
        assert!(
            !off.text.contains("macOS checked"),
            "no count without tccd's answers"
        );

        let shadowed = row(&facts(
            read(DevToolListing::On, 100),
            tccd(DevToolState::Unlisted, 1, 100_001),
        ))
        .expect("a row");
        assert_eq!(shadowed.level, Level::Warn);
        assert!(
            shadowed
                .text
                .contains("is switched on, but tccd still checks its new programs")
                && shadowed.text.contains("checked 1 new program run"),
            "{}",
            shadowed.text
        );

        let allowed = row(&facts(
            DevToolList::refused(DevToolListLabel::NoFullDiskAccess),
            tccd(DevToolState::Allowed, 0, 1),
        ))
        .expect("a row");
        assert_eq!(allowed.level, Level::Ok);
        assert!(
            allowed
                .text
                .contains("(tccd allowed its latest new program)")
        );

        let unknown = row(&facts(
            DevToolList::refused(DevToolListLabel::NoFullDiskAccess),
            DevToolProbe::refused(DevToolProbeLabel::LogUnavailable),
        ))
        .expect("a row");
        assert_eq!(unknown.level, Level::Note);
        assert_eq!(
            unknown.text,
            "not measured (the system TCC.db that holds the list opens only to an app with Full \
             Disk Access, and only an administrator account can read tccd's log)"
        );
    }

    /// A refusal names the fence alone; off macOS there is no row at all; another
    /// responsible app is named, since its switch is the one that applies.
    #[test]
    fn a_refusal_names_the_fence_and_another_responsible_app_is_named() {
        let fenced = row(&Facts::refused(
            DevToolListLabel::RefusedOutOfBundle,
            DevToolProbeLabel::RefusedOutOfBundle,
        ))
        .expect("a row");
        assert_eq!(fenced.level, Level::Note);
        assert_eq!(fenced.text, "not measured (running outside the app bundle)");
        assert_eq!(
            row(&Facts::refused(
                DevToolListLabel::UnsupportedPlatform,
                DevToolProbeLabel::UnsupportedPlatform,
            )),
            None
        );
        let mut under_terminal = facts(
            read(DevToolListing::Absent, 0),
            DevToolProbe::refused(DevToolProbeLabel::NoRecentRequest),
        );
        under_terminal.other_responsible = Some("Terminal".to_string());
        let text = row(&under_terminal).expect("a row").text;
        assert!(
            text.ends_with(
                "; this shell runs under Terminal, not aterm, so aterm's switch does not reach it"
            ),
            "{text}"
        );
    }

    /// The hand-over acts only on `not_granted`: an empty list gets Terminal listed first so
    /// the pane exists, and the pane opens; `granted` and `unknown` do nothing at all.
    #[test]
    fn the_hand_over_opens_the_pane_only_for_not_granted() {
        let quiet = DevToolProbe::refused(DevToolProbeLabel::NoRecentRequest);
        let calls = std::cell::RefCell::new(Vec::new());
        let list = || {
            calls.borrow_mut().push("spctl");
            Ok(())
        };
        let open = || {
            calls.borrow_mut().push("open");
            Ok(())
        };
        let reach = Reach {
            list_terminal: &list,
            open_pane: &open,
        };
        let mut empty = read(DevToolListing::Absent, 0);
        empty.rows = Some(0);
        assert_eq!(
            hand_to_owner(&facts(empty, quiet), &reach),
            Handled::Opened {
                listed_terminal: Some(Ok(()))
            }
        );
        assert_eq!(calls.take(), vec!["spctl", "open"]);
        // Terminal is on the list already (the pane exists): only the pane opens.
        let handled = hand_to_owner(&facts(read(DevToolListing::Absent, 0), quiet), &reach);
        assert_eq!(
            handled,
            Handled::Opened {
                listed_terminal: None
            }
        );
        assert_eq!(calls.take(), vec!["open"]);
        assert_eq!(
            handled_words(&handled),
            "opened System Settings \u{25b8} Privacy & Security \u{25b8} Developer Tools: press +, \
             choose aterm, switch it on (macOS asks for your password)"
        );
        // Switched off: the owner's own choice is shown to them again, never flipped.
        assert_eq!(
            hand_to_owner(&facts(read(DevToolListing::Off, 1), quiet), &reach),
            Handled::Opened {
                listed_terminal: None
            }
        );
        assert_eq!(calls.take(), vec!["open"]);
        assert_eq!(
            hand_to_owner(&facts(read(DevToolListing::On, 1), quiet), &reach),
            Handled::Granted
        );
        let unknown = facts(
            DevToolList::refused(DevToolListLabel::NoFullDiskAccess),
            DevToolProbe::refused(DevToolProbeLabel::LogUnavailable),
        );
        assert!(matches!(hand_to_owner(&unknown, &reach), Handled::Left(_)));
        assert!(calls.take().is_empty(), "granted and unknown run nothing");
        let refused = || Err("LSOpenURLsWithRole() failed".to_string());
        let failing = Reach {
            list_terminal: &list,
            open_pane: &refused,
        };
        let handled = hand_to_owner(&facts(read(DevToolListing::Absent, 0), quiet), &failing);
        assert_eq!(
            handled_words(&handled),
            "could not open the pane (LSOpenURLsWithRole() failed); run `open \
             \"x-apple.systempreferences:com.apple.preference.security?Privacy_DevTools\"`"
        );
        assert!(OPEN_PANE.contains(PANE_URL));
    }

    #[test]
    fn the_marker_is_one_version_and_one_time() {
        assert_eq!(
            parse_opened("0.104.0 1792020637\n"),
            Some(("0.104.0".to_string(), 1_792_020_637))
        );
        for bad in ["", "0.104.0", "0.104.0 x", "0.104.0 1 2"] {
            assert_eq!(parse_opened(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_wait_ends_at_the_switch_or_at_an_unreadable_list() {
        let reads = std::cell::Cell::new(0);
        let got = await_switch("x", std::time::Duration::from_secs(60), |_| {
            reads.set(reads.get() + 1);
            read(DevToolListing::On, 1)
        });
        assert_eq!((got.listing, reads.get()), (DevToolListing::On, 1));
        let got = await_switch("x", std::time::Duration::from_secs(60), |_| {
            DevToolList::refused(DevToolListLabel::NoFullDiskAccess)
        });
        assert_eq!(got.label, DevToolListLabel::NoFullDiskAccess);
        let got = await_switch("x", std::time::Duration::ZERO, |_| {
            read(DevToolListing::Absent, 0)
        });
        assert_eq!(got.listing, DevToolListing::Absent);
    }

    /// `[privacy] observer = false` stops `aterm doctor`'s reads as it stops the window's
    /// (§9.1 O11); only that boolean in that table does, and a file the window would not
    /// load either is not "off".
    #[test]
    fn only_privacy_observer_false_stops_the_devtools_read() {
        assert!(observer_off_in("[privacy]\nobserver = false\n"));
        for not_off in [
            "",
            "[privacy]\n",
            "[privacy]\nobserver = true\n",
            "[privacy]\nobserver = \"false\"\n",
            "observer = false\n",
            "[privacy]\ncheck = false\n",
            "[privacy\nobserver = false\n",
        ] {
            assert!(!observer_off_in(not_off), "{not_off:?}");
        }
    }

    /// A test binary measures nothing: the probes' fence refuses before any open or spawn.
    #[test]
    fn the_real_facts_measure_nothing_from_a_test_binary() {
        let got = super::facts();
        if cfg!(target_os = "macos") {
            let got = got.expect("macOS has the list");
            assert_eq!(got.list.label, DevToolListLabel::RefusedOutOfBundle);
            assert_eq!(got.log.label, DevToolProbeLabel::RefusedOutOfBundle);
            assert_eq!(got.grant(), DevToolGrant::Unknown);
        } else {
            assert_eq!(got, None);
        }
    }
}
