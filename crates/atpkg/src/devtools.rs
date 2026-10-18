// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The Developer Tools list (macOS): whether macOS holds each new program aterm starts for
//! its first-run check.
//!
//! macOS assesses every new binary (syspolicyd, with XProtect) before its first exec, one at
//! a time, and the program waits in `_dyld_start` meanwhile: about 45 s for a hook's
//! `/usr/bin/env`, 1–3 min per freshly built test binary. An app switched on under System
//! Settings ▸ Privacy & Security ▸ Developer Tools has the programs it is responsible for
//! exempted, and every program started in an aterm tab names aterm as its responsible app.
//! Only the owner adds an app there; aterm reads the list and never writes it. The switch
//! applies once macOS asks about aterm again, which a running aterm makes happen at once
//! when it sees the switch on ([`refresh_verdict`]), with nothing relaunched.
//!
//! One detector ([`aterm_containment::consent::devtool_grant`], over the switch's row in the
//! system TCC.db and tccd's own answers) and one row ([`row`]), which `aterm doctor` and
//! `aterm pkg doctor` both print.
//!
//! The words that bring the owner to the switch ([`HELPER_TITLE`] and the functions beside
//! it) are spelled here once, for the window's helper panel, Settings ▸ Security, the digest
//! line and `aterm doctor`, so no two of them can say it differently.

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

/// The command that opens the pane and changes nothing: the owner flips the switch.
pub const OPEN_PANE: &str =
    "open \"x-apple.systempreferences:com.apple.preference.security?Privacy_DevTools\"";

/// Apple's command that adds Terminal to the list, which makes the Developer Tools pane
/// appear on a Mac whose list is empty. It lists Terminal; it does not list aterm.
pub const LIST_TERMINAL: &str = "spctl developer-mode enable-terminal";

// The words the owner reads. Each says only what was measured. Whether System Settings
// accepts aterm dropped on this list is unmeasured (spike D15), so a drag is always paired
// with the + route. What being listed skips says "should" until spike D14 is measured: the
// functions that claim it take `skips`, and every caller passes
// `SpikeEvidence::devtool_skips_first_run_check`. No restart word: once the switch is on, aterm
// has macOS check it again ([`refresh_verdict`]), and that is what reaches its next programs.

/// The helper panel's heading and the first line of `aterm doctor`'s banner.
pub const HELPER_TITLE: &str = "Add aterm to Developer Tools";

/// Why the owner is asked: what macOS does to each new program aterm starts, and what the
/// list changes ("should" until D14, see [`trade`]).
#[must_use]
pub const fn why(skips: bool) -> &'static str {
    if skips {
        "macOS checks each new program aterm starts before letting it run, one at a time, so \
         builds and tests wait for it. Programs started by an app on the Developer Tools list \
         skip that check."
    } else {
        "macOS checks each new program aterm starts before letting it run, one at a time, so \
         builds and tests wait for it. Programs started by an app on the Developer Tools list \
         should skip that check."
    }
}

/// [`WRONG_SWITCHES`]'s words, spelled once for it and for [`HELPER_HOW`]: a macro because
/// `concat!` takes literals, not consts.
macro_rules! wrong_switches {
    () => {
        "The Processor Trace and RDMA switches on that page are different settings."
    };
}

/// The other switches on the Developer Tools page (macOS 27.0.1 holds Processor Trace and
/// RDMA over Thunderbolt there): neither is aterm's, so no surface points at them.
pub const WRONG_SWITCHES: &str = wrong_switches!();

/// [`PLUS_PRESS`]'s words, spelled once for it, [`PLUS_ROUTE`] and the consts that
/// `concat!` them.
macro_rules! plus_press {
    () => {
        "press + below it and choose aterm"
    };
}

/// [`PLUS_ROUTE`]'s words (see [`plus_press`]).
macro_rules! plus_route {
    () => {
        concat!("or ", plus_press!())
    };
}

/// The press of + itself ("it" is the Developer Tools list), for a sentence that offers
/// it alone: once the panel's one drop is spent, there is no drag to pair it with.
pub const PLUS_PRESS: &str = plus_press!();

/// The route through + that rides every drag aterm offers ("it" is the Developer Tools
/// list): whether the list takes aterm dropped on it is unmeasured (spike D15), so no
/// surface offers the drag without it.
pub const PLUS_ROUTE: &str = plus_route!();

/// [`CHECK_SWITCH`]'s words (see [`plus_route`]).
macro_rules! check_switch {
    () => {
        "then check the switch on aterm's row is on"
    };
}

/// The step after the drag or +: aterm's own switch, in one spelling for the panel, the
/// Settings card and `aterm doctor`'s did-line.
pub const CHECK_SWITCH: &str = check_switch!();

/// The step for an aterm the list already holds switched off (see [`plus_route`]).
macro_rules! switch_on {
    () => {
        "switch on aterm's row (macOS asks for your password)"
    };
}

/// How, in the order the owner meets it: the drop or + (macOS asks for the password there),
/// then aterm's own switch, then the switches that are not it.
pub const HELPER_HOW: &str = concat!(
    "Drag aterm into the Developer Tools list (",
    plus_route!(),
    "), enter your password, ",
    check_switch!(),
    ". ",
    wrong_switches!()
);

/// How, where aterm offers no icon to drag (a copy it will not offer — translocated, on a
/// disk image, a dev bundle — or one it has not yet identified): + leads, since a sentence
/// that starts with a drag sends the owner looking for something to drag. Then aterm's own
/// switch, then the switches that are not it.
pub const HELPER_HOW_PLUS: &str = concat!(
    "Press + below the Developer Tools list and choose aterm, enter your password, ",
    check_switch!(),
    ". ",
    wrong_switches!()
);

/// How, for an aterm the list already holds, switched off: its own switch (macOS asks for
/// the password there), then the switches that are not it. Never the drag: it is listed.
pub const HELPER_SWITCH_ON: &str = concat!(
    "aterm is already on the Developer Tools list, switched off: ",
    switch_on!(),
    ". ",
    wrong_switches!()
);

/// What is true for an aterm the list holds switched on while tccd still checks its new
/// programs (its answer came after the switch last changed, `consent::devtool_grant`):
/// there is nothing to drag in and nothing to switch on, and aterm names the two causes
/// the joined verdict allows without claiming a remedy nobody measured.
pub const HELPER_LISTED_ON: &str = "aterm is already on the Developer Tools list and switched \
                                    on: there is nothing to add. macOS still checked a new \
                                    program aterm started after that, so the row may not match \
                                    this copy of aterm, or a policy holds it.";

/// The owner's step, by what the list held of aterm when aterm read it: which how every
/// surface says ([`Step::how`]), and what `aterm doctor`'s did-line asks after it opened
/// the page ([`handled_words`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// aterm is not on the list as read, or the list was not read: add aterm (the drag,
    /// or +), then its switch ([`HELPER_HOW`]).
    Add,
    /// The list, read whole, holds aterm switched off: its switch, never a drag
    /// ([`HELPER_SWITCH_ON`]).
    SwitchOn,
    /// The list, read whole, holds aterm switched on, while tccd still checks its new
    /// programs: nothing to add ([`HELPER_LISTED_ON`]).
    ListedOn,
}

impl Step {
    /// The step for the list as read. Pure.
    #[must_use]
    pub const fn of(list: &DevToolList) -> Self {
        match (list.label, list.listing) {
            (DevToolListLabel::Read, DevToolListing::Off) => Self::SwitchOn,
            (DevToolListLabel::Read, DevToolListing::On) => Self::ListedOn,
            _ => Self::Add,
        }
    }

    /// The how for this step, in the words every surface shares.
    #[must_use]
    pub const fn how(self) -> &'static str {
        match self {
            Self::Add => HELPER_HOW,
            Self::SwitchOn => HELPER_SWITCH_ON,
            Self::ListedOn => HELPER_LISTED_ON,
        }
    }

    /// This step as one action, for the digest line ([`HELPER_STEP`] says the rules): the drag
    /// only for an aterm the list does not hold.
    #[must_use]
    pub const fn action(self) -> &'static str {
        match self {
            Self::Add => HELPER_STEP,
            Self::SwitchOn => STEP_SWITCH_ON,
            Self::ListedOn => STEP_LISTED_ON,
        }
    }
}

/// [`STATUS_PAGE`]'s words (see [`plus_route`]).
macro_rules! status_page {
    () => {
        "aterm Settings \u{25b8} Security"
    };
}

/// Where the list's state stays readable: aterm's own Settings ▸ Security, whose DEVELOPER
/// TOOLS card says it. One spelling for the panel's link, the Settings card and `aterm
/// doctor`.
pub const STATUS_PAGE: &str = status_page!();

/// The helper panel's footer link, and `aterm doctor`'s banner line, to [`STATUS_PAGE`].
pub const STATUS_LINK: &str = concat!("Status: ", status_page!());

/// The how as one action, for the digest line's step: at most 90 characters, with no ` or `
/// and no `; ` (`aterm_messages::need::Step::fault` records a line whose step is two
/// actions instead of raising it). The + route rides the line's details.
pub const HELPER_STEP: &str = "Drag aterm into the Developer Tools list";

/// [`Step::SwitchOn`] as one action ([`HELPER_STEP`]'s rules): its switch, never a drag.
pub const STEP_SWITCH_ON: &str = "Switch on aterm's row in the Developer Tools list";

/// [`Step::ListedOn`] as one action ([`HELPER_STEP`]'s rules): nothing to add and nothing
/// to switch on, and no remedy nobody measured ([`HELPER_LISTED_ON`] names the causes).
pub const STEP_LISTED_ON: &str = "Look at aterm's row in the Developer Tools list";

/// Which switch matters. `listed_terminal_just_now` is set when this hand-over ran
/// [`LIST_TERMINAL`] and it answered Ok: the Terminal row the owner sees is aterm's doing,
/// so the words say why it is there. aterm reads only its own row and a row count, so it
/// never says anything about a Terminal row it did not add, which may already be on.
#[must_use]
pub const fn only_aterm(listed_terminal_just_now: bool) -> &'static str {
    if listed_terminal_just_now {
        "aterm added Terminal, switched off, so that macOS shows this list. Leave Terminal \
         off: only aterm needs to be on."
    } else {
        "Only aterm's switch needs to be on."
    }
}

/// What the owner trades, "should" until D14: the same words for the panel, the Settings
/// card and `aterm doctor`.
#[must_use]
pub const fn trade(skips: bool) -> &'static str {
    if skips {
        "Everything run in aterm, agents and downloaded tools included, then starts without \
         macOS's first-run malware scan or Gatekeeper's prompt for unsigned or quarantined \
         code. You can switch it off there at any time."
    } else {
        "Everything run in aterm, agents and downloaded tools included, should then start \
         without macOS's first-run malware scan or Gatekeeper's prompt for unsigned or \
         quarantined code. You can switch it off there at any time."
    }
}

/// What the panel says once aterm's switch reads on ("should" until D14).
#[must_use]
pub const fn done(skips: bool) -> &'static str {
    if skips {
        "aterm is on the Developer Tools list and switched on. New programs aterm starts skip \
         the first-run check."
    } else {
        "aterm is on the Developer Tools list and switched on. New programs aterm starts \
         should skip the first-run check."
    }
}

/// What the doctors read: the app asked about, the switch's row and tccd's answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    /// The running bundle's `CFBundleIdentifier`, when there is one.
    pub subject: Option<String>,
    /// The switch's row in the system TCC.db ([`consent::probe_devtool_list`]).
    pub list: DevToolList,
    /// tccd's answers for the subject in the last ten minutes ([`consent::probe_devtool`]).
    pub log: DevToolProbe,
    /// The display name of the program macOS holds responsible for this process, when it is
    /// not the subject's bundle (a doctor run in Terminal, or over ssh): aterm's switch does
    /// not reach this shell.
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

/// Read the list and tccd's answers for the running bundle, under the probes' own fences: a
/// binary outside a `.app` (a test binary, `target/release/aterm`) opens and spawns nothing
/// and is `not measured`; one started through `~/.local/bin/aterm` is judged by the bundled
/// image ([`consent::fence_exe`]). The gate is [`ProbeGate::on`]: `[privacy] enabled` and
/// `check` gate the window's background probes, and neither read can raise a dialog.
/// `[privacy] observer = false` refuses both, as it refuses the window's row. `None` off
/// macOS, where there is no such list.
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

/// Why the switch's row was not read, in words: one clause, for a doctor's `not measured
/// (…)` and the window's Settings card alike.
#[must_use]
pub const fn list_reason(label: DevToolListLabel) -> &'static str {
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
        DevToolListLabel::RefusedNoSubject => {
            "aterm holds no usable bundle identifier for this build"
        }
        DevToolListLabel::UnsupportedPlatform => "macOS only",
    }
}

/// Why tccd's answers were not read, in words (see [`list_reason`]).
#[must_use]
pub const fn log_reason(label: DevToolProbeLabel) -> &'static str {
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
        DevToolProbeLabel::RefusedNoSubject => {
            "aterm holds no usable bundle identifier for this build"
        }
        DevToolProbeLabel::UnsupportedPlatform => "macOS only",
    }
}

/// The `devtools:` row, pure over the facts, or `None` where there is no such list.
///
/// `not_granted` is a warning (both doctors count it), never a failure: nothing in it stops a
/// shell from starting, and only the owner can change it. `unknown` says what could not be
/// read; it is never `ok`.
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
    // tccd's own count, when its log was read.
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
            let fix = match Step::of(&facts.list) {
                Step::Add => format!(
                    "fix: add aterm to Developer Tools (`aterm doctor` opens the pane for you, \
                     or `{OPEN_PANE}`; then drag aterm into the list ({PLUS_ROUTE}) and switch \
                     it on; no Developer Tools pane? `{LIST_TERMINAL}` makes it appear, listing \
                     only Terminal) \u{2014} aterm then has macOS check it again"
                ),
                Step::SwitchOn => format!(
                    "fix: switch on aterm's row under Developer Tools (`aterm doctor` opens the \
                     pane for you, or `{OPEN_PANE}`; macOS asks for your password) \u{2014} aterm \
                     then has macOS check it again"
                ),
                Step::ListedOn => format!(
                    "aterm is already listed and switched on, so there is nothing to add: the \
                     row may not match this copy of aterm, or a policy holds it (`aterm doctor` \
                     opens the pane for you, or `{OPEN_PANE}`)"
                ),
            };
            (
                Level::Warn,
                format!(
                    "{who} {is} {PANE} \u{2014} macOS holds every new program it starts for a \
                     first-run check (syspolicyd), one at a time, so builds and tests wait and \
                     timing tests flake{checked} \u{2014} {fix}{under}"
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
        /// The owner's step there, by the list as read ([`Step::of`]).
        step: Step,
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

/// Bring the owner to the switch, only for `not_granted`: on a list read whole and empty (no
/// Developer Tools pane yet) [`LIST_TERMINAL`] runs first, then the pane opens. aterm never
/// adds itself, writes TCC.db, calls tccutil or presses anything in System Settings: the
/// switch and macOS's authentication for it stay the owner's. `unknown` does nothing
/// (fail-closed: the list may already hold aterm).
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
                Ok(()) => Handled::Opened {
                    listed_terminal,
                    step: Step::of(&facts.list),
                },
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
        Some(Ok(())) => format!(
            "ran `{LIST_TERMINAL}` so the pane appears (it adds Terminal, switched off: leave it \
             off), "
        ),
        Some(Err(why)) => format!("`{LIST_TERMINAL}` failed ({why}), "),
    };
    match handled {
        Handled::Granted => "nothing to do".to_string(),
        Handled::Left(why) => format!("did nothing: {why}"),
        Handled::Opened {
            listed_terminal,
            step,
        } => format!(
            "{}opened {PANE}: {}",
            terminal(listed_terminal),
            match step {
                Step::Add => concat!(
                    "drag aterm into the list (",
                    plus_route!(),
                    ") and enter your password, ",
                    check_switch!()
                ),
                Step::SwitchOn =>
                    concat!("aterm is already on the list, switched off: ", switch_on!()),
                Step::ListedOn =>
                    "aterm is already on the list and switched on: there is nothing to add",
            }
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

/// The two markers that hold back one System Settings page aterm brings up for the owner,
/// both in `<state>`: when aterm last opened it ([`Self::last_opened`]) and the owner's Not
/// now ([`Self::declined_until`]). Developer Tools has one pair ([`DEVELOPER_TOOLS`]), Full
/// Disk Access the other ([`FULL_DISK_ACCESS`]), so opening or declining one page never holds
/// back the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageMarkers {
    opened: &'static str,
    declined: &'static str,
}

/// System Settings ▸ Privacy & Security ▸ Developer Tools: `devtools-pane-opened` and
/// `devtools-declined`, which `aterm doctor` reads and writes too.
pub const DEVELOPER_TOOLS: PageMarkers = PageMarkers {
    opened: "devtools-pane-opened",
    declined: "devtools-declined",
};

/// System Settings ▸ Privacy & Security ▸ Full Disk Access: `fda-pane-opened` and
/// `fda-declined`, the window's own hand-over of that page.
pub const FULL_DISK_ACCESS: PageMarkers = PageMarkers {
    opened: "fda-pane-opened",
    declined: "fda-declined",
};

impl PageMarkers {
    /// Where aterm notes that it opened the page for the owner: one line,
    /// `<aterm version> <unix seconds>`. The window's own hand-over opens a page once per
    /// version and an unattended one at most once per [`REOPEN_AFTER_S`], so System Settings
    /// does not come up in front of the owner again and again.
    #[must_use]
    pub fn opened_marker(self) -> Option<std::path::PathBuf> {
        aterm_types::dirs::state_dir().map(|dir| dir.join(self.opened))
    }

    /// The last time aterm opened the page: `(version, unix seconds)`, or `None`.
    #[must_use]
    pub fn last_opened(self) -> Option<(String, u64)> {
        let text = std::fs::read_to_string(self.opened_marker()?).ok()?;
        parse_opened(&text)
    }

    /// How many seconds ago aterm opened the page, when that was under `window_s` ago.
    #[must_use]
    pub fn opened_within(self, window_s: u64) -> Option<u64> {
        let (_, at) = self.last_opened()?;
        opened_ago(at, aterm_time::unix_secs()?, window_s)
    }

    /// Note that aterm opened the page now, for `version`. Best effort: a marker that cannot
    /// be written means only that the page may open once more.
    pub fn note_opened(self, version: &str) {
        let Some(path) = self.opened_marker() else {
            return;
        };
        // `state_dir()`: on macOS `~/Library/Application Support/aterm`, the parent of the
        // atpkg prefix, so never group/other-writable whatever the umask.
        if let Some(dir) = path.parent() {
            let _ = aterm_types::fs_restricted::create_dir_all_unshared(dir, 0o755);
        }
        let Some(now) = aterm_time::unix_secs() else {
            return;
        };
        let _ = std::fs::write(&path, format!("{version} {now}\n"));
    }

    /// Where aterm notes that the owner said Not now to the page's helper panel, or closed
    /// it with aterm not switched on: one `<aterm version> <unix seconds>` line per decline,
    /// newest last. Deleting the file undoes it.
    #[must_use]
    pub fn declined_marker(self) -> Option<std::path::PathBuf> {
        aterm_types::dirs::state_dir().map(|dir| dir.join(self.declined))
    }

    /// The owner's latest decline of the page, while it holds; `None` when there is none, it
    /// has run out, or the marker cannot be read.
    #[must_use]
    pub fn declined_until(self) -> Option<Declined> {
        let text = std::fs::read_to_string(self.declined_marker()?).ok()?;
        decline_holding(&text, aterm_time::unix_secs()?)
    }

    /// Note that the owner declined the page now, under `version`. Best effort: a marker
    /// that cannot be written means only that aterm may offer the page once more.
    pub fn note_declined(self, version: &str) {
        let Some(path) = self.declined_marker() else {
            return;
        };
        // `state_dir()`: on macOS `~/Library/Application Support/aterm`, the parent of the
        // atpkg prefix, so never group/other-writable whatever the umask.
        if let Some(dir) = path.parent() {
            let _ = aterm_types::fs_restricted::create_dir_all_unshared(dir, 0o755);
        }
        let Some(now) = aterm_time::unix_secs() else {
            return;
        };
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let _ = std::fs::write(&path, with_decline(&text, version, now));
    }
}

/// Where aterm notes that it opened the Developer Tools pane for the owner
/// ([`PageMarkers::opened_marker`] of [`DEVELOPER_TOOLS`]).
#[must_use]
pub fn opened_marker() -> Option<std::path::PathBuf> {
    DEVELOPER_TOOLS.opened_marker()
}

/// How long an unattended hand-over (the window's own, `aterm ctl devtools hand-over`, or
/// `aterm doctor` with nobody at its terminal, which an agent may run in a loop) waits
/// before it opens the pane again.
pub const REOPEN_AFTER_S: u64 = 600;

/// The last time aterm opened the Developer Tools pane: `(version, unix seconds)`, or `None`.
#[must_use]
pub fn last_opened() -> Option<(String, u64)> {
    DEVELOPER_TOOLS.last_opened()
}

/// How soon a request made with a person at the screen (`aterm ctl devtools hand-over now`,
/// which `aterm doctor` sends from a terminal) may put the page in front of the owner again.
/// Such a request skips the window's wait for a quiet moment, so it is held to this instead
/// of [`REOPEN_AFTER_S`].
pub const REOPEN_NOW_AFTER_S: u64 = 60;

/// How many seconds ago aterm opened the pane, when that was under `window_s` ago
/// ([`REOPEN_AFTER_S`], [`REOPEN_NOW_AFTER_S`]).
#[must_use]
pub fn opened_within(window_s: u64) -> Option<u64> {
    DEVELOPER_TOOLS.opened_within(window_s)
}

/// The pure half of [`opened_within`]: seconds from `at` to `now`, when under `window_s`.
/// A stamp more than [`STAMP_SKEW_S`] ahead of the clock was written under a clock since
/// corrected: it counts as no recent open, rather than holding the page back until the
/// clock catches up.
fn opened_ago(at: u64, now: u64, window_s: u64) -> Option<u64> {
    if at > now.saturating_add(STAMP_SKEW_S) {
        return None;
    }
    let ago = now.saturating_sub(at);
    (ago < window_s).then_some(ago)
}

/// One marker line, `<version> <unix seconds>`. Pure.
fn parse_opened(text: &str) -> Option<(String, u64)> {
    let mut words = text.split_whitespace();
    let (version, at) = (words.next()?, words.next()?.parse().ok()?);
    words.next().is_none().then(|| (version.to_string(), at))
}

/// Note that aterm opened the Developer Tools pane now, for `version`. Best effort: a
/// marker that cannot be written means only that the pane may open once more.
pub fn note_opened(version: &str) {
    DEVELOPER_TOOLS.note_opened(version);
}

/// Where aterm notes that the owner said Not now to the helper panel, or closed it with
/// aterm not switched on: `<state>/devtools-declined`, one `<aterm version> <unix seconds>`
/// line per decline, newest last. The window's automatic hand-over, `aterm ctl devtools
/// hand-over` and an unattended `aterm doctor` leave the owner alone while it holds
/// ([`declined_until`]); the owner's own presses (the Settings card, the digest line) open
/// the page regardless. Deleting the file undoes it.
#[must_use]
pub fn declined_marker() -> Option<std::path::PathBuf> {
    DEVELOPER_TOOLS.declined_marker()
}

/// How long a first decline holds: the message center's span for the owner's own choice
/// (`aterm_messages::need::Resurface::CHOICE`).
pub const DECLINE_HOLD_S: u64 = 30 * 86_400;

/// How long a repeat decline holds: what `Resurface::CHOICE` backs off to on a second filing
/// (`Resurface::MAX`). A decline never holds for good.
pub const DECLINE_REPEAT_HOLD_S: u64 = 90 * 86_400;

/// How far back an earlier decline makes a new one a repeat: the message center's
/// `FILED_KEEP`.
const DECLINE_KEEP_S: u64 = 180 * 86_400;

/// How far ahead of the clock a marker (a decline, the page's last opening) may be stamped
/// and still count: the message center's `FILING_SKEW_MS`. A stamp beyond it was written
/// under a clock since corrected, so it keeps nothing quiet: the page shown once more is
/// cheaper than one held back for years. The window's Settings card ages the opened marker
/// by the same rule.
pub const STAMP_SKEW_S: u64 = 3_600;

/// Decline lines kept: the newest and the one before it, all a repeat needs.
const DECLINES_KEPT: usize = 2;

/// A decline that still holds ([`declined_until`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declined {
    /// The aterm version whose page the owner declined.
    pub version: String,
    /// When, in unix seconds.
    pub at: u64,
    /// Until when it holds, in unix seconds: [`DECLINE_HOLD_S`] after `at`, or
    /// [`DECLINE_REPEAT_HOLD_S`] when an earlier decline lies within 180 days before it.
    pub until: u64,
}

/// The owner's latest decline, while it holds; `None` when there is none, it has run
/// out, or the marker cannot be read (a missing marker means only that aterm may offer
/// the page once more).
#[must_use]
pub fn declined_until() -> Option<Declined> {
    DEVELOPER_TOOLS.declined_until()
}

/// The pure half of [`declined_until`]: the newest valid line of `text`, and whether it
/// still holds at `now`. Unparsable lines are skipped.
fn decline_holding(text: &str, now: u64) -> Option<Declined> {
    let declines: Vec<(String, u64)> = text.lines().filter_map(parse_opened).collect();
    let ((version, at), earlier) = declines.split_last()?;
    let at = *at;
    if at > now.saturating_add(STAMP_SKEW_S) {
        return None;
    }
    let repeat = earlier
        .iter()
        .any(|&(_, before)| before <= at && at - before <= DECLINE_KEEP_S);
    let until = at.saturating_add(if repeat {
        DECLINE_REPEAT_HOLD_S
    } else {
        DECLINE_HOLD_S
    });
    (now < until).then(|| Declined {
        version: version.clone(),
        at,
        until,
    })
}

/// The marker's next text: `text`'s valid lines with `<version> <now>` appended, the
/// newest [`DECLINES_KEPT`] kept. Pure.
fn with_decline(text: &str, version: &str, now: u64) -> String {
    let mut lines: Vec<String> = text
        .lines()
        .filter_map(parse_opened)
        .map(|(version, at)| format!("{version} {at}"))
        .collect();
    lines.push(format!("{version} {now}"));
    let skip = lines.len().saturating_sub(DECLINES_KEPT);
    lines
        .into_iter()
        .skip(skip)
        .map(|line| line + "\n")
        .collect()
}

/// Note that the owner declined the Developer Tools page now, under `version`. Best
/// effort: a marker that cannot be written means only that aterm may offer the page once
/// more.
pub fn note_declined(version: &str) {
    DEVELOPER_TOOLS.note_declined(version);
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

/// The hidden verb `aterm pkg __devtools-prime` ([`run_prime`]): never advertised,
/// answered on the raw argv before the store lock, which it never takes.
pub const PRIME_VERB: &str = "__devtools-prime";

/// How long the one new program [`run_prime`] starts may take: a hang detector, never
/// a budget (a first exec macOS still checks can wait in `_dyld_start` for a minute
/// under load).
#[cfg(target_os = "macos")]
const PRIME_CEILING: std::time::Duration = std::time::Duration::from_secs(180);

/// MAKE macOS ASK ABOUT ATERM AGAIN, once the owner switched it on: [`run_prime`] in
/// aterm's own executable, started as its OWN responsible process.
///
/// Measured on macOS 27.0 in a VM (2026-10-16, design §7 D14): syspolicyd keeps its
/// verdict for an aterm process it checked before the switch went on — every new
/// program there is still checked, in its old tabs and its new ones, for as long as
/// it was watched — until an aterm process it has not checked starts a program it has
/// not assessed. Then tccd answers `Allowed (System Set)`, syspolicyd logs that the
/// responsible process "is a developer tool", and from then on no aterm process's new
/// programs are checked: the old one's, and the tabs a seamless update adopted. So the
/// window and `aterm doctor` run this once the switch reads on, and nothing has to be
/// relaunched. It grants and writes nothing: with aterm not on the list it is one
/// small check, as for any new program.
///
/// # Errors
/// Why it could not run: not a bundled aterm, the responsibility attribute macOS no
/// longer offers (`aterm_uds::spawnfd::Command::disclaim_responsibility`), or the
/// verb's own first line of complaint.
pub fn refresh_verdict() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let exe = consent::fence_exe().ok_or("aterm's own executable is unknown")?;
        if consent::app_bundle_root(&exe).is_none() {
            return Err("not a bundled aterm, which is what macOS asks about".to_string());
        }
        let out = aterm_uds::deadline::output_until(
            aterm_uds::spawnfd::command(&exe)
                .args(["pkg", PRIME_VERB])
                .disclaim_responsibility(true),
            // The prime's own work ends within REACH_CEILING + PRIME_CEILING of its start
            // ([`prime`]); one more REACH_CEILING for aterm's start and its cleanup, so
            // this deadline never kills it before it removes its directory.
            std::time::Instant::now() + PRIME_CEILING + REACH_CEILING * 2,
        )
        .map_err(|e| format!("aterm pkg {PRIME_VERB}: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        let said = String::from_utf8_lossy(&out.stderr);
        let first = said.lines().next().unwrap_or_default().trim();
        Err(if first.is_empty() {
            format!("aterm pkg {PRIME_VERB} exited {:?}", out.status.code())
        } else {
            crate::progress::sanitize_for_tty(first, 160)
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("the Developer Tools list is macOS's".to_string())
    }
}

/// [`PRIME_VERB`]: start one program nobody has assessed, so macOS asks about this
/// process — aterm, started as its own responsible process by [`refresh_verdict`] —
/// and then exit. `/usr/bin/true` copied under a fresh name into a new private
/// directory and signed ad hoc (codesign names an ad hoc signature after its file, so
/// a fresh name is a fresh code hash, never one macOS assessed already), run once, and
/// removed with the directory however the run ends. Says one line on stderr when it
/// fails; nothing else.
pub fn run_prime() -> std::process::ExitCode {
    match prime() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("aterm pkg {PRIME_VERB}: {why}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// [`run_prime`]'s work.
fn prime() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        // Everything below ends by here, inside `refresh_verdict`'s deadline.
        let by = std::time::Instant::now() + REACH_CEILING + PRIME_CEILING;
        let token = aterm_uds::rand::hex_token::<8>().map_err(|e| format!("no name: {e}"))?;
        let dir = std::env::temp_dir().join(format!("aterm-devtools-prime-{token}"));
        crate::prereq::private_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let _remove = crate::prereq::RemoveOnDrop(&dir);
        let name = format!("aterm-devtools-{token}");
        let program = dir.join(&name);
        std::fs::copy("/usr/bin/true", &program).map_err(|e| format!("copy: {e}"))?;
        let program = program
            .to_str()
            .ok_or("the private directory's path is not UTF-8")?;
        run_quietly(
            "/usr/bin/codesign",
            &["--force", "--sign", "-", "--identifier", &name, program],
        )?;
        let out = aterm_uds::deadline::output_until(&mut aterm_uds::spawnfd::command(program), by)
            .map_err(|e| format!("{name}: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!("{name} exited {:?}", out.status.code()))
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("the Developer Tools list is macOS's".to_string())
    }
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

    /// The row's level for each verdict: `ok` on the list (or tccd allowing), `warn` off it,
    /// switched off, or switched on while tccd still checks — with the count of programs
    /// tccd checked when it answered, and no command that changes the list on the owner's
    /// behalf — and `note`, never `ok`, when nothing could be read.
    #[test]
    fn the_row_says_the_state_the_cost_and_the_fix() {
        let quiet = DevToolProbe::refused(DevToolProbeLabel::NoRecentRequest);
        let on = row(&facts(read(DevToolListing::On, 100), quiet)).expect("a row");
        assert_eq!(on.level, Level::Ok);
        // No row, and tccd checked 96 programs.
        let absent = row(&facts(
            read(DevToolListing::Absent, 0),
            tccd(DevToolState::Unlisted, 96, 1),
        ))
        .expect("a row");
        assert_eq!(absent.level, Level::Warn);
        assert!(absent.text.contains("96"), "{}", absent.text);
        for absent_word in ["tccutil", "sqlite3", "sudo"] {
            assert!(!absent.text.contains(absent_word), "{absent_word}");
        }
        let off = row(&facts(read(DevToolListing::Off, 100), quiet)).expect("a row");
        assert_eq!(off.level, Level::Warn);
        // Listed, switched off: its row's switch, never a drag, and no Terminal listing
        // (the pane exists: aterm is on it).
        assert!(
            off.text.contains("fix: switch on aterm's row") && off.text.contains(OPEN_PANE),
            "{}",
            off.text
        );
        for not_said in ["drag", LIST_TERMINAL] {
            assert!(!off.text.contains(not_said), "{not_said}: {}", off.text);
        }
        let shadowed = row(&facts(
            read(DevToolListing::On, 100),
            tccd(DevToolState::Unlisted, 1, 100_001),
        ))
        .expect("a row");
        assert_eq!(shadowed.level, Level::Warn, "{}", shadowed.text);
        // Listed and on: nothing to add, so no drag, no switch to turn on, no promise.
        assert!(
            shadowed.text.contains("nothing to add"),
            "{}",
            shadowed.text
        );
        for not_said in ["drag", "fix: ", "aterm then has macOS check it again"] {
            assert!(
                !shadowed.text.contains(not_said),
                "{not_said}: {}",
                shadowed.text
            );
        }
        let allowed = row(&facts(
            DevToolList::refused(DevToolListLabel::NoFullDiskAccess),
            tccd(DevToolState::Allowed, 0, 1),
        ))
        .expect("a row");
        assert_eq!(allowed.level, Level::Ok);
        let unknown = row(&facts(
            DevToolList::refused(DevToolListLabel::NoFullDiskAccess),
            DevToolProbe::refused(DevToolProbeLabel::LogUnavailable),
        ))
        .expect("a row");
        assert_eq!(unknown.level, Level::Note);
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
        let handled = hand_to_owner(&facts(empty, quiet), &reach);
        assert_eq!(
            handled,
            Handled::Opened {
                listed_terminal: Some(Ok(())),
                step: Step::Add,
            }
        );
        assert_eq!(calls.take(), vec!["spctl", "open"]);
        // The Terminal row aterm just added is named, so the owner leaves it off.
        let words = handled_words(&handled);
        assert!(
            words.starts_with("ran `spctl developer-mode enable-terminal` so the pane appears")
                && words.contains("leave it off"),
            "{words}"
        );
        // Terminal is on the list already (the pane exists): only the pane opens, and the
        // did-line asks for the drag paired with +, then aterm's own switch.
        let handled = hand_to_owner(&facts(read(DevToolListing::Absent, 0), quiet), &reach);
        assert_eq!(
            handled,
            Handled::Opened {
                listed_terminal: None,
                step: Step::Add,
            }
        );
        assert_eq!(calls.take(), vec!["open"]);
        let words = handled_words(&handled);
        assert!(
            words.starts_with(&format!("opened {PANE}: drag aterm")),
            "{words}"
        );
        for said in [PLUS_ROUTE, CHECK_SWITCH] {
            assert!(words.contains(said), "{said}: {words}");
        }
        assert!(!words.contains("Terminal"), "{words}");
        // Switched off: the owner's own choice is shown to them again, never flipped, and
        // the did-line asks for that row's switch, never a drag.
        let handled = hand_to_owner(&facts(read(DevToolListing::Off, 1), quiet), &reach);
        assert_eq!(
            handled,
            Handled::Opened {
                listed_terminal: None,
                step: Step::SwitchOn,
            }
        );
        assert_eq!(calls.take(), vec!["open"]);
        let words = handled_words(&handled);
        assert!(words.contains("switch on aterm's row"), "{words}");
        assert!(!words.contains("drag"), "{words}");
        // Listed and on while tccd still checks: the page opens, and nothing is to add.
        let shadowed = facts(
            read(DevToolListing::On, 1),
            tccd(DevToolState::Unlisted, 1, 100_001),
        );
        let handled = hand_to_owner(&shadowed, &reach);
        assert_eq!(
            handled,
            Handled::Opened {
                listed_terminal: None,
                step: Step::ListedOn,
            }
        );
        assert_eq!(calls.take(), vec!["open"]);
        let words = handled_words(&handled);
        assert!(
            words.contains("nothing to add") && !words.contains("drag"),
            "{words}"
        );
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
        // How long ago, inside the window; a stamp more than an hour ahead of the clock
        // (written under a clock since corrected) is no recent open.
        let at = 1_792_020_637;
        assert_eq!(opened_ago(at, at + 59, REOPEN_NOW_AFTER_S), Some(59));
        assert_eq!(opened_ago(at, at + 60, REOPEN_NOW_AFTER_S), None);
        assert_eq!(
            opened_ago(at, at - 600, REOPEN_AFTER_S),
            Some(0),
            "small skew"
        );
        assert_eq!(opened_ago(at, at - 2 * 3_600, REOPEN_AFTER_S), None);
    }

    /// Each page aterm brings up keeps its own pair of markers: opening or declining Full Disk
    /// Access never holds back Developer Tools, and `aterm doctor`'s Developer Tools files keep
    /// their names.
    #[test]
    fn each_page_keeps_its_own_markers() {
        assert_eq!(DEVELOPER_TOOLS.opened, "devtools-pane-opened");
        assert_eq!(DEVELOPER_TOOLS.declined, "devtools-declined");
        let names = [
            DEVELOPER_TOOLS.opened,
            DEVELOPER_TOOLS.declined,
            FULL_DISK_ACCESS.opened,
            FULL_DISK_ACCESS.declined,
        ];
        for (i, a) in names.iter().enumerate() {
            for b in &names[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    /// A decline holds 30 days, and 90 when an earlier one lies within 180 days before it
    /// (the message center's spans for the owner's own choice); it never holds for good, a
    /// stamp from a clock since corrected keeps nothing quiet, and a line the marker cannot
    /// parse is skipped rather than read as a decline.
    #[test]
    fn a_decline_holds_a_month_then_three_and_never_for_good() {
        const DAY: u64 = 86_400;
        let at = 1_792_092_226;
        let once = with_decline("", "0.104.0", at);
        assert_eq!(once, format!("0.104.0 {at}\n"));
        let held = decline_holding(&once, at + DAY).expect("a fresh decline holds");
        assert_eq!(
            held,
            Declined {
                version: "0.104.0".to_string(),
                at,
                until: at + 30 * DAY,
            }
        );
        assert_eq!(
            decline_holding(&once, at + 30 * DAY),
            None,
            "a month, then over"
        );

        // Declined again 40 days on: a repeat, so three months.
        let again = at + 40 * DAY;
        let twice = with_decline(&once, "0.105.0", again);
        assert_eq!(twice.lines().count(), 2);
        let held = decline_holding(&twice, again + 89 * DAY).expect("a repeat holds longer");
        assert_eq!(
            (held.version.as_str(), held.until),
            ("0.105.0", again + 90 * DAY)
        );
        assert_eq!(decline_holding(&twice, again + 90 * DAY), None);

        // An earlier decline more than 180 days back does not make a repeat.
        let late = at + 200 * DAY;
        let lapsed = with_decline(&once, "0.110.0", late);
        assert_eq!(
            decline_holding(&lapsed, late).map(|held| held.until),
            Some(late + 30 * DAY)
        );

        // Only the newest two lines are kept; garbage is dropped on the next write.
        let kept = with_decline(&format!("junk\n{twice}"), "0.106.0", again + DAY);
        assert_eq!(kept, format!("0.105.0 {again}\n0.106.0 {}\n", again + DAY));
        assert_eq!(decline_holding("junk\n0.104.0\n", at), None);
        assert_eq!(decline_holding("", at), None);

        // Stamped more than an hour ahead of the clock: holds nothing.
        assert_eq!(decline_holding(&once, at - 2 * 3_600), None);
        assert!(decline_holding(&once, at - 600).is_some());
    }

    /// The words every surface shares: the drag is never offered without the + route (drop
    /// acceptance is unmeasured, D15); "should" is said exactly while D14 is unmeasured;
    /// the digest's step is one action; the wrong switches are named in the how; and no
    /// restart, elimination or coverage word appears.
    #[test]
    fn the_words_pair_the_drag_with_plus_and_say_should_until_d14() {
        for skips in [false, true] {
            for words in [why(skips), trade(skips), done(skips)] {
                assert_eq!(words.contains("should"), !skips, "D14={skips}: {words}");
            }
            assert!(
                trade(skips).contains("Gatekeeper's prompt"),
                "{}",
                trade(skips)
            );
        }
        assert!(
            HELPER_HOW.contains("Drag aterm")
                && HELPER_HOW.contains(PLUS_ROUTE)
                && HELPER_HOW.contains(CHECK_SWITCH),
            "{HELPER_HOW}"
        );
        assert!(HELPER_HOW.ends_with(WRONG_SWITCHES), "{HELPER_HOW}");
        // No icon to drag: + leads, and no drag is spoken of.
        assert!(
            HELPER_HOW_PLUS.starts_with("Press + below the Developer Tools list")
                && HELPER_HOW_PLUS.contains(CHECK_SWITCH)
                && HELPER_HOW_PLUS.ends_with(WRONG_SWITCHES)
                && !HELPER_HOW_PLUS.to_ascii_lowercase().contains("drag"),
            "{HELPER_HOW_PLUS}"
        );
        // Listed and switched off: the switch, never the drag; listed and on: neither.
        assert!(
            HELPER_SWITCH_ON.ends_with(WRONG_SWITCHES) && !HELPER_SWITCH_ON.contains("Drag"),
            "{HELPER_SWITCH_ON}"
        );
        assert!(
            !HELPER_LISTED_ON.to_ascii_lowercase().contains("drag")
                && !HELPER_LISTED_ON.contains("switch on"),
            "{HELPER_LISTED_ON}"
        );
        // Each step's how, by the list as read; an unread list is asked to add aterm.
        let list = |listing, label| DevToolList {
            listing,
            label,
            modified_at_s: Some(1),
            rows: Some(2),
        };
        for (listing, label, step) in [
            (DevToolListing::Absent, DevToolListLabel::Read, Step::Add),
            (DevToolListing::Off, DevToolListLabel::Read, Step::SwitchOn),
            (DevToolListing::On, DevToolListLabel::Read, Step::ListedOn),
            (
                DevToolListing::Unknown,
                DevToolListLabel::NoFullDiskAccess,
                Step::Add,
            ),
        ] {
            assert_eq!(
                Step::of(&list(listing, label)),
                step,
                "{listing:?} {label:?}"
            );
        }
        assert_eq!(Step::Add.how(), HELPER_HOW);
        assert_eq!(Step::SwitchOn.how(), HELPER_SWITCH_ON);
        assert_eq!(Step::ListedOn.how(), HELPER_LISTED_ON);
        // The digest's one action per step: the drag only to add aterm.
        assert_eq!(Step::Add.action(), HELPER_STEP);
        for step in [Step::SwitchOn, Step::ListedOn] {
            assert!(
                !step.action().to_ascii_lowercase().contains("drag")
                    && step.action().contains("Developer Tools list"),
                "{step:?}: {}",
                step.action()
            );
        }
        assert!(STATUS_LINK.ends_with(STATUS_PAGE), "{STATUS_LINK}");
        assert!(PLUS_ROUTE.ends_with(PLUS_PRESS), "{PLUS_ROUTE}");
        assert!(HELPER_TITLE.contains("Developer Tools"));
        // The digest line's one action names the list (aterm-gui's host test runs the
        // message center's own rules on it).
        assert!(
            HELPER_STEP.contains("Developer Tools list"),
            "names the list"
        );
        // Terminal's switch is spoken of only when this hand-over added Terminal.
        assert!(only_aterm(true).contains("Terminal"));
        assert!(!only_aterm(false).contains("Terminal"));
        let every = [
            HELPER_TITLE,
            HELPER_HOW,
            HELPER_HOW_PLUS,
            HELPER_SWITCH_ON,
            HELPER_LISTED_ON,
            STATUS_LINK,
            HELPER_STEP,
            STEP_SWITCH_ON,
            STEP_LISTED_ON,
            WRONG_SWITCHES,
            why(false),
            why(true),
            trade(false),
            trade(true),
            done(false),
            done(true),
            only_aterm(false),
            only_aterm(true),
        ];
        // The owner's restart phrases and the design's own bans. Stems match at a
        // word's start only, so "discover" and "recover" pass.
        let phrases = [
            "restart",
            "relaunch",
            "re-launch",
            "reopen",
            "re-open",
            "reboot",
            "quit and open",
            "quit and reopen",
            "next launch",
            "open aterm again",
            "start aterm again",
            "never prompt",
            "can stay off",
        ];
        let stems = ["cover", "eliminat"];
        for words in every {
            let lower = words.to_ascii_lowercase();
            for banned in phrases {
                assert!(!lower.contains(banned), "{banned:?}: {words}");
            }
            for stem in stems {
                assert!(
                    !lower
                        .split(|c: char| !c.is_ascii_alphabetic())
                        .any(|word| word.starts_with(stem)),
                    "{stem:?}: {words}"
                );
            }
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

    /// `[privacy] observer = false` stops `aterm doctor`'s reads as it stops the window's; only
    /// that boolean in that table does, and a file the window would not load is not "off".
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
