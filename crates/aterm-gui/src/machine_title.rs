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
use aterm_types::machine_name::{Nickname, Refusal};
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

/// Reads and posts share one order across the file watcher and the edit writer. A file's
/// revision orders its own lineage, but a replacement can start again at zero; holding this
/// guard through publication keeps an earlier read from arriving after a later snapshot.
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

/// What Rename's answer `to` sets: a nickname alone as Change Nickname… sets it, another number
/// (or a number that holds no claim yet) as `aterm name set` does, and nothing for fields that
/// ask for nothing.
fn rename_change(to: crate::machine_rename_sheet::RenameTo) -> Option<NameChange> {
    use crate::machine_rename_sheet::RenameTo;
    match to {
        RenameTo::Keep => None,
        RenameTo::Nick(nick) => Some(NameChange::Nickname(NickTo::Word(nick))),
        RenameTo::Name(to) => Some(NameChange::Name(to)),
    }
}

/// The failure a press of `kind` starts from: the one `unset` keeps when the same menu row made
/// it, taken; one another row made stays for that row's next press.
fn unset_for(unset: &mut Option<NameUnset>, kind: EditKind) -> Option<NameUnset> {
    unset
        .as_ref()
        .is_some_and(|unset| unset.asked.kind == kind)
        .then(|| unset.take())
        .flatten()
}

/// What a failed set keeps for its row's next press: the press's `asked`, `why` worded for the
/// alert to lead with.
fn unset_of(generation: u64, asked: &Asked, why: &str) -> NameUnset {
    NameUnset {
        generation,
        asked: asked.clone(),
        why: format!("The name was not changed: {why}."),
    }
}

/// What a press asked, kept with its set so a failure can refill the alert it came from: that
/// alert's menu row, and the words the person typed into each field (`None` for a field left
/// alone, and for a random noun).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Asked {
    pub(crate) kind: EditKind,
    pub(crate) number: Option<String>,
    pub(crate) nick: Option<String>,
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

/// Rename This Mac…'s text, above its fields: why the last answer took no effect (`why`) first.
pub(crate) fn rename_alert_body(why: Option<&str>) -> String {
    let lead = "Other Macs in this fleet reach this Mac by its number, and the menu bar shows its \
                nickname beside it. The line under the fields checks each number against this \
                fleet and its vault as you type; a field left empty keeps what this Mac has.";
    match why {
        Some(why) => format!("{why}\n\n{lead}"),
        None => lead.to_owned(),
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

/// A machine-name edit that failed ([`Wake::MachineNameNotSet`]): the next press of the row it
/// came from shows `why`, its fields holding what that press typed ([`Asked`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NameUnset {
    /// Which press's set this was ([`crate::App::machine_sets`] when it started), so a failure a
    /// later press superseded is dropped.
    pub(crate) generation: u64,
    pub(crate) asked: Asked,
    pub(crate) why: String,
}

/// The window's single writer processes edits, and Rename This Mac…'s reads, in the order their
/// menu presses submitted them, so a read sees every change pressed before it.
pub(crate) struct NameWriter {
    jobs: mpsc::Sender<Job>,
}

/// One turn of the writer.
enum Job {
    Change(NameJob),
    /// Read what Rename This Mac… judges a number by, for press `press`'s alert.
    Preview(u64),
}

struct NameJob {
    to: NameChange,
    generation: u64,
    asked: Asked,
}

impl NameWriter {
    fn start(proxy: EventLoopProxy<Wake>) -> io::Result<Self> {
        let (jobs, queued) = mpsc::channel();
        std::thread::Builder::new()
            .name("aterm-name-writer".to_owned())
            .spawn(move || {
                write_changes(queued, |job| match job {
                    Job::Change(job) => apply_change(&proxy, job),
                    Job::Preview(press) => read_preview(press),
                });
            })?;
        Ok(Self { jobs })
    }
}

/// A failed edit does not end the queue: every later request still gets its turn.
fn write_changes<J>(jobs: mpsc::Receiver<J>, mut apply: impl FnMut(J)) {
    for job in jobs {
        apply(job);
    }
}

fn report_unset(proxy: &EventLoopProxy<Wake>, job: &NameJob, why: String) {
    aterm_log::warn!("machine name: the person's change was not set: {why}");
    let _ = proxy.send_event(Wake::MachineNameNotSet(unset_of(
        job.generation,
        &job.asked,
        &why,
    )));
}

/// Queue `job` on the window's writer, started at its first job: a job it cannot take is
/// answered here, a change as a set that failed and a read as one that did not run.
fn queue(writer: &mut Option<NameWriter>, proxy: EventLoopProxy<Wake>, job: Job) {
    let unqueued = |job: Job, why: String| match job {
        Job::Change(job) => report_unset(&proxy, &job, why),
        Job::Preview(press) => crate::machine_rename_sheet::deliver(
            press,
            crate::machine_rename_sheet::Snapshot::Unread(why),
        ),
    };
    if writer.is_none() {
        match NameWriter::start(proxy.clone()) {
            Ok(started) => *writer = Some(started),
            Err(why) => return unqueued(job, format!("aterm could not start its writer ({why})")),
        }
    }
    if let Some(active) = writer
        && let Err(unsent) = active.jobs.send(job)
    {
        *writer = None;
        unqueued(unsent.0, "aterm's name writer stopped".to_owned());
    }
}

/// Read what Rename This Mac… judges a number by ([`aterm_agent::machine_name::rename_preview`])
/// off the window thread, and bring it to press `press`'s alert through the main queue, which
/// AppKit's modal loop serves while the alert is up.
fn read_preview(press: u64) {
    use crate::machine_rename_sheet::Snapshot;
    use aterm_agent::machine_name::{
        lab_labels, rename_preview, standing_vault, vault_roster_label,
    };
    let snapshot = match aterm_types::dirs::state_dir() {
        None => Snapshot::Unread("aterm has no state folder".to_owned()),
        Some(state) => {
            let vault = standing_vault();
            let lab = || lab_labels(vault.as_ref());
            match rename_preview(
                &state,
                lab,
                vault_roster_label,
                aterm_netplace::unix_ms_now(),
            ) {
                Ok(preview) => Snapshot::Read(Box::new(preview)),
                Err(why) => {
                    aterm_log::info!("machine name: the rename's read failed: {why}");
                    Snapshot::Unread(why.to_string())
                }
            }
        }
    };
    aterm_objc::run_on_main(move |_| crate::machine_rename_sheet::deliver(press, snapshot));
}

/// Apply one queued edit off the window thread, releasing its persistence locks before
/// `post_read` serializes the resulting snapshot and its publication with the file watcher.
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
    /// this Mac's nickname in an alert, then set it on the window's name writer, after every
    /// change pressed before it. A typed word that is no nickname asks again with its refusal and
    /// the word kept; the last set's failure, if one came back since, leads the alert, its field
    /// at the word that press set. A gesture the person did not make, a headless instance,
    /// a name not read yet, or an alert that cannot be shown changes nothing, is journaled, and is
    /// said on the invoke's reply.
    pub(crate) fn change_nickname(&mut self) {
        self.change_machine_nickname();
    }

    /// Rename This Mac… (§5.3): ask for this Mac's number and nickname in two fields over a live
    /// line ([`crate::machine_rename_sheet`]), whose read the window's writer runs after every
    /// change pressed before it, then set the answer on that writer: a number through
    /// [`aterm_agent::machine_name::set_name`] and a nickname alone through
    /// [`aterm_agent::machine_name::set_nick`] ([`rename_change`]). A refusal from the fields
    /// asks again with the typed text kept; the last set's failure, if one came back since,
    /// leads the alert, its fields holding what that press typed. A gesture
    /// the person did not make, a headless instance, a name not read yet, or an alert that cannot
    /// be shown changes nothing, is journaled, and is said on the invoke's reply.
    pub(crate) fn rename_machine(&mut self) {
        use crate::machine_rename_sheet::{Answer, Field, TITLE, ask, begin, end, rename_to};
        let Some((view, unset)) = self.machine_edit_starts(EditKind::Name) else {
            return;
        };
        let mut why = unset.as_ref().map(|u| u.why.clone());
        let (mut number, mut nick) = unset.map_or((None, None), |u| (u.asked.number, u.asked.nick));
        let press = begin();
        if let Some(proxy) = self.proxy.clone() {
            queue(&mut self.machine_writer, proxy, Job::Preview(press));
        }
        loop {
            let body = rename_alert_body(why.as_deref());
            let Some(answer) = ask(
                press,
                TITLE,
                &body,
                &view,
                number.as_deref(),
                nick.as_deref(),
            ) else {
                end(press);
                return self.machine_edit_refused(EditKind::Name, NICK_NOT_ASKED);
            };
            let Answer::Rename {
                number: typed_number,
                nick: typed_nick,
                held,
            } = answer
            else {
                return end(press);
            };
            let fields = (
                Field::open(typed_number.as_deref(), held.number_text()),
                Field::open(typed_nick.as_deref(), &held.word),
            );
            let asked = Asked {
                kind: EditKind::Name,
                number: typed_number,
                nick: typed_nick,
            };
            let change = match rename_to(&fields.0, &fields.1, &held) {
                Ok(to) => rename_change(to),
                Err(refusal) => {
                    why = Some(refusal);
                    number = asked.number;
                    nick = asked.nick;
                    continue;
                }
            };
            end(press);
            if let Some(to) = change {
                self.queue_machine_change(to, asked);
            }
            return;
        }
    }

    /// Change Nickname…'s alert over [`nick_to`].
    fn change_machine_nickname(&mut self) {
        let Some((view, unset)) = self.machine_edit_starts(EditKind::Nickname) else {
            return;
        };
        let mut why = unset.as_ref().map(|u| u.why.clone());
        let mut field = unset
            .and_then(|u| u.asked.nick)
            .unwrap_or_else(|| view.word.clone());
        loop {
            let body = nickname_alert_body(&view, why.as_deref());
            let Some(answer) = crate::menu::ask_nickname(NICK_ALERT_TITLE, &body, &field) else {
                return self.machine_edit_refused(EditKind::Nickname, NICK_NOT_ASKED);
            };
            match nick_to(&answer, &view.word) {
                Ok(None) => return,
                Ok(Some(to)) => {
                    let asked = Asked {
                        kind: EditKind::Nickname,
                        number: None,
                        nick: match &to {
                            NickTo::Word(word) => Some(word.as_str().to_owned()),
                            NickTo::Random => None,
                        },
                    };
                    return self.queue_machine_change(NameChange::Nickname(to), asked);
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

    fn machine_edit_refused(&mut self, kind: EditKind, why: &str) {
        aterm_log::info!("machine name: {kind:?} changed nothing: {why}");
        self.pending_action_refusal = Some(why.to_owned());
    }

    /// The view an edit of `kind` starts from, and the last set of that kind's failure, if one
    /// came back since; `None`, refused, for a gesture the person did not make, a headless
    /// instance, or a name not read yet.
    fn machine_edit_starts(&mut self, kind: EditKind) -> Option<(MachineView, Option<NameUnset>)> {
        if let Some(who) = self.gesture_unvouched() {
            aterm_log::info!("machine name: refused a gesture by {who:?} to change {kind:?}");
            let why = match kind {
                EditKind::Nickname => NICK_PERSON_ONLY,
                EditKind::Name => NAME_PERSON_ONLY,
            };
            self.machine_edit_refused(kind, why);
            return None;
        }
        if crate::menu::process_is_headless() {
            self.machine_edit_refused(kind, NICK_NOT_ASKED);
            return None;
        }
        let Some(view) = self.machine_view.clone() else {
            self.machine_edit_refused(kind, NICK_UNREAD);
            return None;
        };
        let unset = unset_for(&mut self.machine_unset, kind);
        Some((view, unset))
    }

    /// Hand `to`, which the press `asked` for, to the window's writer as the newest press's set.
    fn queue_machine_change(&mut self, to: NameChange, asked: Asked) {
        self.machine_unset = None;
        self.machine_sets = self.machine_sets.wrapping_add(1);
        if let Some(proxy) = self.proxy.clone() {
            let generation = self.machine_sets;
            queue(
                &mut self.machine_writer,
                proxy,
                Job::Change(NameJob {
                    to,
                    generation,
                    asked,
                }),
            );
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
