// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The Developer Tools list (macOS): whether macOS holds each new program aterm starts for its
//! first-run check, and the words that bring the owner to aterm's switch.

use aterm_containment::consent::{
    self, DevToolGrant, DevToolList, DevToolListLabel, DevToolListing, DevToolProbe,
    DevToolProbeLabel, DevToolState, ProbeGate,
};

use crate::doctor::{Level, Row};

/// Where the switch is, in words: the consent module's one spelling.
pub const PANE: &str = consent::DEVTOOLS_SETTINGS_WORDS;

/// The pane's address: the legacy pane id, which System Settings maps to the Privacy & Security
/// extension's `Privacy_DevTools` anchor (the window's opener tries the extension's own
/// identifier first and this one second).
pub const PANE_URL: &str = consent::DEVTOOLS_SETTINGS_URL_LEGACY;

/// The command that opens the pane and changes nothing: the owner flips the switch.
pub const OPEN_PANE: &str =
    "open \"x-apple.systempreferences:com.apple.preference.security?Privacy_DevTools\"";

/// Apple's command that adds Terminal to the list, which makes the Developer Tools pane appear on
/// a Mac whose list is empty.
pub const LIST_TERMINAL: &str = "spctl developer-mode enable-terminal";

/// The helper panel's heading and the first line of `aterm doctor`'s banner.
pub const HELPER_TITLE: &str = "Add aterm to Developer Tools";

/// Why the owner is asked: what macOS does to each new program aterm starts, and what the list
/// changes. `skips` is `aterm_containment::SpikeEvidence::devtool_skips_first_run_check`; without
/// it the words say "should" (as [`trade`] and [`done`] do).
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

/// [`WRONG_SWITCHES`]'s words, spelled once for it and the hows that `concat!` them: a macro,
/// because `concat!` takes literals, not consts.
macro_rules! wrong_switches {
    () => {
        "The Processor Trace and RDMA switches on that page are different settings."
    };
}

/// The other switches on the Developer Tools page (Processor Trace, RDMA over Thunderbolt):
/// neither is aterm's, so no surface tells the owner to flip them.
pub const WRONG_SWITCHES: &str = wrong_switches!();

/// [`PLUS_PRESS`]'s words as a literal for `concat!`, spelled once for it and [`PLUS_ROUTE`].
macro_rules! plus_press {
    () => {
        "press + below it and choose aterm"
    };
}

/// [`PLUS_ROUTE`]'s words, as a literal for `concat!`.
macro_rules! plus_route {
    () => {
        concat!("or ", plus_press!())
    };
}

/// The press of + alone ("it" is the Developer Tools list), for a sentence with no drag to pair
/// it with.
pub const PLUS_PRESS: &str = plus_press!();

/// The route through + that goes with every drag aterm offers ("it" is the Developer Tools
/// list): whether the list accepts aterm dropped on it is unmeasured.
pub const PLUS_ROUTE: &str = plus_route!();

/// [`CHECK_SWITCH`]'s words, as a literal for `concat!`.
macro_rules! check_switch {
    () => {
        "then check the switch on aterm's row is on"
    };
}

/// The step after the drag or +: aterm's own switch, in one spelling for the panel, the Settings
/// card and `aterm doctor`'s did-line.
pub const CHECK_SWITCH: &str = check_switch!();

/// [`HELPER_SWITCH_ON`]'s step for an aterm the list holds switched off, as a literal for
/// `concat!`.
macro_rules! switch_on {
    () => {
        "switch on aterm's row (macOS asks for your password)"
    };
}

/// How, in the order the owner meets it: the drop or + (macOS asks for the password there), then
/// aterm's own switch, then the switches that are not it.
pub const HELPER_HOW: &str = concat!(
    "Drag aterm into the Developer Tools list (",
    plus_route!(),
    "), enter your password, ",
    check_switch!(),
    ". ",
    wrong_switches!()
);

/// How, where aterm offers no icon to drag (a translocated, disk-image or dev copy, or one not
/// yet identified): + alone, so the owner does not look for something to drag.
pub const HELPER_HOW_PLUS: &str = concat!(
    "Press + below the Developer Tools list and choose aterm, enter your password, ",
    check_switch!(),
    ". ",
    wrong_switches!()
);

/// How, for an aterm the list holds switched off: its own switch (macOS asks for the password
/// there), then the switches that are not it.
pub const HELPER_SWITCH_ON: &str = concat!(
    "aterm is already on the Developer Tools list, switched off: ",
    switch_on!(),
    ". ",
    wrong_switches!()
);

/// For an aterm the list holds switched on while tccd still checks its new programs
/// ([`consent::devtool_grant`]): nothing to add or switch on, so it names the two possible causes
/// and no remedy.
pub const HELPER_LISTED_ON: &str = "aterm is already on the Developer Tools list and switched \
                                    on: there is nothing to add. macOS still checked a new \
                                    program aterm started after that, so the row may not match \
                                    this copy of aterm, or a policy holds it.";

/// The owner's step, by what the list held of aterm when read: it picks the how every surface
/// says ([`Step::how`]) and what `aterm doctor`'s did-line asks once the page is open
/// ([`handled_words`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// aterm is not on the list as read, or the list was not read: add aterm (the drag, or +),
    /// then its switch ([`HELPER_HOW`]).
    Add,
    /// The list, read whole, holds aterm switched off: its switch, never a drag
    /// ([`HELPER_SWITCH_ON`]).
    SwitchOn,
    /// The list, read whole, holds aterm switched on while tccd still checks its new programs:
    /// nothing to add ([`HELPER_LISTED_ON`]).
    ListedOn,
}

impl Step {
    /// The step for the list as read.
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

/// [`STATUS_PAGE`]'s words, as a literal for `concat!`.
macro_rules! status_page {
    () => {
        "aterm Settings \u{25b8} Security"
    };
}

/// Where the list's state stays readable: aterm's own Settings ▸ Security, whose Developer Tools
/// card shows it.
pub const STATUS_PAGE: &str = status_page!();

/// The helper panel's footer link, and `aterm doctor`'s banner line, to [`STATUS_PAGE`].
pub const STATUS_LINK: &str = concat!("Status: ", status_page!());

/// The how as one action, for the digest line's step: at most 90 characters, with no ` or ` and
/// no `; ` (`aterm_messages::need::Step::fault` records a step of two actions as a fault instead
/// of raising it).
pub const HELPER_STEP: &str = "Drag aterm into the Developer Tools list";

/// [`Step::SwitchOn`] as one action ([`HELPER_STEP`]'s rules): its switch, never a drag.
pub const STEP_SWITCH_ON: &str = "Switch on aterm's row in the Developer Tools list";

/// [`Step::ListedOn`] as one action ([`HELPER_STEP`]'s rules), with no remedy
/// ([`HELPER_LISTED_ON`] names the causes).
pub const STEP_LISTED_ON: &str = "Look at aterm's row in the Developer Tools list";

/// Which switch matters.
#[must_use]
pub const fn only_aterm(listed_terminal_just_now: bool) -> &'static str {
    if listed_terminal_just_now {
        "aterm added Terminal, switched off, so that macOS shows this list. Leave Terminal \
         off: only aterm needs to be on."
    } else {
        "Only aterm's switch needs to be on."
    }
}

/// What the owner trades ("should" without `skips`, see [`why`]), in the same words for the
/// panel, the Settings card and `aterm doctor`.
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

/// What the panel says once aterm's switch reads on ("should" without `skips`, see [`why`]).
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
    /// The display name of the program macOS holds responsible for this process, when it is not
    /// the subject's bundle (a doctor run in Terminal, or over ssh), which aterm's switch does not
    /// reach.
    pub other_responsible: Option<String>,
    /// This user's programs macOS's program check holds now
    /// ([`aterm_uds::process::held_programs`]), read by the caller (`aterm doctor`), never by
    /// [`facts`]; `None` unread.
    pub program_check: Option<aterm_uds::process::HeldNow>,
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
            program_check: None,
        }
    }

    /// The joined verdict ([`consent::devtool_grant`]).
    #[must_use]
    pub fn grant(&self) -> DevToolGrant {
        consent::devtool_grant(&self.list, &self.log)
    }
}

/// Read the list and tccd's answers for the running bundle under the probes' own fences, or
/// `None` off macOS: a binary outside a `.app` (a test binary, `target/release/aterm`) opens and
/// spawns nothing and is `not measured`; one started through `~/.local/bin/aterm` is judged by
/// the bundled image ([`consent::fence_exe`]).
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
            program_check: None,
        });
    }
    let log = consent::probe_devtool(ProbeGate::on(), id);
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
        program_check: None,
    })
}

/// `[privacy] observer = false` in the aterm.toml the window loads
/// (`aterm_types::dirs::aterm_config_path`).
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

/// Why the switch's row was not read, in one clause, for a doctor's `not measured (…)` and the
/// window's Settings card alike.
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
    let checked = match (facts.log.label, facts.log.not_allowed) {
        (DevToolProbeLabel::Observed, Some(n)) if n > 0 => format!(
            "; macOS checked {n} new program{} run in aterm in the last {} minutes",
            if n == 1 { "" } else { "s" },
            consent::DEVTOOL_WINDOW_S / 60
        ),
        _ => String::new(),
    };
    let held = facts.program_check.filter(|held| held.count > 0);
    let now = held.map(held_words);
    let (level, text) = match facts.grant() {
        DevToolGrant::Granted => {
            let how = if facts.list.label == DevToolListLabel::Read
                && facts.list.listing == DevToolListing::On
            {
                "switched on"
            } else {
                "tccd allowed its latest new program"
            };
            let on = format!(
                "{who} is on {PANE} ({how}), so macOS should not hold the programs it starts \
                 for its first-run check{under}"
            );
            match (held, now) {
                (Some(held), Some(now)) => (
                    Level::Note,
                    format!(
                        "{on}; {now}; if aterm started {}, macOS still checks the programs \
                         aterm starts: look at aterm's row in the Developer Tools list",
                        if held.count == 1 { "it" } else { "them" }
                    ),
                ),
                _ => (Level::Ok, on),
            }
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
            let now = now.map_or_else(String::new, |now| format!("; {now}"));
            (
                Level::Warn,
                format!(
                    "{who} {is} {PANE} \u{2014} macOS holds every new program it starts for a \
                     first-run check (syspolicyd), one at a time, so builds and tests wait and \
                     timing tests flake{checked}{now} \u{2014} {fix}{under}"
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
            let text = match now {
                Some(now) => {
                    format!("{now}; whether aterm is listed is not measured ({why}){under}")
                }
                None => format!("not measured ({why}){under}"),
            };
            (Level::Note, text)
        }
    };
    Some(Row { level, text })
}

/// The `devtools:` row's words for `held` programs (at least one) of this user that macOS's
/// program check holds now. A one-shot reading has no first sighting, so the age is the oldest
/// one's fork age, an upper bound: "started … ago", never "held for".
fn held_words(held: aterm_uds::process::HeldNow) -> String {
    let age = aterm_uds::process::held_age(held.oldest_ms);
    if held.count == 1 {
        format!("now 1 program of this user waits for macOS's program check; it started {age} ago")
    } else {
        format!(
            "now {} programs of this user wait for macOS's program check, the oldest started \
             {age} ago",
            held.count
        )
    }
}

/// Whether an app's row on the Developer Tools list is switched on, as the list reads
/// ([`Listed::of`]): verify's run-start word for the app macOS holds responsible for it
/// ([`listed_for_responsible`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listed {
    /// The list, read whole, holds the app switched on.
    Yes,
    /// The list, read whole, holds the app switched off, or holds no row for it.
    No,
    /// The list was not read.
    Unknown,
}

impl Listed {
    /// The word for `list` as read.
    #[must_use]
    pub const fn of(list: &DevToolList) -> Self {
        match (list.label, list.listing) {
            (DevToolListLabel::Read, DevToolListing::On) => Self::Yes,
            (DevToolListLabel::Read, DevToolListing::Off | DevToolListing::Absent) => Self::No,
            _ => Self::Unknown,
        }
    }

    /// `yes`, `no` or `unknown`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Yes => "yes",
            Self::No => "no",
            Self::Unknown => "unknown",
        }
    }
}

/// The app macOS holds responsible for `pid` (its display name; `None` when macOS names none)
/// and whether the Developer Tools list holds that app's bundle switched on. The list is read
/// under the probe's own fence ([`consent::probe_devtool_list`]), which judges the running
/// executable: a program outside a `.app` (verify, a test binary) reads [`Listed::Unknown`].
#[must_use]
pub fn listed_for_responsible(pid: i32) -> (Option<String>, Listed) {
    let Some(app) = consent::responsible_app(pid) else {
        return (None, Listed::Unknown);
    };
    let protected = consent::protected_roots(&[]);
    let listed = consent::app_bundle_root(&app.path)
        .and_then(|root| consent::bundle_id_of(&root, &protected))
        .map_or(Listed::Unknown, |id| {
            Listed::of(&consent::probe_devtool_list(ProbeGate::on(), &id))
        });
    (Some(app.display_name), listed)
}

/// What the hand-over to the owner did ([`hand_to_owner`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handled {
    /// aterm is on the list: nothing to do.
    Granted,
    /// Nothing was done, and why: the list could not be read (aterm does not act on a state it
    /// did not measure), or this run does not act (read-only, no window session).
    Left(String),
    /// The pane is open for the owner.
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

/// The two things aterm may do toward the switch, injected so a test runs nothing.
pub struct Reach<'a> {
    /// [`LIST_TERMINAL`] ([`list_terminal`]).
    pub list_terminal: &'a dyn Fn() -> Result<(), String>,
    /// Open the pane ([`open_pane`], or the window's own opener).
    pub open_pane: &'a dyn Fn() -> Result<(), String>,
}

/// Bring the owner to the switch, only for `not_granted`: on a list read whole and empty (no
/// Developer Tools pane yet) [`LIST_TERMINAL`] runs first, then the pane opens. aterm never adds
/// itself, writes TCC.db, calls tccutil or presses anything in System Settings: the switch and
/// macOS's authentication for it stay the owner's.
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

/// The two markers in `<state>` that hold back one System Settings page aterm brings up for the
/// owner: when aterm last opened it ([`Self::last_opened`]) and the owner's Not now
/// ([`Self::declined_until`], [`Self::last_declined`]).
///
/// Each line's first word is a key ([`same_step`] compares two): the window writes a
/// [`revision_key`], so it opens a page once per approved System State setup revision;
/// `aterm doctor` writes the running version, which a revision's key never equals.
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
    /// Where aterm notes that it opened the page for the owner: one line, `<key> <unix seconds>`.
    #[must_use]
    pub fn opened_marker(self) -> Option<std::path::PathBuf> {
        aterm_types::dirs::state_dir().map(|dir| dir.join(self.opened))
    }

    /// The last time aterm opened the page: `(key, unix seconds)`, or `None`.
    #[must_use]
    pub fn last_opened(self) -> Option<(String, u64)> {
        self.last_opened_in(&aterm_types::dirs::state_dir()?)
    }

    /// [`Self::last_opened`] with `<state>` given.
    #[must_use]
    pub fn last_opened_in(self, state: &std::path::Path) -> Option<(String, u64)> {
        let text = std::fs::read_to_string(state.join(self.opened)).ok()?;
        parse_opened(&text)
    }

    /// How many seconds ago aterm opened the page, when that was under `window_s` ago.
    #[must_use]
    pub fn opened_within(self, window_s: u64) -> Option<u64> {
        self.opened_within_in(
            &aterm_types::dirs::state_dir()?,
            window_s,
            aterm_time::unix_secs()?,
        )
    }

    /// [`Self::opened_within`] with `<state>` and the clock (`now`, unix seconds) given.
    #[must_use]
    pub fn opened_within_in(self, state: &std::path::Path, window_s: u64, now: u64) -> Option<u64> {
        let (_, at) = self.last_opened_in(state)?;
        opened_ago(at, now, window_s)
    }

    /// Note that aterm opened the page now, under `version` (a [`revision_key`], or the running
    /// version).
    pub fn note_opened(self, version: &str) {
        let (Some(state), Some(now)) = (aterm_types::dirs::state_dir(), aterm_time::unix_secs())
        else {
            return;
        };
        let _ = self.note_opened_in(&state, version, now);
    }

    /// [`Self::note_opened`] with `<state>` and the clock (`now`, unix seconds) given.
    ///
    /// # Errors
    /// `<state>` cannot take the marker's new file, or the write fails; the marker is left as it
    /// was ([`write_marker`]).
    pub fn note_opened_in(
        self,
        state: &std::path::Path,
        version: &str,
        now: u64,
    ) -> std::io::Result<()> {
        let _ = aterm_types::fs_restricted::create_dir_all_unshared(state, 0o755);
        write_marker(&state.join(self.opened), &format!("{version} {now}\n"))
    }

    /// Where aterm notes that the owner said Not now to the page's helper panel, or closed it
    /// with aterm not switched on: one `<key> <unix seconds>` line per decline, newest last.
    #[must_use]
    pub fn declined_marker(self) -> Option<std::path::PathBuf> {
        aterm_types::dirs::state_dir().map(|dir| dir.join(self.declined))
    }

    /// The owner's latest decline of the page, while it holds; `None` when there is none, it has
    /// run out, or the marker cannot be read.
    #[must_use]
    pub fn declined_until(self) -> Option<Declined> {
        self.declined_until_in(&aterm_types::dirs::state_dir()?, aterm_time::unix_secs()?)
    }

    /// [`Self::declined_until`] with `<state>` and the clock (`now`, unix seconds) given.
    #[must_use]
    pub fn declined_until_in(self, state: &std::path::Path, now: u64) -> Option<Declined> {
        let text = std::fs::read_to_string(state.join(self.declined)).ok()?;
        decline_holding(&text, now)
    }

    /// The owner's newest decline whatever its age: `(key, unix seconds)`.
    #[must_use]
    pub fn last_declined(self) -> Option<(String, u64)> {
        self.last_declined_in(&aterm_types::dirs::state_dir()?)
    }

    /// [`Self::last_declined`] with `<state>` given.
    #[must_use]
    pub fn last_declined_in(self, state: &std::path::Path) -> Option<(String, u64)> {
        let text = std::fs::read_to_string(state.join(self.declined)).ok()?;
        text.lines().filter_map(parse_opened).next_back()
    }

    /// Note that the owner declined the page now, under `version` (a [`revision_key`], or the
    /// running version).
    pub fn note_declined(self, version: &str) {
        let (Some(state), Some(now)) = (aterm_types::dirs::state_dir(), aterm_time::unix_secs())
        else {
            return;
        };
        let _ = self.note_declined_in(&state, version, now);
    }

    /// [`Self::note_declined`] with `<state>` and the clock (`now`, unix seconds) given; a marker
    /// that cannot be read is replaced by this decline alone.
    ///
    /// # Errors
    /// As [`Self::note_opened_in`].
    pub fn note_declined_in(
        self,
        state: &std::path::Path,
        version: &str,
        now: u64,
    ) -> std::io::Result<()> {
        let _ = aterm_types::fs_restricted::create_dir_all_unshared(state, 0o755);
        let path = state.join(self.declined);
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        write_marker(&path, &with_decline(&text, version, now))
    }
}

/// Write a page marker whole: a new file beside it, flushed, then renamed over it. A crash, a
/// power loss or a failed write leaves the old marker or the new one, never an empty or torn one,
/// which would read as no page opened and no Not now. The flush is
/// [`aterm_durable::ordered_or_fsync`], so a volume that refuses both ordered flushes (a network
/// home whose server takes no `F_FULLFSYNC`) still takes the marker, as safe as its `fsync`.
fn write_marker(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    write_marker_with(path, text, aterm_durable::ordered_or_fsync)
}

/// [`write_marker`] with its flush given: a flush that fails leaves the marker as it was.
fn write_marker_with(
    path: &std::path::Path,
    text: &str,
    flush: impl FnOnce(&std::fs::File) -> std::io::Result<()>,
) -> std::io::Result<()> {
    use std::io::Write as _;
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("a page marker with no folder"))?;
    let staging = aterm_tempfile::Builder::new()
        .prefix(".page-marker-")
        .tempdir_in(dir)?;
    let staged = staging.path().join("marker");
    let mut file = std::fs::File::create_new(&staged)?;
    file.write_all(text.as_bytes())?;
    flush(&file)?;
    std::fs::rename(&staged, path)
}

/// The markers' key for an approved System State setup revision: `r<revision>`. A key that does
/// not start with `r` and a digit is a version, written by `aterm doctor` or an older aterm.
#[must_use]
pub fn revision_key(revision: u64) -> String {
    format!("r{revision}")
}

/// Whether two marker keys name the same step: one's `@`-separated words begin the other's. The
/// window writes `r<revision>@<bundle id>@<this Mac>` once it has read both, and fewer words
/// before that, so a key written cold (`r4`) and one written warm are one revision's; keys that
/// differ in a word both carry (another revision, a dev bundle, another Mac) are not. Pure.
#[must_use]
pub fn same_step(a: &str, b: &str) -> bool {
    a.split('@')
        .zip(b.split('@'))
        .all(|(left, right)| left == right)
}

/// Where aterm notes that it opened the Developer Tools pane for the owner
/// ([`PageMarkers::opened_marker`] of [`DEVELOPER_TOOLS`]).
#[must_use]
pub fn opened_marker() -> Option<std::path::PathBuf> {
    DEVELOPER_TOOLS.opened_marker()
}

/// How long an unattended hand-over (the window's own, `aterm ctl devtools hand-over`, or
/// `aterm doctor` with nobody at its terminal, which an agent may run in a loop) waits before it
/// opens the pane again.
pub const REOPEN_AFTER_S: u64 = 600;

/// The last time aterm opened the Developer Tools pane: `(key, unix seconds)`, or `None`.
#[must_use]
pub fn last_opened() -> Option<(String, u64)> {
    DEVELOPER_TOOLS.last_opened()
}

/// How soon a request made with a person at the screen (`aterm ctl devtools hand-over now`,
/// which `aterm doctor` sends from a terminal) may put the page in front of the owner again.
pub const REOPEN_NOW_AFTER_S: u64 = 60;

/// How many seconds ago aterm opened the pane, when that was under `window_s` ago
/// ([`REOPEN_AFTER_S`], [`REOPEN_NOW_AFTER_S`]).
#[must_use]
pub fn opened_within(window_s: u64) -> Option<u64> {
    DEVELOPER_TOOLS.opened_within(window_s)
}

/// The pure half of [`opened_within`]: seconds from `at` to `now`, when under `window_s`.
fn opened_ago(at: u64, now: u64, window_s: u64) -> Option<u64> {
    if at > now.saturating_add(STAMP_SKEW_S) {
        return None;
    }
    let ago = now.saturating_sub(at);
    (ago < window_s).then_some(ago)
}

/// One marker line, `<key> <unix seconds>`. Pure.
fn parse_opened(text: &str) -> Option<(String, u64)> {
    let mut words = text.split_whitespace();
    let (version, at) = (words.next()?, words.next()?.parse().ok()?);
    words.next().is_none().then(|| (version.to_string(), at))
}

/// Note that aterm opened the Developer Tools pane now, for `version`.
pub fn note_opened(version: &str) {
    DEVELOPER_TOOLS.note_opened(version);
}

/// Where aterm notes that the owner said Not now to the helper panel, or closed it with aterm not
/// switched on: `<state>/devtools-declined` ([`PageMarkers::declined_marker`]).
#[must_use]
pub fn declined_marker() -> Option<std::path::PathBuf> {
    DEVELOPER_TOOLS.declined_marker()
}

/// How long a first decline holds: the message center's span for the owner's own choice
/// (`aterm_messages::need::Resurface::CHOICE`).
pub const DECLINE_HOLD_S: u64 = 30 * 86_400;

/// How long a repeat decline holds: what `Resurface::CHOICE` backs off to on a second filing
/// (`Resurface::MAX`).
pub const DECLINE_REPEAT_HOLD_S: u64 = 90 * 86_400;

/// How far back an earlier decline makes a new one a repeat: the message center's `FILED_KEEP`.
const DECLINE_KEEP_S: u64 = 180 * 86_400;

/// How far ahead of the clock a marker (a decline, the page's last opening) may be stamped and
/// still count: the message center's `FILING_SKEW_MS`.
pub const STAMP_SKEW_S: u64 = 3_600;

/// Decline lines kept: the newest and the one before it, all a repeat needs.
const DECLINES_KEPT: usize = 2;

/// A decline that still holds ([`declined_until`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declined {
    /// The key the decline was written under: a [`revision_key`], or an aterm version.
    pub version: String,
    /// When, in unix seconds.
    pub at: u64,
    /// Until when it holds, in unix seconds: [`DECLINE_HOLD_S`] after `at`, or
    /// [`DECLINE_REPEAT_HOLD_S`] when an earlier decline lies within 180 days before it.
    pub until: u64,
}

/// The owner's latest decline, while it holds; `None` when there is none, it has run out, or the
/// marker cannot be read.
#[must_use]
pub fn declined_until() -> Option<Declined> {
    DEVELOPER_TOOLS.declined_until()
}

/// The pure half of [`declined_until`]: the newest valid line of `text`, and whether it
/// still holds at `now`.
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
/// newest [`DECLINES_KEPT`] kept.
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

/// Note that the owner declined the Developer Tools page now, under `version`.
pub fn note_declined(version: &str) {
    DEVELOPER_TOOLS.note_declined(version);
}

/// Wait for the switch: read the list every two seconds until it reads `on` for `subject`, it
/// cannot be read whole, or `limit` passes.
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

/// Open the pane with `/usr/bin/open` ([`PANE_URL`]).
///
/// # Errors
/// `open`'s own first line of complaint, or why it could not run.
pub fn open_pane() -> Result<(), String> {
    run_quietly("/usr/bin/open", &[PANE_URL])
}

/// The hidden verb `aterm pkg __devtools-prime` ([`run_prime`]): never advertised, answered on
/// the raw argv before the store lock, which it never takes.
pub const PRIME_VERB: &str = "__devtools-prime";

/// How long the one new program [`run_prime`] starts may take: a hang detector, never
/// a budget (a first exec macOS still checks can wait in `_dyld_start` for a minute
/// under load).
#[cfg(target_os = "macos")]
const PRIME_CEILING: std::time::Duration = std::time::Duration::from_secs(180);

/// [`PRIME_VERB`]'s command: the front door of the `.app` at `bundle`, `Contents/MacOS/aterm`.
/// Not this process's own path: a window started under its alias (`Contents/MacOS/aterm-gui`)
/// would start the verb under that name too, and the router hands a window alias's arguments to a
/// window, which refuses `pkg`.
#[cfg(any(target_os = "macos", test))]
fn prime_command(bundle: &std::path::Path) -> aterm_uds::spawnfd::Command {
    let mut command = aterm_uds::spawnfd::command(bundle.join("Contents/MacOS/aterm"));
    command.args(["pkg", PRIME_VERB]);
    command
}

/// Make macOS assess aterm again once the owner switched it on: [`run_prime`] in aterm's own
/// executable, started as its own responsible process.
///
/// # Errors
/// Why it could not run: not a bundled aterm, a macOS without the responsibility attribute
/// (`aterm_uds::spawnfd::Command::disclaim_responsibility`), or the verb's own first line of
/// complaint.
pub fn refresh_verdict() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let exe = consent::fence_exe().ok_or("aterm's own executable is unknown")?;
        let bundle = consent::app_bundle_root(&exe)
            .ok_or("not a bundled aterm, which is what macOS asks about")?;
        let out = aterm_uds::deadline::output_until(
            prime_command(&bundle).disclaim_responsibility(true),
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

/// [`PRIME_VERB`]: start one program nobody has assessed, so macOS asks about this process
/// (aterm, started as its own responsible process by [`refresh_verdict`]), and then exit.
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
        let by = std::time::Instant::now() + REACH_CEILING + PRIME_CEILING;
        let token = aterm_uds::rand::hex_token::<8>().map_err(|e| format!("no name: {e}"))?;
        let dir = std::env::temp_dir().join(format!("aterm-devtools-prime-{token}"));
        crate::prereq::private_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let remove = crate::prereq::RemoveOnDrop(&dir);
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
        let out = match aterm_uds::deadline::output_until(
            &mut aterm_uds::spawnfd::command(program),
            by,
        ) {
            // Its own exit shows it ran, so its directory may go.
            Ok(out) if out.status.code().is_some() => out,
            other => {
                // Killed, signalled or unstarted: macOS may still be assessing its first exec.
                std::mem::forget(remove);
                let why = match other {
                    Ok(out) => format!("ended by {}", out.status),
                    Err(e) => e.to_string(),
                };
                return Err(format!(
                    "{name}: {why}; kept {} since it never showed it ran",
                    dir.display()
                ));
            }
        };
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

    #[test]
    fn the_prime_runs_the_bundles_front_door_whatever_name_this_aterm_ran_under() {
        let bundle = std::path::Path::new(if cfg!(windows) {
            r"C:\Applications\aterm.app"
        } else {
            "/Applications/aterm.app"
        });
        let command = prime_command(bundle);
        let program = std::path::Path::new(command.get_program());
        assert_eq!(program, bundle.join("Contents/MacOS/aterm"));
        assert_eq!(program.file_name(), Some(std::ffi::OsStr::new("aterm")));
        let args: Vec<&std::ffi::OsStr> = command.get_args().collect();
        assert_eq!(args, ["pkg", PRIME_VERB]);
    }

    const ATERM: &str = "com.aterm.aterm";

    fn facts(list: DevToolList, log: DevToolProbe) -> Facts {
        Facts {
            subject: Some(ATERM.to_string()),
            list,
            log,
            other_responsible: None,
            program_check: None,
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

    /// The row's level per verdict: `ok` granted, `warn` not granted (with tccd's count and no
    /// command that changes the list for the owner), and `note`, never `ok`, when nothing could
    /// be read.
    #[test]
    fn the_row_says_the_state_the_cost_and_the_fix() {
        let quiet = DevToolProbe::refused(DevToolProbeLabel::NoRecentRequest);
        let on = row(&facts(read(DevToolListing::On, 100), quiet)).expect("a row");
        assert_eq!(on.level, Level::Ok);
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

    /// The row counts this user's programs macOS's program check holds now, word for word,
    /// under every verdict.
    #[test]
    fn the_devtools_row_counts_the_live_wait() {
        use aterm_uds::process::HeldNow;
        const NOW: &str = "now 2 programs of this user wait for macOS's program check, the \
                           oldest started 9 h 59 m ago";
        const LOOK: &str = "look at aterm's row in the Developer Tools list";
        let quiet = DevToolProbe::refused(DevToolProbeLabel::NoRecentRequest);
        let two = HeldNow {
            count: 2,
            oldest_ms: 35_940_000,
        };
        let one = HeldNow {
            count: 1,
            oldest_ms: 20_000,
        };
        let held = |base: &Facts, held: Option<HeldNow>| {
            row(&Facts {
                program_check: held,
                ..base.clone()
            })
            .expect("a row")
        };
        let off = facts(read(DevToolListing::Off, 100), quiet);
        let on = facts(read(DevToolListing::On, 100), quiet);
        let unread = facts(
            DevToolList::refused(DevToolListLabel::NoFullDiskAccess),
            DevToolProbe::refused(DevToolProbeLabel::LogUnavailable),
        );
        for base in [&off, &on, &unread] {
            let plain = row(base).expect("a row");
            for none in [None, Some(HeldNow::default())] {
                assert_eq!(held(base, none), plain, "nothing held, no clause");
            }
        }

        let plain = row(&off).expect("a row");
        let warn = held(&off, Some(two));
        assert_eq!(warn.level, Level::Warn);
        assert_eq!(
            warn.text,
            plain.text.replacen(
                "timing tests flake \u{2014}",
                &format!("timing tests flake; {NOW} \u{2014}"),
                1
            ),
            "the count after tccd's"
        );

        let plain = row(&on).expect("a row");
        assert_eq!(plain.level, Level::Ok);
        let listed = held(&on, Some(two));
        assert_eq!(listed.level, Level::Note, "{}", listed.text);
        assert_eq!(
            listed.text,
            format!(
                "{}; {NOW}; if aterm started them, macOS still checks the programs aterm \
                 starts: {LOOK}",
                plain.text
            )
        );
        assert_eq!(
            held(&on, Some(one)).text,
            format!(
                "{}; now 1 program of this user waits for macOS's program check; it started \
                 20 s ago; if aterm started it, macOS still checks the programs aterm starts: \
                 {LOOK}",
                plain.text
            )
        );

        let plain = row(&unread).expect("a row");
        let why = plain
            .text
            .strip_prefix("not measured (")
            .and_then(|t| t.strip_suffix(')'))
            .expect("the unmeasured words");
        let unknown = held(&unread, Some(two));
        assert_eq!(unknown.level, Level::Note);
        assert_eq!(
            unknown.text,
            format!("{NOW}; whether aterm is listed is not measured ({why})")
        );
    }

    /// Verify's word for an app's row: on is yes; off and no row are no; a list not read whole
    /// is unknown.
    #[test]
    fn a_listing_reads_yes_only_for_a_row_switched_on() {
        for (list, want) in [
            (read(DevToolListing::On, 1), Listed::Yes),
            (read(DevToolListing::Off, 1), Listed::No),
            (read(DevToolListing::Absent, 0), Listed::No),
            (read(DevToolListing::Unknown, 0), Listed::Unknown),
            (
                DevToolList::refused(DevToolListLabel::NoFullDiskAccess),
                Listed::Unknown,
            ),
            (
                DevToolList::refused(DevToolListLabel::RefusedOutOfBundle),
                Listed::Unknown,
            ),
            (DevToolList::pending(), Listed::Unknown),
        ] {
            assert_eq!(Listed::of(&list), want, "{list:?}");
        }
        assert_eq!(
            [Listed::Yes, Listed::No, Listed::Unknown].map(Listed::word),
            ["yes", "no", "unknown"]
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
        let handled = hand_to_owner(&facts(empty, quiet), &reach);
        assert_eq!(
            handled,
            Handled::Opened {
                listed_terminal: Some(Ok(())),
                step: Step::Add,
            }
        );
        assert_eq!(calls.take(), vec!["spctl", "open"]);
        let words = handled_words(&handled);
        assert!(
            words.starts_with("ran `spctl developer-mode enable-terminal` so the pane appears")
                && words.contains("leave it off"),
            "{words}"
        );
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

    /// Each page keeps its own pair of markers, and the Developer Tools files keep their names.
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

    /// A decline holds 30 days, or 90 after an earlier one within 180 days, never for good; a
    /// stamp from a clock since corrected keeps nothing quiet, and an unparsable line is skipped.
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

        let again = at + 40 * DAY;
        let twice = with_decline(&once, "0.105.0", again);
        assert_eq!(twice.lines().count(), 2);
        let held = decline_holding(&twice, again + 89 * DAY).expect("a repeat holds longer");
        assert_eq!(
            (held.version.as_str(), held.until),
            ("0.105.0", again + 90 * DAY)
        );
        assert_eq!(decline_holding(&twice, again + 90 * DAY), None);

        let late = at + 200 * DAY;
        let lapsed = with_decline(&once, "0.110.0", late);
        assert_eq!(
            decline_holding(&lapsed, late).map(|held| held.until),
            Some(late + 30 * DAY)
        );

        let kept = with_decline(&format!("junk\n{twice}"), "0.106.0", again + DAY);
        assert_eq!(kept, format!("0.105.0 {again}\n0.106.0 {}\n", again + DAY));
        assert_eq!(decline_holding("junk\n0.104.0\n", at), None);
        assert_eq!(decline_holding("", at), None);

        assert_eq!(decline_holding(&once, at - 2 * 3_600), None);
        assert!(decline_holding(&once, at - 600).is_some());
    }

    /// A key written cold and one written warm for the same revision are one step; another
    /// revision, bundle or Mac, or a version, is not. The newest decline is read whatever its
    /// age, so a Not now holds for the rest of its revision.
    #[test]
    fn a_key_matches_its_own_step_warm_or_cold_and_the_newest_decline_is_read() {
        assert_eq!(revision_key(7), "r7");
        let warm = "r4@com.aterm.aterm@UUID";
        for (a, b, want) in [
            ("r4", "r4", true),
            ("r4", warm, true),
            (warm, "r4", true),
            ("r4@com.aterm.aterm", warm, true),
            (warm, warm, true),
            ("r40", "r4", false),
            ("r3", warm, false),
            ("r4@com.aterm.aterm.dev@UUID", warm, false),
            ("r4@com.aterm.aterm@OTHER", warm, false),
            ("0.110.0", "r4", false),
            ("", "r4", false),
        ] {
            assert_eq!(same_step(a, b), want, "{a} vs {b}");
        }
        let state = aterm_tempfile::tempdir().expect("tempdir");
        assert_eq!(FULL_DISK_ACCESS.last_declined_in(state.path()), None);
        let once = with_decline("", "0.110.0", 1_000);
        let twice = with_decline(&once, &revision_key(4), 5_000_000_000);
        std::fs::write(state.path().join(FULL_DISK_ACCESS.declined), twice).expect("write");
        assert_eq!(
            FULL_DISK_ACCESS.last_declined_in(state.path()),
            Some(("r4".to_string(), 5_000_000_000))
        );
        assert_eq!(DEVELOPER_TOOLS.last_declined_in(state.path()), None);
    }

    /// A page marker is replaced whole: each note reads back as the newest and nothing is left
    /// beside it, and a write that cannot finish (into a folder that takes no new file, or one
    /// whose flush fails) leaves the marker as it was, where a write in place would have
    /// replaced it.
    #[cfg(unix)]
    #[test]
    fn a_page_marker_is_replaced_whole_or_left_as_it_was() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let state = dir.path().join("state");
        FULL_DISK_ACCESS
            .note_opened_in(&state, "r4", 1_000)
            .expect("noted");
        FULL_DISK_ACCESS
            .note_opened_in(&state, "r5", 2_000)
            .expect("noted again");
        assert_eq!(
            FULL_DISK_ACCESS.last_opened_in(&state),
            Some(("r5".to_owned(), 2_000))
        );
        DEVELOPER_TOOLS
            .note_declined_in(&state, "r4", 1_000)
            .expect("declined");
        DEVELOPER_TOOLS
            .note_declined_in(&state, "r5", 2_000)
            .expect("declined again");
        assert_eq!(
            DEVELOPER_TOOLS.last_declined_in(&state),
            Some(("r5".to_owned(), 2_000))
        );
        let mut names: Vec<_> = std::fs::read_dir(&state)
            .expect("the state folder")
            .map(|entry| entry.expect("an entry").file_name())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["devtools-declined", "fda-pane-opened"],
            "nothing staged is left"
        );
        let text = |name: &str| std::fs::read_to_string(state.join(name)).expect("a marker");
        let before = (
            text(FULL_DISK_ACCESS.opened),
            text(DEVELOPER_TOOLS.declined),
        );
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o500))
            .expect("a folder that takes no new file");
        let refused = (
            FULL_DISK_ACCESS.note_opened_in(&state, "r6", 3_000),
            DEVELOPER_TOOLS.note_declined_in(&state, "r6", 3_000),
        );
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))
            .expect("the folder writable again");
        assert!(refused.0.is_err() && refused.1.is_err(), "{refused:?}");
        assert_eq!(
            (
                text(FULL_DISK_ACCESS.opened),
                text(DEVELOPER_TOOLS.declined)
            ),
            before
        );
        let path = state.join(FULL_DISK_ACCESS.opened);
        let failed = write_marker_with(&path, "r7 4000\n", |_| {
            Err(std::io::Error::from_raw_os_error(5))
        });
        assert_eq!(failed.map_err(|e| e.raw_os_error()), Err(Some(5)));
        assert_eq!(text(FULL_DISK_ACCESS.opened), before.0, "a failed flush");
    }

    /// The words every surface shares: the drag is never offered without the + route;
    /// "should" is said exactly without `skips`; each step reads the list as it stands; the
    /// wrong switches close the how.
    #[test]
    fn the_words_pair_the_drag_with_plus_and_say_should_until_d14() {
        for skips in [false, true] {
            for words in [why(skips), trade(skips), done(skips)] {
                assert_eq!(words.contains("should"), !skips, "skips={skips}: {words}");
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
        assert!(
            HELPER_HOW_PLUS.starts_with("Press + below the Developer Tools list")
                && HELPER_HOW_PLUS.contains(CHECK_SWITCH)
                && HELPER_HOW_PLUS.ends_with(WRONG_SWITCHES)
                && !HELPER_HOW_PLUS.to_ascii_lowercase().contains("drag"),
            "{HELPER_HOW_PLUS}"
        );
        assert!(
            HELPER_SWITCH_ON.ends_with(WRONG_SWITCHES) && !HELPER_SWITCH_ON.contains("Drag"),
            "{HELPER_SWITCH_ON}"
        );
        assert!(
            !HELPER_LISTED_ON.to_ascii_lowercase().contains("drag")
                && !HELPER_LISTED_ON.contains("switch on"),
            "{HELPER_LISTED_ON}"
        );
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
        assert!(
            HELPER_STEP.contains("Developer Tools list"),
            "names the list"
        );
        assert!(only_aterm(true).contains("Terminal"));
        assert!(!only_aterm(false).contains("Terminal"));
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
