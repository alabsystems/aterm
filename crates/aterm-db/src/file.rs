// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SHARED FILES, aterm-db's side (docs/DESIGN-shared-files-2026-11-11.md): which files this
//! build compiles and the collections they project ([`types`]), the materializer that writes
//! their snapshots and faces from this Mac's replica ([`materialize`]): the registry's owner
//! files, the journal ring's Mac files, and the views folded from them ([`limits`]); the
//! publisher that
//! signs the writes this Mac queued ([`publish`]), the write verbs that queue them (`write`,
//! private to this module, and clippy.toml lets only [`command::main_entry`] and
//! [`rename_nick`], a rename of this Mac asked for by name, run them, so an owner act is made
//! only by a verb a person or a program ran by name), and `aterm file` ([`command`]).
//!
//! Reading a shared file is `aterm_file::File::read`, in a crate that cannot reach this one: it
//! answers from the snapshot whatever this Mac's custody is doing. Only the materializer here
//! opens custody, and it turns every refusal into a state word rather than an error.

pub mod command;
pub(crate) mod growth;
pub mod limits;
pub mod materialize;
pub mod publish;
pub mod types;
mod write;

pub use types::{
    ACCOUNTS, Class, Computed, EVENTS, FILES, Fold, LIMITS, LINKS, LOGINS, MACHINES, Store, Type,
    USAGE, find,
};

use aterm_secrets::client::Paths;
use std::path::Path;

pub use write::Env;

/// This Mac's own `lab/machines` row: the live row the last snapshot here marks `here` (the
/// machine this Mac's own facts row binds), with this Mac's queued writes laid over it
/// (`aterm_file::File::read`), read from the local files alone, with no custody opened. `None`
/// where no snapshot is here or it marks no row.
fn here_row(paths: &Paths) -> Option<aterm_file::Row> {
    let read =
        aterm_file::File::at(&root(&crate::generation::original(paths)), &MACHINES.file()).read();
    read.rows()
        .iter()
        .find(|row| row.live && row.cell("here") == "yes")
        .cloned()
}

/// This Mac's own `lab/machines` row, by key ([`here_row`]), as [`rename_nick`]'s write is read
/// against it.
#[must_use]
pub fn here(paths: &Paths) -> Option<String> {
    here_row(paths).map(|row| row.pk)
}

/// The nickname this Mac's own `lab/machines` row holds ([`here_row`]): a value written from any
/// Mac once a pass brought it here, and this Mac's own queued write at once. `None` where no row
/// is this Mac's.
#[must_use]
pub fn here_nickname(paths: &Paths) -> Option<crate::alab::view::MachineNickname> {
    here_row(paths).map(|row| crate::alab::view::MachineNickname::of_file_row(&row))
}

/// A vouched live snapshot's own row and nickname, including queued local edits.
/// This is evidence only: an automatic local write must also admit the current native
/// registry source through its persistence. Retained held or off rows remain display data.
#[must_use]
pub fn live_here_nickname(paths: &Paths) -> Option<(String, crate::alab::view::MachineNickname)> {
    let read =
        aterm_file::File::at(&root(&crate::generation::original(paths)), &MACHINES.file()).read();
    if read.status.word != aterm_file::Word::Live {
        return None;
    }
    let mut rows = read
        .rows()
        .iter()
        .filter(|row| row.live && row.cell("here") == "yes");
    let row = rows.next()?;
    if rows.next().is_some() {
        return None;
    }
    Some((
        row.pk.clone(),
        crate::alab::view::MachineNickname::of_file_row(row),
    ))
}

/// Why the registry's `nickname` column refuses `nick` in the machine row `row`, as `aterm file
/// set` would refuse it (the column's text rule and the value guard), or `None` where it takes
/// it.
#[must_use]
pub fn nick_refusal(row: &str, nick: &str) -> Option<String> {
    let column = crate::alab::registry::MACHINE.column("nickname")?;
    crate::typed::grid::cell_edits(&crate::alab::registry::MACHINE, row, column, nick, None).err()
}

/// A rename of this Mac that a person or a program asked for by name (`aterm name
/// nick|set|undo`, Rename This Mac…, Settings ▸ This Mac, Undo Last Rename): the `lab/machines`
/// row `row` (its key, [`here`]) gets `nick` as its
/// `nickname`, queued and offered to the publisher for `env.attempt` exactly as `aterm file set
/// lab/machines <row> nickname <nick>` queues it, so the registry keeps who wrote it, when, and
/// every earlier value (tracker package FL-144). clippy.toml lets only those verbs run it, as it
/// lets only `aterm file`'s entry run the write verbs: each makes an owner act. Returns the set
/// verb's answer (`published <operation>`, `queued <id>: <why>`, `held <id>: <why>`, or that the
/// cell already says it) and its exit code.
///
/// # Errors
/// Nothing was queued, with the reason: as `aterm file set` refuses (no snapshot of
/// `lab/machines` here, no row `row`, one not published yet, a value the column refuses, an
/// owner act `env.by` cannot name).
pub fn rename_nick(
    paths: &Paths,
    config: &Path,
    row: &str,
    nick: &str,
    env: &Env<'_>,
) -> Result<(String, u8), String> {
    let words = [
        "set".to_owned(),
        MACHINES.name.to_owned(),
        row.to_owned(),
        "nickname".to_owned(),
        nick.to_owned(),
    ];
    #[expect(
        clippy::disallowed_methods,
        reason = "`rename_nick`, the one door a rename of this Mac asked for by name takes: it \
                  runs the explicit `aterm file set` verb on this Mac's own row"
    )]
    let said = write::verb(paths, config, &words, env);
    said.map_err(|refused| match refused {
        write::Refusal::Usage(why) | write::Refusal::Problem(why) => why,
    })
}

/// Where this Mac's shared files live: `~/.aterm/files` of the home `paths` names. A test's
/// stand-in home so keeps its files in its own scratch folder.
#[must_use]
pub fn root(paths: &Paths) -> aterm_file::Root {
    aterm_file::Root::for_home(&paths.home)
}

/// Seams another crate's tests take a stand-in lab's home through: a writer succession its
/// strict readers refuse while a shared file still answers. Absent from release builds.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub mod fixture {
    use aterm_secrets::{client::Paths, commons::bootstrap::Bootstrap};

    /// Fence `paths` under a writer succession whose candidate is never admitted, so it stays
    /// pending (FL-14, ATERM-KI-1109-1): every strict reader of the home refuses with "Commons
    /// writer succession is still preserving old records", and a materializer pass records its
    /// records `held` (`succession`).
    pub fn fence_pending(paths: &Paths, bootstrap: &Bootstrap) {
        crate::generation::file_seams::fence_pending(paths, bootstrap);
    }
}

/// Seams of this crate's tests: how often this thread reached for custody, a step run between
/// a pass's first look at its home's standing and its open of custody, one run between a pass's
/// read and its record, one run where a table view is about to authenticate the records a roll
/// carried, a part a pass's key leaves out, a later build's rules, the roster a pass reads (this
/// Mac's machine, a member that left, a roster that does not read), and the owner act and `set`
/// draft a test outside this module writes with.
#[cfg(test)]
#[path = "file_seam_tests.rs"]
pub(crate) mod seam;

#[cfg(test)]
mod model_read_tests;
#[cfg(test)]
pub(crate) mod model_tests;
#[cfg(test)]
mod tests;
