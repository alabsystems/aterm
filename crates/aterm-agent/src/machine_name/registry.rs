// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! This Mac's name in the lab registry (decision FL-144-REGISTRY): a rename asked for by name
//! (`aterm name nick|set|undo`, Rename This Mac…, Settings ▸ This Mac, Undo Last Rename) queues
//! what it leaves in this Mac's own `lab/machines` row, the nickname (tracker package FL-144) and
//! a number it moves to (FL-170), as one write before `machine-name.toml` commits
//! ([`queue_rename`], FL-194), as `aterm file set lab/machines <row> nickname=<word>
//! label=<number>` queues it, so a row that would refuse either refuses the rename and one
//! operation holds it; the command that made it publishes it ([`offer`]). [`row_nick`] and
//! [`row_label`] read what that row was given from any Mac, for the name to follow (FL-169,
//! FL-170). The registry keeps who wrote each value and when, and every earlier one, and every
//! Mac of the lab reads it: nothing here records an author or a time. A Mac whose last
//! `lab/machines` snapshot marks no row as its own (no lab, the registry off, no binding yet)
//! has only its file. So does a name kept anywhere but this account's live state (a test's
//! scratch folder, a private state root such as a test binary's or a lab instance's): as
//! [`super::ensure`] reads the registry only for the live state, a rename of any other name is
//! no rename of this Mac's row.

use std::path::Path;
use std::time::Duration;

pub use aterm_db::file::Queued;
use aterm_db::file::Renamed;
use aterm_net::fleet_member::name_record::ByKind;
use aterm_types::machine_name::{Nickname, Number};

use crate::harness::alab_roster::{self, ReceiptGuard, SourceSnapshot};

/// How long a command gives the publisher to sign its write before it answers `queued`: the
/// bound `aterm file set` gives it.
pub const INLINE: Duration = aterm_db::file::publish::INLINE;

/// What became of a rename's write to this Mac's registry row ([`queue_rename`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Written {
    /// No `lab/machines` row is this Mac's, so only its file holds the name.
    Unbound,
    /// The row's cells say these values already.
    Unchanged,
    /// The write was queued, as this intent ([`offer`] publishes it).
    Queued(Queued),
    /// Nothing was queued, and why.
    Refused(String),
}

/// The word an owner act names its author by: a person, or a program.
const fn by_word(kind: ByKind) -> &'static str {
    match kind {
        ByKind::Person => "person",
        ByKind::Agent | ByKind::Auto => "program",
    }
}

/// The live vault home's paths where `state` is this account's live naming state; `None` for a
/// name kept anywhere else, whose registry row this is not.
fn live(state: &Path) -> Option<aterm_secrets::client::Paths> {
    if aterm_types::dirs::runs_under_private_state_root()
        || aterm_types::dirs::state_dir().as_deref() != Some(state)
    {
        return None;
    }
    aterm_secrets::client::Paths::live().ok()
}

/// This Mac's own `lab/machines` row, by key, as its last snapshot marks it, where `state` is
/// this account's live naming state; `None` for a name kept anywhere else.
fn own_row(state: &Path) -> Option<String> {
    aterm_db::file::here(&live(state)?)
}

/// This Mac's own `lab/machines` row as the last pass published it, where `state` is this
/// account's live naming state, with the cells a write of this Mac's has not reached yet
/// ([`aterm_db::file::here_published`]): a follow reads only what the registry settled. A write
/// of its nickname or number the publisher held, because another writer's value reached the
/// cell first, gives way here once the row shows that value
/// ([`aterm_db::file::HereRow::held`]; until a pass brings it the cell stays pending): the row's
/// value stands, the held write is dropped ([`aterm_db::file::yield_held`]) and journaled, and
/// the name follows the row. No person is asked to choose.
fn here(state: &Path) -> Option<aterm_db::file::HereRow> {
    let paths = live(state)?;
    let here = aterm_db::file::here_published(&paths)?;
    let held = here.held().to_vec();
    // A retained snapshot (held, off, unvouched) decides nothing: its row may not be the
    // registry's now.
    if held.is_empty() || !here.live() {
        return Some(here);
    }
    let Some(config) = aterm_types::dirs::aterm_config_path() else {
        return Some(here);
    };
    for aterm_db::file::HeldName { id, cells } in held {
        let asked = cells
            .iter()
            .map(|(column, value)| format!("{column} {value}"))
            .collect::<Vec<_>>()
            .join(" and ");
        let env = aterm_db::file::Env {
            by: by_word(ByKind::Auto),
            session: None,
            now_ms: aterm_time::unix_millis().unwrap_or(0),
            attempt: Duration::ZERO,
            json: false,
            stdin: None,
        };
        #[expect(
            clippy::disallowed_methods,
            reason = "this Mac's held write of its own registry row's nickname or number gives \
                      way to the values the row holds, which the name follows"
        )]
        let dropped = aterm_db::file::yield_held(&paths, &config, &id, &env);
        match dropped {
            Ok(()) => aterm_log::info!(
                "machine name: this Mac's write of {asked} to its lab registry row met another \
                 writer's value; the row's values stand and this Mac follows them (dropped {id})"
            ),
            Err(why) => aterm_log::info!(
                "machine name: this Mac's held write {id} of {asked} to its lab registry row \
                 was not dropped ({why})"
            ),
        }
    }
    aterm_db::file::here_published(&paths)
}

/// The nickname a person or a program chose for this Mac's own `lab/machines` row, from any Mac,
/// as the name kept in `state` reads it (tracker package FL-169), from the published row
/// ([`here`]). `Some(None)` where the cell is empty, an automatic path wrote it, or it is
/// conflicted or no nickname; `None` where no row is that name's (as for [`queue_rename`]) or a write
/// of the cell this Mac made is not published yet, so a follow waits for it.
#[must_use]
pub fn row_nick(state: &Path) -> Option<Option<Nickname>> {
    #[cfg(test)]
    if let Some(seam) = ROW.with(|row| row.borrow().clone()) {
        return seam.follow.map(|follow| follow.nickname);
    }
    let here = here(state)?;
    if here.pending(Renamed::Nickname) {
        return None;
    }
    Some(match here.nickname() {
        aterm_db::alab::view::MachineNickname::Named {
            word,
            automatic: false,
        } => Nickname::parse(&word).ok(),
        _ => None,
    })
}

/// The number an owner wrote to this Mac's own `lab/machines` row's `label` from any Mac, as the
/// name kept in `state` reads it (tracker package FL-170), from the published row ([`here`]),
/// where the column's claim rule gives it to this row. `Some(None)` where the cell is empty, an
/// automatic path or an import wrote it, another row won it, or it is conflicted or no number;
/// `None` as for [`row_nick`], and while the claim is undecided or the snapshot predates the
/// claim column, so a follow waits for the claim rule's answer. Like [`row_nick`] it is what the
/// files here show, a retained snapshot's included: it spares work and decides what an asked
/// rename queues, and authorizes no automatic rename ([`for_number`]).
#[must_use]
pub fn row_label(state: &Path) -> Option<Option<Number>> {
    #[cfg(test)]
    if let Some(seam) = ROW.with(|row| row.borrow().as_ref().map(|seam| seam.label)) {
        return seam;
    }
    let here = here(state)?;
    if here.pending(Renamed::Label) {
        return None;
    }
    owner_number(&here.label())
}

/// The owner's number `label` gives its row: `Some(None)` where it gives none, `None` while the
/// claim is undecided ([`row_label`]).
fn owner_number(label: &aterm_db::alab::view::MachineLabel) -> Option<Option<Number>> {
    Some(match label {
        aterm_db::alab::view::MachineLabel::Named {
            label,
            automatic: false,
            claim,
        } => match claim {
            None => return None,
            Some(claim) if claim == label => Number::parse(label)
                .ok()
                .filter(|num| num.to_string() == *label),
            Some(_) => None,
        },
        _ => None,
    })
}

/// One local follow decision. Its observation alone cannot authorize persistence.
#[derive(Clone)]
pub(super) struct Follow {
    number: Option<Number>,
    pub(super) nickname: Option<Nickname>,
    guard: Option<ReceiptGuard>,
    row: Option<(
        aterm_secrets::client::Paths,
        String,
        aterm_db::alab::view::MachineNickname,
    )>,
}

impl Follow {
    pub(super) fn names(&self, number: Option<Number>) -> bool {
        #[cfg(test)]
        if self.guard.is_none() {
            return true;
        }
        self.number.is_some() && self.number == number
    }

    pub(super) fn with_admitted<T>(
        &self,
        persist: impl FnOnce() -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        #[cfg(test)]
        if self.guard.is_none() {
            return persist();
        }
        self.guard
            .as_ref()
            .ok_or_else(|| std::io::Error::other("the registry follow has no native admission"))?
            .with_admitted(|| {
                if let Some((paths, key, nickname)) = &self.row
                    && aterm_db::file::live_here_nickname(paths)
                        != Some((key.clone(), nickname.clone()))
                {
                    return Err("the live machine nickname changed before admission".into());
                }
                persist().map_err(|why| why.to_string().into())
            })
            .map_err(|why| std::io::Error::other(why.to_string()))
    }
}

/// Automatic following requires a live, vouched file row matching the captured own binding.
/// The host's guard re-admits the full native source, membership and policy through the write.
/// Explicit edits keep using [`row_nick`] even while publication is pending.
pub(super) fn for_follow(state: &Path) -> Option<Follow> {
    #[cfg(test)]
    if let Some(seam) = ROW.with(|row| row.borrow().clone()) {
        return seam.follow;
    }
    let paths = live(state)?;
    let config = aterm_types::dirs::aterm_config_path()?;
    let captured = alab_roster::capture_for(&config);
    let SourceSnapshot::Present(lab) = captured.snapshot() else {
        return None;
    };
    let own = lab.me.as_ref()?;
    let name = lab.machine_name.as_ref()?.as_ref().ok()?;
    if own.label.as_deref() != Some(name.num.to_string().as_str()) {
        return None;
    }
    let (row, observed) = aterm_db::file::live_here_nickname(&paths)?;
    if row != own.key.as_str() {
        return None;
    }
    let nickname = match &observed {
        aterm_db::alab::view::MachineNickname::Named {
            word,
            automatic: false,
        } => Some(Nickname::parse(word).ok()?),
        aterm_db::alab::view::MachineNickname::Conflict => return None,
        _ => None,
    };
    let expected = name.nick.as_ref().and_then(|(nick, class)| {
        (*class == aterm_net::fleet_member::name_record::NickClass::Chosen).then_some(nick)
    });
    if nickname.as_ref() != expected {
        return None;
    }
    Some(Follow {
        number: Some(name.num),
        nickname,
        guard: Some(captured.native_guard().ok()?.clone()),
        row: Some((paths, row, observed)),
    })
}

/// One local decision to take the row's number as this Mac's (tracker package FL-170). Like
/// [`Follow`], its observation alone cannot authorize persistence.
#[derive(Clone)]
pub(super) struct Renumber {
    pub(super) number: Number,
    guard: Option<ReceiptGuard>,
    row: Option<(
        aterm_secrets::client::Paths,
        String,
        aterm_db::alab::view::MachineLabel,
    )>,
}

impl Renumber {
    /// Run `persist`, the write of the renumbered name, under the native source's admission,
    /// where this Mac's live row still holds the label this decision read with no write of this
    /// Mac's on its way.
    pub(super) fn with_admitted<T>(
        &self,
        persist: impl FnOnce() -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        #[cfg(test)]
        if self.guard.is_none() {
            return persist();
        }
        self.guard
            .as_ref()
            .ok_or_else(|| std::io::Error::other("the registry follow has no native admission"))?
            .with_admitted(|| {
                if let Some((paths, key, label)) = &self.row
                    && !aterm_db::file::here_published(paths).is_some_and(|here| {
                        here.live()
                            && here.pk() == key
                            && !here.pending(Renamed::Label)
                            && here.label() == *label
                    })
                {
                    return Err("the live machine label changed before admission".into());
                }
                persist().map_err(|why| why.to_string().into())
            })
            .map_err(|why| std::io::Error::other(why.to_string()))
    }
}

/// What authorizes taking the row's number ([`super::follow_number`]): as [`for_follow`], a
/// live, vouched file row that is the captured source's own binding, whose label is an owner's
/// number the claim rule gives this row ([`row_label`]) and the number the captured source
/// names this Mac by. The host's guard re-admits the native source, membership and policy
/// through the write; a held, off, revoked or unvouched snapshot renumbers nothing.
pub(super) fn for_number(state: &Path) -> Option<Renumber> {
    #[cfg(test)]
    if let Some(seam) = ROW.with(|row| row.borrow().clone()) {
        return seam.label.flatten().map(|number| Renumber {
            number,
            guard: seam.admission,
            row: None,
        });
    }
    let paths = live(state)?;
    let config = aterm_types::dirs::aterm_config_path()?;
    let captured = alab_roster::capture_for(&config);
    let SourceSnapshot::Present(lab) = captured.snapshot() else {
        return None;
    };
    let own = lab.me.as_ref()?;
    let name = lab.machine_name.as_ref()?.as_ref().ok()?;
    if own.label.as_deref() != Some(name.num.to_string().as_str()) {
        return None;
    }
    let here = here(state)?;
    if !here.live() || here.pk() != own.key.as_str() || here.pending(Renamed::Label) {
        return None;
    }
    let label = here.label();
    let number = owner_number(&label).flatten()?;
    if number != name.num {
        return None;
    }
    Some(Renumber {
        number,
        guard: Some(captured.native_guard().ok()?.clone()),
        row: Some((paths, here.pk().to_owned(), label)),
    })
}

/// What a test's [`row_nick`], [`for_follow`], [`row_label`] and [`for_number`] answer in place
/// of the registry.
#[cfg(test)]
#[derive(Clone, Default)]
pub(super) struct Seam {
    /// The nickname follow: [`row_nick`] answers its nickname, and nothing where it is `None`.
    pub(super) follow: Option<Follow>,
    /// What [`row_label`] answers; [`for_number`] decides for a number it names.
    pub(super) label: Option<Option<Number>>,
    /// The guard [`for_number`]'s decision carries (`None`: a test's, which admits).
    pub(super) admission: Option<ReceiptGuard>,
}

#[cfg(test)]
thread_local! {
    /// What a test's row reads answer, while [`with_row`] or [`with_label`] runs.
    pub(super) static ROW: std::cell::RefCell<Option<Seam>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn with_seam<T>(seam: Seam, body: impl FnOnce() -> T) -> T {
    let held = ROW.with(|cell| cell.replace(Some(seam)));
    let out = body();
    ROW.with(|cell| cell.replace(held));
    out
}

/// Run `body` while [`row_nick`] reads this Mac's row as holding the chosen nickname `row`
/// (`None`: none chosen), and [`row_label`] answers nothing, on this thread.
#[cfg(test)]
pub(crate) fn with_row<T>(row: Option<&str>, body: impl FnOnce() -> T) -> T {
    let follow = Follow {
        number: None,
        nickname: row.map(|word| Nickname::parse(word).expect("a nickname")),
        guard: None,
        row: None,
    };
    with_seam(
        Seam {
            follow: Some(follow),
            ..Seam::default()
        },
        body,
    )
}

/// Run `body` while [`row_label`] reads this Mac's row as holding the owner's number `label`
/// (`None`: none to take), or with `pending`, a cell a write of this Mac's own still changes
/// (or a held one waits for the row to show what it met), and [`row_nick`] answers nothing, on
/// this thread.
#[cfg(test)]
pub(crate) fn with_label<T>(label: Option<&str>, pending: bool, body: impl FnOnce() -> T) -> T {
    let num = label.map(|label| Number::parse(label).expect("a number"));
    with_seam(
        Seam {
            label: (!pending).then_some(num),
            ..Seam::default()
        },
        body,
    )
}

#[cfg(test)]
thread_local! {
    /// What a test's [`queue_rename`] answers, and the writes it was asked to queue (each as its
    /// cells, `<column>=<value>`), while [`with_door`] runs.
    static DOOR: std::cell::RefCell<Option<(Written, Vec<Vec<String>>)>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `body` while [`queue_rename`] answers `answer` on this thread, with the writes it was
/// asked to queue, each as its cells (`<column>=<value>`).
#[cfg(test)]
pub(crate) fn with_door<T>(answer: Written, body: impl FnOnce() -> T) -> (T, Vec<Vec<String>>) {
    let held = DOOR.with(|door| door.replace(Some((answer, Vec::new()))));
    let out = body();
    let (_, asked) = DOOR.with(|door| door.replace(held)).expect("the door seam");
    (out, asked)
}

/// Queue `cells`, what a rename of the name kept in `state` leaves in this Mac's registry row
/// (where `state` is this account's live state): its nickname (tracker package FL-144) and a
/// number it moves to (FL-170), as one write (FL-194), as `kind` asked in the aterm session
/// `session`, through the explicit rename door ([`aterm_db::file::rename`]), while the rename
/// holds the naming lock and before the file holds them. The queue runs no publisher
/// ([`aterm_db::file::Env::attempt`] zero), which takes no custody under that lock; the command
/// that made the rename publishes it ([`offer`]), and a window's own shared-files thread or a
/// later pass does otherwise. Cells the row says already are left out; a row that says them all
/// answers [`Written::Unchanged`].
#[must_use]
pub fn queue_rename(
    state: &Path,
    cells: &[(Renamed, String)],
    kind: ByKind,
    session: Option<&str>,
) -> Written {
    #[cfg(test)]
    if let Some(answer) = DOOR.with(|door| {
        door.borrow_mut().as_mut().map(|(answer, asked)| {
            asked.push(
                cells
                    .iter()
                    .map(|(cell, value)| format!("{}={value}", cell.column()))
                    .collect(),
            );
            answer.clone()
        })
    }) {
        return answer;
    }
    write_with(own_row(state).as_deref(), cells, kind, |row, cells, by| {
        let paths = aterm_secrets::client::Paths::live().map_err(|why| why.to_string())?;
        let config = aterm_types::dirs::aterm_config_path()
            .ok_or_else(|| "no configuration folder".to_owned())?;
        let env = aterm_db::file::Env {
            by,
            session,
            now_ms: aterm_time::unix_millis().unwrap_or(0),
            attempt: Duration::ZERO,
            json: false,
            stdin: None,
        };
        #[expect(
            clippy::disallowed_methods,
            reason = "a rename of this Mac asked for by name (`aterm name nick|set|undo`, Rename \
                      This Mac…, Settings ▸ This Mac) writes its own registry row"
        )]
        let said = aterm_db::file::rename(&paths, &config, row, cells, &env);
        said
    })
}

/// Why this Mac's registry row would refuse `cells`, what a rename of the name kept in `state`
/// would leave there, as [`queue_rename`] would be refused for the column's rule
/// ([`aterm_db::file::cell_refusal`]); `None` where it takes them or no row is that name's. A
/// dry run asks this, queueing nothing.
#[must_use]
pub fn refusal(state: &Path, cells: &[(Renamed, String)]) -> Option<String> {
    #[cfg(test)]
    if let Some(answer) = DOOR.with(|door| door.borrow().as_ref().map(|(answer, _)| answer.clone()))
    {
        return match answer {
            Written::Refused(why) => Some(why),
            _ => None,
        };
    }
    let row = own_row(state)?;
    cells.iter().find_map(|(cell, value)| {
        aterm_db::file::cell_refusal(&row, *cell, value)
            .map(|why| format!("{}: {why}", cell.column()))
    })
}

/// Offer what this Mac queued for its registry row to the publisher for [`INLINE`], where `state`
/// is this account's live naming state ([`aterm_db::file::offer`]), and say what became of the
/// rename's write `queued` ([`queue_rename`]), one line a person reads beside the name; `None`
/// for a name kept anywhere else.
#[must_use]
pub fn offer(state: &Path, queued: &Queued) -> Option<String> {
    let paths = live(state)?;
    let config = aterm_types::dirs::aterm_config_path()?;
    let said = aterm_db::file::offer(
        &paths,
        &config,
        queued,
        INLINE,
        aterm_time::unix_millis().unwrap_or(0),
    );
    Some(note(&said))
}

/// `said`, a write verb's answer of one or more lines, as one line a person reads beside the
/// name.
fn note(said: &str) -> String {
    format!(
        "lab registry: {}",
        said.lines().map(str::trim).collect::<Vec<_>>().join("; ")
    )
}

/// [`queue_rename`] for the row `row` (`None`: no row is this Mac's), with `write` taking the
/// row's key, the cells and the author's word and answering as [`aterm_db::file::rename`] does.
pub(crate) fn write_with(
    row: Option<&str>,
    cells: &[(Renamed, String)],
    kind: ByKind,
    write: impl FnOnce(&str, &[(Renamed, &str)], &str) -> Result<Option<Queued>, String>,
) -> Written {
    let Some(row) = row else {
        return Written::Unbound;
    };
    let cells: Vec<(Renamed, &str)> = cells
        .iter()
        .map(|(cell, value)| (*cell, value.as_str()))
        .collect();
    match write(row, &cells, by_word(kind)) {
        Ok(Some(queued)) => Written::Queued(queued),
        Ok(None) => Written::Unchanged,
        Err(why) => Written::Refused(why),
    }
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
