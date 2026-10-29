// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm ctl --host any spawn agent=<claude|codex> -`: the fleet Mac a worker goes to, picked
//! from one survey of every member by the accounts it can run the worker on.

use super::survey::{self, Fields, PeerSurvey};

/// The word `--host` takes for whichever member can take a worker.
pub(crate) const ANY: &str = "any";

/// The programs whose account rows a pick weighs: an `agent=` word and the `pool=` of the rows
/// it runs on.
pub(crate) const PROGRAMS: [&str; 2] = ["claude", "codex"];

/// The conditions a row takes a session in (`usable`, `unseen` and `low`, as the harness's
/// `Condition::runs`).
const RUNS: [&str; 3] = ["usable", "unseen", "low"];

/// The condition of a row a wall stands on (`Condition::Limited`), until its `walled_until_ms`.
const LIMITED: &str = "limited";

/// One account row of a member's survey, as a pick weighs it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Row {
    /// Its pool's program (`pool=`).
    pub(crate) program: String,
    /// Its label (`id=`).
    pub(crate) label: String,
    /// Its login is made there (`enrolled=1`).
    pub(crate) enrolled: bool,
    /// Its condition word (`condition=`).
    pub(crate) condition: String,
    /// The five-hour window used, in percent; `None` where no window runs (it reset, or was never
    /// read).
    pub(crate) five_hour: Option<u64>,
    /// The week used, in percent, as [`Self::five_hour`].
    pub(crate) seven_day: Option<u64>,
    /// When its wall ends, in Unix milliseconds on the member's clock (`walled_until=`, in
    /// seconds).
    pub(crate) walled_until_ms: Option<u64>,
}

impl Row {
    fn of(fields: &Fields) -> Self {
        let number = |key: &str| fields.get(key).and_then(|v| v.parse().ok());
        Self {
            program: fields.get("pool").unwrap_or_default().to_owned(),
            label: fields.get("id").unwrap_or_default().to_owned(),
            enrolled: fields.get("enrolled") == Some("1"),
            condition: fields.get("condition").unwrap_or_default().to_owned(),
            five_hour: number("pct5"),
            seven_day: number("pct7"),
            walled_until_ms: survey::walled_until_ms(fields),
        }
    }

    /// Whether a worker of `program` can run on it now: a row of that program, enrolled, in a
    /// condition that takes a session, or `limited` by a wall that ended by `at_ms` (the member's
    /// clock, so no clock between the two Macs is compared: its rows may be read up to a minute
    /// before it answers; a wall with no clock to judge it by stands).
    fn takes(&self, program: &str, at_ms: Option<u64>) -> bool {
        let ended = || {
            self.walled_until_ms
                .zip(at_ms)
                .is_some_and(|(until, at)| until <= at)
        };
        self.program == program
            && self.enrolled
            && (RUNS.contains(&self.condition.as_str()) || (self.condition == LIMITED && ended()))
    }
}

/// One member a survey of the fleet answered, as a pick weighs it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Member {
    /// The node its book entry names, which the spawn dials it by (`--host <node>`).
    pub(crate) node: Option<String>,
    /// It proved the key this Mac binds that node to.
    pub(crate) proven: bool,
    /// How the read named it.
    pub(crate) label: String,
    /// Its host, as its own facts say it (`host=`).
    pub(crate) host: Option<String>,
    /// The address its answer came from.
    pub(crate) addr: Option<String>,
    /// It lets this Mac drive it (`drive=1` in its instance facts).
    pub(crate) drive: bool,
    /// Its clock when it answered (`at_ms=`).
    pub(crate) at_ms: Option<u64>,
    /// Its account rows, `None` where its aterm says nothing of them (one older than the field).
    pub(crate) accounts: Option<Vec<Row>>,
    /// The pools whose rows it could not read (`accounts_unread=`).
    pub(crate) unread: Vec<String>,
    /// Its sessions at work (the survey's `busy`).
    pub(crate) busy: usize,
}

impl Member {
    /// What a person reads it as: its own host, else the name the read reached it by.
    pub(crate) fn host(&self) -> &str {
        self.host.as_deref().unwrap_or(&self.label)
    }

    /// Whether a spawn may go to it at all: it proved its node, and it lets this Mac drive it.
    fn drivable(&self) -> bool {
        self.proven && self.node.is_some() && self.drive
    }

    /// Its row that a worker of `program` would best run on: the least used week, then the least
    /// used five hours ([`used`]), then by label.
    fn best(&self, program: &str) -> Option<&Row> {
        self.accounts
            .iter()
            .flatten()
            .filter(|row| row.takes(program, self.at_ms))
            .min_by(|a, b| {
                (used(a.seven_day), used(a.five_hour), &a.label).cmp(&(
                    used(b.seven_day),
                    used(b.five_hour),
                    &b.label,
                ))
            })
    }
}

/// A window's use as a pick ranks it: a window with no figure is unused, since its member sends
/// none once it has reset or where it was never read (a row the harness reads as fresh).
fn used(pct: Option<u64>) -> u64 {
    pct.unwrap_or(0)
}

/// The members each answered survey of `peers` is, in their order; one that did not answer, or
/// answered no survey, is left out.
pub(crate) fn members_of(peers: &[PeerSurvey]) -> Vec<Member> {
    peers
        .iter()
        .filter_map(|peer| {
            let lines = peer.answer.as_ref().ok()?;
            let survey = survey::parse_reply(lines).ok()?;
            let header = &survey.header;
            Some(Member {
                node: peer.node.clone(),
                proven: !peer.unproven,
                label: peer.label.clone(),
                host: header.get("host").map(str::to_owned),
                addr: peer.addr.clone(),
                drive: header.get("drive") == Some("1"),
                at_ms: header.get("at_ms").and_then(|ms| ms.parse().ok()),
                accounts: survey
                    .accounts
                    .as_ref()
                    .map(|rows| rows.iter().map(Row::of).collect()),
                unread: survey.unread.clone(),
                busy: survey::busy_sessions(&survey),
            })
        })
        .collect()
}

/// The member a pick chose, the row it ranked it by, and how many could take the worker.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Chosen<'a> {
    /// The member.
    pub(crate) member: &'a Member,
    /// Its best row ([`Member::best`]).
    pub(crate) row: &'a Row,
    /// The members that could take it, this one among them.
    pub(crate) able: usize,
}

/// Why a pick chose no member: how far the members it read got.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    /// Members that answered.
    pub(crate) answered: usize,
    /// Of those, the ones that proved their node and let this Mac drive them.
    pub(crate) drivable: usize,
    /// Of those, the ones whose aterm says nothing of its accounts.
    pub(crate) silent: usize,
    /// Of those, the ones whose aterm could not read the program's pool (`accounts_unread=`).
    pub(crate) unread: usize,
}

/// The member of `members` a worker of `program` goes to: of those that let this Mac drive them
/// and have a row it can run on now, the one whose best row ([`Member::best`]) has used the least
/// of its week, then of its five hours, then the one with the fewest sessions at work, then the
/// first by host (and node). Never one that does not let this Mac drive it.
/// # Errors
/// No member qualifies: what the members it read were.
pub(crate) fn pick<'a>(program: &str, members: &'a [Member]) -> Result<Chosen<'a>, Counts> {
    let able: Vec<(&Member, &Row)> = members
        .iter()
        .filter(|member| member.drivable())
        .filter_map(|member| Some((member, member.best(program)?)))
        .collect();
    let chosen = able.iter().min_by(|(a, ra), (b, rb)| {
        (
            used(ra.seven_day),
            used(ra.five_hour),
            a.busy,
            a.host().to_ascii_lowercase(),
            &a.node,
        )
            .cmp(&(
                used(rb.seven_day),
                used(rb.five_hour),
                b.busy,
                b.host().to_ascii_lowercase(),
                &b.node,
            ))
    });
    match chosen {
        Some((member, row)) => Ok(Chosen {
            member,
            row,
            able: able.len(),
        }),
        None => {
            let drivable: Vec<&Member> = members.iter().filter(|m| m.drivable()).collect();
            let count = |of: &dyn Fn(&Member) -> bool| drivable.iter().filter(|m| of(m)).count();
            Err(Counts {
                answered: members.len(),
                drivable: drivable.len(),
                silent: count(&|m| m.accounts.is_none()),
                unread: count(&|m| {
                    m.accounts.is_some() && m.unread.iter().any(|pool| pool == program)
                }),
            })
        }
    }
}

/// Why the pick chose `chosen` for `program`: its row's figures, its busy sessions, and of how
/// many.
pub(crate) fn why(program: &str, chosen: &Chosen<'_>) -> String {
    let row = chosen.row;
    let mut figures = String::new();
    for (window, pct) in [("7d", row.seven_day), ("5h", row.five_hour)] {
        if let Some(pct) = pct {
            figures.push_str(&format!(" {window} {pct}%"));
        }
    }
    if figures.is_empty() {
        figures = if row.condition == LIMITED {
            " its wall ended".to_owned()
        } else {
            format!(" {}", aterm_codec::tty::one_line(&row.condition, 16))
        };
    }
    let busy = match chosen.member.busy {
        1 => "1 busy session".to_owned(),
        n => format!("{n} busy sessions"),
    };
    format!(
        "{program} account {}{figures}, {busy}; the best of {} that can take it",
        aterm_codec::tty::one_line(&survey::masked_label(&row.label), 32),
        chosen.able
    )
}

/// The refusal of a pick for `program` that chose no member, with how far the `counts` got and
/// the `alone` reason the read gave for reading no other machine.
pub(crate) fn none_able(program: &str, counts: &Counts, alone: Option<&str>) -> String {
    if counts.answered == 0 {
        return format!(
            "--host any: no Mac can take agent={program}: no fleet member answered{}",
            alone.map_or_else(String::new, |why| format!(" ({why})"))
        );
    }
    let mut asides = Vec::new();
    if counts.silent > 0 {
        asides.push(format!(
            "{} of those run an aterm too old to say its accounts",
            counts.silent
        ));
    }
    if counts.unread > 0 {
        asides.push(format!(
            "{} of those could not read their {program} account records",
            counts.unread
        ));
    }
    let asides = if asides.is_empty() {
        String::new()
    } else {
        format!(" ({})", asides.join("; "))
    };
    format!(
        "--host any: no Mac can take agent={program}: {} members answered, {} of them let this \
         Mac drive them (`aterm fleet allow drive --from <this Mac>` there), and none of those has \
         an enrolled, unwalled {program} account{asides}",
        counts.answered, counts.drivable
    )
}

#[cfg(test)]
#[path = "any_host_tests.rs"]
mod tests;
