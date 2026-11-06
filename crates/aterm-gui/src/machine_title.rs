// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The menu bar's machine title, `m8 Heron` right of the version
//! (`docs/DESIGN-machine-names-2026-11-04.md` §5.1). A reader thread makes this machine's name
//! where none is there yet ([`aterm_agent::machine_name::ensure_in_view`]), reads it again when
//! `machine-name.toml`, the fleet key or one of the vault's standing files changes
//! ([`aterm_secrets::client::Paths::standing_files`]), and posts [`Wake::MachineName`] when what
//! it shows changed. It hears a change through [`FileWatch`] (kqueue on each file and its
//! folder, so a replace by rename is heard), so it polls nothing; but while the vault's
//! subscriber runs it replaces its `state.toml` at least once a minute, and each replace is one
//! read of the name and the vault's local files, which posts nothing when nothing changed. The
//! window thread only sets titles, so opening the menu reads no file.

use std::io;
use std::path::Path;
use std::sync::{Mutex, mpsc};
use std::time::Duration;

use amacs_files::watch::FileWatch;
use aterm_agent::machine_name::{InFleet, NameTo, NickTo};
use aterm_secrets::client::Paths;
use aterm_types::machine_name::{Nickname, Number, Refusal};
use aterm_window::event_loop::EventLoopProxy;

use crate::Wake;

/// What the title and the menu's first row show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MachineView {
    /// The name file's revision, for snapshot equality; a new identity or file may reset it.
    pub(crate) rev: u64,
    /// The NAME: `m8`, or `m0` while unnumbered.
    pub(crate) name: String,
    /// The nickname as the menu bar shows it (`Heron`).
    pub(crate) nick: String,
    /// The nickname as stored, which Change Nickname…'s field starts with (`heron`).
    pub(crate) word: String,
    /// Whether this machine is in a fleet: its vault's standing, or a fleet key that reads
    /// ([`InFleet`], §11.85).
    pub(crate) fleet: InFleet,
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

/// The menu's first row: `m8 · Heron · in a fleet`, or `· joining a fleet`, `· not in a fleet`,
/// `· fleet not read` ([`InFleet::phrase`]).
pub(crate) fn machine_menu_info_row(view: &MachineView) -> String {
    format!(
        "{} \u{00B7} {} \u{00B7} {}",
        view.name,
        view.nick,
        view.fleet.phrase()
    )
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
            let vault = aterm_agent::machine_name::standing_vault();
            match watch(&state, vault.as_ref()) {
                Ok((_watch, changes)) => {
                    read_loop(&state, vault.as_ref(), roster, post, |timeout| {
                        heard(&changes, timeout)
                    });
                }
                Err(e) => {
                    aterm_log::info!(
                        "machine name: no file watch ({e}), so the title is read at launch only"
                    );
                    read_loop(&state, vault.as_ref(), roster, post, unwatched);
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

/// The watch on the files the title comes from, `machine-name.toml`, the fleet key and the
/// standing files of `vault` where one is read: `changes` hears one `()` for each whose stamp
/// (device, inode, length, modification and change times) moved, and one at the watch's first
/// look at each. The state directory is made private first, as a reader makes it; the vault's
/// folder is left as it is, and where it is not there yet the watch listens on the nearest
/// folder above it, so a vault first made after launch is heard.
///
/// # Errors
/// The state directory cannot be made private, or the platform's file events cannot start.
fn watch(state: &Path, vault: Option<&Paths>) -> io::Result<(FileWatch, mpsc::Receiver<()>)> {
    aterm_uds::private_file::ensure_private_dir(state)?;
    let (tell, changes) = mpsc::channel();
    let watch = FileWatch::platform(Box::new(move |_| {
        let _ = tell.send(());
    }))?;
    watch.watch(0, state.join(aterm_agent::machine_name::file::FILE));
    watch.watch(1, state.join(aterm_net::fleet::KEY_FILE));
    for (key, file) in (2..).zip(vault.into_iter().flat_map(Paths::standing_files)) {
        watch.watch(key, file);
    }
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
/// directory made private first, as `aterm name` makes it, and whether this machine is in a fleet
/// ([`InFleet::read`] over `state` and the vault at `vault`, where one is read).
fn read(
    state: &Path,
    vault: Option<&Paths>,
    roster: impl FnOnce() -> io::Result<Option<String>>,
) -> io::Result<MachineView> {
    aterm_uds::private_file::ensure_private_dir(state)?;
    let (name, _) =
        aterm_agent::machine_name::ensure_in_view(state, roster, aterm_netplace::unix_ms_now())?;
    Ok(MachineView {
        rev: name.rev,
        name: name.name(),
        nick: name.nick.display(),
        word: name.nick.as_str().to_owned(),
        fleet: InFleet::read(state, vault),
    })
}

/// Reads and posts share one order across the file watcher and the edit writer. Nickname-only
/// edits before publication deliberately keep revision zero, so revision comparison alone
/// cannot prevent an earlier snapshot from posting after a later one.
static READ_POST: Mutex<()> = Mutex::new(());

fn post_read(
    state: &Path,
    vault: Option<&Paths>,
    roster: impl FnOnce() -> io::Result<Option<String>>,
    post: impl FnOnce(MachineView) -> bool,
) -> io::Result<bool> {
    let _serial = READ_POST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    read(state, vault, roster).map(post)
}

/// Read the name and `post` it, then read again at each change `wait` hears, posting only a view
/// that changed, until `post` says nobody listens or `wait` hears nothing more. A read that fails
/// (a vault roster busy past its wait, a file that cannot be read) keeps the title as it is, is
/// said once until a read succeeds, and is tried again at the next change or after
/// [`RETRY_AFTER`]: nothing is made of a failed read.
fn read_loop(
    state: &Path,
    vault: Option<&Paths>,
    roster: impl Fn() -> io::Result<Option<String>>,
    mut post: impl FnMut(MachineView) -> bool,
    mut wait: impl FnMut(Option<Duration>) -> Heard,
) {
    let mut shown: Option<MachineView> = None;
    let mut failing = false;
    loop {
        let timeout = match post_read(state, vault, &roster, |view| {
            if shown.as_ref() != Some(&view) {
                if !post(view.clone()) {
                    return false;
                }
                shown = Some(view);
            }
            true
        }) {
            Ok(false) => return,
            Ok(true) => {
                failing = false;
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

/// What a control client should use instead of opening the person's rename alert.
pub(crate) const NAME_PERSON_ONLY: &str =
    "Rename This Mac\u{2026} is the person's own act at the window; from a shell, `aterm name set`";

/// The rename field accepts a number followed by an optional nickname, preserving the nickname
/// when only a number is given. An untouched field changes nothing, including an unnumbered Mac.
pub(crate) fn name_to(text: &str, held: &MachineView) -> Result<Option<NameTo>, String> {
    let text = text.trim();
    if text == format!("{} {}", held.name, held.word) {
        return Ok(None);
    }
    let (number, nick) = text
        .split_once(char::is_whitespace)
        .map_or((text, None), |(number, nick)| (number, Some(nick.trim())));
    let num = Number::parse(number)
        .map_err(|why| format!("\u{201c}{number}\u{201d} is no machine number: {why}."))?;
    let nick = nick
        .filter(|nick| !nick.is_empty())
        .map(|nick| {
            Nickname::parse(nick)
                .map_err(|why| format!("\u{201c}{nick}\u{201d} is no nickname: {why}."))
        })
        .transpose()?;
    if num.to_string() == held.name && nick.as_ref().is_none_or(|nick| nick.as_str() == held.word) {
        return Ok(None);
    }
    Ok(Some(NameTo { num, nick }))
}

/// Which machine-name field an alert edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditKind {
    Nickname,
    Name,
}

enum NameChange {
    Nickname(NickTo),
    Name(NameTo),
}

impl NameChange {
    fn kind(&self) -> EditKind {
        match self {
            Self::Nickname(_) => EditKind::Nickname,
            Self::Name(_) => EditKind::Name,
        }
    }

    fn word(&self) -> Option<String> {
        match self {
            Self::Nickname(NickTo::Word(nick)) => Some(nick.as_str().to_owned()),
            Self::Nickname(NickTo::Random) => None,
            Self::Name(to) => Some(match &to.nick {
                Some(nick) => format!("{} {}", to.num, nick.as_str()),
                None => to.num.to_string(),
            }),
        }
    }
}

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

/// A machine-name edit that failed ([`Wake::MachineNameNotSet`]): its next matching menu press
/// shows `why` and starts the field at `word` (none for a random noun).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NameUnset {
    /// Which press's set this was ([`crate::App::machine_sets`] when it started), so a failure a
    /// later press superseded is dropped.
    pub(crate) generation: u64,
    pub(crate) kind: EditKind,
    pub(crate) word: Option<String>,
    pub(crate) why: String,
}

/// The window's single writer processes edits in the order their menu presses submitted them.
pub(crate) struct NameWriter {
    jobs: mpsc::Sender<NameJob>,
}

struct NameJob {
    to: NameChange,
    generation: u64,
}

impl NameWriter {
    fn start(proxy: EventLoopProxy<Wake>) -> io::Result<Self> {
        let (jobs, queued) = mpsc::channel();
        std::thread::Builder::new()
            .name("aterm-name-writer".to_owned())
            .spawn(move || write_changes(queued, |job| apply_change(&proxy, job)))?;
        Ok(Self { jobs })
    }
}

/// A failed edit does not end the queue: every later request still gets its turn.
fn write_changes(jobs: mpsc::Receiver<NameJob>, mut apply: impl FnMut(NameJob)) {
    for job in jobs {
        apply(job);
    }
}

fn report_unset(proxy: &EventLoopProxy<Wake>, job: &NameJob, why: String) {
    aterm_log::warn!("machine name: the person's change was not set: {why}");
    let _ = proxy.send_event(Wake::MachineNameNotSet(NameUnset {
        generation: job.generation,
        kind: job.to.kind(),
        word: job.to.word(),
        why: format!("The name was not changed: {why}."),
    }));
}

fn queue_change(
    writer: &mut Option<NameWriter>,
    proxy: EventLoopProxy<Wake>,
    to: NameChange,
    generation: u64,
) {
    let job = NameJob { to, generation };
    if writer.is_none() {
        match NameWriter::start(proxy.clone()) {
            Ok(started) => *writer = Some(started),
            Err(why) => {
                report_unset(
                    &proxy,
                    &job,
                    format!("aterm could not start its writer ({why})"),
                );
                return;
            }
        }
    }
    if let Some(active) = writer
        && let Err(unsent) = active.jobs.send(job)
    {
        *writer = None;
        report_unset(&proxy, &unsent.0, "aterm's name writer stopped".to_owned());
    }
}

/// Apply one queued edit off the window thread. Read and publication share the reader's lock
/// so an older file snapshot cannot arrive after this edit's new view.
fn apply_change(proxy: &EventLoopProxy<Wake>, job: NameJob) {
    use aterm_agent::machine_name::{set_name, set_nick};
    use aterm_net::fleet_member::name_record::ByKind;
    let Some(state) = aterm_types::dirs::state_dir() else {
        report_unset(proxy, &job, "aterm has no state folder".to_owned());
        return;
    };
    let roster = aterm_agent::machine_name::vault_roster_label;
    let vault = aterm_agent::machine_name::standing_vault();
    let stamp = aterm_netplace::unix_ms_now();
    let set = match &job.to {
        NameChange::Nickname(to) => set_nick(&state, to, ByKind::Person, roster, stamp, false)
            .map(|set| (set.name, set.changed, set.shared)),
        NameChange::Name(to) => {
            let lab = || aterm_agent::machine_name::lab_labels(vault.as_ref());
            set_name(&state, to, lab, ByKind::Person, roster, stamp, false)
                .map(|set| (set.name, set.changed, set.shared))
        }
    };
    match set {
        Ok((name, changed, shared)) => {
            aterm_log::info!(
                "machine name: the person set {} ({}{})",
                name.title(),
                if changed { "changed" } else { "as it was" },
                if shared {
                    ", which another machine in view also carries"
                } else {
                    ""
                }
            );
            if let Err(why) = post_read(&state, vault.as_ref(), roster, |view| {
                proxy.send_event(Wake::MachineName(view)).is_ok()
            }) {
                aterm_log::info!(
                    "machine name: not read after the change ({why}); the title follows at its reader's next read"
                );
            }
        }
        Err(why) => report_unset(proxy, &job, why.to_string()),
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
        self.change_machine_edit(EditKind::Nickname);
    }

    /// Rename This Mac… shares the nickname alert's input and background writer, with a field
    /// for the number and optional nickname. A delayed failure is kept for the next press.
    pub(crate) fn rename_machine(&mut self) {
        self.change_machine_edit(EditKind::Name);
    }

    fn change_machine_edit(&mut self, kind: EditKind) {
        let refused = |app: &mut Self, why: &str| {
            aterm_log::info!("machine name: {kind:?} changed nothing: {why}");
            app.pending_action_refusal = Some(why.to_owned());
        };
        if let Some(who) = self.gesture_unvouched() {
            aterm_log::info!("machine name: refused a gesture by {who:?} to change {kind:?}");
            return refused(
                self,
                match kind {
                    EditKind::Nickname => NICK_PERSON_ONLY,
                    EditKind::Name => NAME_PERSON_ONLY,
                },
            );
        }
        if crate::menu::process_is_headless() {
            return refused(self, NICK_NOT_ASKED);
        }
        let Some(view) = self.machine_view.clone() else {
            return refused(self, NICK_UNREAD);
        };
        let unset = if self
            .machine_unset
            .as_ref()
            .is_some_and(|unset| unset.kind == kind)
        {
            self.machine_unset.take()
        } else {
            None
        };
        let mut why = unset.as_ref().map(|u| u.why.clone());
        let mut field = unset.and_then(|u| u.word).unwrap_or_else(|| match kind {
            EditKind::Nickname => view.word.clone(),
            EditKind::Name => format!("{} {}", view.name, view.word),
        });
        loop {
            let answer = match kind {
                EditKind::Nickname => {
                    let body = nickname_alert_body(&view, why.as_deref());
                    crate::menu::ask_nickname(NICK_ALERT_TITLE, &body, &field)
                }
                EditKind::Name => {
                    let lead = "Type this Mac's number and nickname, for example m86 MissBunny. Type only a number to keep its nickname.";
                    let body = why
                        .as_ref()
                        .map_or_else(|| lead.to_owned(), |why| format!("{why}\n\n{lead}"));
                    crate::menu::ask_machine_name("Rename This Mac", &body, &field)
                }
            };
            let Some(answer) = answer else {
                return refused(self, NICK_NOT_ASKED);
            };
            let change = match kind {
                EditKind::Nickname => {
                    nick_to(&answer, &view.word).map(|to| to.map(NameChange::Nickname))
                }
                EditKind::Name => match &answer {
                    NickAnswer::Typed(text) => {
                        name_to(text, &view).map(|to| to.map(NameChange::Name))
                    }
                    NickAnswer::Cancel | NickAnswer::Random => Ok(None),
                },
            };
            match change {
                Ok(None) => return,
                Ok(Some(to)) => {
                    self.machine_unset = None;
                    self.machine_sets = self.machine_sets.wrapping_add(1);
                    if let Some(proxy) = self.proxy.clone() {
                        queue_change(&mut self.machine_writer, proxy, to, self.machine_sets);
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

    /// [`Wake::MachineNameNotSet`]: keep the failure for the next matching menu press, unless a later
    /// press started another set since. It raises nothing: a set can fail seconds after the
    /// press (a lock held, a slow read), when an alert that opened by itself would take keys the
    /// person meant for a terminal.
    pub(crate) fn machine_name_not_set(&mut self, unset: NameUnset) {
        if unset.generation != self.machine_sets {
            aterm_log::info!(
                "machine name: a superseded set failed ({}); a later press stands",
                unset.why
            );
            return;
        }
        self.machine_unset = Some(unset);
    }

    /// Accept the next serialized read, retitle the menu, and rebuild the status item's name.
    /// Revision numbers order one name-file lineage, not an identity or file replacement.
    pub(crate) fn machine_name_read(&mut self, view: MachineView) {
        self.machine_view = Some(view);
        self.refresh_machine_menu();
        self.mark_status_dirty();
    }
}

#[cfg(test)]
mod tests;
