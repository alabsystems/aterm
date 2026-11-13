// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Prompt-free CLI observation of the coordinator which already owns workstation setup.

use aterm_machines::init::{ReadBack, WorkstationObservation};
use aterm_types::macos_setup::{
    Access, SETUP_STATUS_SCHEMA, Selection, Setting, Standing, Subject,
};
use std::collections::BTreeMap;
use std::time::Duration;

/// The running coordinator's `setup status` answer as asked, before a reader decides the rows it
/// asks about ([`Self::observation`]): asked once, so readers that learn their rows later (the
/// `aterm status` setup area, from its record's plan) read the same answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackgroundAnswer {
    /// The one line the front door answered.
    Reply(String),
    /// No answer to read: why, as the observation it is.
    Missing(WorkstationObservation),
}

impl BackgroundAnswer {
    /// What this answer says of the rows `requested` names ([`observation`]): its private
    /// settings, and the grants where it selects the Permissions rows.
    #[must_use]
    pub fn observation(&self, requested: Selection) -> WorkstationObservation {
        match self {
            Self::Reply(reply) => observation(reply, requested),
            Self::Missing(missing) => missing.clone(),
        }
    }
}

/// Ask the running coordinator for its published state without starting setup, selecting its
/// tab, reading other apps' settings, or requesting a permission.
#[must_use]
pub fn ask_background_setup() -> BackgroundAnswer {
    match aterm_ctl::front_door_ask("setup status\n", Duration::from_secs(2)) {
        aterm_ctl::FrontDoorAnswer::Answered(reply) => BackgroundAnswer::Reply(reply),
        aterm_ctl::FrontDoorAnswer::Stopped(_) | aterm_ctl::FrontDoorAnswer::Silent(_) => {
            BackgroundAnswer::Missing(WorkstationObservation::Waiting(
                "background setup has not answered yet".into(),
            ))
        }
        aterm_ctl::FrontDoorAnswer::Absent => BackgroundAnswer::Missing(unavailable()),
    }
}

/// Read the running coordinator's published state of the rows `requested` names
/// ([`ask_background_setup`], [`BackgroundAnswer::observation`]). Only those rows decide the
/// result; every other row the reply names is a diagnostic.
#[must_use]
pub fn check_background_workstation(requested: Selection) -> WorkstationObservation {
    ask_background_setup().observation(requested)
}

/// What this build reads of a `setup status` reply for the rows `requested` names (tests of the
/// crates that compose the window's answer with this reader).
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn background_workstation_of_reply(
    reply: &str,
    requested: Selection,
) -> WorkstationObservation {
    observation(reply, requested)
}

fn unavailable() -> WorkstationObservation {
    WorkstationObservation::Unavailable(
        "background setup has not published a usable workstation observation".into(),
    )
}

fn subjects(words: &str) -> bool {
    !words.is_empty()
        && words.len() <= 1024
        && words.split(',').all(|word| {
            !word.is_empty()
                && word
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// The settings read privately that `words` names, `None` where one of them is no such row
/// ([`Subject::from_setup_word`], [`Selection::private`]).
fn private_rows(words: &str) -> Option<Selection> {
    let mut rows = Selection::none();
    for word in words.split(',') {
        let Some(Subject::Setting(setting)) = Subject::from_setup_word(word) else {
            return None;
        };
        if !Selection::all().private().contains(setting) {
            return None;
        }
        rows.set(setting, true);
    }
    Some(rows)
}

/// The one value of `key=` in `fields`, `None` for none or several.
fn one<'a>(fields: &[&'a str], key: &str) -> Option<&'a str> {
    let mut values = fields.iter().filter_map(|word| word.strip_prefix(key));
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

/// What the head of `setup=` says: its word and the rows it names.
enum Head<'a> {
    Done,
    /// Everything else is as for `done`; these private rows were read back settled longer ago
    /// than a reading stands, and nothing read them since.
    ReadBack(Selection),
    Pending(Option<&'a str>),
    /// Every other row is settled and no usable reading stands behind these.
    Unread(&'a str),
    /// Every other row is settled and these were asked and not answered, which the coordinator
    /// asks again by itself ([`Standing::Asking`]: the ALab vault off its LAN).
    Asking(&'a str),
}

/// The tails `setup=` carries after its head, each at most once
/// (`aterm_gui`'s `setup_field`).
#[derive(Default)]
struct Tails<'a> {
    needs: Option<&'a str>,
    unread: Option<&'a str>,
    read_back: Option<Selection>,
    asking: Option<&'a str>,
    at_login: Option<Selection>,
    changed: Option<Selection>,
    offered: Option<&'a str>,
}

impl<'a> Tails<'a> {
    /// The tails of `parts` after `head`, `None` for a tail that head does not carry, one said
    /// twice, or one that names what its head cannot (a press of a row not pending, a private
    /// row that is no such row).
    fn of(head: &Head<'_>, parts: impl Iterator<Item = &'a str>) -> Option<Self> {
        let settled = matches!(
            head,
            Head::Done | Head::ReadBack(_) | Head::Unread(_) | Head::Asking(_)
        );
        let pending = matches!(head, Head::Pending(_));
        let mut tails = Self::default();
        for part in parts {
            let (key, words) = part.split_once(':').filter(|(_, words)| subjects(words))?;
            match key {
                "needs" if matches!(head, Head::Pending(Some(_))) && tails.needs.is_none() => {
                    tails.needs = Some(words);
                }
                "unread" if pending && tails.unread.is_none() => tails.unread = Some(words),
                "read-back"
                    if (pending || matches!(head, Head::Unread(_) | Head::Asking(_)))
                        && tails.read_back.is_none() =>
                {
                    tails.read_back = Some(private_rows(words)?);
                }
                "asking" if settled && tails.asking.is_none() => tails.asking = Some(words),
                "at-login" if settled && tails.at_login.is_none() => {
                    tails.at_login = Some(private_rows(words)?);
                }
                "changed" if settled && tails.changed.is_none() => {
                    tails.changed = Some(private_rows(words)?);
                }
                "offered" if tails.offered.is_none() => tails.offered = Some(words),
                _ => return None,
            }
        }
        if let (Some(needs), Head::Pending(pending)) = (tails.needs, head)
            && needs.split(',').any(|word| {
                !pending
                    .unwrap_or_default()
                    .split(',')
                    .any(|pending| pending == word)
            })
        {
            return None;
        }
        Some(tails)
    }
}

/// What `rows=` says of one row (`aterm_gui`'s `RowSaid`): its standing, or that no usable
/// reading stands behind it, or that it is a grant on offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Said {
    Standing(Standing),
    Unread,
    Offered,
}

impl Said {
    fn of(word: &str) -> Option<Self> {
        match word {
            "unread" => Some(Self::Unread),
            "offered" => Some(Self::Offered),
            word => Standing::from_word(word).map(Self::Standing),
        }
    }

    /// Whether a row said so may be verified: read as wanted, or (`differs`) changed since aterm
    /// set it, which no pass sets again.
    const fn settled(self) -> bool {
        matches!(
            self,
            Self::Standing(
                Standing::InPlace | Standing::NotNeeded | Standing::AtLogin | Standing::Differs
            )
        )
    }
}

/// `rows=`: each managed row's word and what is said of it, `None` for `-` with any other word,
/// a word no row has, a row said twice, or a word no standing has.
fn said_rows(words: &str) -> Option<BTreeMap<&'static str, Said>> {
    let mut rows = BTreeMap::new();
    if words == "-" {
        return Some(rows);
    }
    if words.len() > 4096 {
        return None;
    }
    for row in words.split(',') {
        let (subject, said) = row.split_once(':')?;
        let subject = Subject::from_setup_word(subject)?;
        if rows.insert(subject.setup_word(), Said::of(said)?).is_some() {
            return None;
        }
    }
    Some(rows)
}

/// Whether each row `words` names is said `want` in `rows`.
fn all_said(words: Option<&str>, rows: &BTreeMap<&'static str, Said>, want: Said) -> bool {
    words.is_none_or(|words| words.split(',').all(|word| rows.get(word) == Some(&want)))
}

fn observation(reply: &str, requested: Selection) -> WorkstationObservation {
    if reply.lines().count() != 1 || reply.split_ascii_whitespace().next() != Some("OK") {
        return unavailable();
    }
    let fields: Vec<_> = reply.split_ascii_whitespace().collect();
    // The running aterm is an older build than this command (an update put this one on disk
    // before it relaunched): its fields mean what this build no longer reads, and it speaks this
    // schema once it relaunches into this build, which ends by itself.
    match one(&fields, "schema=").and_then(|schema| schema.parse::<u32>().ok()) {
        Some(SETUP_STATUS_SCHEMA) => {}
        Some(older) if older < SETUP_STATUS_SCHEMA => {
            return WorkstationObservation::Waiting(
                "the running aterm is older than this command; it answers once it relaunches \
                 into this build"
                    .into(),
            );
        }
        _ => return unavailable(),
    }
    let (Some(setup), Some(verified), Some(verified_at), Some(generation), Some(rows)) = (
        one(&fields, "setup="),
        one(&fields, "verified="),
        one(&fields, "verified_at="),
        one(&fields, "observed_generation="),
        one(&fields, "rows="),
    ) else {
        return unavailable();
    };
    if generation.parse::<u64>().is_err() {
        return unavailable();
    }
    let Some(rows) = said_rows(rows) else {
        return unavailable();
    };
    // The private rows read back settled, at any age; never a row nothing reads privately.
    let Some(verified) = verified
        .parse::<u16>()
        .ok()
        .and_then(Selection::from_bits)
        .filter(|verified| *verified == verified.private())
    else {
        return unavailable();
    };
    // When the oldest verified row was read: present exactly when a row was verified, and never
    // 0 (a reading with no clock to date it is no reading of any time).
    let read_at = match (verified_at, verified.is_empty()) {
        ("-", true) => None,
        (at, false) => match at.parse::<u64>() {
            Ok(at) if at != 0 => Some(at),
            _ => return unavailable(),
        },
        _ => return unavailable(),
    };
    let mut parts = setup.split(';');
    let head = match parts.next().unwrap_or_default() {
        "done" => Head::Done,
        "pending" => Head::Pending(None),
        word => {
            if let Some(words) = word
                .strip_prefix("pending:")
                .filter(|words| subjects(words))
            {
                Head::Pending(Some(words))
            } else if let Some(rows) = word
                .strip_prefix("read-back:")
                .filter(|words| subjects(words))
                .and_then(private_rows)
            {
                Head::ReadBack(rows)
            } else if let Some(words) = word.strip_prefix("unread:").filter(|words| subjects(words))
            {
                Head::Unread(words)
            } else if let Some(words) = word.strip_prefix("asking:").filter(|words| subjects(words))
            {
                Head::Asking(words)
            } else {
                return unavailable();
            }
        }
    };
    let Some(tails) = Tails::of(&head, parts) else {
        return unavailable();
    };
    // `setup=` and `rows=` are one classification of each row: a row `setup=` says unread, on
    // offer, waiting on a press or read back that `rows=` and `verified` do not say so, or a
    // verified row `rows=` does not read settled, says two things of one reading, which is no
    // evidence of either.
    let unread = match &head {
        Head::Unread(words) => Some(*words),
        _ => tails.unread,
    };
    let read_back = match &head {
        Head::ReadBack(rows) => *rows,
        _ => tails.read_back.unwrap_or_else(Selection::none),
    };
    let verified_said = Setting::ALL
        .into_iter()
        .filter(|setting| verified.contains(*setting))
        .all(|setting| {
            rows.get(Subject::Setting(setting).setup_word())
                .is_some_and(|said| said.settled())
        });
    let asked = match &head {
        Head::Asking(words) => Some(*words),
        _ => None,
    };
    if !all_said(unread, &rows, Said::Unread)
        || !all_said(asked, &rows, Said::Standing(Standing::Asking))
        || !all_said(tails.offered, &rows, Said::Offered)
        || !all_said(tails.needs, &rows, Said::Standing(Standing::NeedsYou))
        || read_back.intersection(verified) != read_back
        || !verified_said
    {
        return unavailable();
    }
    decide(requested, verified, read_at, &rows)
}

/// What `rows=` and `verified` say of the rows `requested` names: each of its private settings
/// verified (read back settled, at `read_at` or later), else what its row says; and, where it
/// selects the Permissions rows, each grant the coordinator manages. A press any of them waits
/// on is a permission, any other step left a wait, a row with no usable reading unread; else
/// they are read back settled, a verified row `rows=` says `differs` named as changed since
/// aterm set it, never as in place, with the grants on offer beside them.
fn decide(
    requested: Selection,
    verified: Selection,
    read_at: Option<u64>,
    rows: &BTreeMap<&'static str, Said>,
) -> WorkstationObservation {
    let mut needs = Vec::new();
    let mut waiting = Vec::new();
    let mut unread = Vec::new();
    let mut read_back = ReadBack::default();
    let mut left = |subject: Subject, said: Option<Said>| match said {
        Some(Said::Standing(Standing::NeedsYou)) => needs.push(subject.setup_word()),
        // A row the coordinator does not manage is one it neither reads nor sets.
        None | Some(Said::Unread) => unread.push(subject.setup_word()),
        Some(Said::Standing(standing)) => {
            waiting.push(format!("{}={}", subject.setup_word(), standing.word()));
        }
        Some(Said::Offered) => {}
    };
    for setting in Setting::ALL
        .into_iter()
        .filter(|setting| requested.private().contains(*setting))
    {
        let subject = Subject::Setting(setting);
        let said = rows.get(subject.setup_word()).copied();
        if verified.contains(setting) {
            read_back.rows.set(setting, true);
            read_back
                .at_login
                .set(setting, said == Some(Said::Standing(Standing::AtLogin)));
            read_back
                .changed
                .set(setting, said == Some(Said::Standing(Standing::Differs)));
        } else {
            left(subject, said);
        }
    }
    if requested.contains(Setting::Permissions) {
        for access in Access::ALL {
            let subject = Subject::Access(access);
            match rows.get(subject.setup_word()).copied() {
                // A grant the coordinator does not manage here is none this plan asks for.
                None | Some(Said::Standing(Standing::InPlace | Standing::NotNeeded)) => {}
                Some(Said::Offered) => read_back.offered.push(access),
                said => left(subject, said),
            }
        }
    }
    read_back.read_at = read_at.filter(|_| !read_back.rows.is_empty());
    if !needs.is_empty() {
        WorkstationObservation::Permission(format!(
            "background setup needs a person's action: {}",
            needs.join(", ")
        ))
    } else if !waiting.is_empty() {
        WorkstationObservation::Waiting(format!(
            "background setup has not read back as wanted: {}",
            waiting.join(", ")
        ))
    } else if !unread.is_empty() {
        WorkstationObservation::Unread {
            rows: unread.join(","),
            read_back,
        }
    } else {
        WorkstationObservation::Complete(read_back)
    }
}

#[cfg(test)]
#[path = "background_observation_tests.rs"]
mod tests;
