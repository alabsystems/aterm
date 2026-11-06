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
use aterm_agent::machine_name::NickTo;
use aterm_types::machine_name::{Nickname, Refusal};
use aterm_window::event_loop::EventLoopProxy;

use crate::Wake;

/// What the title and the menu's first row show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MachineView {
    /// The NAME: `m8`, or `m0` while unnumbered.
    pub(crate) name: String,
    /// The nickname as the menu bar shows it (`Heron`).
    pub(crate) nick: String,
    /// The nickname as stored, which Change Nickname…'s field starts with (`heron`).
    pub(crate) word: String,
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
        word: name.nick.as_str().to_owned(),
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

/// The title of the alert Change Nickname… raises (§5.4).
pub(crate) const NICK_ALERT_TITLE: &str = "Change Nickname";
/// The alert's buttons, in order: Change takes Return and Cancel Escape; New Random Noun has no
/// key of its own (with keyboard navigation on, Tab and Space reach every button).
pub(crate) const NICK_BUTTONS: [&str; 3] = ["Change", "Cancel", "New Random Noun"];

/// What `invoke ChangeNickname` answers when a control client's, aterm's accessibility tree's or
/// a relayed gesture dispatched it: the press is the person's own, and an agent has the command.
pub(crate) const NICK_PERSON_ONLY: &str = "Change Nickname\u{2026} is the person's own act at the window; from a shell, `aterm name nick`";
/// What it answers before the title's reader has read this machine's name.
pub(crate) const NICK_UNREAD: &str = "aterm has not read this Mac's name yet";
/// What it answers when no alert can be shown: a headless instance's, or one off the window
/// thread.
pub(crate) const NICK_NOT_ASKED: &str = "nothing was changed: aterm could not show its question";

/// The alert's text: where the nickname shows, and, above it, why the last answer took no effect
/// (`why`).
pub(crate) fn nickname_alert_body(view: &MachineView, why: Option<&str>) -> String {
    let lead = format!(
        "This Mac's nickname shows in the menu bar beside its number, {}. Type another, a word \
         that starts with a letter, or let aterm pick a new random noun.",
        view.name
    );
    match why {
        Some(why) => format!("{why}\n\n{lead}"),
        None => lead,
    }
}

/// What the person answered the nickname alert.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NickAnswer {
    /// Change (or Return), with the field's text.
    Typed(String),
    /// New Random Noun.
    Random,
    /// Cancel, Escape, or any other end of the alert.
    Cancel,
}

/// The answer `response`, the alert's modal response, stands for; `typed` reads the field, only
/// for Change.
pub(crate) fn nick_answer(response: isize, typed: impl FnOnce() -> String) -> NickAnswer {
    use crate::appkit::consts::{NS_ALERT_FIRST_BUTTON_RETURN, NS_ALERT_THIRD_BUTTON_RETURN};
    match response {
        NS_ALERT_FIRST_BUTTON_RETURN => NickAnswer::Typed(typed()),
        NS_ALERT_THIRD_BUTTON_RETURN => NickAnswer::Random,
        _ => NickAnswer::Cancel,
    }
}

/// What `answer` asks of [`aterm_agent::machine_name::set_nick`], `held` being the nickname as
/// stored (the field's first text): nothing for a cancel or for the held word typed back, so an
/// untouched field changes nothing (a drawn noun stays drawn), and for a typed word that is no
/// nickname the refusal the alert shows above its text next time.
pub(crate) fn nick_to(answer: &NickAnswer, held: &str) -> Result<Option<NickTo>, String> {
    match answer {
        NickAnswer::Cancel => Ok(None),
        NickAnswer::Random => Ok(Some(NickTo::Random)),
        NickAnswer::Typed(text) => {
            let word = text.trim();
            if word == held {
                return Ok(None);
            }
            match Nickname::parse(word) {
                Ok(nick) => Ok(Some(NickTo::Word(nick))),
                Err(Refusal::Empty) => Err("Type a nickname first.".to_owned()),
                Err(why) => Err(format!("\u{201c}{word}\u{201d} is no nickname: {why}.")),
            }
        }
    }
}

/// A nickname set that failed ([`Wake::NicknameNotSet`]): the next Change Nickname… shows `why`
/// above its text and starts its field at `word`, the nickname that was not set (none for a
/// random noun).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NickUnset {
    /// Which press's set this was ([`crate::App::nickname_sets`] when it started), so a failure a
    /// later press superseded is dropped.
    pub(crate) generation: u64,
    pub(crate) word: Option<String>,
    pub(crate) why: String,
}

/// Set this machine's nickname to `to` as the person's act, on a thread of its own: the set reads
/// every book's records, and the vault's roster where no name is there yet, which the window
/// thread must not wait on. A change re-reads the name and posts [`Wake::MachineName`], so the
/// title follows even where its reader has no file watch; a failure is journaled and comes back as
/// [`Wake::NicknameNotSet`].
fn spawn_set_nick(proxy: EventLoopProxy<Wake>, to: NickTo, generation: u64) {
    let word = match &to {
        NickTo::Word(nick) => Some(nick.as_str().to_owned()),
        NickTo::Random => None,
    };
    let unset = move |why: String| {
        aterm_log::warn!("machine name: the person's nickname was not set: {why}");
        NickUnset {
            generation,
            word,
            why: format!("The nickname was not changed: {why}."),
        }
    };
    let Some(state) = aterm_types::dirs::state_dir() else {
        let _ = proxy.send_event(Wake::NicknameNotSet(unset(
            "aterm has no state folder".to_owned(),
        )));
        return;
    };
    let reporter = proxy.clone();
    // The closure moves into the thread; a spawn that fails hands nothing back, so its report
    // takes the words the closure would have used.
    let spawn_failed = unset.clone();
    let spawned = std::thread::Builder::new()
        .name("aterm-nickname".to_owned())
        .spawn(move || {
            let set = aterm_agent::machine_name::set_nick(
                &state,
                &to,
                aterm_net::fleet_member::name_record::ByKind::Person,
                aterm_agent::machine_name::vault_roster_label,
                aterm_netplace::unix_ms_now(),
                false,
            );
            match set {
                Ok(set) => {
                    aterm_log::info!(
                        "machine name: the person set the nickname {} ({}{})",
                        set.name.nick.as_str(),
                        if set.changed { "changed" } else { "as it was" },
                        if set.shared {
                            ", which another machine in view also carries"
                        } else {
                            ""
                        }
                    );
                    match read(&state, aterm_agent::machine_name::vault_roster_label) {
                        Ok(view) => {
                            let _ = proxy.send_event(Wake::MachineName(view));
                        }
                        Err(e) => aterm_log::info!(
                            "machine name: not read after the nickname was set ({e}); the title follows at its reader's next read"
                        ),
                    }
                }
                Err(e) => {
                    let _ = proxy.send_event(Wake::NicknameNotSet(unset(e.to_string())));
                }
            }
        });
    if let Err(e) = spawned {
        let _ = reporter.send_event(Wake::NicknameNotSet(spawn_failed(format!(
            "aterm could not start its writer ({e})"
        ))));
    }
}

impl crate::App {
    /// Change Nickname… (§5.4 of `docs/DESIGN-machine-names-2026-11-04.md`): ask the person for
    /// this Mac's nickname in an alert, then set it on a thread of its own. A typed word that is
    /// no nickname asks again with its refusal and the word kept; the last set's failure, if one
    /// came back since, leads the alert. A gesture the person did not make, a headless instance,
    /// a name not read yet, or an alert that cannot be shown changes nothing, is journaled, and is
    /// said on the invoke's reply.
    pub(crate) fn change_nickname(&mut self) {
        let refused = |app: &mut Self, why: &str| {
            aterm_log::info!("machine name: Change Nickname\u{2026} changed nothing: {why}");
            app.pending_action_refusal = Some(why.to_owned());
        };
        if let Some(who) = self.gesture_unvouched() {
            aterm_log::info!("machine name: refused a gesture by {who:?} to change the nickname");
            return refused(self, NICK_PERSON_ONLY);
        }
        if crate::menu::process_is_headless() {
            return refused(self, NICK_NOT_ASKED);
        }
        let Some(view) = self.machine_view.clone() else {
            return refused(self, NICK_UNREAD);
        };
        let unset = self.nickname_unset.take();
        let mut why = unset.as_ref().map(|u| u.why.clone());
        let mut field = unset
            .and_then(|u| u.word)
            .unwrap_or_else(|| view.word.clone());
        loop {
            let body = nickname_alert_body(&view, why.as_deref());
            let Some(answer) = crate::menu::ask_nickname(NICK_ALERT_TITLE, &body, &field) else {
                return refused(self, NICK_NOT_ASKED);
            };
            match nick_to(&answer, &view.word) {
                Ok(None) => return,
                Ok(Some(to)) => {
                    self.nickname_sets = self.nickname_sets.wrapping_add(1);
                    if let Some(proxy) = self.proxy.clone() {
                        spawn_set_nick(proxy, to, self.nickname_sets);
                    }
                    return;
                }
                Err(refusal) => {
                    why = Some(refusal);
                    if let NickAnswer::Typed(text) = answer {
                        field = text;
                    }
                }
            }
        }
    }

    /// [`Wake::MachineName`]: hold the name the reader read, retitle the machine menu, and owe the
    /// status item a rebuild, whose tooltip and This Mac row carry the title too (§5.5) though no
    /// session row moved.
    pub(crate) fn machine_name_read(&mut self, view: MachineView) {
        self.machine_view = Some(view);
        self.refresh_machine_menu();
        self.mark_status_dirty();
    }

    /// [`Wake::NicknameNotSet`]: keep the failure for the next Change Nickname…, unless a later
    /// press started another set since. It raises nothing: a set can fail seconds after the
    /// press (a lock held, a slow read), when an alert that opened by itself would take keys the
    /// person meant for a terminal.
    pub(crate) fn nickname_not_set(&mut self, unset: NickUnset) {
        if unset.generation != self.nickname_sets {
            aterm_log::info!(
                "machine name: a superseded nickname set failed ({}); a later press stands",
                unset.why
            );
            return;
        }
        self.nickname_unset = Some(unset);
    }
}

#[cfg(test)]
mod tests;
