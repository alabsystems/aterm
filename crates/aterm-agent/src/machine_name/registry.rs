// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! This Mac's nickname in the lab registry (tracker package FL-144, decision FL-144-REGISTRY):
//! once a rename asked for by name (`aterm name nick|set|undo`, Rename This Mac…, Settings ▸
//! This Mac, Undo Last Rename) has committed `machine-name.toml`, [`write_nick`] writes the
//! nickname it holds to this Mac's own `lab/machines` row, as `aterm file set lab/machines <row>
//! nickname <word>` writes it. The registry keeps who wrote each value and when, and every
//! earlier one, and every Mac of the lab reads it: nothing here records an author or a time.
//! [`refusal`] says, before anything changes, why that row would refuse a nickname, so a rename
//! never leaves the file and the row apart for a word the registry cannot hold. A Mac whose last
//! `lab/machines` snapshot marks no row as its own (no lab, the registry off, no binding yet)
//! has only its file. So does a name kept anywhere but this account's live state (a test's
//! scratch folder, a private state root such as a test binary's or a lab instance's): as
//! [`super::ensure`] reads the registry only for the live state, a rename of any other name is
//! no rename of this Mac's row.

use std::path::Path;
use std::time::Duration;

use aterm_net::fleet_member::name_record::ByKind;
use aterm_types::machine_name::{Nickname, Number};

use crate::harness::alab_roster::{self, ReceiptGuard, SourceSnapshot};

/// How long a command gives the publisher to sign its write before it answers `queued`: the
/// bound `aterm file set` gives it.
pub const INLINE: Duration = aterm_db::file::publish::INLINE;

/// What became of a rename's write to this Mac's registry row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Written {
    /// No `lab/machines` row is this Mac's, so only its file holds the nickname.
    Unbound,
    /// The row says this nickname already.
    Unchanged,
    /// The write was queued: `aterm file set`'s answer (published, queued or held).
    Said(String),
    /// Nothing was queued, and why.
    Refused(String),
}

impl Written {
    /// What a person reads of it beside the name, one line, or `None` where there is nothing to
    /// say.
    #[must_use]
    pub fn note(&self) -> Option<String> {
        match self {
            Self::Unbound | Self::Unchanged => None,
            Self::Said(said) => Some(format!("lab registry: {said}")),
            Self::Refused(why) => Some(format!("the lab registry's row was not written: {why}")),
        }
    }
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

/// The nickname a person or a program chose for this Mac's own `lab/machines` row, from any Mac,
/// as the name kept in `state` reads it (tracker package FL-169): the last snapshot with this
/// Mac's queued writes over it, in the order they were queued, so a rename's own write agrees at
/// once and every process reads one value. `Some(None)` where the row's cell is empty, an
/// automatic path wrote it, or it is conflicted or no nickname; `None` where no row is that
/// name's (as for [`write_nick`]).
#[must_use]
pub fn row_nick(state: &Path) -> Option<Option<Nickname>> {
    #[cfg(test)]
    if let Some(row) = ROW.with(|row| row.borrow().clone()) {
        return Some(row.nickname);
    }
    Some(match aterm_db::file::here_nickname(&live(state)?)? {
        aterm_db::alab::view::MachineNickname::Named {
            word,
            automatic: false,
        } => Nickname::parse(&word).ok(),
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
/// Explicit edits keep using [`row_nick`]'s overlay even while publication is pending.
pub(super) fn for_follow(state: &Path) -> Option<Follow> {
    #[cfg(test)]
    if let Some(row) = ROW.with(|row| row.borrow().clone()) {
        return Some(row);
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

#[cfg(test)]
thread_local! {
    /// The row a test's [`row_nick`] reads in place of the registry, while [`with_row`] runs.
    static ROW: std::cell::RefCell<Option<Follow>> = const { std::cell::RefCell::new(None) };
}

/// Run `body` while [`row_nick`] reads this Mac's row as holding the chosen nickname `row`
/// (`None`: none chosen), on this thread.
#[cfg(test)]
pub(crate) fn with_row<T>(row: Option<&str>, body: impl FnOnce() -> T) -> T {
    let row = Follow {
        number: None,
        nickname: row.map(|word| Nickname::parse(word).expect("a nickname")),
        guard: None,
        row: None,
    };
    let held = ROW.with(|cell| cell.replace(Some(row)));
    let out = body();
    ROW.with(|cell| cell.replace(held));
    out
}

/// Why this Mac's registry row would refuse `nick` as the nickname of the name kept in `state`,
/// or `None` where it takes it or no row is that name's.
#[must_use]
pub fn refusal(state: &Path, nick: &Nickname) -> Option<String> {
    let row = own_row(state)?;
    aterm_db::file::nick_refusal(&row, nick.as_str())
}

/// Write `nick`, the nickname the name kept in `state` holds after a rename, to this Mac's
/// registry row (where `state` is this account's live state), as `kind` asked in the aterm
/// session `session`,
/// through the explicit rename door ([`aterm_db::file::rename_nick`]), giving the publisher
/// `attempt` before it answers: [`INLINE`] for a command, nothing for a window, whose own
/// shared-files thread publishes what is queued at its next pass.
#[must_use]
pub fn write_nick(
    state: &Path,
    nick: &Nickname,
    kind: ByKind,
    session: Option<&str>,
    attempt: Duration,
) -> Written {
    write_nick_with(own_row(state).as_deref(), nick, kind, |row, nick, by| {
        let paths = aterm_secrets::client::Paths::live().map_err(|why| why.to_string())?;
        let config = aterm_types::dirs::aterm_config_path()
            .ok_or_else(|| "no configuration folder".to_owned())?;
        let env = aterm_db::file::Env {
            by,
            session,
            now_ms: aterm_time::unix_millis().unwrap_or(0),
            attempt,
            json: false,
            stdin: None,
        };
        #[expect(
            clippy::disallowed_methods,
            reason = "a rename of this Mac asked for by name (`aterm name nick|set|undo`, Rename \
                      This Mac…, Settings ▸ This Mac) writes its own registry row's nickname"
        )]
        let said = aterm_db::file::rename_nick(&paths, &config, row, nick, &env);
        said.map(|(said, _)| said)
    })
}

/// [`write_nick`] for the row `row` (`None`: no row is this Mac's), with `write` taking the
/// row's key, the nickname and the author's word and answering as `aterm file set` does.
pub(crate) fn write_nick_with(
    row: Option<&str>,
    nick: &Nickname,
    kind: ByKind,
    write: impl FnOnce(&str, &str, &str) -> Result<String, String>,
) -> Written {
    let Some(row) = row else {
        return Written::Unbound;
    };
    match write(row, nick.as_str(), by_word(kind)) {
        Ok(said) if said.starts_with("unchanged") => Written::Unchanged,
        Ok(said) => Written::Said(said.lines().map(str::trim).collect::<Vec<_>>().join("; ")),
        Err(why) => Written::Refused(why),
    }
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
