// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Settings ▸ This Mac's reads and writes, on the window's one name writer
//! ([`super::NameWriter`]), so a page read sees every change the panel, the page or a shell
//! made before it was asked for, and the page's writes take their turn among the panel's.
//! The page asks for a read when it is opened, refreshed or restored and when the title's
//! reader hears a change ([`crate::App::machine_name_read`]); only the newest read lands
//! ([`crate::App::machine_page_read`]). A press resolves against the read the page showed
//! ([`page_change`]), with Rename This Mac…'s own judges, and a page write's failure comes back
//! to the page alone ([`crate::App::machine_page_not_set`]), never to the next Rename panel.
//! Each read also reads this Mac's History from its registry row, bounded by
//! [`PAGE_HISTORY_WAIT`] so the writer it runs on never waits long behind it, and says what Undo
//! Last Rename gives back, or why it cannot ([`undo_offer`]), and lists History in `aterm name
//! history`'s lines ([`history_rows`]), both made once as the read lands, never at a draw; the
//! undo takes back only the rename that read showed.

use std::sync::Arc;
use std::time::Duration;

use aterm_agent::machine_name::history::{self, History, NO_HISTORY, NO_ROW, PAGE_ROWS, Undo};
use aterm_agent::machine_name::{InFleet, NickTo, UndoTo};
use aterm_types::machine_name::OS_NOUN;
use aterm_window::event_loop::EventLoopProxy;

use super::{Asked, Job, MachineView, NameChange, NameJob, Origin, naming, queue, rename_change};
use crate::Wake;
use crate::machine_rename_sheet::{Field, Held, RenameTo, Snapshot, request, sentence, verdict};
use crate::native_app::MachineRequest;
use crate::native_settings::{PageRead, THIS_MACHINE};

/// How long a page read gives this Mac's History ([`history::read`]): it runs on the window's one
/// name writer, so a Save pressed meanwhile waits behind it at most this long; past it the page
/// says History was not read, offers no undo, and Refresh reads again.
pub(crate) const PAGE_HISTORY_WAIT: Duration = Duration::from_secs(2);

/// What one page read found: Rename This Mac…'s snapshot, this Mac's fleet membership where the
/// read got that far, and its History, or why it was not read.
#[derive(Debug)]
pub(crate) struct PageFound {
    pub(crate) snapshot: Snapshot,
    pub(crate) fleet: Option<InFleet>,
    pub(crate) history: Result<History, String>,
}

/// History as the page shows it ([`history_rows`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PageHistory {
    /// Its lines, newest first ([`History::lines`]).
    Rows(Vec<String>),
    /// One sentence in place of rows: no registry row is this Mac's ([`NO_ROW`]), or it holds no
    /// write to its number or nickname ([`NO_HISTORY`]).
    Empty(&'static str),
    /// History was not read: why.
    Unread(String),
}

/// What the name writer brings Settings ▸ This Mac.
#[derive(Debug)]
pub(crate) enum PageWake {
    /// Read `seq`'s result.
    Read(u64, Box<PageFound>),
    /// Page write `generation` took no effect, or left its publication pending: the words the
    /// page's notice shows.
    NotSet(u64, String),
}

/// A read the page shows: the name its fields are judged against, the snapshot their line and
/// primary judge by, the facts above them, and what the read leaves Undo Last Rename and History.
#[derive(Debug)]
pub(crate) struct PageSnapshot {
    /// The read's sequence, which each write names ([`MachineRequest::Save`]'s `rev`).
    pub(crate) rev: u64,
    pub(crate) held: Held,
    pub(crate) snapshot: Snapshot,
    pub(crate) facts: Vec<(String, String)>,
    /// What Undo Last Rename offers over this read ([`undo_offer`]).
    pub(crate) undo: UndoOffer,
    /// History as the page shows it ([`history_rows`]); `None` where the read found no name
    /// (the editor kept over a failed read).
    pub(crate) history: Option<PageHistory>,
}

/// What Undo Last Rename offers over one read: its words, and the undo it writes where it is
/// available. `undo` names the one rename `undo_last` takes back, refusing where the row no
/// longer shows it.
#[derive(Debug)]
pub(crate) struct UndoOffer {
    pub(crate) line: String,
    pub(crate) undo: Option<Undo>,
}

/// What Undo Last Rename offers over `snapshot` and `history`, as `aterm name undo` would answer
/// from the same read ([`history::undo_of`], then
/// [`aterm_agent::machine_name::RenamePreview::undo_unchecked`]): the title it returns this Mac
/// to ([`UndoTo::title`]), a number left for the undo to check where the vault's labels were not
/// read, as Save leaves one; or why it cannot, in the backend's words, after the title it would
/// return to where History names one. With no read of the name it waits on one, and with no
/// History read it says why.
pub(crate) fn undo_offer(snapshot: &Snapshot, history: &Result<History, String>) -> UndoOffer {
    let refused = |line: String| UndoOffer { line, undo: None };
    let Some(preview) = snapshot.preview() else {
        return refused(format!(
            "Undo Last Rename waits on a fresh read of this {OS_NOUN}'s name; Refresh reads it \
             again."
        ));
    };
    let history = match history {
        Ok(history) => history,
        Err(why) => {
            return refused(format!(
                "Undo Last Rename needs this {OS_NOUN}'s History, which was not read ({why}); \
                 Refresh reads it again."
            ));
        }
    };
    let undo = match history::undo_of(history) {
        Ok(undo) => undo,
        Err(refused_undo) => return refused(sentence(&refused_undo.why)),
    };
    let name = preview.name();
    match preview.undo_unchecked(&undo) {
        Ok(unchecked) => {
            let unchecked = match (unchecked, &undo.to) {
                (Some(why), UndoTo::Name(back)) => format!(
                    " {} could not be checked here ({why}); Undo Last Rename checks it.",
                    back.num
                ),
                _ => String::new(),
            };
            UndoOffer {
                line: format!(
                    "Undo Last Rename returns this {OS_NOUN} to {}.{unchecked}",
                    undo.to.title(name)
                ),
                undo: Some(undo),
            }
        }
        Err(why) => refused(format!(
            "Undo Last Rename would return this {OS_NOUN} to {}. {}",
            undo.to.title(name),
            sentence(&why.why)
        )),
    }
}

/// History as `aterm name history` lists it, newest first, one line each, at most
/// [`PAGE_ROWS`] writes ([`History::lines`]); one sentence where no row is this Mac's or it
/// holds no write; or why it was not read.
pub(crate) fn history_rows(history: &Result<History, String>) -> PageHistory {
    match history {
        Err(why) => PageHistory::Unread(why.clone()),
        Ok(history) if history.row.is_none() => PageHistory::Empty(NO_ROW),
        Ok(history) if history.writes.is_empty() && history.pending.is_empty() => {
            PageHistory::Empty(NO_HISTORY)
        }
        Ok(history) => PageHistory::Rows(history.lines(Some(PAGE_ROWS))),
    }
}

/// What a press asks of the writer, resolved against the read the page showed.
#[derive(Debug)]
pub(crate) enum PageWrite {
    Change(NameChange),
    /// Open Authorize Naming…'s tab; its success synchronizes the saved name.
    Authorize,
    /// Nothing is written: why, in the page's notice.
    Refused(String),
}

/// The refusal of a write that names a read the page has since replaced.
pub(crate) const STALE: &str = "the page read again; press again over what it shows now";
/// The refusal of a write pressed before any read with a name landed.
pub(crate) fn page_unread() -> String {
    format!("Settings \u{25B8} {THIS_MACHINE} has not read this {OS_NOUN}'s name yet")
}
/// What the page shows where it has no writer to read or write with.
pub(crate) const NO_WRITER: &str = "aterm has no name writer in this process";
/// The end of the page's notice for a pending macOS step. The page's read observes neither
/// macOS's names nor the naming rule ([`read_page`]), so its primary offers no Retry Sync or
/// Authorize & Sync for that step; the panel's, whose read does, offers them.
pub(super) const RETRIED_IN_THE_PANEL: &str =
    " (Rename This Mac\u{2026} in the menu bar retries macOS's names)";

/// Resolve `asked` against `read`, the read the page showed when it was pressed, with Rename
/// This Mac…'s judges: Save is what [`request`] makes of the typed text (an unchanged seeded or
/// automatic number saves it, a chosen one whose publication is pending synchronizes it), and
/// is refused where the live line refuses it or nothing would change; Retry Sync and Authorize
/// are only what the untouched fields resolve to; New Random Name draws a noun again; Undo Last
/// Rename takes back the registry row's newest rename as the read showed it, and is refused in
/// the words the page showed where the read offers no undo.
pub(crate) fn page_change(asked: &MachineRequest, read: &PageSnapshot) -> PageWrite {
    let rev = match asked {
        MachineRequest::Read => return PageWrite::Refused("a read writes nothing".to_owned()),
        MachineRequest::Save { rev, .. }
        | MachineRequest::Random { rev }
        | MachineRequest::Sync { rev }
        | MachineRequest::Authorize { rev }
        | MachineRequest::Undo { rev } => *rev,
    };
    if rev != read.rev {
        return PageWrite::Refused(STALE.to_owned());
    }
    let untouched = || {
        (
            Field::open(None, read.held.number_text()),
            Field::open(None, &read.held.word),
        )
    };
    match asked {
        MachineRequest::Read => unreachable!("answered above"),
        MachineRequest::Random { .. } => PageWrite::Change(NameChange::Nickname(NickTo::Random)),
        MachineRequest::Undo { .. } => read.undo.undo.clone().map_or_else(
            || PageWrite::Refused(read.undo.line.clone()),
            |undo| PageWrite::Change(NameChange::Undo(undo)),
        ),
        MachineRequest::Save { number, nick, .. } => {
            let number = Field::open(number.as_deref(), read.held.number_text());
            let nick = Field::open(nick.as_deref(), &read.held.word);
            let judged = verdict(&read.snapshot, &read.held, &number, &nick);
            if !judged.renames {
                return PageWrite::Refused(judged.text);
            }
            match request(&read.snapshot, &read.held, &number, &nick) {
                Err(why) => PageWrite::Refused(why),
                Ok(RenameTo::Authorize) => PageWrite::Authorize,
                Ok(to) => rename_change(to).map_or_else(
                    || PageWrite::Refused("nothing to save: the fields hold this name".to_owned()),
                    PageWrite::Change,
                ),
            }
        }
        MachineRequest::Sync { .. } => {
            let (number, nick) = untouched();
            match request(&read.snapshot, &read.held, &number, &nick) {
                Ok(RenameTo::Synchronize(number)) => {
                    PageWrite::Change(NameChange::Synchronize(number))
                }
                _ => PageWrite::Refused("this name has nothing waiting to synchronize".to_owned()),
            }
        }
        MachineRequest::Authorize { .. } => {
            let (number, nick) = untouched();
            let primary = request(&read.snapshot, &read.held, &number, &nick);
            if primary == Ok(RenameTo::Authorize)
                || read
                    .snapshot
                    .authorize_saved(&read.held, &number, &nick)
                    .is_some()
            {
                PageWrite::Authorize
            } else {
                PageWrite::Refused("this name's naming permission needs nothing".to_owned())
            }
        }
    }
}

/// How the NAME was claimed, in the page's words.
fn claimed(name: &aterm_agent::machine_name::file::MachineName, waits: bool) -> String {
    use aterm_types::machine_name::Class;
    if name.num.is_none() {
        return "No number yet".to_owned();
    }
    let how = match name.class {
        Class::Auto => "Drawn by aterm",
        Class::Chosen => "Chosen",
        Class::Seeded => "Carried over from this Mac's earlier name",
    };
    if waits {
        format!("{how}; not claimed in this fleet yet")
    } else {
        how.to_owned()
    }
}

/// This Mac's fleet membership, in the page's words.
fn fleet_words(fleet: &InFleet) -> String {
    match fleet {
        InFleet::Paused(why) | InFleet::Unread(why) => format!("{} ({why})", fleet.phrase()),
        InFleet::In | InFleet::Joining | InFleet::Out => fleet.phrase().to_owned(),
    }
}

/// The page read `found`, read `seq`, shows. A read that failed keeps the editor where some
/// name was shown before, judged against the window's view or else the page's last read
/// (`view`, `previous`), as Rename This Mac… judges against the view until its read lands; with
/// neither it says why the name is unread.
pub(crate) fn page_read(
    seq: u64,
    found: PageFound,
    view: Option<&MachineView>,
    previous: Option<&PageSnapshot>,
) -> PageRead {
    let PageFound {
        snapshot,
        fleet,
        history,
    } = found;
    let fleet = fleet.or_else(|| view.map(|view| view.fleet.clone()));
    let mut facts = Vec::new();
    let held = match (&snapshot, snapshot.preview()) {
        (_, Some(preview)) => {
            let held = Held::of_read(preview);
            let name = preview.name();
            facts.push(("Name".to_owned(), held.name.clone()));
            facts.push(("Nickname".to_owned(), held.nick.clone()));
            facts.push(("Claimed".to_owned(), claimed(name, held.waits)));
            // A read never mints the key; the first write of this Mac's name does (a Save or
            // New Random Name here, the panel's or `aterm name`'s), whose checks are judged as
            // this node's against the books, and so does joining a fleet.
            facts.push((
                "Node".to_owned(),
                preview.node().map_or_else(
                    || {
                        format!(
                            "No node key yet; this {OS_NOUN} makes one at its first rename or \
                             when it joins a fleet"
                        )
                    },
                    |node| node.to_string(),
                ),
            ));
            if let Some(fleet) = &fleet {
                facts.push(("Fleet".to_owned(), fleet_words(fleet)));
            }
            facts.push((
                "Earlier numbers".to_owned(),
                if name.was.is_empty() {
                    "None".to_owned()
                } else {
                    name.was
                        .iter()
                        .map(|was| was.num.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                },
            ));
            held
        }
        (Snapshot::Unread(why), None) => {
            let Some(held) = view.map(Held::of_view).or_else(|| {
                previous.map(|previous| Held {
                    read: false,
                    waits: false,
                    ..previous.held.clone()
                })
            }) else {
                return PageRead::Unread(why.clone());
            };
            facts.push(("Name".to_owned(), held.name.clone()));
            facts.push(("Nickname".to_owned(), held.nick.clone()));
            if let Some(fleet) = &fleet {
                facts.push(("Fleet".to_owned(), fleet_words(fleet)));
            }
            facts.push((
                "Status".to_owned(),
                format!("This {OS_NOUN}'s name could not be read: {why}"),
            ));
            held
        }
        (Snapshot::Reading | Snapshot::Read(_), None) => return PageRead::Reading,
    };
    // Made once here, as the read lands, so no draw or press judges History again.
    let undo = undo_offer(&snapshot, &history);
    let history = snapshot.preview().map(|_| history_rows(&history));
    PageRead::Read(Arc::new(PageSnapshot {
        rev: seq,
        held,
        snapshot,
        facts,
        undo,
        history,
    }))
}

/// Read `seq`: what Rename This Mac… reads ([`super::read_preview`]: the books, the vault's
/// labels, this Mac's membership and its naming status from local files), with no diagnostics
/// or discovery, then this Mac's History from its registry row within [`PAGE_HISTORY_WAIT`]
/// ([`history::read`], which only reads), brought to the window thread as [`PageWake::Read`].
pub(super) fn read_page(seq: u64, proxy: &EventLoopProxy<Wake>) {
    use aterm_agent::machine_name::{
        lab_labels, rename_preview, standing_vault, vault_roster_label,
    };
    let mut fleet = None;
    let state = aterm_types::dirs::state_dir();
    let snapshot = match &state {
        None => Snapshot::Unread("aterm has no state folder".to_owned()),
        Some(state) => {
            let vault = standing_vault();
            let lab = || lab_labels(vault.as_ref());
            match rename_preview(
                state,
                lab,
                vault_roster_label,
                aterm_netplace::unix_ms_now(),
            ) {
                Ok(preview) => match aterm_types::dirs::aterm_config_path() {
                    Some(config) => {
                        let membership = InFleet::read(state, vault.as_ref());
                        fleet = Some(membership.clone());
                        let names =
                            naming::NamingStatus::read_local(state, &config, preview.name())
                                .with_fleet_membership(membership);
                        Snapshot::named(preview, names)
                    }
                    None => Snapshot::Unread("aterm has no configuration path".to_owned()),
                },
                Err(why) => {
                    aterm_log::info!("machine name: the page's read failed: {why}");
                    Snapshot::Unread(why.to_string())
                }
            }
        }
    };
    let history = match &state {
        None => Err("aterm has no state folder".to_owned()),
        Some(state) => {
            let read = history::read(state, std::time::Instant::now() + PAGE_HISTORY_WAIT);
            if let Err(why) = &read {
                aterm_log::info!("machine name: the page's History read failed: {why}");
            }
            read
        }
    };
    page_landed(
        proxy,
        seq,
        PageFound {
            snapshot,
            fleet,
            history,
        },
    );
}

/// Bring read `seq`'s `found` to the window thread.
pub(super) fn page_landed(proxy: &EventLoopProxy<Wake>, seq: u64, found: PageFound) {
    let _ = proxy.send_event(Wake::MachinePage(PageWake::Read(seq, Box::new(found))));
}

impl crate::App {
    /// Ask the window's writer for a page read, after every write queued before it; with no
    /// writer to ask, the page says so at once.
    pub(crate) fn queue_machine_page_read(&mut self) {
        self.machine_page.reads = self.machine_page.reads.wrapping_add(1);
        let seq = self.machine_page.reads;
        match self.proxy.clone() {
            Some(proxy) => queue(&mut self.machine_writer, proxy, Job::PageRead(seq)),
            None => self.machine_page_read(
                seq,
                PageFound {
                    snapshot: Snapshot::Unread(NO_WRITER.to_owned()),
                    fleet: None,
                    history: Err(NO_WRITER.to_owned()),
                },
            ),
        }
    }

    /// One of the page's requests: a read, or a write resolved against the read it showed
    /// ([`page_change`]). Who pressed was judged as the press was dispatched (`name/press/` is
    /// person-only), and the page needs no alert, so a headless instance writes too.
    pub(crate) fn machine_page_request(&mut self, asked: MachineRequest) {
        if asked == MachineRequest::Read {
            return self.queue_machine_page_read();
        }
        let write = match &self.machine_page.state.read {
            PageRead::Read(read) => page_change(&asked, read),
            PageRead::Reading | PageRead::Unread(_) | PageRead::Unsupported(_) => {
                PageWrite::Refused(page_unread())
            }
        };
        let to = match write {
            PageWrite::Refused(why) => {
                aterm_log::info!("machine name: the page changed nothing: {why}");
                self.machine_page.state.notice = Some(why);
                return self.machine_page_published();
            }
            PageWrite::Authorize => {
                self.machine_page.sets = self.machine_page.sets.wrapping_add(1);
                self.machine_page.state.notice = None;
                self.machine_page_published();
                return self.open_machine_name_authorization(Origin::Page, self.machine_page.sets);
            }
            PageWrite::Change(to) => to,
        };
        let asked = match asked {
            MachineRequest::Save { number, nick, .. } => Asked { number, nick },
            _ => Asked {
                number: None,
                nick: None,
            },
        };
        self.queue_machine_page_change(to, asked);
    }

    /// Hand the page's `to` to the window's writer as its newest write, then read after it.
    pub(super) fn queue_machine_page_change(&mut self, to: NameChange, asked: Asked) {
        let Some(proxy) = self.proxy.clone() else {
            self.machine_page.state.notice = Some(format!("Last attempt: {NO_WRITER}"));
            return self.machine_page_published();
        };
        self.machine_page.sets = self.machine_page.sets.wrapping_add(1);
        self.machine_page.state.notice = None;
        self.machine_page.state.busy = true;
        queue(
            &mut self.machine_writer,
            proxy,
            Job::Change(NameJob {
                to,
                generation: self.machine_page.sets,
                asked,
                origin: Origin::Page,
            }),
        );
        self.queue_machine_page_read();
        self.machine_page_published();
    }

    /// [`PageWake`]: a read lands, or a page write's failure comes back.
    pub(crate) fn machine_page_wake(&mut self, wake: PageWake) {
        match wake {
            PageWake::Read(seq, found) => self.machine_page_read(seq, *found),
            PageWake::NotSet(generation, why) => self.machine_page_not_set(generation, why),
        }
    }

    /// Read `seq` landed: the page shows it only while it is the newest read asked for, so a
    /// read a later one supersedes never replaces what that one finds.
    pub(crate) fn machine_page_read(&mut self, seq: u64, found: PageFound) {
        if seq != self.machine_page.reads {
            return;
        }
        let previous = match &self.machine_page.state.read {
            PageRead::Read(read) => Some(Arc::clone(read)),
            PageRead::Reading | PageRead::Unread(_) | PageRead::Unsupported(_) => None,
        };
        let state = &mut self.machine_page.state;
        state.read = page_read(seq, found, self.machine_view.as_ref(), previous.as_deref());
        state.busy = false;
        self.machine_page_published();
    }

    /// Page write `generation` took no effect, or its publication is pending: the page's notice
    /// says why until its next write, unless a later page write started since. The Rename panel's
    /// own failure ([`Self::machine_name_not_set`]) is left as it is.
    pub(crate) fn machine_page_not_set(&mut self, generation: u64, why: String) {
        if generation != self.machine_page.sets {
            aterm_log::info!("machine name: a superseded page write failed ({why})");
            return;
        }
        self.machine_page.state.notice = Some(why);
        self.machine_page_published();
    }

    /// Whether `origin`'s newest write is `generation`.
    pub(crate) const fn machine_write_is_newest(&self, origin: Origin, generation: u64) -> bool {
        generation
            == match origin {
                Origin::Panel => self.machine_sets,
                Origin::Page => self.machine_page.sets,
            }
    }
}

#[cfg(test)]
#[path = "page_fixture.rs"]
pub(crate) mod fixture;

#[cfg(test)]
#[path = "page_tests.rs"]
mod tests;
