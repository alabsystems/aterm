// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One Numeric ID/Name panel with actual aterm, fleet and macOS naming status. Its explicit
//! open reads the books and system names on the name writer, after earlier requests, then
//! delivers them through AppKit's modal loop. An unchanged saved number with pending
//! publication offers Retry Sync; only a changed number selects another canonical name.
//!
//! Each field is the person's only once what it holds differs from what aterm would put there,
//! the read's text once that lands ([`Field`]): a field they have not touched follows the read on
//! its own, whatever the other holds, and asks for nothing, so once the read lands a window view
//! a change pressed before has outdated neither shows a stale number or name nor turns into
//! one; while the read is out, or after it failed, the fields stay at the view, still asking for
//! nothing, and the line says so. A Number left alone or emptied keeps this Mac's number, so a
//! name alone keeps the number, an unnumbered Mac's included
//! ([`RenameTo`]). A number a migration still waits on holds no claim:
//! the Numeric ID field opens empty, and typing that number claims it. Rename is unavailable while
//! the line refuses what is typed. The set reads again under its locks, so a number the
//! line passed can still be refused, and that refusal leads the alert at the next press, its
//! fields holding what that press typed ([`crate::machine_title::NameUnset`]).

use aterm_agent::machine_name::file::MachineName;
use aterm_agent::machine_name::{NameTo, RenamePreview};
use aterm_types::machine_name::{Nickname, Number, Refusal, Verdict};

use crate::machine_title::MachineView;
use crate::machine_title::naming::NamingStatus;

/// The name the fields are judged against: the window's view until the press's read lands, then
/// the read's, which follows every change pressed before it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Held {
    /// The NAME: `m8`, or `unnumbered` while unnumbered.
    pub(crate) name: String,
    /// The name as the menu bar shows it.
    pub(crate) nick: String,
    /// The display spelling, which the Name field holds.
    pub(crate) word: String,
    /// Whether this is the press's read rather than the window's view, which may be stale.
    pub(crate) read: bool,
    /// Whether the number shown is one a migration still waits on, holding no claim yet
    /// ([`RenamePreview::waits`]).
    pub(crate) waits: bool,
}

impl Held {
    pub(crate) fn of_view(view: &MachineView) -> Self {
        Self {
            name: view.name.clone(),
            nick: view.nick.clone(),
            word: view.word.clone(),
            read: false,
            waits: false,
        }
    }

    pub(crate) fn of_read(preview: &RenamePreview) -> Self {
        let name: &MachineName = preview.name();
        Self {
            name: name.name(),
            nick: name.display_nick(),
            word: name.display_nick(),
            read: true,
            waits: preview.waits(),
        }
    }

    /// What an untouched Numeric ID field holds: the NAME, or nothing for a number that holds no
    /// claim yet, so typing it is a choice the field can tell from leaving it.
    pub(crate) fn number_text(&self) -> &str {
        if self.waits || self.unnumbered() {
            ""
        } else {
            self.name.strip_prefix('m').unwrap_or(&self.name)
        }
    }

    /// Whether this Mac has no number yet (no assigned ID).
    fn unnumbered(&self) -> bool {
        Number::parse(&self.name).is_err()
    }

    /// Whether a rename to `num` claims the number this Mac shows but holds no claim to yet.
    fn claims(&self, num: Number) -> bool {
        self.waits && num.to_string() == self.name
    }
}

/// One field as the open alert keeps it: its text, and the text aterm would put in it now
/// (`opened`: the held name's, moved to each read that lands), `None` for a field a failed set's
/// words filled until a read lands. A field is the person's ([`Field::typed`]) while its text
/// differs from that.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Field {
    text: String,
    opened: Option<String>,
}

impl Field {
    /// A field holding `typed`, the person's words from an earlier press, or else `held`, the
    /// held name's text, which it follows while untouched.
    pub(crate) fn open(typed: Option<&str>, held: &str) -> Self {
        match typed {
            Some(typed) => Self {
                text: typed.to_owned(),
                opened: None,
            },
            None => Self {
                text: held.to_owned(),
                opened: Some(held.to_owned()),
            },
        }
    }

    /// A field holding `text`, which aterm put there as `opened` (`None`: a failed set's words).
    pub(crate) fn at(text: String, opened: Option<String>) -> Self {
        Self { text, opened }
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn opened(&self) -> Option<&str> {
        self.opened.as_deref()
    }

    /// What the person typed: `None` while the field holds what aterm put there.
    pub(crate) fn typed(&self) -> Option<&str> {
        (self.opened.as_deref() != Some(self.text.as_str())).then_some(self.text.as_str())
    }

    /// Follow a read that found `fresh`: an untouched field takes it, and a typed one, a failed
    /// set's words included, keeps its text but is judged against `fresh` from now on, so typing
    /// the window's outdated value back asks for it and typing the read's own asks for nothing.
    /// Whether the text changed.
    pub(crate) fn follow(&mut self, fresh: &str) -> bool {
        let Some(opened) = &mut self.opened else {
            self.opened = Some(fresh.to_owned());
            return false;
        };
        let untouched = *opened == self.text;
        fresh.clone_into(opened);
        if !untouched || self.text == fresh {
            return false;
        }
        fresh.clone_into(&mut self.text);
        true
    }
}

/// The alert's title, which its menu row names with an ellipsis.
pub(crate) const TITLE: &str = "Rename This Mac";

/// Save (Return), or Retry Sync for pending publication, and Cancel (Escape).
pub(crate) const BUTTONS: [&str; 2] = ["Save", "Cancel"];

#[derive(Debug)]
pub(crate) struct PanelRead {
    preview: RenamePreview,
    naming: Option<NamingStatus>,
}

/// What the live line judges a number from.
#[derive(Debug)]
pub(crate) enum Snapshot {
    /// The press's read has not come back.
    Reading,
    /// This Mac's books and vault labels as the read found them.
    Read(Box<PanelRead>),
    /// The read failed, with why.
    Unread(String),
}

impl Snapshot {
    pub(crate) fn named(preview: RenamePreview, naming: NamingStatus) -> Self {
        Self::Read(Box::new(PanelRead {
            preview,
            naming: Some(naming),
        }))
    }

    pub(crate) fn read(preview: RenamePreview) -> Self {
        Self::Read(Box::new(PanelRead {
            preview,
            naming: None,
        }))
    }

    fn unchanged_request(&self, held: &Held) -> Option<RenameTo> {
        let Self::Read(read) = self else {
            return None;
        };
        if !held.read || held.waits || read.preview.name().name() != held.name {
            return None;
        }
        let num = read.preview.name().num?;
        if read.preview.name().class != aterm_types::machine_name::Class::Chosen {
            Some(RenameTo::Name(NameTo { num, nick: None }))
        } else if read.naming.as_ref().is_some_and(NamingStatus::authorize) {
            Some(RenameTo::Authorize)
        } else if read.naming.as_ref().is_some_and(NamingStatus::retry) {
            Some(RenameTo::Synchronize(num))
        } else {
            None
        }
    }
}

/// The live line under the fields, and whether Rename applies what they hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Line {
    pub(crate) text: String,
    pub(crate) renames: bool,
}

/// What Rename asks of this Mac's name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RenameTo {
    /// Nothing the fields changed: neither was typed into, a typed name is empty, or what
    /// was typed is what the read holds.
    Keep,
    /// A name alone, the number kept
    /// ([`aterm_agent::machine_name::set_nick`]), an unnumbered Mac's included.
    Nick(Nickname),
    /// A number explicitly selected by the person, including a displayed seeded number,
    /// with a typed name or keeping the held one
    /// ([`aterm_agent::machine_name::set_name`]).
    Name(NameTo),
    /// Publish the already-saved number, without choosing it again.
    Synchronize(Number),
    /// Explicitly authorize only the naming rule; no captured name crosses that prompt.
    Authorize,
}

/// What the Number and Name fields ask for, `held` being the name they are judged against
/// ([`Held`]). Only what the person typed asks for anything ([`Field::typed`]); an emptied field
/// keeps what this Mac has. A number is typed as `aterm name set` takes it, `m86` or `86`.
/// Typed text that equals the read's name keeps it, but for a number that holds no claim yet,
/// which typing claims; before the read lands, a typed number or name is asked for as typed,
/// since a change pressed before may still move the name the window shows, and the set judges it
/// from what it finds.
///
/// # Errors
/// The words the line shows for a typed number or name that is none.
pub(crate) fn rename_to(number: &Field, nick: &Field, held: &Held) -> Result<RenameTo, String> {
    let nick = match nick.typed().map(str::trim) {
        None | Some("") => None,
        Some(word) if held.read && word == held.word => None,
        Some(word) => Some(
            Nickname::parse(word)
                .map_err(|why| format!("\u{201c}{word}\u{201d} is no name: {why}."))?,
        ),
    };
    let num = match number.typed().map(str::trim) {
        None => None,
        Some(typed) => match Number::parse(typed) {
            Ok(num) if held.read && !held.waits && num.to_string() == held.name => None,
            Ok(num) => Some(num),
            // An emptied field keeps the current ID, including an unassigned one.
            Err(Refusal::Empty) => None,
            Err(why) => {
                return Err(format!(
                    "\u{201c}{typed}\u{201d} is no machine number: {why}."
                ));
            }
        },
    };
    Ok(match (num, nick) {
        (None, None) => RenameTo::Keep,
        (None, Some(nick)) => RenameTo::Nick(nick),
        (Some(num), nick) => RenameTo::Name(NameTo { num, nick }),
    })
}

/// The line for the fields `number` and `nick` judged against `held` over `snapshot`: a field's
/// refusal, the number's verdict as [`RenamePreview::judge`] gives it, and that another Mac
/// carries the name too (information only: two Macs may share one, §5.4). Before the read
/// lands, when it failed, and for a number whose vault labels it could not read, Rename stays
/// available, since the set checks the number itself, and the line names nothing of the window's
/// view, which the read may outdate.
pub(crate) fn line(snapshot: &Snapshot, held: &Held, number: &Field, nick: &Field) -> Line {
    let to = match request(snapshot, held, number, nick) {
        Ok(to) => to,
        Err(why) => {
            return Line {
                text: why,
                renames: false,
            };
        }
    };
    let preview = match snapshot {
        Snapshot::Read(read) => Some(&read.preview),
        Snapshot::Reading | Snapshot::Unread(_) => None,
    };
    let expected = preview.and_then(|preview| {
        use aterm_types::machine_name::GeneratedNames;
        let name = preview.name();
        match &to {
            RenameTo::Name(to) => Some(GeneratedNames::new(
                to.num,
                to.nick.as_ref().unwrap_or(&name.nick),
            )),
            RenameTo::Nick(nickname) => {
                name.num.map(|number| GeneratedNames::new(number, nickname))
            }
            _ => name
                .num
                .map(|number| GeneratedNames::new(number, &name.nick)),
        }
    });
    let requested_nick = match &to {
        RenameTo::Nick(nick) => Some(nick),
        RenameTo::Name(name) => name.nick.as_ref(),
        _ => None,
    };
    if let Some(nick) = requested_nick
        && preview.is_some_and(|preview| preview.shared(nick))
    {
        return Line {
            text: format!(
                "The name {} is already used by another machine.",
                nick.as_str()
            ),
            renames: false,
        };
    }
    let (text, renames) = match to {
        RenameTo::Keep => (
            match snapshot {
                Snapshot::Reading => "Reading this Mac's name\u{2026}".to_owned(),
                Snapshot::Unread(why) => format!(
                    "This Mac's name could not be read here ({why}). Type a number or name; \
                     Save checks it."
                ),
                Snapshot::Read(_) if held.waits => format!(
                    "This Mac shows {}, which it has not claimed in this fleet yet. Type {} \
                     to claim it, or another number.",
                    held.name, held.name
                ),
                Snapshot::Read(_) if held.unnumbered() => {
                    "This Mac has no number yet. Type one, such as m8.".to_owned()
                }
                Snapshot::Read(_) => "Edit the number or name.".to_owned(),
            },
            true,
        ),
        RenameTo::Nick(nick) => (
            if held.read && held.unnumbered() {
                format!(
                    "This Mac has no number yet; Save makes its name {}.",
                    nick.as_str(),
                )
            } else {
                format!(
                    "Save keeps this Mac's number and makes its name {}.",
                    nick.as_str(),
                )
            },
            true,
        ),
        RenameTo::Name(to) => {
            let num = to.num;
            let (text, renames) = match preview {
                None => match snapshot {
                    Snapshot::Unread(why) => (
                        format!("{num} could not be checked here ({why}); Save checks it."),
                        true,
                    ),
                    Snapshot::Reading | Snapshot::Read(_) => {
                        (format!("Checking {num} against this fleet\u{2026}"), true)
                    }
                },
                Some(preview) if let Some(why) = preview.unchecked(num) => (
                    format!("{num} could not be checked here ({why}); Save checks it."),
                    true,
                ),
                Some(preview) => match preview.judge(num) {
                    Ok(_)
                        if preview.name().num == Some(num)
                            && preview.name().class != aterm_types::machine_name::Class::Chosen
                            && !held.waits =>
                    {
                        (
                            format!("Save uses {num} across aterm, the fleet and macOS."),
                            true,
                        )
                    }
                    Ok(_) if held.claims(num) => (
                        format!("Save claims {num}, which this Mac shows but has not claimed yet."),
                        true,
                    ),
                    Ok(Verdict::EarlierName { of }) => (
                        format!("{num} is free; another Mac ({of}) had it earlier."),
                        true,
                    ),
                    Ok(_) => (format!("{num} is free."), true),
                    Err(why) => (sentence(&why), false),
                },
            };
            (text, renames)
        }
        RenameTo::Synchronize(number) => {
            (format!("Retry name synchronization for {number}."), true)
        }
        RenameTo::Authorize => (
            "Authorize naming only. The current saved name is read again after it finishes."
                .to_owned(),
            true,
        ),
    };
    let text = match snapshot {
        Snapshot::Read(read) if let Some(status) = &read.naming => {
            let names = match expected.as_ref() {
                Some(expected) => status.text_for(Some(expected), &held.name),
                None => status.text(&held.name),
            };
            format!("{names}\n\n{text}")
        }
        _ => text,
    };
    Line { text, renames }
}

pub(crate) fn request(
    snapshot: &Snapshot,
    held: &Held,
    number: &Field,
    nick: &Field,
) -> Result<RenameTo, String> {
    let to = rename_to(number, nick, held)?;
    Ok(match to {
        RenameTo::Keep => snapshot.unchanged_request(held).unwrap_or(RenameTo::Keep),
        to => to,
    })
}

/// `why` as a sentence: a capital first letter unless it opens with a machine number (`m20 is…`),
/// and a full stop.
fn sentence(why: &str) -> String {
    let mut chars = why.chars();
    let opens_with_number = Number::leading(why).is_some();
    let mut text = match chars.next() {
        Some(first) if !opens_with_number => first.to_uppercase().chain(chars).collect(),
        _ => why.to_owned(),
    };
    if !text.ends_with('.') {
        text.push('.');
    }
    text
}

/// Keep `snapshot` as press `press`'s, in `slot`, which holds the newest press's: whether it was
/// kept. A read for a press that ended, or that a later press superseded, is dropped.
fn keep_read(slot: &mut Option<(u64, Snapshot)>, press: u64, snapshot: Snapshot) -> bool {
    match slot {
        Some((at, held)) if *at == press => {
            *held = snapshot;
            true
        }
        _ => false,
    }
}

/// Discovery updates only status, never the name snapshot or either field's baseline.
fn keep_discovery(
    slot: &mut Option<(u64, Snapshot)>,
    press: u64,
    available: Result<bool, String>,
) -> bool {
    let Some((at, Snapshot::Read(read))) = slot else {
        return false;
    };
    if *at != press {
        return false;
    }
    let Some(naming) = &mut read.naming else {
        return false;
    };
    naming.discovered(available);
    true
}

/// End press `press` in `slot`: a later press's snapshot stays.
fn close(slot: &mut Option<(u64, Snapshot)>, press: u64) {
    if slot.as_ref().is_some_and(|(at, _)| *at == press) {
        *slot = None;
    }
}

pub(crate) use macos::{Answer, ask, begin, deliver, deliver_discovery, end};

#[cfg(test)]
mod tests;

mod macos {
    use std::cell::{Cell, RefCell};

    use aterm_objc::{CGPoint, CGRect, CGSize, Id, Obj, class, sel};

    use super::{BUTTONS, Field, Held, Line, Snapshot, close, keep_read, line};
    use crate::appkit::{self, MainThread};
    use crate::machine_title::MachineView;

    /// The accessory's width; each field row's height, and the line's.
    const WIDTH: f64 = 480.0;
    const LABEL_WIDTH: f64 = 72.0;
    const ROW: f64 = 24.0;
    const GAP: f64 = 8.0;
    const LINE_MIN: f64 = 24.0;
    const LINE_MAX: f64 = 240.0;
    const SCROLLER: f64 = 16.0;

    /// What the person answered Rename This Mac.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(crate) enum Answer {
        /// Save (or Return), with the request resolved from that press's displayed snapshot
        /// and the person's raw typed fields, retained only for refusal recovery.
        Rename {
            number: Option<String>,
            nick: Option<String>,
            requested: Result<super::RenameTo, String>,
        },
        /// Cancel, Escape, or any other end of the alert.
        Cancel,
    }

    /// The open alert's views, the name its fields are judged against, and which press opened it.
    struct Open {
        press: u64,
        held: Held,
        number: Obj,
        /// The text aterm last put in the Numeric ID field ([`Field`]'s `opened`).
        number_opened: Option<String>,
        nick: Obj,
        /// The text aterm last put in the Name field.
        nick_opened: Option<String>,
        line: Obj,
        scroll: Obj,
        accessory: Obj,
        alert: Obj,
        number_label: Obj,
        nick_label: Obj,
        line_height: Cell<f64>,
        rename: Obj,
    }

    impl Open {
        fn fields(&self) -> (Field, Field) {
            (
                Field::at(text(&self.number), self.number_opened.clone()),
                Field::at(text(&self.nick), self.nick_opened.clone()),
            )
        }
    }

    thread_local! {
        /// The newest press's number: a read another press superseded is dropped.
        static PRESSES: Cell<u64> = const { Cell::new(0) };
        /// The newest press's snapshot, kept while its alert reopens after a refusal.
        static SNAPSHOT: RefCell<Option<(u64, Snapshot)>> = const { RefCell::new(None) };
        /// The alert on screen, while one is.
        static OPEN: RefCell<Option<Open>> = const { RefCell::new(None) };
    }

    /// Start a press: its read's snapshot is [`Snapshot::Reading`] until [`deliver`] brings it.
    pub(crate) fn begin() -> u64 {
        let press = PRESSES.get().wrapping_add(1);
        PRESSES.set(press);
        SNAPSHOT.set(Some((press, Snapshot::Reading)));
        press
    }

    /// End press `press`: a read that lands later is dropped.
    pub(crate) fn end(press: u64) {
        SNAPSHOT.with_borrow_mut(|slot| close(slot, press));
    }

    /// Bring press `press`'s read to its alert, on the main thread: the fields are judged against
    /// the name it read from now on, each field the person has not typed into follows it
    /// ([`Field::follow`]), and the line is redrawn. A read for a press that ended or that a
    /// later press superseded is dropped.
    pub(crate) fn deliver(press: u64, snapshot: Snapshot) {
        let fresh = match &snapshot {
            Snapshot::Read(read) => Some(Held::of_read(&read.preview)),
            Snapshot::Reading | Snapshot::Unread(_) => None,
        };
        let kept = SNAPSHOT.with_borrow_mut(|slot| keep_read(slot, press, snapshot));
        if !kept {
            return;
        }
        if let Some(fresh) = fresh {
            // Decide under the borrow and set the fields after it: AppKit may call back in.
            let retext = OPEN.with_borrow_mut(|open| {
                let open = open.as_mut().filter(|open| open.press == press)?;
                let (mut number, mut nick) = open.fields();
                let mut retext = Vec::new();
                if number.follow(fresh.number_text()) {
                    retext.push((open.number.clone_retained(), number.text().to_owned()));
                }
                if nick.follow(&fresh.word) {
                    retext.push((open.nick.clone_retained(), nick.text().to_owned()));
                }
                open.number_opened = number.opened().map(str::to_owned);
                open.nick_opened = nick.opened().map(str::to_owned);
                open.held = fresh;
                Some(retext)
            });
            for (field, words) in retext.unwrap_or_default() {
                set_text(&field, &words);
            }
        }
        refresh();
    }

    pub(crate) fn deliver_discovery(press: u64, available: Result<bool, String>) {
        if SNAPSHOT.with_borrow_mut(|slot| super::keep_discovery(slot, press, available)) {
            refresh();
        }
    }

    /// The name press `press`'s fields are judged against: its read's once that has landed, else
    /// the window's `view`.
    fn held_for(press: u64, view: &MachineView) -> Held {
        SNAPSHOT.with_borrow(|held| match held {
            Some((at, Snapshot::Read(read))) if *at == press => Held::of_read(&read.preview),
            _ => Held::of_view(view),
        })
    }

    /// Put `words` in `field`. A field being edited stays so, the caret in it: AppKit hands the
    /// new value to its field editor (the drive's `Editing` steps hold it to that).
    fn set_text(field: &Obj, words: &str) {
        let Some(words) = appkit::nsstring(words) else {
            return;
        };
        // SAFETY: main thread (the caller, `deliver`, runs on the main queue), on a live field the
        // open alert's accessory holds. `-setStringValue:` is `-(void)(NSString *)` and copies its
        // argument.
        unsafe { appkit::send_v_id(field.id(), sel!(setStringValue:), words.id()) };
    }

    /// Read `field`'s text on the main thread.
    fn text(field: &Obj) -> String {
        // SAFETY: `-stringValue` is `-(NSString *)`, +0, on a live field the open alert's
        // accessory holds, on the main thread (every caller runs there: a delegate callback, a
        // main-queue block, or `ask`).
        unsafe {
            let typed = appkit::send_id(field.id(), sel!(stringValue));
            if typed.is_null() {
                String::new()
            } else {
                appkit::nsstring_to_rust(typed)
            }
        }
    }

    /// Show the open alert's line for what its fields hold, and make Rename available or not.
    fn refresh() {
        OPEN.with_borrow(|open| {
            let Some(open) = open else {
                return;
            };
            let (number, nick) = open.fields();
            let Line {
                text: words,
                renames,
            } = SNAPSHOT.with_borrow(|held| match held {
                Some((at, snapshot)) if *at == open.press => {
                    line(snapshot, &open.held, &number, &nick)
                }
                _ => line(&Snapshot::Reading, &open.held, &number, &nick),
            });
            let Some(words) = appkit::nsstring(&words) else {
                return;
            };
            let primary = SNAPSHOT.with_borrow(|held| match held {
                Some((at, snapshot)) if *at == open.press => {
                    match super::request(snapshot, &open.held, &number, &nick) {
                        Ok(super::RenameTo::Authorize) => "Authorize & Sync",
                        Ok(super::RenameTo::Synchronize(_)) => "Retry Sync",
                        _ => BUTTONS[0],
                    }
                }
                _ => BUTTONS[0],
            });
            let Some(button) = appkit::nsstring(primary) else {
                return;
            };
            // SAFETY: `-setStringValue:` is `-(void)(NSString *)` and copies its argument;
            // `-setTitle:` copies its NSString and `-setEnabled:` is `-(void)(BOOL)`; all on live views the alert holds, on the main
            // thread.
            unsafe {
                appkit::send_v_id(open.line.id(), sel!(setStringValue:), words.id());
                appkit::send_v_bool(open.rename.id(), sel!(setEnabled:), renames);
                appkit::send_v_id(open.rename.id(), sel!(setTitle:), button.id());
                fit_status(open);
            }
        });
    }

    /// Measure the actual wrapping cell, fit ordinary text, and scroll longer documents.
    /// # Safety
    /// Main thread; every view is retained by the live alert's Open record.
    unsafe fn fit_status(open: &Open) {
        // SAFETY: caller's contract; cellSizeForBounds has the same typed NSSize(NSRect)
        // signature as the existing developer-tools native text measurement.
        unsafe {
            let cell = appkit::send_id(open.line.id(), sel!(cell));
            if cell.is_null() {
                return;
            }
            let measured = appkit::send_size_rect(
                cell,
                sel!(cellSizeForBounds:),
                rect(0.0, 0.0, WIDTH - SCROLLER, f64::from(u16::MAX)),
            )
            .height;
            if !measured.is_finite() || measured <= 0.0 {
                return;
            }
            let document = (measured.ceil() + 4.0).max(LINE_MIN);
            let shown = document.min(LINE_MAX);
            appkit::send_v_rect(
                open.line.id(),
                sel!(setFrame:),
                rect(0.0, 0.0, WIDTH - SCROLLER, document),
            );
            appkit::send_v_rect(
                open.scroll.id(),
                sel!(setFrame:),
                rect(0.0, 0.0, WIDTH, shown),
            );
            let clip = appkit::send_id(open.scroll.id(), sel!(contentView));
            if !clip.is_null() {
                let y = if appkit::send_bool(open.line.id(), sel!(isFlipped)) {
                    0.0
                } else {
                    (document - shown).max(0.0)
                };
                appkit::send_v_point(clip, sel!(scrollToPoint:), CGPoint { x: 0.0, y });
                appkit::send_v_id(open.scroll.id(), sel!(reflectScrolledClipView:), clip);
            }
            if (shown - open.line_height.get()).abs() < 0.5 {
                return;
            }
            open.line_height.set(shown);
            let top = shown + GAP + ROW + GAP;
            let field_x = LABEL_WIDTH + GAP;
            let field_width = WIDTH - field_x;
            for (view, frame) in [
                (
                    &open.number_label,
                    rect(0.0, top + 2.0, LABEL_WIDTH, ROW - 4.0),
                ),
                (&open.number, rect(field_x, top, field_width, ROW)),
                (
                    &open.nick_label,
                    rect(0.0, shown + GAP + 2.0, LABEL_WIDTH, ROW - 4.0),
                ),
                (&open.nick, rect(field_x, shown + GAP, field_width, ROW)),
            ] {
                appkit::send_v_rect(view.id(), sel!(setFrame:), frame);
            }
            appkit::send_v_rect(
                open.accessory.id(),
                sel!(setFrame:),
                rect(0.0, 0.0, WIDTH, top + ROW),
            );
            appkit::send_v(open.alert.id(), sel!(layout));
        }
    }

    aterm_objc::declare_class! {
        /// The Number and Name fields' delegate: each edit redraws the line.
        pub(crate) struct RenameSheetTarget: NSObject {
            const NAME: &str = "ATermRenameSheetTarget";
            type Ivars = ();
            protocols: [NSObject, NSTextFieldDelegate];

            /// A field's text changed.
            @sel(controlTextDidChange:)
            fn control_text_did_change(&self, _notification: Id) {
                refresh();
            }
        }
    }

    /// Clears [`OPEN`] however [`ask`] ends.
    struct Opened;

    impl Drop for Opened {
        fn drop(&mut self) {
            OPEN.set(None);
        }
    }

    fn rect(x: f64, y: f64, width: f64, height: f64) -> CGRect {
        CGRect {
            origin: CGPoint { x, y },
            size: CGSize { width, height },
        }
    }

    /// # Safety
    /// Main thread.
    unsafe fn label(words: &str, frame: CGRect) -> Option<Obj> {
        let words = appkit::nsstring(words)?;
        // SAFETY: `+wrappingLabelWithString:` is the documented non-raising factory, +0, retained here;
        // `-setFrame:` is `-(void)(NSRect)`; main thread (the caller's contract).
        unsafe {
            let label = Obj::retain(appkit::send_id_id(
                class(c"NSTextField").as_id(),
                sel!(wrappingLabelWithString:),
                words.id(),
            ))?;
            appkit::send_v_rect(label.id(), sel!(setFrame:), frame);
            Some(label)
        }
    }

    /// # Safety
    /// Main thread.
    unsafe fn field(words: &str, frame: CGRect, target: Id) -> Option<Obj> {
        let words = appkit::nsstring(words)?;
        // SAFETY: `+[NSTextField alloc]` then `-initWithFrame:` (`-(instancetype)(NSRect)`) is
        // +1, adopted into `Obj`; `-setStringValue:` copies its argument and `-setDelegate:`
        // (`-(void)(id)`) keeps a weak reference, which `ask` keeps alive past `runModal`; main
        // thread (the caller's contract).
        unsafe {
            let field = Obj::from_owned(appkit::send_id_rect(
                appkit::alloc(class(c"NSTextField")),
                sel!(initWithFrame:),
                frame,
            ))?;
            appkit::send_v_id(field.id(), sel!(setStringValue:), words.id());
            appkit::send_v_id(field.id(), sel!(setDelegate:), target);
            Some(field)
        }
    }

    /// Ask for this Mac's number and name: an alert titled `title` over `body`, over the live
    /// line for press `press` ([`begin`]), with Save/Retry Sync (Return) and Cancel (Escape). The Number
    /// field holds `number` and the Name field `nick`, words the person typed at an earlier
    /// press, which stay theirs; a field given `None` opens at the name held, `view`'s until the
    /// press's read lands, and follows that read ([`deliver`]). The person's ⌘X, ⌘C, ⌘V and ⌘A
    /// edit the field being edited, Tab moves between the two, and Return does nothing while
    /// the primary action is unavailable, as AppKit answers a click on a disabled button
    /// ([`crate::alert_keys::watch_field_alert_keys`]); a key a control client posted does
    /// nothing. `None` when no alert could be shown, and when a key queued before it opened
    /// would not drain, since that key would land in a field or press Rename.
    pub(crate) fn ask(
        press: u64,
        title: &str,
        body: &str,
        view: &MachineView,
        number: Option<&str>,
        nick: Option<&str>,
    ) -> Option<Answer> {
        if crate::menu::os_ui_refused(title) {
            return None;
        }
        let mtm = MainThread::new()?;
        let _park = crate::watchdog::park_modal();
        let _up = crate::alert_keys::ConfirmationUp::hold();
        if !crate::hwkey::fence_queued_keys(mtm) {
            aterm_log::warn!("machine name: the key queue would not drain; not asking");
            return None;
        }
        // The key fence runs the loop, so the press's read may have landed already.
        let held = held_for(press, view);
        let number = Field::open(number, held.number_text());
        let nick = Field::open(nick, &held.word);
        let [rename, cancel] = BUTTONS;
        let (Some(title), Some(body), Some(rename), Some(cancel)) = (
            appkit::nsstring(title),
            appkit::nsstring(body),
            appkit::nsstring(rename),
            appkit::nsstring(cancel),
        ) else {
            // Foundation refused a string, so no alert can be built.
            return None;
        };
        let target = RenameSheetTarget::alloc_init(mtm, ())?;
        let alert = crate::app_icon::alert(mtm)?;
        let top = LINE_MIN + GAP + ROW + GAP;
        let field_x = LABEL_WIDTH + GAP;
        let field_width = WIDTH - field_x;
        // SAFETY: main thread (`mtm` above proves it), for `label` and `field`.
        let (number_label, number_field, nick_label, nick_field, words) = unsafe {
            (
                label("Numeric ID", rect(0.0, top + 2.0, LABEL_WIDTH, ROW - 4.0))?,
                field(
                    number.text(),
                    rect(field_x, top, field_width, ROW),
                    target.as_id(),
                )?,
                label(
                    "Name",
                    rect(0.0, LINE_MIN + GAP + 2.0, LABEL_WIDTH, ROW - 4.0),
                )?,
                field(
                    nick.text(),
                    rect(field_x, LINE_MIN + GAP, field_width, ROW),
                    target.as_id(),
                )?,
                label("", rect(0.0, 0.0, WIDTH - SCROLLER, LINE_MIN))?,
            )
        };
        // SAFETY: `+[NSView alloc]` then `-initWithFrame:` is +1, adopted into `Obj`, on the main
        // thread.
        let accessory = unsafe {
            Obj::from_owned(appkit::send_id_rect(
                appkit::alloc(class(c"NSView")),
                sel!(initWithFrame:),
                rect(0.0, 0.0, WIDTH, top + ROW),
            ))
        }?;
        // A measured status document stays fully readable when long expected/actual names
        // need more space than a compact alert. Only its viewport is bounded.
        // SAFETY: main thread; NSScrollView initWithFrame returns +1, adopted into Obj.
        let scroll = unsafe {
            Obj::from_owned(appkit::send_id_rect(
                appkit::alloc(class(c"NSScrollView")),
                sel!(initWithFrame:),
                rect(0.0, 0.0, WIDTH, LINE_MIN),
            ))
        }?;
        // SAFETY: the live scroll view owns its document view; BOOL/NSUInteger/id signatures.
        unsafe {
            appkit::send_v_bool(scroll.id(), sel!(setDrawsBackground:), false);
            appkit::send_v_bool(scroll.id(), sel!(setHasVerticalScroller:), true);
            appkit::send_v_bool(scroll.id(), sel!(setHasHorizontalScroller:), false);
            appkit::send_v_bool(scroll.id(), sel!(setAutohidesScrollers:), true);
            appkit::send_v_usize(scroll.id(), sel!(setBorderType:), 0);
            appkit::send_v_id(scroll.id(), sel!(setDocumentView:), words.id());
        }
        // SAFETY: standard `NSAlert`, `NSView`, `NSTextField` and `NSButton` messages on the main
        // thread. `-setMessageText:` and `-setInformativeText:` copy their argument, so the +1
        // strings may drop at the end of this frame; `-addSubview:` and `-setAccessoryView:`
        // retain the view they are given. `-setLineBreakMode:` takes an `NSLineBreakMode`
        // (`NSUInteger`, 0 is word wrapping) and `-setUsesSingleLineMode:` a `BOOL`, so the line
        // wraps within its frame. `-setNextKeyView:` is `-(void)(NSView *)` and keeps a weak
        // reference: both fields live as long as the accessory. `-addButtonWithTitle:` is
        // `-(NSButton *)(NSString *)` and `-window` is `-(NSWindow *)`, both +0, so both are
        // retained into `Obj`, not adopted. `-layout` is `-(void)`, and
        // `-setInitialFirstResponder:` `-(void)(NSView *)`, which the window does not retain: the
        // accessory holds the field for the alert's life. `-runModal` is `-(NSModalResponse)`,
        // an `NSInteger`. `target`, the fields' delegate, lives in this frame past `runModal`, and
        // `-setDelegate: nil` clears each field's reference to it before it drops.
        unsafe {
            appkit::send_v_id(alert.id(), sel!(setMessageText:), title.id());
            appkit::send_v_id(alert.id(), sel!(setInformativeText:), body.id());
            appkit::send_v_usize(words.id(), sel!(setLineBreakMode:), 0);
            appkit::send_v_bool(words.id(), sel!(setUsesSingleLineMode:), false);
            for view in [
                &number_label,
                &number_field,
                &nick_label,
                &nick_field,
                &scroll,
            ] {
                appkit::send_v_id(accessory.id(), sel!(addSubview:), view.id());
            }
            appkit::send_v_id(number_field.id(), sel!(setNextKeyView:), nick_field.id());
            appkit::send_v_id(nick_field.id(), sel!(setNextKeyView:), number_field.id());
            appkit::send_v_id(alert.id(), sel!(setAccessoryView:), accessory.id());
            let accept = Obj::retain(appkit::send_id_id(
                alert.id(),
                sel!(addButtonWithTitle:),
                rename.id(),
            ));
            let refuse = Obj::retain(appkit::send_id_id(
                alert.id(),
                sel!(addButtonWithTitle:),
                cancel.id(),
            ));
            appkit::send_v(alert.id(), sel!(layout));
            let panel = Obj::retain(appkit::send_id(alert.id(), sel!(window)));
            if let Some(panel) = &panel {
                appkit::send_v_id(
                    panel.id(),
                    sel!(setInitialFirstResponder:),
                    number_field.id(),
                );
            }
            let _opened = accept.as_ref().map(|accept| {
                OPEN.set(Some(Open {
                    press,
                    held: held.clone(),
                    number: number_field.clone_retained(),
                    number_opened: number.opened().map(str::to_owned),
                    nick: nick_field.clone_retained(),
                    nick_opened: nick.opened().map(str::to_owned),
                    line: words.clone_retained(),
                    scroll: scroll.clone_retained(),
                    accessory: accessory.clone_retained(),
                    alert: alert.clone_retained(),
                    number_label: number_label.clone_retained(),
                    nick_label: nick_label.clone_retained(),
                    line_height: Cell::new(LINE_MIN),
                    rename: accept.clone_retained(),
                }));
                Opened
            });
            refresh();
            // Return presses Rename and Escape Cancel, as in every aterm alert; a key a control
            // client posted presses neither, nor types into a field.
            let _keys = match (panel, accept, refuse) {
                (Some(panel), Some(accept), Some(refuse)) => {
                    crate::alert_keys::watch_field_alert_keys(
                        panel,
                        vec![number_field.clone_retained(), nick_field.clone_retained()],
                        accept,
                        refuse,
                    )
                }
                _ => None,
            };
            let response = appkit::send_isize(alert.id(), sel!(runModal));
            // The fields may outlive this frame in the autoreleased alert: drop their weak
            // references before the delegate goes.
            for field in [&number_field, &nick_field] {
                appkit::send_v_id(field.id(), sel!(setDelegate:), Id::NIL);
            }
            drop(target);
            // Rename stands for what the fields hold as the alert ends; with no Rename button
            // added (`accept` nil) nothing was open, and nothing could have pressed it.
            let renamed = (response == crate::appkit::consts::NS_ALERT_FIRST_BUTTON_RETURN)
                .then(|| {
                    OPEN.with_borrow(|open| {
                        open.as_ref().map(|open| {
                            let (number, nick) = open.fields();
                            Answer::Rename {
                                number: number.typed().map(str::to_owned),
                                nick: nick.typed().map(str::to_owned),
                                requested: SNAPSHOT.with_borrow(|snapshot| match snapshot {
                                    Some((at, snapshot)) if *at == open.press => {
                                        super::request(snapshot, &open.held, &number, &nick)
                                    }
                                    _ => super::request(
                                        &Snapshot::Reading,
                                        &open.held,
                                        &number,
                                        &nick,
                                    ),
                                }),
                            }
                        })
                    })
                })
                .flatten();
            Some(renamed.unwrap_or(Answer::Cancel))
        }
    }
}
