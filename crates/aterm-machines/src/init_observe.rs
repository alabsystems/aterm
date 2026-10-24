// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! System State's reading of one setting from the shared engine's rows
//! (`docs/DESIGN-system-state-2026-10-21.md` §3.3, §5): one standing per row,
//! and a current value only in the page's exact words. A value the rows do not
//! establish stays `None` (the page's `—`); nothing here guesses one.
//!
//! The words come from a typed [`Reading`] the item took in its own probe and
//! hands over as its row's `detail` (the engine's item-specific output a caller
//! acts on), never from the human `from` line `aterm doctor` and `aterm init`
//! print, which stays the engine's. Two settings are read from shared items
//! that take no reading: Caps Lock from the keyboard items' rendered mapping, and
//! Trackpad scrolling from the defaults items' rendered value.

use crate::model::{Effect, ItemReport, Outcome};
use aterm_types::macos_setup::{Observation, Setting, Standing, Subject};
use std::sync::{Mutex, PoisonError};

/// What an init item's probe read, in terms System State words (design §3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reading {
    /// No attached keyboard service maps Caps Lock: the key still does Caps Lock.
    CapsLock,
    /// Natural scrolling is on in System Settings.
    NaturalOn,
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
    /// A Rust build or tooling cache sits where Spotlight indexes it.
    BuildsIndexed,
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

    /// What a waiting row's note says (design §3.3): one short line in a person's words,
    /// with no item id, file mode or command.
    #[must_use]
    pub const fn note(self) -> &'static str {
        match self {
            Self::CommandLineTools => "Needs Apple's Command Line Tools",
            Self::Settings => "aterm's GitHub settings file can't be used",
            Self::Token => "Waits for your GitHub token from the Secrets vault",
            Self::TokenUnusable => "Your GitHub token file can't be used",
            Self::Unreached => "GitHub can't be reached",
            Self::TokenRefused => "GitHub doesn't accept your token",
            Self::OtherAccount => "Your GitHub token is for another account",
            Self::GitHub => "Can't read your repositories from GitHub",
            Self::InTheWay => "A folder is in the way of a repository",
        }
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
            Self::IconsHidden => "icons-hidden".to_owned(),
            Self::ClickReveals => "click-reveals".to_owned(),
            Self::WidgetsShown => "widgets-shown".to_owned(),
            Self::Apps { pinned, wanted } => format!("apps:{pinned}:{wanted}"),
            Self::SignedIn => "signed-in".to_owned(),
            Self::DocsInstalled => "docs-installed".to_owned(),
            Self::BuildsIndexed => "builds-indexed".to_owned(),
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
            "icons-hidden" => Self::IconsHidden,
            "click-reveals" => Self::ClickReveals,
            "widgets-shown" => Self::WidgetsShown,
            "apps" => Self::Apps {
                pinned: number()?,
                wanted: number()?,
            },
            "signed-in" => Self::SignedIn,
            "docs-installed" => Self::DocsInstalled,
            "builds-indexed" => Self::BuildsIndexed,
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
            Self::BuildsIndexed => "Builds indexed".to_owned(),
            Self::Repositories { present, count, .. } if present < count => {
                format!("{present} of {count}")
            }
            Self::Repositories {
                signed_in: false, ..
            } => "Needs sign-in".to_owned(),
            // A probe that could not go on read no value.
            Self::Repositories { .. } | Self::Waits(_) => return None,
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

/// The worse of two standings for one row made of several items.
const fn rank(standing: Standing) -> u8 {
    match standing {
        Standing::NotNeeded => 0,
        Standing::InPlace => 1,
        Standing::AtLogin => 2,
        Standing::Unknown => 3,
        Standing::Differs => 4,
        Standing::Waiting => 5,
        Standing::NeedsYou => 6,
        Standing::Failed => 7,
    }
}

/// One setting's row from the rows its items reported. No rows is an unknown row.
/// Its description (`detail`) is its worst parts' reasons (`why`), and a row that
/// differs has none: the engine's `from → to` line is never one (§3.3). A row that
/// waits takes its note from the first waiting part whose item said why
/// ([`Reading::Waits`]); the engine's line stays its description, which the page
/// never shows under it.
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
    let now = match standing {
        Standing::InPlace | Standing::AtLogin => Some(wanted.clone()),
        Standing::NotNeeded | Standing::Unknown => None,
        Standing::Differs | Standing::Waiting | Standing::NeedsYou | Standing::Failed => {
            now(setting, rows)
        }
    };
    // The worst parts' reasons. A row that differs has none to give: its value
    // names the part that differs, and a planned part's `from → to` line (and
    // a `why` it carries, the order the plan reads it in) is the engine's,
    // which `aterm init` and `aterm doctor` print.
    let detail = if standing == Standing::Differs {
        Vec::new()
    } else {
        rows.iter()
            .filter(|row| self::standing(row) == standing)
            .filter_map(|row| row.why.clone())
            .collect::<Vec<_>>()
    };
    let note = if standing == Standing::Waiting {
        rows.iter()
            .filter(|row| self::standing(row) == Standing::Waiting)
            .find_map(|row| match Reading::of(row) {
                Some(Reading::Waits(wait)) => Some(wait.note().to_owned()),
                _ => None,
            })
    } else {
        None
    };
    Observation {
        subject,
        standing,
        now,
        wanted,
        note,
        detail: (!detail.is_empty()).then(|| detail.join("; ")),
    }
}

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
            Reading::IconsHidden,
            Reading::ClickReveals,
            Reading::WidgetsShown,
            Reading::Apps {
                pinned: 7,
                wanted: 3,
            },
            Reading::SignedIn,
            Reading::DocsInstalled,
            Reading::BuildsIndexed,
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
                &[Reading::BuildsIndexed],
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

    /// A WAITING ROW SAYS WHY IN A PERSON'S WORDS (design §3.3): its note is the reason
    /// the first waiting part's item decided (`Reading::Waits`), never the engine's line,
    /// which stays its description; a waiting part whose item decided none, a part of
    /// another standing beside it, and a row that does not wait give no note.
    #[test]
    fn a_waiting_row_notes_the_reason_its_item_decided_and_never_the_engine_line() {
        let blocked = |wait: Option<Wait>, why: &str| ItemReport {
            why: Some(why.to_owned()),
            detail: wait.map(|wait| Reading::Waits(wait).token()),
            ..row(Outcome::Blocked, None, None)
        };
        let line = "GitHub token source must be an owned private regular file (0600), without \
                    links, at most 64 KiB";
        for wait in Wait::ALL {
            let read = observation(Setting::Repositories, &[&blocked(Some(wait), line)]);
            assert_eq!(read.standing, Standing::Waiting, "{wait:?}");
            assert_eq!(read.note.as_deref(), Some(wait.note()), "{wait:?}");
            assert_eq!((read.now, read.wanted.as_str()), (None, "All"), "{wait:?}");
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
            &[&planned, &silent, &blocked(Some(Wait::Token), line)],
        );
        assert_eq!(read.note.as_deref(), Some(Wait::Token.note()));
        let read = observation(Setting::Repositories, &[&planned, &silent]);
        assert_eq!((read.standing, read.note), (Standing::Waiting, None));
        // A row that does not wait has no note, whatever its parts read.
        let mut failed = blocked(Some(Wait::Unreached), line);
        failed.outcome = Outcome::Failed;
        let read = observation(Setting::Repositories, &[&failed]);
        assert_eq!((read.standing, read.note), (Standing::Failed, None));
    }

    /// NO ENGINE LINE IN A DESCRIPTION (design §3.3: a row that differs shows
    /// its value alone). A planned part's `from → to` line, and the `why` a
    /// planned part carries (the order the plan reads it in), is the engine's,
    /// which `aterm init` prints: a row that differs has no description, and a
    /// row of another standing takes its worst parts' reasons, never their
    /// `from → to`.
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
