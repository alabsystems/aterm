// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Where a machine's updater stands, as another machine of its fleet is told it: the word for
//! the arm of `aterm update status`'s decision its line comes from, and the few structured facts
//! beside it, carried as `update*=` words on a survey's `instance` line (`aterm fleet peers`'
//! UPDATE column, `aterm ctl survey --host`'s `update` line).
//!
//! Only closed words, numbers and version words cross the wire. An updater's free text (its last
//! outcome, a hold's or a refusal's reason, a failure's message) can name a path or an address,
//! and none of it is ever carried: [`UpdateFact`] has no field that could hold it, and its reader
//! drops any value outside its grammar.

listed! {
    /// Where an updater stands: the arm of `aterm update status`'s decision its line comes from.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub enum UpdateStanding {
        /// This copy does not update itself.
        Off,
        /// A Linux copy installed a newer build and waits for an aterm to start from it.
        Installed,
        /// A newer build is downloaded and not installed yet.
        Downloaded,
        /// This copy cannot update itself (a dev build, a copy outside an installed app).
        Cannot,
        /// A newer build is held after it failed here, until its hold lapses.
        Held,
        /// A newer build is downloaded and its last install was refused.
        Refused,
        /// A newer build is downloaded and did not install.
        NotInstalling,
        /// The last completed check could not read the release channel.
        Unreadable,
        /// This build predates the channel's keys: only a reinstall moves it on.
        TooOld,
        /// Checks or installs keep failing.
        Failing,
        /// The last check did not finish.
        CheckFailed,
        /// A hold lapsed: the build it held is tried again.
        Retrying,
        /// A newer release is out and not downloaded yet.
        Behind,
        /// Up to date as of the last completed check.
        Current,
        /// No check has completed yet.
        Unchecked,
        /// The updater's state could not be read.
        Unread,
    }
}

impl UpdateStanding {
    /// The standing's word on the wire (`update=`) and in `aterm fleet peers`' UPDATE column.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Installed => "installed",
            Self::Downloaded => "downloaded",
            Self::Cannot => "cannot",
            Self::Held => "held",
            Self::Refused => "refused",
            Self::NotInstalling => "not-installing",
            Self::Unreadable => "unreadable",
            Self::TooOld => "too-old",
            Self::Failing => "failing",
            Self::CheckFailed => "check-failed",
            Self::Retrying => "retrying",
            Self::Behind => "behind",
            Self::Current => "current",
            Self::Unchecked => "unchecked",
            Self::Unread => "unread",
        }
    }

    /// The standing [`Self::word`] names, exactly.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|standing| standing.word() == word)
    }
}

/// The standing word an `update=` says: one this build knows, or one a newer aterm said
/// ([`StandingWord::of`]), kept as said so a reader shows it rather than nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StandingWord {
    /// A standing this build knows.
    Known(UpdateStanding),
    /// A word in the standing grammar this build does not know: a newer aterm's.
    Unknown(String),
}

impl StandingWord {
    /// Longest unknown word kept.
    const MAX: usize = 24;

    /// The standing `word` says: a known one, else an unknown one where it is 1 to 24 bytes of
    /// lowercase ASCII letters and `-` starting with a letter, else `None`.
    #[must_use]
    pub fn of(word: &str) -> Option<Self> {
        if let Some(known) = UpdateStanding::from_word(word) {
            return Some(Self::Known(known));
        }
        let grammatical = word.len() <= Self::MAX
            && word.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
            && word.bytes().all(|b| b.is_ascii_lowercase() || b == b'-');
        grammatical.then(|| Self::Unknown(word.to_owned()))
    }

    /// The word as the wire says it.
    #[must_use]
    pub fn word(&self) -> &str {
        match self {
            Self::Known(standing) => standing.word(),
            Self::Unknown(word) => word,
        }
    }

    /// The standing, where this build knows it.
    #[must_use]
    pub const fn known(&self) -> Option<UpdateStanding> {
        match self {
            Self::Known(standing) => Some(*standing),
            Self::Unknown(_) => None,
        }
    }
}

/// The words `update_why=` may say: a hold's cause (`aterm_update_core::update_record::Cause::word`)
/// and a failing check's or install's class. Any other word is dropped where it is read.
pub const WHY_WORDS: [&str; 9] = [
    "crash-loop",
    "unproven",
    "stage",
    "linux-rollback",
    "start-failed",
    "network",
    "manifest",
    "pipeline",
    "apply",
];

/// `word` as one of [`WHY_WORDS`], else `None`.
#[must_use]
pub fn why_word(word: &str) -> Option<&'static str> {
    WHY_WORDS.into_iter().find(|why| *why == word)
}

/// What a machine's updater says of itself to its fleet: its standing and the structured facts
/// beside it, each `None` where it says nothing of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateFact {
    /// Where it stands (`update=`).
    pub standing: StandingWord,
    /// When its last check completed, Unix seconds (`update_at=`).
    pub checked_at: Option<u64>,
    /// The build its standing names, as [`build_name`] says it (`update_to=`): the held, staged,
    /// installed or offered one.
    pub to: Option<String>,
    /// When a live hold lapses, Unix seconds (`update_until=`).
    pub until: Option<u64>,
    /// Why, as one of [`WHY_WORDS`] (`update_why=`).
    pub why: Option<&'static str>,
    /// How many tries the standing counts (`update_tries=`): a hold's, or a download's failed
    /// installs.
    pub tries: Option<u32>,
    /// Whether automatic checks are on (`update_checks=on|off`).
    pub checks: Option<bool>,
    /// When it was said, Unix seconds on the clock of the machine that said it: the `at_ms=` of
    /// the `instance` line it was read from, which these words sit beside and do not carry. It is
    /// the clock `checked_at` and `until` are on, so [`Self::line`] tells their age and the time
    /// left by it and by no reader's clock. `None` for a fact not read from such a line.
    pub said_at: Option<u64>,
}

impl UpdateFact {
    /// A fact that says `standing` and nothing else.
    #[must_use]
    pub const fn of(standing: UpdateStanding) -> Self {
        Self {
            standing: StandingWord::Known(standing),
            checked_at: None,
            to: None,
            until: None,
            why: None,
            tries: None,
            checks: None,
            said_at: None,
        }
    }

    /// The space-led `update*=` words a survey's `instance` line carries, `-` for each field it
    /// says nothing of; [`Self::of_fields`] reads them back.
    #[must_use]
    pub fn tokens(&self) -> String {
        let number = |n: Option<u64>| n.map_or_else(|| "-".to_owned(), |n| n.to_string());
        format!(
            " update={} update_at={} update_to={} update_until={} update_why={} \
             update_tries={} update_checks={}",
            self.standing.word(),
            number(self.checked_at),
            self.to
                .as_deref()
                .filter(|to| is_build_name(to))
                .map_or_else(|| "-".to_owned(), aterm_codec::pct::encode),
            number(self.until),
            self.why.and_then(why_word).unwrap_or("-"),
            number(self.tries.map(u64::from)),
            match self.checks {
                Some(true) => "on",
                Some(false) => "off",
                None => "-",
            },
        )
    }

    /// The fact a record's decoded `key=value` fields say: `None` where they have no `update=`
    /// (an aterm older than these words) or its word is not one ([`StandingWord::of`]). A field
    /// outside its grammar reads as `None` alone, so a reader keeps the rest, and nothing here
    /// ever fails the record it is read from. The record's own `at_ms=` is when it was said
    /// ([`Self::said_at`]).
    #[must_use]
    pub fn of_fields<K: AsRef<str>, V: AsRef<str>>(fields: &[(K, V)]) -> Option<Self> {
        let get = |key: &str| {
            fields
                .iter()
                .find(|(k, _)| k.as_ref() == key)
                .map(|(_, v)| v.as_ref())
                .filter(|v| !v.is_empty() && *v != "-")
        };
        Some(Self {
            standing: StandingWord::of(get("update")?)?,
            checked_at: get("update_at").and_then(digits),
            to: get("update_to")
                .filter(|to| is_build_name(to))
                .map(str::to_owned),
            until: get("update_until").and_then(digits),
            why: get("update_why").and_then(why_word),
            tries: get("update_tries")
                .and_then(digits)
                .and_then(|n| u32::try_from(n).ok()),
            checks: match get("update_checks") {
                Some("on") => Some(true),
                Some("off") => Some(false),
                _ => None,
            },
            said_at: get("at_ms").and_then(digits).map(|ms| ms / 1000),
        })
    }

    /// Whether a reader may pass it by: up to date, with automatic checks not said off.
    #[must_use]
    pub fn routine(&self) -> bool {
        self.standing.known() == Some(UpdateStanding::Current) && self.checks != Some(false)
    }

    /// The fact in words, as of when it was said ([`Self::said_at`], on the clock of the machine
    /// that said it): what stands, then when it last checked and whether automatic checks are off
    /// (`0.199.0 is held for 3 h more (crash-loop, try 2) · checked 10 min ago · automatic checks
    /// off`). A fact with no `said_at` has no clock to tell an age or a time left on, and says
    /// neither. An unknown standing is said by its word.
    #[must_use]
    pub fn line(&self) -> String {
        let to = self.to.as_deref().unwrap_or("a newer build");
        let tries = |n: u32| {
            if n == 1 {
                "1 try".to_owned()
            } else {
                format!("{n} tries")
            }
        };
        let why = self.why.map(|why| format!(" ({why})")).unwrap_or_default();
        let mut line = match self.standing.known() {
            Some(UpdateStanding::Off) => "does not update itself".to_owned(),
            Some(UpdateStanding::Installed) => {
                format!("{to} is installed and waits for an aterm to start from it")
            }
            Some(UpdateStanding::Downloaded) => format!("{to} is downloaded, not installed yet"),
            Some(UpdateStanding::Cannot) => "this copy cannot update itself".to_owned(),
            Some(UpdateStanding::Held) => {
                let left = self
                    .until
                    .zip(self.said_at)
                    .filter(|(until, now_s)| until > now_s)
                    .map(|(until, now_s)| format!(" for {} more", left_words(until - now_s)))
                    .unwrap_or_default();
                let mut said: Vec<String> = self.why.iter().map(|why| (*why).to_owned()).collect();
                said.extend(self.tries.map(|n| format!("try {n}")));
                let said = if said.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", said.join(", "))
                };
                format!("{to} is held{left}{said}")
            }
            Some(UpdateStanding::Refused) => {
                format!("{to} is downloaded; its last install was refused")
            }
            Some(UpdateStanding::NotInstalling) => format!(
                "{to} is downloaded and did not install{}",
                self.tries
                    .map(|n| format!(" ({})", tries(n)))
                    .unwrap_or_default()
            ),
            Some(UpdateStanding::Unreadable) => "cannot read its release channel".to_owned(),
            Some(UpdateStanding::TooOld) => {
                "too old to check for updates: a reinstall moves it on".to_owned()
            }
            Some(UpdateStanding::Failing) if self.why == Some("apply") => {
                "the last updates did not install".to_owned()
            }
            Some(UpdateStanding::Failing) => format!("update checks keep failing{why}"),
            Some(UpdateStanding::CheckFailed) => format!("the last check did not finish{why}"),
            Some(UpdateStanding::Retrying) => format!("trying {to} again"),
            Some(UpdateStanding::Behind) => format!(
                "a newer release{} is out, not downloaded yet",
                self.to
                    .as_deref()
                    .map(|to| format!(" ({to})"))
                    .unwrap_or_default()
            ),
            Some(UpdateStanding::Current) => "up to date".to_owned(),
            Some(UpdateStanding::Unchecked) => "has not checked for updates yet".to_owned(),
            Some(UpdateStanding::Unread) => "its updater's state could not be read".to_owned(),
            None => self.standing.word().to_owned(),
        };
        if let Some((at, now_s)) = self.checked_at.zip(self.said_at) {
            line.push_str(" \u{b7} checked ");
            line.push_str(&ago_words(
                i64::try_from(at).unwrap_or(i64::MAX),
                i64::try_from(now_s).unwrap_or(i64::MAX),
            ));
        }
        if self.checks == Some(false) {
            line.push_str(" \u{b7} automatic checks off");
        }
        line
    }
}

/// `text` as a number: 1 to 20 ASCII digits, nothing else.
fn digits(text: &str) -> Option<u64> {
    ((1..=20).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// `version` as a version word: 1 to 64 bytes of ASCII letters, digits, `.`, `+`, `-` and `_`,
/// so a peer's escape bytes, a path or an address never pass for one; else `None`.
#[must_use]
pub fn version_word(version: &str) -> Option<String> {
    ((1..=64).contains(&version.len())
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-' | b'_')))
    .then(|| version.to_owned())
}

/// A build as a person names it: its version where that is a version word ([`version_word`]),
/// else `build <n>`.
#[must_use]
pub fn build_name(build: u64, version: Option<&str>) -> String {
    version
        .and_then(version_word)
        .unwrap_or_else(|| format!("build {build}"))
}

/// Whether `name` is one [`build_name`] can say: a version word, or `build ` and 1 to 20 digits.
#[must_use]
pub fn is_build_name(name: &str) -> bool {
    version_word(name).is_some()
        || name
            .strip_prefix("build ")
            .is_some_and(|n| digits(n).is_some())
}

/// `secs` of time left in plain words: "under a minute", "40 min", "3 h".
#[must_use]
pub fn left_words(secs: u64) -> String {
    const HOUR: u64 = 3600;
    if secs < 60 {
        "under a minute".to_string()
    } else if secs < 2 * HOUR {
        format!("{} min", secs.div_ceil(60))
    } else {
        format!("{} h", secs.div_ceil(HOUR))
    }
}

/// How long before `now` the instant `at` was, as a person says it (both Unix seconds): "just
/// now" under a minute (an `at` after `now` included), then minutes, hours, "1 day ago" and days.
#[must_use]
pub fn ago_words(at: i64, now: i64) -> String {
    let age = now.saturating_sub(at);
    if age < 60 {
        "just now".to_string()
    } else if age < 3600 {
        format!("{} min ago", age / 60)
    } else if age < 86_400 {
        format!("{} h ago", age / 3600)
    } else if age < 2 * 86_400 {
        "1 day ago".to_string()
    } else {
        format!("{} days ago", age / 86_400)
    }
}

#[cfg(test)]
#[path = "update_standing_tests.rs"]
mod tests;
