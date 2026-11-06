// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The menu bar's machine title, `m8 Heron` right of the version
//! (`docs/DESIGN-machine-names-2026-11-04.md` §5.1). A reader thread makes this machine's name
//! where none is there yet ([`aterm_agent::machine_name::ensure_in_view`]), reads it again when
//! `machine-name.toml` or the fleet key changes, and posts [`Wake::MachineName`]. It hears a change
//! through [`FileWatch`] (kqueue on each file and its folder, so a replace by rename is heard),
//! so at rest it wakes nothing; the window thread only sets titles, so opening the menu reads no
//! file.

use std::io;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use amacs_files::watch::FileWatch;
use aterm_window::event_loop::EventLoopProxy;

use crate::Wake;

/// What the title and the menu's first row show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MachineView {
    /// The NAME: `m8`, or `m0` while unnumbered.
    pub(crate) name: String,
    /// The nickname as the menu bar shows it (`Heron`).
    pub(crate) nick: String,
    /// This machine holds a fleet key that reads.
    pub(crate) fleet: bool,
}

/// The most scalars of a nickname the title shows: a longer one keeps one fewer and `…`.
pub(crate) const NICK_SHOWN: usize = 16;

/// How long the reader waits after a read that failed before it reads again, where no change
/// comes first.
const RETRY_AFTER: Duration = Duration::from_secs(60);

/// `m8 Heron`: the NAME, a space and the nickname clipped to [`NICK_SHOWN`] scalars. It carries
/// no glyph, colour or count, and changes only with the NAME or nickname.
pub(crate) fn machine_menu_bar_title(view: &MachineView) -> String {
    let nick = clipped(&view.nick, NICK_SHOWN);
    if nick.is_empty() {
        view.name.clone()
    } else {
        format!("{} {nick}", view.name)
    }
}

/// The menu's first row: `m8 · Heron · in a fleet`, or `· not in a fleet`.
pub(crate) fn machine_menu_info_row(view: &MachineView) -> String {
    let fleet = if view.fleet {
        "in a fleet"
    } else {
        "not in a fleet"
    };
    format!("{} \u{00B7} {} \u{00B7} {fleet}", view.name, view.nick)
}

fn clipped(text: &str, most: usize) -> String {
    if text.chars().count() <= most {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(most.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

/// Start the reader thread that posts [`Wake::MachineName`], once the menu bar is installed.
pub(crate) fn spawn(proxy: EventLoopProxy<Wake>) {
    let Some(state) = aterm_types::dirs::state_dir() else {
        aterm_log::info!("machine name: no state directory, so no title");
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("aterm-machine-name".to_owned())
        .spawn(move || {
            let post = |view| proxy.send_event(Wake::MachineName(view)).is_ok();
            let roster = aterm_agent::machine_name::vault_roster_label;
            match watch(&state) {
                Ok((_watch, changes)) => {
                    read_loop(&state, roster, post, |timeout| heard(&changes, timeout));
                }
                Err(e) => {
                    aterm_log::info!(
                        "machine name: no file watch ({e}), so the title is read at launch only"
                    );
                    read_loop(&state, roster, post, unwatched);
                }
            }
        });
    if let Err(e) = spawned {
        aterm_log::info!("machine name: no reader thread ({e}), so no title");
    }
}

/// What a wait between reads heard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Heard {
    /// A watched file changed, or the watch took its first look at one.
    Change,
    /// The wait's timeout passed with no change.
    Quiet,
    /// Nothing more can be heard: the watch ended.
    Ended,
}

/// The watch on the files the title comes from, `machine-name.toml` and the fleet key: `changes`
/// hears one `()` for each whose stamp (device, inode, length, modification and change times)
/// moved, and one at the watch's first look at each. The state directory is made private first,
/// since a folder that is not there yet would leave the watch nothing to arm.
///
/// # Errors
/// The state directory cannot be made private, or the platform's file events cannot start.
fn watch(state: &Path) -> io::Result<(FileWatch, mpsc::Receiver<()>)> {
    aterm_uds::private_file::ensure_private_dir(state)?;
    let (tell, changes) = mpsc::channel();
    let watch = FileWatch::platform(Box::new(move |_| {
        let _ = tell.send(());
    }))?;
    watch.watch(0, state.join(aterm_agent::machine_name::file::FILE));
    watch.watch(1, state.join(aterm_net::fleet::KEY_FILE));
    Ok((watch, changes))
}

/// Wait for a change, at most `timeout` where one is given. Every change told by then goes with
/// the first, since one read covers them all.
fn heard(changes: &mpsc::Receiver<()>, timeout: Option<Duration>) -> Heard {
    let first = match timeout {
        None => changes
            .recv()
            .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
        Some(timeout) => changes.recv_timeout(timeout),
    };
    match first {
        Ok(()) => {
            while changes.try_recv().is_ok() {}
            Heard::Change
        }
        Err(mpsc::RecvTimeoutError::Timeout) => Heard::Quiet,
        Err(mpsc::RecvTimeoutError::Disconnected) => Heard::Ended,
    }
}

/// The wait where no watch could start: a failed read is tried again after its timeout, and one
/// that succeeded is the last, so the title changes at the next launch.
fn unwatched(timeout: Option<Duration>) -> Heard {
    match timeout {
        Some(timeout) => {
            std::thread::sleep(timeout);
            Heard::Quiet
        }
        None => Heard::Ended,
    }
}

/// The name as it stands, made where none is there yet (`roster` is read only then), in a state
/// directory made private first, as `aterm name` makes it.
fn read(
    state: &Path,
    roster: impl FnOnce() -> io::Result<Option<String>>,
) -> io::Result<MachineView> {
    aterm_uds::private_file::ensure_private_dir(state)?;
    let (name, _) =
        aterm_agent::machine_name::ensure_in_view(state, roster, aterm_netplace::unix_ms_now())?;
    Ok(MachineView {
        name: name.name(),
        nick: name.nick.display(),
        fleet: aterm_net::fleet::read_key_file(state).is_ok(),
    })
}

/// Read the name and `post` it, then read again at each change `wait` hears, posting only a view
/// that changed, until `post` says nobody listens or `wait` hears nothing more. A read that fails
/// (a vault roster busy past its wait, a file that cannot be read) keeps the title as it is, is
/// said once until a read succeeds, and is tried again at the next change or after
/// [`RETRY_AFTER`]: nothing is made of a failed read.
fn read_loop(
    state: &Path,
    roster: impl Fn() -> io::Result<Option<String>>,
    mut post: impl FnMut(MachineView) -> bool,
    mut wait: impl FnMut(Option<Duration>) -> Heard,
) {
    let mut shown: Option<MachineView> = None;
    let mut failing = false;
    loop {
        let timeout = match read(state, &roster) {
            Ok(view) => {
                failing = false;
                if shown.as_ref() != Some(&view) {
                    if !post(view.clone()) {
                        return;
                    }
                    shown = Some(view);
                }
                None
            }
            Err(e) => {
                if !failing {
                    aterm_log::info!(
                        "machine name: not read ({e}); trying again at a change or in a minute"
                    );
                    failing = true;
                }
                Some(RETRY_AFTER)
            }
        };
        if wait(timeout) == Heard::Ended {
            return;
        }
    }
}

#[cfg(test)]
mod tests;
