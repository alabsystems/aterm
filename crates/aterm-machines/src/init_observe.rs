// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! System State's reading of one setting from the shared engine's rows
//! (`docs/DESIGN-system-state-2026-10-21.md` §3.3, §5): one standing per row, and a current
//! value only in the page's exact words. A value the rows do not establish stays `None` (the
//! page's `—`).
//!
//! The words come from a typed [`Reading`] the item took in its own probe and hands over as its
//! row's `detail`, not from the human `from` line `aterm doctor` and `aterm init` print. The
//! exceptions are two settings whose shared items take no reading: Caps Lock reads the keyboard
//! items' rendered mapping and Trackpad scrolling the defaults items' rendered value. Caps Lock
//! also reads, from its items' outcomes and effects, a live mapping that lasts until the next
//! login ([`until_login`]).

use crate::model::{Effect, ItemReport, Outcome};
use aterm_types::macos_setup::{FailureKind, Observation, Setting, Standing, Subject};
use std::sync::{Mutex, PoisonError};

/// What an init item's probe read, in System State's terms (design §3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reading {
    /// No attached keyboard service maps Caps Lock: the key still does Caps Lock.
    CapsLock,
    /// Natural scrolling is on in System Settings.
    NaturalOn,
    /// The wanted value is live now, while the saved preference macOS loads at the next login
    /// differs: the row reads the live value until then ([`until_login`]).
    UntilLogin,
    /// Desktop icons are hidden.
    IconsHidden,
    /// A click on the wallpaper reveals the desktop.
    ClickReveals,
    /// Widgets are shown on the desktop.
    WidgetsShown,
    /// The Dock pins `pinned` apps, where the profile pins `wanted`.
    Apps {
        /// Apps the Dock pins now.
        pinned: usize,
        /// Apps the profile pins.
        wanted: usize,
    },
    /// The account is still registered (a disabled one included).
    SignedIn,
    /// A rustup toolchain carries rust-docs.
    DocsInstalled,
    /// aterm's Rust build or tooling cache is missing, which the item makes; the counts are those
    /// of [`Self::KeptOut`].
    BuildsIndexed {
        /// The person's project build folders Spotlight indexes.
        projects: usize,
        /// The Trust toolchain aterm is using is still where Spotlight indexes it.
        toolchain: bool,
    },
    /// aterm's caches are kept out of Spotlight, with what the row only notes: `projects`
    /// of the person's own cargo build folders Spotlight indexes, theirs and never aterm's
    /// to rename ([`projects_note`]), and whether the Trust toolchain aterm is using (its
    /// build or the rustup view laid from it) is still where Spotlight indexes it, which
    /// atpkg moves out at its next update ([`TOOLCHAIN_NOTE`]). Neither is a step or a
    /// change.
    KeptOut {
        /// The person's project build folders Spotlight indexes.
        projects: usize,
        /// The Trust toolchain aterm is using is still where Spotlight indexes it.
        toolchain: bool,
    },
    /// `present` of the `count` repositories are cloned; `signed_in` is whether the
    /// native GitHub credentials verified.
    Repositories {
        /// Repositories cloned.
        present: usize,
        /// Repositories the plan names.
        count: usize,
        /// The native GitHub credentials verified.
        signed_in: bool,
    },
    /// The probe could not go on (its row is blocked), for this reason.
    Waits(Wait),
}

/// Why a blocked part cannot go on, as its item decided it in its probe: the reason a
/// waiting row's note gives (design §3.3), never the engine's line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    /// Command Line Tools git, which every clone runs, is not installed.
    CommandLineTools,
    /// aterm's GitHub settings (`~/.config/aterm/init.toml`) cannot be used.
    Settings,
    /// The GitHub token file the vault syncs is not on this Mac yet.
    Token,
    /// The GitHub token file is there but cannot be used: not the person's own private
    /// file, or not one literal token.
    TokenUnusable,
    /// GitHub reached no answer: the network, a proxy or a deadline.
    Unreached,
    /// GitHub does not take the token as a sign-in.
    TokenRefused,
    /// The token signs in as another account than the one the settings name.
    OtherAccount,
    /// GitHub's answer about the repositories could not be read or used.
    GitHub,
    /// Something already in the home folder sits where a repository goes.
    InTheWay,
}

impl Wait {
    /// Every reason, for the round trip.
    pub const ALL: [Self; 9] = [
        Self::CommandLineTools,
        Self::Settings,
        Self::Token,
        Self::TokenUnusable,
        Self::Unreached,
        Self::TokenRefused,
        Self::OtherAccount,
        Self::GitHub,
        Self::InTheWay,
    ];

    fn slug(self) -> &'static str {
        match self {
            Self::CommandLineTools => "command-line-tools",
            Self::Settings => "settings",
            Self::Token => "token",
            Self::TokenUnusable => "token-unusable",
            Self::Unreached => "unreached",
            Self::TokenRefused => "token-refused",
            Self::OtherAccount => "other-account",
            Self::GitHub => "github",
            Self::InTheWay => "in-the-way",
        }
    }

    /// What ends this wait (design §3.6): GitHub answering again, by itself
    /// ([`FailureKind::Transient`]), or a person's act for every other reason, which no pass
    /// retries: the token the vault syncs is refused, missing, unusable or another account's,
    /// aterm's GitHub settings cannot be used, a folder is in a repository's place, or Apple's
    /// Command Line Tools are not installed ([`FailureKind::NeedsPerson`]).
    #[must_use]
    pub const fn kind(self) -> FailureKind {
        match self {
            Self::Unreached | Self::GitHub => FailureKind::Transient,
            Self::CommandLineTools
            | Self::Settings
            | Self::Token
            | Self::TokenUnusable
            | Self::TokenRefused
            | Self::OtherAccount
            | Self::InTheWay => FailureKind::NeedsPerson,
        }
    }

    /// What the row's note says (design §3.3): one short line in a person's words, with no
    /// item id, file mode or command. A wait that ends by itself says what it waits on; one
    /// only a person ends ([`Self::kind`]) says their remedy.
    #[must_use]
    pub const fn note(self) -> &'static str {
        match self {
            Self::CommandLineTools => "Install Apple's Command Line Tools",
            Self::Settings => "Fix aterm's GitHub settings; aterm doctor says how",
            Self::Token => "Add your GitHub token to the Secrets vault",
            Self::TokenUnusable => "Fix your GitHub token file; aterm doctor says how",
            Self::Unreached => "GitHub can't be reached",
            Self::TokenRefused => "GitHub refuses your token: renew it in the vault",
            Self::OtherAccount => "Your GitHub token is for another account; replace it",
            Self::GitHub => "Can't read your repositories from GitHub",
            Self::InTheWay => "A folder blocks a repository; aterm doctor names it",
        }
    }

    /// Whether the wait is for a GitHub sign-in that works: the row reads `Needs sign-in`.
    const fn signs_in(self) -> bool {
        matches!(self, Self::Token | Self::TokenRefused | Self::OtherAccount)
    }
}

/// The prefix of a reading carried in a row's `detail`.
const TOKEN: &str = "reading:";

impl Reading {
    /// The reading as its row carries it (`detail`): a token, not prose.
    #[must_use]
    pub fn token(self) -> String {
        let body = match self {
            Self::CapsLock => "caps-lock".to_owned(),
            Self::NaturalOn => "natural-on".to_owned(),
            Self::UntilLogin => "until-login".to_owned(),
            Self::IconsHidden => "icons-hidden".to_owned(),
            Self::ClickReveals => "click-reveals".to_owned(),
            Self::WidgetsShown => "widgets-shown".to_owned(),
            Self::Apps { pinned, wanted } => format!("apps:{pinned}:{wanted}"),
            Self::SignedIn => "signed-in".to_owned(),
            Self::DocsInstalled => "docs-installed".to_owned(),
            Self::BuildsIndexed {
                projects,
                toolchain,
            } => format!("builds-indexed:{projects}:{}", u8::from(toolchain)),
            Self::KeptOut {
                projects,
                toolchain,
            } => format!("kept-out:{projects}:{}", u8::from(toolchain)),
            Self::Repositories {
                present,
                count,
                signed_in,
            } => format!("repositories:{present}:{count}:{}", u8::from(signed_in)),
            Self::Waits(wait) => format!("waits:{}", wait.slug()),
        };
        format!("{TOKEN}{body}")
    }

    /// The reading `row` carries, if its item took one.
    #[must_use]
    pub fn of(row: &ItemReport) -> Option<Self> {
        let body = row.detail.as_deref()?.strip_prefix(TOKEN)?;
        let mut parts = body.split(':');
        let kind = parts.next()?;
        let mut number = || parts.next()?.parse::<usize>().ok();
        let reading = match kind {
            "caps-lock" => Self::CapsLock,
            "natural-on" => Self::NaturalOn,
            "until-login" => Self::UntilLogin,
            "icons-hidden" => Self::IconsHidden,
            "click-reveals" => Self::ClickReveals,
            "widgets-shown" => Self::WidgetsShown,
            "apps" => Self::Apps {
                pinned: number()?,
                wanted: number()?,
            },
            "signed-in" => Self::SignedIn,
            "docs-installed" => Self::DocsInstalled,
            "builds-indexed" => Self::BuildsIndexed {
                projects: number()?,
                toolchain: number()? == 1,
            },
            "kept-out" => Self::KeptOut {
                projects: number()?,
                toolchain: number()? == 1,
            },
            "repositories" => Self::Repositories {
                present: number()?,
                count: number()?,
                signed_in: number()? == 1,
            },
            "waits" => {
                let slug = parts.next()?;
                Self::Waits(Wait::ALL.into_iter().find(|wait| wait.slug() == slug)?)
            }
            _ => return None,
        };
        parts.next().is_none().then_some(reading)
    }

    /// What the Rust in Spotlight row only notes, where the reading counted it: the
    /// person's project build folders, and the toolchain build aterm is using, Spotlight
    /// indexes.
    const fn noted(self) -> Option<(usize, bool)> {
        match self {
            Self::BuildsIndexed {
                projects,
                toolchain,
            }
            | Self::KeptOut {
                projects,
                toolchain,
            } => Some((projects, toolchain)),
            _ => None,
        }
    }

    /// The page's word for this reading (design §3.3), or `None` where it names no
    /// current value: every repository present and signed in is the wanted value.
    #[must_use]
    pub fn word(self) -> Option<String> {
        Some(match self {
            Self::CapsLock => "Caps Lock".to_owned(),
            Self::NaturalOn => "On".to_owned(),
            Self::IconsHidden => "Icons hidden".to_owned(),
            Self::ClickReveals => "Click reveals".to_owned(),
            Self::WidgetsShown => "Shown".to_owned(),
            // As many pins as the profile wants, but not its apps: a bare count would
            // read the same as the wanted value.
            Self::Apps { pinned, wanted } if pinned == wanted => "Other apps".to_owned(),
            Self::Apps { pinned: 1, .. } => "1 app".to_owned(),
            Self::Apps { pinned, .. } => format!("{pinned} apps"),
            Self::SignedIn => "Signed in".to_owned(),
            Self::DocsInstalled => "Docs indexed".to_owned(),
            Self::BuildsIndexed { .. } => "Builds indexed".to_owned(),
            Self::Repositories { present, count, .. } if present < count => {
                format!("{present} of {count}")
            }
            Self::Repositories {
                signed_in: false, ..
            } => "Needs sign-in".to_owned(),
            // A sign-in only a person gives: the token is missing, refused or another
            // account's.
            Self::Waits(wait) if wait.signs_in() => "Needs sign-in".to_owned(),
            // A probe that could not go on read no value, and the person's own
            // project folders are no value of the row's. A value live until the next login
            // is the row's wanted one, which its observation marks ([`until_login`]).
            Self::Repositories { .. }
            | Self::Waits(_)
            | Self::KeptOut { .. }
            | Self::UntilLogin => {
                return None;
            }
        })
    }
}

/// The reading an item's last probe took, which its row's `detail` carries after
/// the run: the read-back's after an apply, the plan's otherwise.
#[derive(Debug, Default)]
pub(crate) struct LastReading(Mutex<Option<Reading>>);

impl LastReading {
    /// Record what this probe read (`None` when it read no word).
    pub(crate) fn set(&self, reading: Option<Reading>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = reading;
    }

    /// The row's `detail`: the last reading's token.
    pub(crate) fn detail(&self) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .map(Reading::token)
    }
}

/// How one engine row stands (design §5). A blocked row waits, with its reason:
/// the console, the network, the vault, or a grant that another row (Accessibility
/// for the sign-outs) asks a person for, so this row never asks for one itself.
/// A row fails only when a change was attempted and did not land (`Failed`).
#[must_use]
pub fn standing(row: &ItemReport) -> Standing {
    match row.outcome {
        Outcome::Ok => Standing::InPlace,
        Outcome::Planned => Standing::Differs,
        Outcome::Changed => match row.effect {
            Effect::Login | Effect::Restart => Standing::AtLogin,
            Effect::Now | Effect::Relaunch => Standing::InPlace,
        },
        Outcome::Person => Standing::NeedsYou,
        Outcome::Blocked | Outcome::Deferred => Standing::Waiting,
        Outcome::NotApplicable => Standing::NotNeeded,
        Outcome::Unknown => Standing::Unknown,
        Outcome::Failed => Standing::Failed,
    }
}

/// A standing's severity: a row made of several items takes its worst part's.
const fn rank(standing: Standing) -> u8 {
    match standing {
        Standing::NotNeeded => 0,
        Standing::InPlace | Standing::Untold => 1,
        Standing::AtLogin => 2,
        Standing::Unknown => 3,
        Standing::Differs => 4,
        Standing::Waiting => 5,
        Standing::NeedsYou => 6,
        Standing::Failed => 7,
    }
}

/// One setting's row from the rows its items reported; no rows is an unknown row. Its
/// description (`detail`) is its worst parts' reasons (`why`), and a row that differs has none
/// (§3.3). A waiting row's note is the [`Reading::Waits`] reason of the first waiting part that
/// gives one, and a row that needs a person the remedy of the first such part that gives one
/// ([`Wait::note`]), never the engine's line.
#[must_use]
pub fn observation(setting: Setting, rows: &[&ItemReport]) -> Observation {
    let subject = Subject::Setting(setting);
    let Some(standing) = rows
        .iter()
        .map(|row| standing(row))
        .max_by_key(|s| rank(*s))
    else {
        return Observation::unknown(subject);
    };
    let wanted = wanted(setting, rows);
    let marked = standing == Standing::Differs && until_login(setting, rows);
    let now = match standing {
        Standing::InPlace | Standing::AtLogin => Some(wanted.clone()),
        Standing::NotNeeded | Standing::Unknown | Standing::Untold => None,
        Standing::Differs if marked => Some(aterm_types::macos_setup::until_login(&wanted)),
        Standing::Differs | Standing::Waiting | Standing::NeedsYou | Standing::Failed => {
            now(setting, rows)
        }
    };
    // A value live until the next login: what the Mac reads after it, which a row that will
    // change shows as its from-side (design §3.3).
    let after_login = if marked {
        after_login(setting, rows)
    } else {
        None
    };
    // A row that differs has no reasons to give: the planned parts' lines are the engine's.
    let detail = if standing == Standing::Differs {
        Vec::new()
    } else {
        rows.iter()
            .filter(|row| self::standing(row) == standing)
            .filter_map(|row| row.why.clone())
            .collect::<Vec<_>>()
    };
    let noted = rows.iter().find_map(|row| {
        Reading::of(row)
            .and_then(Reading::noted)
            .filter(|(projects, toolchain)| *projects > 0 || *toolchain)
            .map(|counts| (counts, *row))
    });
    let note = match standing {
        // What a waiting row waits on, or the remedy of a row only a person moves, as the
        // item decided it ([`Wait::kind`]).
        Standing::Waiting | Standing::NeedsYou => rows
            .iter()
            .filter(|row| self::standing(row) == standing)
            .find_map(|row| match Reading::of(row) {
                Some(Reading::Waits(wait)) => Some(wait.note().to_owned()),
                _ => None,
            }),
        // The toolchain build aterm is using, which atpkg moves at its next update, and
        // the person's own project build folders, Spotlight indexes: a quiet note, never
        // a step of the row's, where nothing else is said under it.
        Standing::InPlace | Standing::AtLogin | Standing::Differs => {
            noted.map(|((projects, toolchain), _)| {
                [
                    toolchain.then(|| TOOLCHAIN_NOTE.to_owned()),
                    (projects > 0).then(|| projects_note(projects)),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("; ")
            })
        }
        _ => None,
    };
    // In place, the folders themselves stand behind the note: the line of the item
    // that counted them, which names each.
    let detail = match noted {
        Some((_, row))
            if detail.is_empty() && matches!(standing, Standing::InPlace | Standing::AtLogin) =>
        {
            row.to.iter().cloned().collect()
        }
        _ => detail,
    };
    Observation {
        subject,
        standing,
        now,
        after_login,
        wanted,
        note,
        detail: (!detail.is_empty()).then(|| detail.join("; ")),
    }
}

/// The Rust in Spotlight row's note for `count` of the person's project build folders Spotlight
/// indexes: how to keep one out, asked of no one. aterm never renames them: a program started
/// from one may still be under macOS's first-exec check.
#[must_use]
pub fn projects_note(count: usize) -> String {
    let each = if count == 1 { "it" } else { "each" };
    format!(
        "Spotlight indexes {count} of your project build folders; renaming {each} to its .noindex name and linking the old name to it keeps it out"
    )
}

/// The Rust in Spotlight row's note for the Trust toolchain aterm is using while Spotlight
/// still indexes it, its build or the rustup view laid from it: atpkg never moves either
/// while it is in use, and lays them aside at its next update, so the note asks nothing of
/// the person. A build no longer in use atpkg moves by itself at its next pass, and the
/// row says nothing of it.
pub const TOOLCHAIN_NOTE: &str =
    "aterm moves the Rust toolchain it is using out of Spotlight at its next update";

/// The wanted value in the row's words: [`Setting::wanted`], with the count the
/// repositories' reading names.
fn wanted(setting: Setting, rows: &[&ItemReport]) -> String {
    if setting == Setting::Repositories
        && let Some(count) = rows.iter().find_map(|row| match Reading::of(row) {
            Some(Reading::Repositories { count, .. }) => Some(count),
            _ => None,
        })
    {
        return format!("All {count}");
    }
    setting.wanted().to_owned()
}

/// Whether a row that differs reads its wanted value live, while the saved preference macOS
/// loads at the next login differs (design §3.3): the row then stands as the Mac will after
/// that login, and its value is the live one, marked "until login". Its item says so
/// ([`Reading::UntilLogin`]), or, for the Caps Lock key, whose saved mappings and live
/// mappings are separate items, every part that differs is a saved mapping (an item whose
/// change takes effect at login) while every live part (one that takes effect now) is in place.
fn until_login(setting: Setting, rows: &[&ItemReport]) -> bool {
    let differing: Vec<&ItemReport> = rows
        .iter()
        .copied()
        .filter(|row| standing(row) == Standing::Differs)
        .collect();
    if differing.is_empty() {
        return false;
    }
    if differing
        .iter()
        .all(|row| Reading::of(row) == Some(Reading::UntilLogin))
    {
        return true;
    }
    differing.iter().all(|row| row.effect == Effect::Login) && live_in_place(setting, rows)
}

/// Whether the part of `setting` that takes effect now reads in place: the Caps Lock key's
/// live mappings, which its items read apart from the saved ones macOS loads at the next login
/// ([`until_login`]). A run that set both left nothing for that login, so its row is in place
/// now, never "at login"; a setting with no live part of its own (Trackpad scrolling's keys are
/// all saved preferences) has none in place.
#[must_use]
pub fn live_in_place(setting: Setting, rows: &[&ItemReport]) -> bool {
    let live: Vec<&ItemReport> = rows
        .iter()
        .copied()
        .filter(|row| row.effect == Effect::Now)
        .collect();
    setting == Setting::CapsLock
        && !live.is_empty()
        && live.iter().all(|row| row.outcome == Outcome::Ok)
}

/// What a row whose wanted value is live only until the next login reads after it, in its
/// words: the saved part that differs. Natural scrolling's one item reads both, and says it
/// is off now while the saved direction is not ([`Reading::UntilLogin`]), so macOS scrolls
/// naturally again after the login.
fn after_login(setting: Setting, rows: &[&ItemReport]) -> Option<String> {
    if rows
        .iter()
        .any(|row| Reading::of(row) == Some(Reading::UntilLogin))
    {
        return (setting == Setting::NaturalScrolling).then(|| "On".to_owned());
    }
    now(setting, rows)
}

/// The current value of a row that differs, in its exact words, when every part of
/// it that differs agrees on them (or, where the word says "some", when one does).
fn now(setting: Setting, rows: &[&ItemReport]) -> Option<String> {
    let differing: Vec<&ItemReport> = rows
        .iter()
        .copied()
        .filter(|row| {
            !matches!(
                standing(row),
                Standing::InPlace | Standing::AtLogin | Standing::NotNeeded
            )
        })
        .collect();
    if differing.is_empty() {
        return None;
    }
    let word = |row: &ItemReport| match setting {
        // The keyboard items' rendered mapping, or the attached-keyboard reading.
        Setting::CapsLock => match Reading::of(row) {
            Some(reading) => reading.word(),
            None => row.from.as_deref().and_then(caps_lock_word),
        },
        // A defaults item's rendered value: one gesture off.
        Setting::TrackpadScrolling => {
            (row.from.as_deref() == Some("false")).then(|| "Some off".to_owned())
        }
        _ => Reading::of(row).and_then(Reading::word),
    };
    match setting {
        Setting::TrackpadScrolling | Setting::Spotlight => {
            differing.iter().find_map(|row| word(row))
        }
        Setting::Permissions | Setting::Secrets => None,
        _ => {
            let first = word(differing.first()?)?;
            differing
                .iter()
                .all(|row| word(row).as_deref() == Some(first.as_str()))
                .then_some(first)
        }
    }
}

/// Caps Lock still does Caps Lock in a keyboard item's rendered mapping: no remap
/// at all, or remaps of other keys only. Each seen model must say it.
fn caps_lock_word(from: &str) -> Option<String> {
    let unmapped = |described: &str| {
        described == "no remap"
            || described.split(", ").all(|pair| {
                pair.split_once('→')
                    .is_some_and(|(source, _)| source != "Caps Lock")
            })
    };
    let caps = Some("Caps Lock".to_owned());
    if unmapped(from) {
        return caps;
    }
    // The seen-models reading: `<model>: <mapping>; <model>: <mapping>`.
    let models: Vec<&str> = from.split("; ").collect();
    models
        .iter()
        .all(|model| {
            model
                .split_once(": ")
                .is_some_and(|(_, described)| unmapped(described))
        })
        .then_some(())
        .and(caps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_converge::StoodAside;

    fn row(outcome: Outcome, from: Option<&str>, to: Option<&str>) -> ItemReport {
        ItemReport {
            id: "item".into(),
            family: "test".into(),
            title: "item".into(),
            outcome,
            from: from.map(str::to_owned),
            to: to.map(str::to_owned),
            why: None,
            revert: None,
            effect: Effect::Now,
            detail: None,
            layer: None,
            class: None,
            explain: None,
            stood_aside: None,
            essential: false,
        }
    }

    fn planned(from: &str) -> ItemReport {
        row(Outcome::Planned, Some(from), Some("wanted"))
    }

    /// A planned row whose item took `reading`, under the engine's own `from` line.
    fn read_as(reading: Reading) -> ItemReport {
        ItemReport {
            detail: Some(reading.token()),
            ..planned("the engine's line, which System State does not parse")
        }
    }

    #[test]
    fn every_reading_round_trips_through_its_rows_detail() {
        let readings = [
            Reading::CapsLock,
            Reading::NaturalOn,
            Reading::UntilLogin,
            Reading::IconsHidden,
            Reading::ClickReveals,
            Reading::WidgetsShown,
            Reading::Apps {
                pinned: 7,
                wanted: 3,
            },
            Reading::SignedIn,
            Reading::DocsInstalled,
            Reading::BuildsIndexed {
                projects: 0,
                toolchain: false,
            },
            Reading::BuildsIndexed {
                projects: 3,
                toolchain: true,
            },
            Reading::KeptOut {
                projects: 1,
                toolchain: false,
            },
            Reading::KeptOut {
                projects: 0,
                toolchain: true,
            },
            Reading::Repositories {
                present: 2,
                count: 5,
                signed_in: false,
            },
            Reading::Repositories {
                present: 5,
                count: 5,
                signed_in: true,
            },
        ];
        for reading in readings
            .into_iter()
            .chain(Wait::ALL.into_iter().map(Reading::Waits))
        {
            assert_eq!(Reading::of(&read_as(reading)), Some(reading));
        }
        // Another item's detail (a public key line, a version) is no reading.
        for other in [
            "ssh-ed25519 AAAA",
            "26A428",
            "reading:apps:7",
            "reading:x",
            "reading:waits:nothing",
            "reading:waits:token:1",
        ] {
            let mut row = planned("x");
            row.detail = Some(other.into());
            assert_eq!(Reading::of(&row), None, "{other}");
        }
    }

    #[test]
    fn every_engine_result_maps_to_the_design_standing() {
        let mut changed = row(Outcome::Changed, None, Some("x"));
        assert_eq!(standing(&changed), Standing::InPlace);
        changed.effect = Effect::Login;
        assert_eq!(standing(&changed), Standing::AtLogin);
        changed.effect = Effect::Restart;
        assert_eq!(standing(&changed), Standing::AtLogin);
        let mut blocked = row(Outcome::Blocked, None, None);
        assert_eq!(standing(&blocked), Standing::Waiting);
        // A sign-out blocked on Accessibility waits: the Accessibility row is the one
        // that asks for a person (§5: Blocked is Waiting, with its reason).
        blocked.stood_aside = Some(StoodAside::Permission("Accessibility".into()));
        assert_eq!(standing(&blocked), Standing::Waiting);
        for (outcome, want) in [
            (Outcome::Ok, Standing::InPlace),
            (Outcome::Planned, Standing::Differs),
            (Outcome::Person, Standing::NeedsYou),
            (Outcome::Deferred, Standing::Waiting),
            (Outcome::NotApplicable, Standing::NotNeeded),
            (Outcome::Unknown, Standing::Unknown),
            (Outcome::Failed, Standing::Failed),
        ] {
            assert_eq!(standing(&row(outcome, None, None)), want, "{outcome:?}");
        }
    }

    /// A Restore puts back a saved preference macOS loads only at the next login, while the
    /// value aterm set stays live until then: the row stands as the Mac will after that login
    /// (it differs from what aterm wants) and reads the live value, marked.
    #[test]
    fn a_value_live_until_the_next_login_reads_as_itself_marked() {
        let at_login = |from: &str| ItemReport {
            effect: Effect::Login,
            ..planned(from)
        };
        let live = row(Outcome::Ok, None, Some("Caps Lock acts as Control"));
        let saved = at_login("no remap");
        let read = observation(Setting::CapsLock, &[&saved, &live]);
        assert_eq!(read.standing, Standing::Differs);
        assert_eq!(read.now.as_deref(), Some("Control · until login"));
        // What the Mac reads after that login, which a row that will change shows as its
        // from-side: the saved mapping's word, unmarked.
        assert_eq!(read.after_login.as_deref(), Some("Caps Lock"));
        assert_eq!(read.wanted, "Control");
        // Live not as wanted, or not read at all: the saved mapping's word, and nothing else
        // after the login.
        let unmapped = read_as(Reading::CapsLock);
        for rows in [vec![&saved, &unmapped], vec![&saved]] {
            let read = observation(Setting::CapsLock, &rows);
            assert_eq!(read.now.as_deref(), Some("Caps Lock"), "{rows:?}");
            assert_eq!(read.after_login, None, "{rows:?}");
        }
        // A live part that could not be read proves nothing either.
        let unread = row(Outcome::Unknown, None, None);
        assert_ne!(
            observation(Setting::CapsLock, &[&saved, &live, &unread])
                .now
                .as_deref(),
            Some("Control · until login")
        );
        // Natural scrolling's one item reads both, and says so itself.
        let read = observation(Setting::NaturalScrolling, &[&read_as(Reading::UntilLogin)]);
        assert_eq!(read.standing, Standing::Differs);
        assert_eq!(read.now.as_deref(), Some("Off · until login"));
        assert_eq!(read.after_login.as_deref(), Some("On"));
        // Trackpad scrolling's keys are all saved preferences: one in place says nothing live.
        let gesture = at_login("false");
        let read = observation(Setting::TrackpadScrolling, &[&gesture, &live]);
        assert_eq!(read.now.as_deref(), Some("Some off"));
        assert_eq!(read.after_login, None);
    }

    /// A run that set the Caps Lock key's live mappings with its saved ones left nothing for
    /// the next login: its live part reads in place. Trackpad scrolling has no live part, and a
    /// live part not read, or not as wanted, is none in place.
    #[test]
    fn only_a_live_mapping_read_in_place_puts_a_row_in_effect_now() {
        let saved = ItemReport {
            effect: Effect::Login,
            ..row(Outcome::Ok, None, Some("Caps Lock→Left Control"))
        };
        let live = row(Outcome::Ok, None, Some("Caps Lock acts as Control"));
        assert!(live_in_place(Setting::CapsLock, &[&saved, &live]));
        assert!(!live_in_place(Setting::CapsLock, &[&saved]));
        for other in [Outcome::Unknown, Outcome::Planned] {
            let other = row(other, None, None);
            assert!(!live_in_place(Setting::CapsLock, &[&saved, &live, &other]));
        }
        assert!(!live_in_place(Setting::TrackpadScrolling, &[&saved, &live]));
    }

    #[test]
    fn a_row_takes_its_worst_part_and_says_its_value_only_when_read() {
        let ok = row(Outcome::Ok, None, Some("Caps Lock→Left Control"));
        let read = observation(Setting::CapsLock, &[&ok]);
        assert_eq!(read.standing, Standing::InPlace);
        assert_eq!(read.now.as_deref(), Some("Control"));
        let saved = planned("no remap");
        let live = read_as(Reading::CapsLock);
        let read = observation(Setting::CapsLock, &[&ok, &saved, &live]);
        assert_eq!(read.standing, Standing::Differs);
        assert_eq!(read.now.as_deref(), Some("Caps Lock"));
        assert_eq!(read.wanted, "Control");
        // Caps Lock mapped to something else is neither word: no value is shown.
        let other = planned("Caps Lock→Left Option");
        assert_eq!(observation(Setting::CapsLock, &[&saved, &other]).now, None);
        let seen = planned("321-9456-0: no remap; 76-0-0: Left Shift→Left Control");
        assert_eq!(
            observation(Setting::CapsLock, &[&seen]).now.as_deref(),
            Some("Caps Lock")
        );
        let unknown = row(Outcome::Unknown, None, None);
        let read = observation(Setting::CapsLock, &[&saved, &unknown]);
        assert_eq!(read.standing, Standing::Differs);
        let read = observation(Setting::CapsLock, &[&unknown]);
        assert_eq!((read.standing, read.now), (Standing::Unknown, None));
        let skipped = row(Outcome::NotApplicable, None, None);
        let read = observation(Setting::Spotlight, &[&skipped, &skipped]);
        assert_eq!((read.standing, read.now), (Standing::NotNeeded, None));
        assert_eq!(observation(Setting::Dock, &[]).standing, Standing::Unknown);
    }

    #[test]
    fn each_setting_names_only_the_part_it_read() {
        let apps = |pinned| Reading::Apps { pinned, wanted: 3 };
        let cases: [(Setting, &[Reading], Option<&str>); 12] = [
            (Setting::NaturalScrolling, &[Reading::NaturalOn], Some("On")),
            (Setting::NaturalScrolling, &[], None),
            (
                Setting::Desktop,
                &[Reading::IconsHidden],
                Some("Icons hidden"),
            ),
            (Setting::Desktop, &[], None),
            (Setting::Widgets, &[Reading::WidgetsShown], Some("Shown")),
            (Setting::Dock, &[apps(7)], Some("7 apps")),
            (Setting::Dock, &[apps(1)], Some("1 app")),
            // Three other apps never read as the wanted "3 apps".
            (Setting::Dock, &[apps(3)], Some("Other apps")),
            (Setting::Messages, &[Reading::SignedIn], Some("Signed in")),
            (Setting::FaceTime, &[], None),
            (
                Setting::Spotlight,
                &[Reading::DocsInstalled],
                Some("Docs indexed"),
            ),
            (
                Setting::Spotlight,
                &[Reading::BuildsIndexed {
                    projects: 0,
                    toolchain: false,
                }],
                Some("Builds indexed"),
            ),
        ];
        for (setting, readings, want) in cases {
            let mut rows: Vec<ItemReport> = readings.iter().map(|r| read_as(*r)).collect();
            if rows.is_empty() {
                rows.push(planned("a reading with no word"));
            }
            let refs: Vec<&ItemReport> = rows.iter().collect();
            let read = observation(setting, &refs);
            assert_eq!(read.standing, Standing::Differs, "{setting:?} {readings:?}");
            assert_eq!(read.now.as_deref(), want, "{setting:?} {readings:?}");
            // The engine's `from` line under the reading is no description.
            assert_eq!(read.detail, None, "{setting:?} {readings:?}");
        }
        // Spotlight names the part that read a word.
        let profile = planned("rustup's profile is default, which installs rust-docs");
        let docs = read_as(Reading::DocsInstalled);
        let read = observation(Setting::Spotlight, &[&profile, &docs]);
        assert_eq!(read.now.as_deref(), Some("Docs indexed"));
        // Trackpad gestures read the defaults items' rendered values.
        let (absent, off, on) = (planned("absent"), planned("false"), planned("1"));
        let read = observation(Setting::TrackpadScrolling, &[&absent, &off]);
        assert_eq!(read.now.as_deref(), Some("Some off"));
        assert_eq!(
            observation(Setting::TrackpadScrolling, &[&absent, &on]).now,
            None
        );
        // Parts that disagree name no value.
        let (seven, one) = (read_as(apps(7)), read_as(apps(1)));
        assert_eq!(observation(Setting::Dock, &[&seven, &one]).now, None);
    }

    /// What Spotlight still indexes that the row cannot change itself is a quiet note,
    /// never a step: the person's own project build folders, and the toolchain build aterm
    /// is using, which atpkg moves at its next update.
    #[test]
    fn what_spotlight_still_indexes_is_a_note_never_a_step() {
        let kept = |projects: usize, toolchain: bool| ItemReport {
            detail: Some(
                Reading::KeptOut {
                    projects,
                    toolchain,
                }
                .token(),
            ),
            ..row(
                Outcome::Ok,
                None,
                Some(
                    "cargo-build.noindex and tooling.noindex are .noindex; Spotlight indexes these project build folders of yours, which aterm leaves as they are: /Users//a/x/target",
                ),
            )
        };
        let profile = row(Outcome::Ok, None, None);
        let read = observation(Setting::Spotlight, &[&profile, &kept(3, false)]);
        assert_eq!(read.standing, Standing::InPlace);
        assert_eq!(
            read.note.as_deref(),
            Some(
                "Spotlight indexes 3 of your project build folders; renaming each to its .noindex name and linking the old name to it keeps it out"
            )
        );
        assert!(
            read.detail
                .as_deref()
                .is_some_and(|d| d.contains("/Users//a/x/target")),
            "the list stands behind the row: {read:?}"
        );
        assert_eq!(
            projects_note(1),
            "Spotlight indexes 1 of your project build folders; renaming it to its .noindex name and linking the old name to it keeps it out"
        );
        // The toolchain build aterm is using, which atpkg moves: said first, asked of no one.
        let read = observation(Setting::Spotlight, &[&kept(0, true)]);
        assert_eq!(read.standing, Standing::InPlace);
        assert_eq!(
            read.note.as_deref(),
            Some("aterm moves the Rust toolchain it is using out of Spotlight at its next update")
        );
        let read = observation(Setting::Spotlight, &[&kept(2, true)]);
        assert_eq!(
            read.note.as_deref(),
            Some(
                "aterm moves the Rust toolchain it is using out of Spotlight at its next update; Spotlight indexes 2 of your project build folders; renaming each to its .noindex name and linking the old name to it keeps it out"
            )
        );
        // A change aterm makes says it too; none counted says nothing.
        let read = observation(
            Setting::Spotlight,
            &[&read_as(Reading::BuildsIndexed {
                projects: 2,
                toolchain: false,
            })],
        );
        assert_eq!(read.standing, Standing::Differs);
        assert!(
            read.note
                .is_some_and(|note| note.starts_with("Spotlight indexes 2 "))
        );
        let read = observation(Setting::Spotlight, &[&kept(0, false)]);
        assert_eq!((read.standing, read.note), (Standing::InPlace, None));
    }

    #[test]
    fn repositories_count_what_is_present_and_want_all_of_them() {
        let partial = read_as(Reading::Repositories {
            present: 3,
            count: 7,
            signed_in: false,
        });
        let read = observation(Setting::Repositories, &[&partial]);
        assert_eq!(read.now.as_deref(), Some("3 of 7"));
        assert_eq!(read.wanted, "All 7");
        let sign_in = read_as(Reading::Repositories {
            present: 7,
            count: 7,
            signed_in: false,
        });
        let read = observation(Setting::Repositories, &[&sign_in]);
        assert_eq!(read.now.as_deref(), Some("Needs sign-in"));
        assert_eq!(read.wanted, "All 7");
        let ok = ItemReport {
            detail: Some(
                Reading::Repositories {
                    present: 7,
                    count: 7,
                    signed_in: true,
                }
                .token(),
            ),
            ..row(Outcome::Ok, None, Some("7 matching worktrees present"))
        };
        let read = observation(Setting::Repositories, &[&ok]);
        assert_eq!(read.now.as_deref(), Some("All 7"));
        let mut blocked = row(Outcome::Blocked, None, None);
        blocked.why = Some("offline".into());
        let read = observation(Setting::Repositories, &[&blocked]);
        assert_eq!(read.standing, Standing::Waiting);
        assert_eq!((read.now, read.wanted.as_str()), (None, "All"));
        assert_eq!(read.detail.as_deref(), Some("offline"));
    }

    /// A waiting row's note is the reason of the first waiting part whose item decided one, and a
    /// row only a person moves says their remedy (design §3.6), never the engine's line; a row
    /// with no such part, or one that neither waits nor needs a person, gives no note.
    #[test]
    fn a_row_notes_the_reason_or_remedy_its_item_decided_and_never_the_engine_line() {
        let blocked = |wait: Option<Wait>, why: &str| ItemReport {
            why: Some(why.to_owned()),
            detail: wait.map(|wait| Reading::Waits(wait).token()),
            ..row(Outcome::Blocked, None, None)
        };
        let line = "GitHub token source must be an owned private regular file (0600), without \
                    links, at most 64 KiB";
        for wait in Wait::ALL {
            // As the item reports it: a wait that ends by itself is blocked, one only a person
            // ends is theirs.
            let person = wait.kind() == FailureKind::NeedsPerson;
            let mut part = blocked(Some(wait), line);
            if person {
                part.outcome = Outcome::Person;
            }
            let read = observation(Setting::Repositories, &[&part]);
            let standing = if person {
                Standing::NeedsYou
            } else {
                Standing::Waiting
            };
            assert_eq!(read.standing, standing, "{wait:?}");
            assert_eq!(read.note.as_deref(), Some(wait.note()), "{wait:?}");
            let now = matches!(wait, Wait::Token | Wait::TokenRefused | Wait::OtherAccount)
                .then_some("Needs sign-in");
            assert_eq!(
                (read.now.as_deref(), read.wanted.as_str()),
                (now, "All"),
                "{wait:?}"
            );
            // The engine's line stays the description, for `aterm init` and the doctor.
            assert_eq!(read.detail.as_deref(), Some(line), "{wait:?}");
            // Short, and a person's words: no item id, path, file mode or flag.
            let note = wait.note();
            assert!(note.chars().count() <= 52, "{note}");
            for engine in ["@", "/", "~", "`", "--", "0600", "init.", ".toml"] {
                assert!(!note.contains(engine), "{note:?} says {engine:?}");
            }
        }
        // The first waiting part that says why gives the note; a planned part beside it
        // and a waiting part that says nothing do not.
        let silent = blocked(None, "waits for init.github.auth@alice");
        let planned = read_as(Reading::Repositories {
            present: 1,
            count: 2,
            signed_in: true,
        });
        let read = observation(
            Setting::Repositories,
            &[&planned, &silent, &blocked(Some(Wait::Unreached), line)],
        );
        assert_eq!(read.note.as_deref(), Some(Wait::Unreached.note()));
        let read = observation(Setting::Repositories, &[&planned, &silent]);
        assert_eq!((read.standing, read.note), (Standing::Waiting, None));
        // A row that does not wait has no note, whatever its parts read.
        let mut failed = blocked(Some(Wait::Unreached), line);
        failed.outcome = Outcome::Failed;
        let read = observation(Setting::Repositories, &[&failed]);
        assert_eq!((read.standing, read.note), (Standing::Failed, None));
    }

    /// A row that differs has no description (design §3.3): a planned part's `from → to` and its
    /// `why` are the engine's; a row of another standing takes its worst parts' reasons.
    #[test]
    fn a_row_that_differs_carries_no_engine_line_as_its_description() {
        let gestures = [
            planned("absent"),
            planned("false"),
            ItemReport {
                why: Some("read after workstation.x, which this plan changes first (y)".into()),
                ..planned("absent")
            },
        ];
        let read = observation(
            Setting::TrackpadScrolling,
            &gestures.iter().collect::<Vec<_>>(),
        );
        assert_eq!(read.standing, Standing::Differs);
        assert_eq!(read.detail, None, "{read:?}");
        let dock = row(
            Outcome::Planned,
            Some("com.apple.dock persistent-apps, com.apple.dock show-recents"),
            Some("Dock pins: aterm"),
        );
        assert_eq!(observation(Setting::Dock, &[&dock]).detail, None);
        // A waiting row gives its reason, not a planned part's line beside it,
        // and a blocked part with no reason gives none.
        let mut offline = row(Outcome::Blocked, Some("absent"), Some("true"));
        offline.why = Some("offline".into());
        let read = observation(Setting::TrackpadScrolling, &[&gestures[0], &offline]);
        assert_eq!(read.standing, Standing::Waiting);
        assert_eq!(read.detail.as_deref(), Some("offline"));
        let silent = row(Outcome::Blocked, Some("absent"), Some("true"));
        assert_eq!(
            observation(Setting::TrackpadScrolling, &[&silent]).detail,
            None
        );
        // A failure's reason stays its description.
        let mut failed = row(Outcome::Failed, Some("absent"), Some("true"));
        failed.why = Some("applied, but it still reads absent".into());
        assert_eq!(
            observation(Setting::TrackpadScrolling, &[&failed])
                .detail
                .as_deref(),
            Some("applied, but it still reads absent")
        );
    }
}
