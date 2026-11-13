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
    /// `present` of the `count` repository checkouts and their selected required
    /// dependencies are ready; `signed_in` is whether aterm's route returns the synced token.
    Repositories {
        /// Repositories whose checkouts and selected required dependencies are ready.
        present: usize,
        /// Repositories the plan names.
        count: usize,
        /// aterm's GitHub route hands back the synced token.
        signed_in: bool,
    },
    /// The probe could not go on (its row is blocked), for this reason.
    Waits(Wait),
}

/// Why a blocked part cannot go on, as its item decided it in its probe: the reason a
/// waiting row's note gives (design §3.3), never the engine's line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    /// Neither managed Git nor the developer tools' Git is ready yet.
    GitReadiness,
    /// aterm's GitHub settings (`~/.config/aterm/init.toml`) cannot be used.
    Settings,
    /// The GitHub token file the vault syncs is not on this Mac yet.
    Token,
    /// The GitHub token file is there but cannot be used: not this account's own private
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
    /// The selected vault has not supplied a usable repository inventory yet.
    RepositoryInventory,
    /// Something already in the home folder sits where a repository goes.
    InTheWay,
    /// The disk has no room for the repositories left: each clone would take free space below
    /// the line the window's disk watch keeps.
    DiskSpace,
}

impl Wait {
    /// Every reason, for the round trip.
    pub const ALL: [Self; 11] = [
        Self::GitReadiness,
        Self::Settings,
        Self::Token,
        Self::TokenUnusable,
        Self::Unreached,
        Self::TokenRefused,
        Self::OtherAccount,
        Self::GitHub,
        Self::RepositoryInventory,
        Self::InTheWay,
        Self::DiskSpace,
    ];

    fn slug(self) -> &'static str {
        match self {
            Self::GitReadiness => "git-readiness",
            Self::Settings => "settings",
            Self::Token => "token",
            Self::TokenUnusable => "token-unusable",
            Self::Unreached => "unreached",
            Self::TokenRefused => "token-refused",
            Self::OtherAccount => "other-account",
            Self::GitHub => "github",
            Self::RepositoryInventory => "repository-inventory",
            Self::InTheWay => "in-the-way",
            Self::DiskSpace => "disk-space",
        }
    }

    /// What ends this wait (design §3.6): Git becoming ready, GitHub answering again, room on
    /// the disk (which the disk watch, a finished download or a person frees), or a GitHub token
    /// the vault delivers (its need is filed in the ledger, never with a person), tried again
    /// while the run has tries left ([`FailureKind::Transient`]). A credential never waits on a
    /// person. Only aterm's settings
    /// (an account other than the token's included) or an existing repository destination need
    /// a person's act ([`FailureKind::NeedsPerson`]).
    #[must_use]
    pub const fn kind(self) -> FailureKind {
        match self {
            Self::GitReadiness
            | Self::Token
            | Self::TokenUnusable
            | Self::TokenRefused
            | Self::Unreached
            | Self::GitHub
            | Self::RepositoryInventory
            | Self::DiskSpace => FailureKind::Transient,
            Self::Settings | Self::OtherAccount | Self::InTheWay => FailureKind::NeedsPerson,
        }
    }

    /// What the row's note says (design §3.3): one short line in a person's words, with no
    /// item id, file mode or command. A wait that ends by itself says what it waits on; one
    /// only a person ends ([`Self::kind`]) says their remedy.
    #[must_use]
    pub const fn note(self) -> &'static str {
        match self {
            Self::GitReadiness => "Waiting for Git to finish installing",
            Self::Settings => "Fix aterm's GitHub settings; aterm doctor says how",
            Self::Token => "Waits for the GitHub token from the Secrets vault",
            Self::TokenUnusable => "GitHub token file is unusable",
            Self::Unreached => "GitHub can't be reached",
            Self::TokenRefused => "Token refused by GitHub; waits for a new one",
            Self::OtherAccount => "GitHub account setting differs from the token's",
            Self::GitHub => "Can't read your repositories from GitHub",
            Self::RepositoryInventory => "Waits for repository metadata from the Secrets vault",
            Self::InTheWay => "A folder blocks a repository; aterm doctor names it",
            Self::DiskSpace => "Not enough free disk space for the rest",
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
        let mut number = || crate::parse_uint::<usize>(parts.next()?);
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
            // Not signed in: the token is missing, refused or another account's.
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
    // A successful operation is not evidence that every repository is present. Read-back's
    // typed inventory owns that fact, including when an older or partial pass reports success.
    if matches!(row.outcome, Outcome::Ok | Outcome::Changed)
        && let Some(Reading::Repositories {
            present,
            count,
            signed_in,
        }) = Reading::of(row)
        && (present != count || !signed_in)
    {
        return Standing::Differs;
    }
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
        Standing::InPlace => 1,
        Standing::AtLogin => 2,
        Standing::Unknown => 3,
        Standing::Differs => 4,
        Standing::Asking => 5,
        Standing::Waiting => 6,
        Standing::NeedsYou => 7,
        Standing::Failed => 8,
    }
}

/// Why a part of a row failed, where the step that stopped it decided a reason
/// ([`Reading::Waits`]): the page's words for a step a run could not finish ([`Wait::note`],
/// System State design §3.6), never the engine's line, which the log keeps.
#[must_use]
pub fn failure_note(rows: &[&ItemReport]) -> Option<&'static str> {
    rows.iter()
        .filter(|row| standing(row) == Standing::Failed)
        .find_map(|row| match Reading::of(row) {
            Some(Reading::Waits(wait)) => Some(wait.note()),
            _ => None,
        })
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
        Standing::NotNeeded | Standing::Unknown => None,
        Standing::Differs if marked => Some(aterm_types::macos_setup::until_login(&wanted)),
        Standing::Differs
        | Standing::Asking
        | Standing::Waiting
        | Standing::NeedsYou
        | Standing::Failed => now(setting, rows),
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
        read: aterm_types::macos_setup::Read::Unstamped,
    }
}

/// Project the complete native producer's paid post-apply cohort as current facts.
/// The caller must supply every part prepared for this setting, including held,
/// unread and failed parts. This changes no operation outcome or journal record.
#[must_use]
pub fn readback_observation(setting: Setting, rows: &[&ItemReport]) -> Observation {
    let live: Vec<_> = rows
        .iter()
        .copied()
        .filter(|row| row.effect == Effect::Now)
        .collect();
    let caps_live = setting == Setting::CapsLock
        && !live.is_empty()
        && live.iter().all(|row| {
            matches!(row.outcome, Outcome::Ok | Outcome::Changed)
                && row.stood_aside.is_none()
                && Reading::of(row) != Some(Reading::CapsLock)
        });
    let normalized: Vec<_> = rows
        .iter()
        .map(|row| {
            let mut row = (**row).clone();
            row.outcome = if row.outcome == Outcome::NotApplicable
                || (row.stood_aside.is_some()
                    && matches!(row.outcome, Outcome::Ok | Outcome::Changed))
            {
                // A held item can have read a real difference. Its operation's exemption
                // supplies no successful native value for the owning setting.
                Outcome::Unknown
            } else if row.outcome == Outcome::Changed {
                match Reading::of(&row) {
                    Some(Reading::UntilLogin | Reading::CapsLock) => Outcome::Planned,
                    _ if matches!(row.effect, Effect::Now | Effect::Relaunch) || caps_live => {
                        Outcome::Ok
                    }
                    _ => row.outcome,
                }
            } else {
                row.outcome
            };
            row
        })
        .collect();
    observation(setting, &normalized.iter().collect::<Vec<_>>())
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
#[path = "init_observe_tests.rs"]
mod tests;
