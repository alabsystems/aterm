// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Claude sign-in in a window of aterm's own, finished by itself: the one visible sign-in on
//! macOS ([`authorize`]), which only a sign-in this Mac started for its own rows opens
//! (`open_url::WINDOW_ENV`). Nothing about the sign-in leaves this Mac: the window is hosted
//! here, and the account's mailbox is read here (the fleet's mail relay is never asked,
//! [`authorize`]).
//!
//! - **The window is aterm's own.** The authorization page opens in a small titled window the
//!   `aterm` executable hosts (`aterm __sign-in-window`, `aterm-signin-window`), which this
//!   process starts and drives over its pipes. It is no browser of the person's: nothing of
//!   theirs is read, pressed or signed out. It comes to the front without making aterm the
//!   active application, and is closed when the flow ends either way. A person may watch it,
//!   type in it, or close it; closing it ends the sign-in ([`Stop::Closed`]). What it opens
//!   (its title, which names the account, and the account's store) reaches the host on its
//!   standard input, never in an argument. One window is open at a time on this Mac, a
//!   sign-in's or a token's ([`window_turn`]).
//! - **A session per account.** The page's cookies live in a website data store of the
//!   account's own ([`store_id`]), so no account shares a claude.ai session with another or
//!   with the person's browsers, and a later sign-in of the same account finds its session
//!   and needs only the consent page.
//! - **Where a page goes.** The window's page, and the page it reads a link in, are sent only
//!   to the flow's pinned hosts and to the waiting sign-in's own loopback port
//!   (`MailboxFlow::navigation_hosts`, `loopback_port`): the host refuses any other
//!   navigation of a whole page before it starts.
//! - **What is read.** One script per read answers, for a document on the flow's own hosts,
//!   its address, its title, the start of its text, the marks of Cloudflare's challenge, the
//!   texts of its enabled buttons and whether its login-code field holds a value; of any
//!   other document it answers the address alone ([`Facts`]). The address a decision is made
//!   on is the one the document itself gives, never the one the web view is on its way to.
//! - **What is set and pressed.** Every set and every press is told the address of the
//!   document the read before it was made in, and does nothing in any other: a page that
//!   moved between the read and the act is read again. The account's email is set in the
//!   sign-in page's email field (through the field's own value setter, with the `input` and
//!   `change` events a page's framework listens for) and read back, at the press, before
//!   `Continue with email` is pressed. Three ordinary buttons are pressed by their exact
//!   text, each at most once per flow ([`Button`]): `Continue with email`, `Verify email
//!   address` over a code aterm itself set (a code a person types is theirs to verify), and
//!   `Authorize`. The code is the account's own, read from its configured mailbox once and
//!   held until the field keeps it; the page is read again after the mailbox was, before the
//!   code is set. A mailbox that cannot be read ([`MAX_MAILBOX_ERRORS`] reads in all), or
//!   whose answer cannot be used (a code no sign-in page takes, a link that is not this
//!   sign-in's, a link whose page shows such a code), is no longer read and ends nothing: the
//!   code page waits, until the deadline, for a person at this Mac to type the code, as it
//!   does for an account with no mailbox the window reads ([`Phase::CodeNoMailbox`]), and a
//!   deadline there says why the mailbox was dropped ([`Stop::MailboxDropped`]). Where the
//!   mailbox holds a sign-in link instead, the link is opened in a page of the same store that is not shown ([`Desk::link_code`]): Claude's link shows the
//!   login code for the page that asked for it, which stands as it was and takes that code
//!   (measured 2026-11-12: opened in the window's own page, the link showed the code and left
//!   no field to set it in). The link is never opened in the window's page: its page is read
//!   again until it shows the code, and one that went on by itself (a link that signs its
//!   store in) has the window's page asked for the authorization again. The code and the
//!   link reach the window's host on its standard input, never in an argument, and no report
//!   holds either.
//! - **Whose account.** `Authorize` is pressed only where the consent page names the account's
//!   email, or names no address in a store this flow itself signed in as that account (in
//!   this run, or in an earlier one it recorded, [`Desk::store_is_the_accounts`]). A page
//!   that names another address, or none in a store nothing recorded, ends the sign-in with
//!   nothing pressed ([`Stop::WrongAccount`], [`Stop::UnknownSession`]), and the account's
//!   own store is emptied ([`Desk::forget`]) so its next sign-in starts at the sign-in page:
//!   that forgets a session in aterm's window alone, and signs nothing out anywhere.
//! - **Cloudflare's check is never operated.** A page that is its check is waited on with
//!   nothing read from its controls, pressed or filled: it may clear by itself, and a person
//!   at the desk may complete it. One that stands until the deadline ends the sign-in naming
//!   Cloudflare ([`Stop::BotCheck`]).
//! - **The consent page's human check is never operated.** Claude's consent page may carry an
//!   hCaptcha and keep `Authorize` disabled until it is answered. It comes and goes (measured
//!   2026-11-12: in this window it stood for the whole four minutes in three sign-ins of one
//!   account, and in two further loads of the page; hours later two accounts' sign-ins met
//!   none, found `Authorize` enabled and finished by themselves). The flow reads only that the check's widget is in the page and that
//!   `Authorize` is disabled, waits, and presses `Authorize` once the page enables it: after
//!   the check passed by itself, or a person at the desk completed it in the window. One
//!   that stands until the deadline ends the sign-in naming it ([`Stop::HumanCheck`]), and
//!   the row is left to a person at this Mac.
//! - **The callback.** A loopback flow's callback is a navigation the window follows: the
//!   page requests `http://localhost:<port>/callback` itself, the waiting `claude auth login`
//!   answers it and sends the page on to its success page, and seeing that page ends the flow
//!   ([`Done::Delivered`]). Nothing here requests the callback a second time. A hosted flow's
//!   callback stays in the page's address and is returned for its one delivery
//!   ([`Done::Callback`]).
//!
//! The flow is a state machine over injected seams ([`Desk`]): [`drive`] holds every decision,
//! and [`Window`] is the desk that talks to the window's host. No test opens a window.

use super::headless_oauth::{
    MAILBOX_TIMEOUT, MailboxConfig, MailboxCredential, Marker, Page, VisibleDone, VisibleStop,
};
use super::open_url::MailboxFlow;
use std::collections::BTreeSet;
use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use aterm_json::Value;
use aterm_signin_window::wire;

/// How often the page is read.
const TICK: Duration = Duration::from_millis(500);

/// How long one sign-in in the window may take, the opening of the window included.
pub(super) const WITHIN: Duration = Duration::from_secs(240);

/// How long a loopback callback may stand in the page's address before the waiting sign-in's
/// listener has plainly not answered it ([`Stop::ListenerSilent`]).
const CALLBACK_SETTLE: Duration = Duration::from_secs(15);

/// The gap between two reads of the mailbox.
const MAILBOX_EVERY: Duration = Duration::from_secs(2);

/// The most a sign-in link's page is waited on, at its first read, for the login code it
/// shows; each later read of it is one look.
const LINK_WITHIN: Duration = Duration::from_secs(20);

/// The most one read of the mailbox waits. A mailbox's first connection is inside it and
/// ends the sign-in where it does not fit: Proton's took 5 s cold and 1 s warm (measured
/// 2026-11-12 on this Mac).
const MAILBOX_WITHIN: Duration = Duration::from_secs(20);

/// How many reads or acts in a row may fail before the flow ends on the last one's reason.
const MAX_UNREAD: usize = 6;

/// How many reads of the mailbox may fail, for any reason but that its sign-in email has not
/// come yet, before the window stops reading it: one refused connection is tried again
/// (measured 2026-11-12: Proton's answered "connection failed" once and read the next time), a
/// refused password is not asked for four minutes. A mailbox dropped so does not end the
/// sign-in ([`Progress::drop_mailbox`]): its line says once why, and the code page waits,
/// until the deadline, for a person at this Mac to type the code ([`Phase::CodeNoMailbox`]).
const MAX_MAILBOX_ERRORS: usize = 3;

/// How long a sign-in waits for another sign-in window on this Mac to close before it opens
/// its own ([`window_turn`]), and how often it looks.
const TURN_WITHIN: Duration = WITHIN;
const TURN_EVERY: Duration = Duration::from_millis(500);

/// How many times a value that did not stay in its field (the account's email, the login
/// code) is set, and how many reads may find no email field beside `Continue with email`.
const MAX_FILLS: usize = 3;

/// One ordinary sign-in button; each is pressed at most once per flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Button {
    /// The sign-in page's `Continue with email`.
    ContinueWithEmail,
    /// The code page's `Verify email address`, pressed only once its field holds a code.
    VerifyEmailAddress,
    /// The OAuth consent page's `Authorize`.
    Authorize,
}

impl Button {
    /// Its text, as the progress line says it.
    pub(super) const fn title(self) -> &'static str {
        match self {
            Self::ContinueWithEmail => "Continue with email",
            Self::VerifyEmailAddress => "Verify email address",
            Self::Authorize => "Authorize",
        }
    }

    /// Whether a button whose text is `shown` is this one: the whole text, its runs of white
    /// space taken as one space and its letters' case apart (a page's style may capitalize
    /// what it shows). Never a part of a longer text.
    pub(super) fn is(self, shown: &str) -> bool {
        let shown = shown.split_whitespace().collect::<Vec<_>>().join(" ");
        shown.eq_ignore_ascii_case(self.title())
    }
}

/// A field of the sign-in's pages that aterm sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Field {
    /// The sign-in page's email field.
    Email,
    /// The code page's login-code field.
    Code,
}

/// How setting a field ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Filled {
    /// The field holds the value, read back.
    Kept,
    /// The field was set and does not hold the value.
    NotKept,
    /// The page has no such field.
    NoField,
    /// Nothing was set: the document is no longer the one the read before was made in, or
    /// it is Cloudflare's page. The next read decides, and this is counted against nothing.
    Moved,
}

/// What one read of the window's page found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Facts {
    /// The address of the document that was read, as that document itself says it; empty
    /// while none is loaded. Every decision about the page's content is made on it.
    pub url: String,
    /// The address the web view says it is at, which names a navigation's destination before
    /// its document has replaced the one that was read. Only this flow's own callback is
    /// read from it; empty where the reader gives none.
    pub active: String,
    /// The error code of a navigation of the window's page that failed since the last read
    /// (an address nothing answered), where one did.
    pub load_failed: Option<i64>,
    /// Its title.
    pub title: String,
    /// The start of its visible text.
    pub text: String,
    /// The marks of Cloudflare's challenge machinery on it.
    pub markers: Vec<Marker>,
    /// The texts of its enabled, visible buttons, in document order; none is read from a page
    /// that is Cloudflare's check or block page.
    pub buttons: Vec<String>,
    /// The texts of its visible buttons that are disabled.
    pub disabled: Vec<String>,
    /// The human check the page carries, where it carries one (`hCaptcha`, `reCAPTCHA`,
    /// `Turnstile`): only that its widget is in the page. Nothing of it is read or touched.
    pub check: Option<String>,
    /// Its login-code field, where it has one: whether it holds a value.
    pub code: Option<bool>,
}

/// Why a read or an act did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DeskError {
    /// This process has no desktop session: no window was opened.
    NoDesktop,
    /// The window is gone: it said it was closed, which a person does.
    Gone,
    /// The window's host ended without saying its window closed: it failed.
    Ended,
    /// The window was never opened: why.
    NoWindow(String),
    /// This read or act failed, and the next may not: why, with no address, code or page text.
    Unread(String),
}

/// The seams a sign-in runs over: the window, the mailbox and the clock.
pub(super) trait Desk {
    /// Opens `url` in a window of its own, within `within`.
    fn open(&mut self, url: &str, within: Duration) -> Result<(), DeskError>;
    /// Reads the window's page.
    fn facts(&mut self) -> Result<Facts, DeskError>;
    /// Presses `button` in the document at `at` (the address the read before gave), and in
    /// no other; whether it was there, enabled and pressed. `Continue with email` is pressed
    /// only while the email field holds `email`, and `Verify email address` only while the
    /// code field holds a value, each read at the press.
    fn press(&mut self, button: Button, email: &str, at: &str) -> Result<bool, DeskError>;
    /// Sets `field` of the document at `at` (the address the read before gave), and of no
    /// other, to `value`, and reads it back.
    fn fill(&mut self, field: Field, value: &str, at: &str) -> Result<Filled, DeskError>;
    /// Opens `url` in the window.
    fn go(&mut self, url: &str) -> Result<(), DeskError>;
    /// Reads the sign-in email's `link` in a page of the window's store that is not shown,
    /// leaving the window's page as it stands: the link is opened there once, at the first
    /// call for it, and that page is read for at most `within` (one look where it is zero).
    fn link_code(&mut self, link: &str, within: Duration) -> Result<Linked, DeskError>;
    /// Empties the window's data store and its record ([`Desk::note_signed_in`]): the next
    /// page in it starts signed out. Nothing of any other store is touched.
    fn forget(&mut self);
    /// Closes the window.
    fn close(&mut self);
    /// Whether the window's data store is one an earlier sign-in of this flow's signed in as
    /// the account ([`Desk::note_signed_in`]).
    fn store_is_the_accounts(&self) -> bool;
    /// Records that this flow signed the window's data store in as the account.
    fn note_signed_in(&mut self);
    /// Whether the account has a mailbox this desk can read.
    fn has_mailbox(&self) -> bool;
    /// The account's newest sign-in code or magic link, each given once, waiting at most
    /// `within`: [`MAILBOX_TIMEOUT`] where no sign-in email has come yet, any other words
    /// where the mailbox cannot be read.
    fn mailbox(&mut self, within: Duration) -> Result<MailboxCredential, &'static str>;
    /// The clock.
    fn now(&self) -> Instant;
    /// Waits.
    fn pause(&mut self, wait: Duration);
    /// Whether the sign-in was cancelled.
    fn cancelled(&self) -> bool;
    /// Says where the sign-in stands; never an address, a code or page text.
    fn say(&mut self, line: &str);
}

/// What a read of a sign-in link's page, aside, found ([`Desk::link_code`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Linked {
    /// The login code the page shows.
    Code(zeroize::Zeroizing<String>),
    /// The page stands at the link and shows no code yet.
    Waiting,
    /// The page went on from the link by itself: a link that signs its store in.
    Moved,
}

/// Where a sign-in stands, said once as it gets there and named by a sign-in that ran out of
/// time ([`Stop::TimedOut`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    /// The window is open and its page has not loaded.
    Opening,
    /// Cloudflare's check stands; nothing is pressed.
    BotCheck,
    /// The consent page holds its `Authorize` behind a human check; nothing is pressed.
    HumanCheck,
    /// A Claude page with none of the controls this flow presses.
    Waiting,
    /// `Continue with email` was pressed.
    EmailSent,
    /// The code page stands and the mailbox is being read.
    Code,
    /// The code page stands and the account has no mailbox this window reads, or the one it
    /// had was dropped ([`Progress::drop_mailbox`]) and is no longer read: the code is a
    /// person's to type.
    CodeNoMailbox,
    /// The code field holds a value aterm did not set: a person is typing the code, and
    /// `Verify email address` is theirs to press.
    CodeTyped,
    /// The code aterm set did not stay in the field.
    CodeNotKept,
    /// The mailbox's sign-in link showed the login code, which goes in the page's field.
    LinkCode,
    /// The mailbox's sign-in link is open aside and has shown no login code yet.
    LinkWait,
    /// The mailbox's sign-in link went on by itself, and the window's page asked for the
    /// authorization again.
    MagicLink,
    /// `Verify email address` was pressed.
    Verifying,
    /// `Authorize` was pressed.
    Authorized,
    /// The page is handing the callback to the waiting sign-in.
    Callback,
}

impl Phase {
    /// Its words: where the sign-in stands.
    pub(super) const fn words(self) -> &'static str {
        match self {
            Self::Opening => "its page had not loaded",
            Self::BotCheck => {
                "Cloudflare's check stood, which aterm never operates (it may clear by itself, \
                 and a person at this Mac may complete it)"
            }
            Self::HumanCheck => {
                "the consent page held Authorize behind a human check, which aterm never \
                 operates (a person at this Mac may complete it in the window)"
            }
            Self::Waiting => "its page showed none of the sign-in's buttons",
            Self::EmailSent => "Continue with email was pressed and no code page followed",
            Self::Code => "it waited for the account's sign-in email",
            Self::CodeNoMailbox => {
                "the page asked for a login code and nobody typed one in the window"
            }
            Self::CodeTyped => {
                "the code field held a value aterm did not set, and Verify email address was \
                 not pressed"
            }
            Self::CodeNotKept => "the login code did not stay in the page's field",
            Self::LinkCode => {
                "the sign-in email's link showed a login code and the page did not take it"
            }
            Self::LinkWait => "the sign-in email's link showed no login code",
            Self::MagicLink => {
                "the sign-in email's link signed in by itself and no consent page followed"
            }
            Self::Verifying => "Verify email address was pressed and no consent page followed",
            Self::Authorized => "Authorize was pressed and no callback followed",
            Self::Callback => "the page was handing the callback to the waiting sign-in",
        }
    }
}

/// How a sign-in in the window ended with its callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Done {
    /// A loopback flow: the page itself requested the callback and the waiting sign-in
    /// answered it. Nothing is left to deliver.
    Delivered,
    /// A hosted flow's callback, read from the page's address: its one delivery is the
    /// caller's.
    Callback(String),
}

/// Why a sign-in in the window ended without its callback. None holds an address, a code, a
/// link or page text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Stop {
    /// This process has no desktop session to open a window in.
    NoDesktop,
    /// Cloudflare's check stood until the deadline, on this host.
    BotCheck(Option<String>),
    /// Cloudflare's block page answered, on this host.
    BlockPage(Option<String>),
    /// The consent page held its `Authorize` behind a human check until the deadline, on
    /// this host.
    HumanCheck(Option<String>),
    /// The consent page names an address that is not the account's: the window's store is
    /// signed in to Claude as another account.
    WrongAccount,
    /// The consent page names no address, and nothing establishes that this flow signed the
    /// window's store in as the account.
    UnknownSession,
    /// The sign-in page's email field: why the account's email is not in it.
    Email(&'static str),
    /// The sign-in was cancelled.
    Cancelled,
    /// A person closed the window.
    Closed,
    /// The page left the flow's pinned pages.
    LeftFlow,
    /// The window's page did not load: the navigation's error code.
    Load(i64),
    /// The window was never opened: why.
    NoWindow(String),
    /// Another sign-in window on this Mac stayed open for as long as this one waited.
    Busy,
    /// The page reached a local callback that is not this flow's.
    InvalidCallback,
    /// The callback stood in the page's address and the waiting sign-in's listener never
    /// answered it.
    ListenerSilent,
    /// The deadline came: where the sign-in stood.
    TimedOut(Phase),
    /// The deadline came on the code page after the account's mailbox was dropped
    /// ([`Progress::drop_mailbox`]) and nobody typed the code: why it was dropped, in the
    /// mailbox's own words, so the row's record says what to repair.
    MailboxDropped(&'static str),
    /// The window could not be read or driven: why.
    Desk(String),
}

impl std::fmt::Display for Stop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoDesktop => f.write_str(super::headless_oauth::WINDOW_NO_DESKTOP),
            Self::BotCheck(_) => f.write_str(super::headless_oauth::WINDOW_BOT_CHECK),
            Self::BlockPage(_) => f.write_str(super::headless_oauth::WINDOW_BLOCK_PAGE),
            Self::HumanCheck(_) => f.write_str(super::headless_oauth::WINDOW_HUMAN_CHECK),
            Self::WrongAccount => f.write_str(super::headless_oauth::WINDOW_WRONG_ACCOUNT),
            Self::UnknownSession => f.write_str(
                "the sign-in window's consent page names no account, and aterm authorizes only \
                 an account the page names or one it signed in itself; the window's session \
                 was forgotten, so the next sign-in starts at the sign-in page",
            ),
            Self::Email(why) => write!(f, "the sign-in page's email field: {why}"),
            Self::Cancelled => f.write_str("account work admissions are paused"),
            Self::Closed => f.write_str(super::headless_oauth::WINDOW_CLOSED),
            Self::LeftFlow => f.write_str("the sign-in window left the Claude sign-in's pages"),
            Self::Load(code) => write!(
                f,
                "the sign-in window's page did not load (error {code}): this Mac may be \
                 offline"
            ),
            Self::NoWindow(why) => write!(f, "the sign-in window was not opened: {why}"),
            Self::Busy => f.write_str(
                "another sign-in window on this Mac stayed open, and one is open at a time",
            ),
            Self::InvalidCallback => {
                f.write_str("the sign-in window reached an invalid local callback")
            }
            Self::ListenerSilent => f.write_str(
                "the sign-in window reached the callback, and the waiting sign-in's local \
                 listener did not answer it",
            ),
            Self::TimedOut(phase) => write!(
                f,
                "the sign-in in aterm's window was not completed in {} s: {}",
                WITHIN.as_secs(),
                phase.words()
            ),
            Self::MailboxDropped(why) => write!(
                f,
                "the sign-in in aterm's window was not completed in {} s: nobody typed the \
                 login code, and the account's mailbox could not be used ({why})",
                WITHIN.as_secs()
            ),
            Self::Desk(why) => write!(f, "the sign-in window could not be driven: {why}"),
        }
    }
}

impl From<Stop> for VisibleStop {
    fn from(stop: Stop) -> Self {
        match stop {
            Stop::NoDesktop => Self::NoDesktop,
            Stop::BotCheck(host) => Self::BotCheck(host),
            Stop::BlockPage(host) => Self::BlockPage(host),
            Stop::HumanCheck(host) => Self::HumanCheck(host),
            Stop::WrongAccount => Self::WrongAccount,
            Stop::Closed => Self::Closed,
            other => Self::Failed(other.to_string()),
        }
    }
}

impl From<DeskError> for Stop {
    fn from(error: DeskError) -> Self {
        match error {
            DeskError::NoDesktop => Self::NoDesktop,
            DeskError::Gone => Self::Closed,
            DeskError::Ended => Self::Desk("its host ended".into()),
            DeskError::NoWindow(why) => Self::NoWindow(why),
            DeskError::Unread(why) => Self::Desk(why),
        }
    }
}

/// Why one read's decision did not finish: the flow's end, or an act that failed and may not
/// the next time, which is counted with the reads that failed ([`MAX_UNREAD`]).
enum Halt {
    Stop(Stop),
    Unread(String),
}

impl From<Stop> for Halt {
    fn from(stop: Stop) -> Self {
        Self::Stop(stop)
    }
}

impl From<DeskError> for Halt {
    fn from(error: DeskError) -> Self {
        match error {
            DeskError::Unread(why) => Self::Unread(why),
            other => Self::Stop(other.into()),
        }
    }
}

/// One sign-in.
pub(super) struct SignIn<'a> {
    /// The authorization page it opens.
    pub authorize_url: &'a str,
    /// The flow that page starts.
    pub flow: &'a MailboxFlow,
    /// The account's email.
    pub email: &'a str,
    /// How long it may take, the opening of its window included.
    pub within: Duration,
}

/// What the flow has done so far.
#[derive(Default)]
struct Progress {
    continued: bool,
    verified: bool,
    authorized: bool,
    /// The code field kept a code aterm set: `Verify email address` is pressed over it.
    code_set: bool,
    /// The code field did not keep aterm's code however often it was set.
    code_refused: bool,
    /// The sign-in link went on by itself and the authorization was asked for again.
    link_opened: bool,
    /// The sign-in link was read aside at least once.
    link_read: bool,
    /// How many reads of the mailbox failed; at [`MAX_MAILBOX_ERRORS`] it is dropped
    /// ([`Progress::drop_mailbox`]).
    mail_errors: usize,
    /// Why the mailbox was dropped and is no longer read, in its own words, never a code.
    mailbox_dropped: Option<&'static str>,
    /// The mailbox's code or link, held from the read that gave it until the page takes it:
    /// the mailbox gives each once.
    held: Option<Held>,
    fills: usize,
    email_fills: usize,
    unread: usize,
    phase: Option<Phase>,
    bot_host: Option<String>,
    bot_stands: bool,
    /// Whether the consent page's human check holds `Authorize` at the last read.
    human_stands: bool,
    callback_at: Option<Instant>,
    mailbox_at: Option<Instant>,
    /// Where the page was when `Verify email address` was pressed.
    verified_at: Option<String>,
    /// Whether the store was recorded as the account's.
    noted: bool,
}

/// A sign-in code or link the mailbox gave, scrubbed when dropped.
enum Held {
    Code(zeroize::Zeroizing<String>),
    Link(zeroize::Zeroizing<String>),
}

impl Progress {
    fn enter(&mut self, phase: Phase, desk: &mut dyn Desk) {
        if self.phase != Some(phase) {
            self.phase = Some(phase);
            desk.say(match phase {
                Phase::Opening => "the sign-in window is opening the Claude sign-in",
                Phase::BotCheck => {
                    "the page shows Cloudflare's check; aterm never operates it and is waiting \
                     for it to clear (a person at this Mac may complete it)"
                }
                Phase::HumanCheck => {
                    "the consent page holds Authorize behind a human check (hCaptcha); aterm \
                     never operates it and is waiting (a person at this Mac may complete it in \
                     the window)"
                }
                Phase::Waiting => "waiting for the sign-in page",
                Phase::EmailSent => "pressed Continue with email",
                Phase::Code => "reading the account's mailbox for its login code",
                Phase::CodeNoMailbox => {
                    "the page asks for a login code and this account has no mailbox the \
                     window can read; a person may type it in the window"
                }
                Phase::CodeTyped => {
                    "the code field holds a value aterm did not set; Verify email address is \
                     the person's to press"
                }
                Phase::CodeNotKept => "the login code did not stay in the page's field",
                Phase::LinkCode => "read the login code the sign-in email's link shows",
                Phase::LinkWait => {
                    "the sign-in email's link is open aside and shows no login code yet"
                }
                Phase::MagicLink => {
                    "the sign-in email's link signed in by itself; asking for the \
                     authorization again"
                }
                Phase::Verifying => "pressed Verify email address",
                Phase::Authorized => "pressed Authorize",
                Phase::Callback => "the page is handing the callback to the waiting sign-in",
            });
        }
    }

    /// Drops the mailbox, for `why` (its own words, never a code): it is no longer read, the
    /// line says once why, and the code page waits, until the deadline, for a person at this
    /// Mac to type the code ([`Phase::CodeNoMailbox`]), whose deadline then names `why`
    /// ([`Stop::MailboxDropped`]). A mailbox that cannot be read and one whose answer cannot be
    /// used are dropped alike: neither gives the person a code.
    fn drop_mailbox(&mut self, why: &'static str, desk: &mut dyn Desk) {
        if self.mailbox_dropped.is_none() {
            self.mailbox_dropped = Some(why);
            desk.say(&format!(
                "the account's mailbox could not be used ({why}); the window no longer reads it"
            ));
        }
        self.enter(Phase::CodeNoMailbox, desk);
    }

    /// Whether this run signed the window's store in itself.
    const fn signed_in_here(&self) -> bool {
        self.continued || self.verified || self.link_opened
    }

    /// Whether the page at `url` shows that the code this run verified was taken: the page
    /// has left the one `Verify email address` was pressed on. The store's session is the
    /// account's from then on, whether or not this run reaches its callback.
    fn session_made(&self, url: &str) -> bool {
        self.verified
            && url.starts_with("https://")
            && self
                .verified_at
                .as_deref()
                .is_some_and(|at| at != super::headless_oauth::url_place(url))
    }

    fn out_of_time(&self) -> Stop {
        if self.bot_stands {
            return Stop::BotCheck(self.bot_host.clone());
        }
        if self.human_stands {
            return Stop::HumanCheck(self.bot_host.clone());
        }
        match (self.phase, self.mailbox_dropped) {
            (Some(Phase::CodeNoMailbox), Some(why)) => Stop::MailboxDropped(why),
            (phase, _) => Stop::TimedOut(phase.unwrap_or(Phase::Opening)),
        }
    }
}

/// Which account a consent page's text names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Account {
    /// The one asked for.
    Named,
    /// Another: the page names an address, and none is the one asked for.
    Other,
    /// None: the page names no address.
    Unnamed,
}

/// The email addresses in `text`, lowercase, Anthropic's own apart.
fn emails_in(text: &str) -> Vec<String> {
    text.split(|c: char| {
        !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-' | '@'))
    })
    .filter_map(|token| {
        let token = token.trim_matches(|c: char| matches!(c, '.' | '-' | '_'));
        let (local, domain) = token.split_once('@')?;
        let dotted = domain.rsplit_once('.').is_some_and(|(host, tld)| {
            !host.is_empty() && tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic())
        });
        (!local.is_empty() && dotted && !domain.contains('@')).then(|| token.to_ascii_lowercase())
    })
    .filter(|email| !email.ends_with("@anthropic.com"))
    .collect()
}

/// Which account the consent page with `text` names, for a sign-in as `email`: the one asked
/// for where any address on it is `email`, another where it holds addresses and none is, none
/// where it holds no address. Whatever words stand beside an address, one that is not the
/// account's is never authorized.
fn account_named(text: &str, email: &str) -> Account {
    let named = emails_in(text);
    if named
        .iter()
        .any(|shown| super::accounts::email_matches(shown, email))
    {
        Account::Named
    } else if named.is_empty() {
        Account::Unnamed
    } else {
        Account::Other
    }
}

/// Whether `page` is Cloudflare's check: its title is the interstitial's, or its words stand
/// beside one of its marks ([`super::headless_oauth::bot_check`]).
fn is_bot_check(page: &Page) -> bool {
    super::headless_oauth::CHALLENGE_TITLES.contains(&page.title.trim())
        || super::headless_oauth::bot_check(page)
}

/// A sign-in code as the mailbox gives it: short, letters and digits only.
fn usable_code(code: &str) -> bool {
    (4..=12).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// An email a sign-in page's field takes: one address of printable ASCII with no quote or
/// backslash.
fn usable_email(email: &str) -> bool {
    email.len() <= 254
        && email
            .bytes()
            .all(|b| b.is_ascii_graphic() && b != b'"' && b != b'\\')
        && email
            .split_once('@')
            .is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.'))
}

fn local_address(url: &str) -> bool {
    url.starts_with("http://127.0.0.1:") || url.starts_with("http://localhost:")
}

/// One read of the page, decided: the callback where the flow is done, `None` to read again.
fn step(
    run: &SignIn<'_>,
    desk: &mut dyn Desk,
    did: &mut Progress,
    facts: &Facts,
    (now, left): (Instant, Duration),
) -> Result<Option<Done>, Halt> {
    let url = facts.url.as_str();
    // Whether a human check holds the page is said anew by each read: one that held the
    // consent page says nothing of a page that has since left it.
    did.human_stands = false;
    // This flow's own callback, in the document's address or in the one the web view is on
    // its way to. Nothing else is read from an address no document has reached.
    let callback = [url, facts.active.as_str()]
        .into_iter()
        .find(|at| run.flow.is_callback(at));
    if let Some(callback) = callback {
        if run.flow.hosted() {
            return Ok(Some(Done::Callback(callback.to_owned())));
        }
        let first = *did.callback_at.get_or_insert(now);
        if now.saturating_duration_since(first) >= CALLBACK_SETTLE {
            return Err(Stop::ListenerSilent.into());
        }
        did.bot_stands = false;
        did.enter(Phase::Callback, desk);
        return Ok(None);
    }
    if run.flow.answered_by_listener(url)
        || (did.callback_at.is_some() && run.flow.on_listeners_host(url))
    {
        return Ok(Some(Done::Delivered));
    }
    if local_address(url) || local_address(&facts.active) {
        return Err(Stop::InvalidCallback.into());
    }
    if let Some(code) = facts.load_failed {
        // After Authorize the navigation that failed is the callback's: the waiting
        // sign-in's listener did not answer. A page that never loaded leaves nothing to
        // read. A page that stands is read as it stands.
        if !run.flow.hosted() && did.authorized {
            return Err(Stop::ListenerSilent.into());
        }
        if !url.starts_with("https://") {
            return Err(Stop::Load(code).into());
        }
    }
    if url.is_empty() || !url.starts_with("https://") {
        did.enter(Phase::Opening, desk);
        return Ok(None);
    }
    if !run.flow.admits_provider_url(url) {
        return Err(Stop::LeftFlow.into());
    }
    let host = super::login_drive::https_host(url);
    let page = Page {
        url: url.to_owned(),
        title: facts.title.to_lowercase(),
        text: facts.text.clone(),
        markers: facts.markers.clone(),
        ..Page::default()
    };
    if super::headless_oauth::block_page(&page) {
        return Err(Stop::BlockPage(host).into());
    }
    if is_bot_check(&page) {
        did.bot_stands = true;
        did.bot_host = host;
        did.enter(Phase::BotCheck, desk);
        return Ok(None);
    }
    did.bot_stands = false;
    let shows = |button: Button| facts.buttons.iter().any(|shown| button.is(shown));

    // A consent page that carries a human check keeps its Authorize disabled until the check
    // is answered. aterm never operates one: it waits, with nothing read from the check and
    // nothing pressed, and presses Authorize once the page itself enables it (the check
    // passed by itself, or a person at the desk completed it).
    did.human_stands = !did.authorized
        && !shows(Button::Authorize)
        && facts.check.is_some()
        && facts.disabled.iter().any(|held| Button::Authorize.is(held));
    if did.human_stands {
        if account_named(&facts.text, run.email) == Account::Other {
            return Err(Stop::WrongAccount.into());
        }
        did.bot_host = host;
        did.enter(Phase::HumanCheck, desk);
        return Ok(None);
    }

    if shows(Button::Authorize) && !did.authorized {
        // The page says whose consent this is, or this flow made the store's session itself;
        // nothing else is authorized, and another account's session is left as it is.
        match account_named(&facts.text, run.email) {
            Account::Named => {}
            Account::Other => return Err(Stop::WrongAccount.into()),
            Account::Unnamed if did.signed_in_here() || desk.store_is_the_accounts() => {}
            Account::Unnamed => return Err(Stop::UnknownSession.into()),
        }
        if desk.press(Button::Authorize, run.email, url)? {
            did.authorized = true;
            did.enter(Phase::Authorized, desk);
        }
        return Ok(None);
    }

    let code_page = facts.code.is_some() || shows(Button::VerifyEmailAddress);
    if code_page && !did.verified {
        return code_step(run, desk, did, facts, (now, left)).map(|()| None);
    }

    if shows(Button::ContinueWithEmail) && !did.continued {
        // The address the code is mailed to is the account's, set here and read back at the
        // press: a field that holds another address is never continued with.
        match desk.fill(Field::Email, run.email, url)? {
            Filled::Kept => {
                if desk.press(Button::ContinueWithEmail, run.email, url)? {
                    did.continued = true;
                    did.enter(Phase::EmailSent, desk);
                    return Ok(None);
                }
            }
            Filled::Moved => {}
            ended => {
                did.email_fills += 1;
                if did.email_fills >= MAX_FILLS {
                    return Err(Stop::Email(if ended == Filled::NoField {
                        "the page shows Continue with email and no email field"
                    } else {
                        "it did not keep the account's email"
                    })
                    .into());
                }
            }
        }
    }
    if did.phase.is_none_or(|phase| {
        matches!(
            phase,
            Phase::Opening | Phase::BotCheck | Phase::HumanCheck | Phase::Callback
        )
    }) {
        did.enter(Phase::Waiting, desk);
    }
    Ok(None)
}

/// The code page, read with its field not yet verified: the mailbox's code or link is read
/// once, held, and given to the page until the page keeps it. Nothing is set in the read that
/// asked the mailbox, which may have taken seconds: the next read of the page decides.
fn code_step(
    run: &SignIn<'_>,
    desk: &mut dyn Desk,
    did: &mut Progress,
    facts: &Facts,
    (now, left): (Instant, Duration),
) -> Result<(), Halt> {
    let url = facts.url.as_str();
    let shows_verify = facts
        .buttons
        .iter()
        .any(|shown| Button::VerifyEmailAddress.is(shown));
    let holds_code = matches!(did.held, Some(Held::Code(_)));
    if facts.code == Some(true) && !holds_code {
        if !did.code_set {
            // A value aterm did not set is a person's typing: theirs to finish, and theirs
            // to verify. Pressing over it would spend the code on its first digit.
            did.enter(Phase::CodeTyped, desk);
            return Ok(());
        }
        if shows_verify && desk.press(Button::VerifyEmailAddress, run.email, url)? {
            did.verified = true;
            did.verified_at = Some(super::headless_oauth::url_place(url));
            did.enter(Phase::Verifying, desk);
        }
        return Ok(());
    }
    if did.code_set || did.code_refused {
        did.enter(Phase::CodeNotKept, desk);
        return Ok(());
    }
    if did.link_opened {
        return Ok(());
    }
    if did.held.is_none() {
        if !desk.has_mailbox() || did.mailbox_dropped.is_some() {
            did.enter(Phase::CodeNoMailbox, desk);
            return Ok(());
        }
        did.enter(Phase::Code, desk);
        let due = did
            .mailbox_at
            .is_none_or(|at| now.saturating_duration_since(at) >= MAILBOX_EVERY);
        if !due {
            return Ok(());
        }
        did.mailbox_at = Some(now);
        did.held = match desk.mailbox(left.min(MAILBOX_WITHIN)) {
            // An answer that cannot be used gives the person no code either: it is dropped
            // (scrubbed) with the mailbox, and the code is theirs to type.
            Ok(MailboxCredential::Otp(code)) => {
                let code = zeroize::Zeroizing::new(code);
                if usable_code(&code) {
                    Some(Held::Code(code))
                } else {
                    did.drop_mailbox("its code is not one a sign-in page takes", desk);
                    None
                }
            }
            Ok(MailboxCredential::MagicLink(link)) => {
                let link = zeroize::Zeroizing::new(link);
                if run.flow.admits_provider_url(&link) {
                    Some(Held::Link(link))
                } else {
                    did.drop_mailbox("its link is not this sign-in's", desk);
                    None
                }
            }
            // No sign-in email yet: read again.
            Err(why) if why == MAILBOX_TIMEOUT => None,
            // A mailbox that cannot be read is asked again a few times, never for the whole
            // sign-in: one connection that failed may not fail twice. Then it is dropped, not
            // the sign-in: a person at this Mac may still type the code in the window, whose
            // own deadline bounds the wait. Every window opens at this Mac's own desk
            // (`cli::opens_sign_in_window_by`), an upkeep's included, and the drive runs in
            // the opener, which is told nothing of who started it, so it waits for a person
            // in every run.
            Err(why) => {
                did.mail_errors += 1;
                if did.mail_errors >= MAX_MAILBOX_ERRORS {
                    did.drop_mailbox(why, desk);
                }
                None
            }
        };
        return Ok(());
    }
    match did.held.take() {
        Some(Held::Code(code)) => match desk.fill(Field::Code, &code, url) {
            Ok(Filled::Kept) => did.code_set = true,
            Ok(Filled::NotKept) => {
                did.fills += 1;
                if did.fills >= MAX_FILLS {
                    did.code_refused = true;
                    did.enter(Phase::CodeNotKept, desk);
                } else {
                    did.held = Some(Held::Code(code));
                }
            }
            // The page was read again since: the code is held for the next read.
            Ok(Filled::Moved) => did.held = Some(Held::Code(code)),
            Ok(Filled::NoField) => {
                return Err(Stop::Desk(
                    "the page asks for a login code and shows no field for one".into(),
                )
                .into());
            }
            Err(error) => {
                // The mailbox gave the code once: it is held for the next read.
                did.held = Some(Held::Code(code));
                return Err(error.into());
            }
        },
        Some(Held::Link(link)) => {
            // The link's page is given time at its first read, and one look at each later.
            let within = if did.link_read {
                Duration::ZERO
            } else {
                left.min(LINK_WITHIN)
            };
            did.link_read = true;
            match desk.link_code(&link, within) {
                // Claude's link shows the code for the page that asked for it: that page
                // stands as it was, and its field takes the code at the next read.
                Ok(Linked::Code(code)) => {
                    if usable_code(&code) {
                        did.held = Some(Held::Code(code));
                        did.enter(Phase::LinkCode, desk);
                    } else {
                        did.drop_mailbox(
                            "its link shows a code that is not one a sign-in page takes",
                            desk,
                        );
                    }
                }
                // The link is never opened in the window's page, whose code field it would
                // replace: its page is read again, aside, until it shows the code.
                Ok(Linked::Waiting) => {
                    did.held = Some(Held::Link(link));
                    did.enter(Phase::LinkWait, desk);
                }
                // A link that went on by itself has signed its store in, if it did anything:
                // the window's page asks for the authorization again, once.
                Ok(Linked::Moved) => {
                    desk.go(run.authorize_url)?;
                    did.link_opened = true;
                    did.enter(Phase::MagicLink, desk);
                }
                Err(error) => {
                    did.held = Some(Held::Link(link));
                    return Err(error.into());
                }
            }
        }
        None => {}
    }
    Ok(())
}

fn steps(run: &SignIn<'_>, desk: &mut dyn Desk, end: Instant) -> Result<Done, Stop> {
    let mut did = Progress::default();
    did.enter(Phase::Opening, desk);
    loop {
        if desk.cancelled() {
            return Err(Stop::Cancelled);
        }
        let now = desk.now();
        let left = end.saturating_duration_since(now);
        if left.is_zero() {
            return Err(did.out_of_time());
        }
        let unread = match desk.facts() {
            Ok(facts) => {
                let stepped = step(run, desk, &mut did, &facts, (now, left));
                if !did.noted
                    && (did.session_made(&facts.url)
                        || (did.signed_in_here() && matches!(stepped, Ok(Some(_)))))
                {
                    // The store's session is the account's from here: a later flow in it
                    // (the account's token, or this sign-in run again) meets the consent
                    // page alone.
                    desk.note_signed_in();
                    did.noted = true;
                }
                match stepped {
                    Ok(Some(done)) => return Ok(done),
                    Ok(None) => None,
                    Err(Halt::Stop(stop)) => return Err(stop),
                    Err(Halt::Unread(why)) => Some(why),
                }
            }
            Err(DeskError::Unread(why)) => Some(why),
            Err(error) => return Err(error.into()),
        };
        // A read that failed and an act that failed are counted together: either may not
        // fail the next time, and neither is tried for the whole sign-in.
        match unread {
            None => did.unread = 0,
            Some(why) => {
                did.unread += 1;
                if did.unread >= MAX_UNREAD {
                    return Err(Stop::Desk(why));
                }
            }
        }
        let left = end.saturating_duration_since(desk.now());
        desk.pause(left.min(TICK));
    }
}

/// Runs `run` over `desk`: opens the page in a window of its own, reads and advances it until
/// its callback, a stop or the one deadline `run.within` sets before the window opens, and
/// closes that window however it ended. A sign-in that ended on a consent page for another
/// account, or for nobody this flow can name, empties the window's store first
/// ([`Desk::forget`]): the session there is not the account's, and the next sign-in starts
/// signed out.
///
/// # Errors
/// Each [`Stop`].
pub(super) fn drive(run: &SignIn<'_>, desk: &mut dyn Desk) -> Result<Done, Stop> {
    if desk.cancelled() {
        return Err(Stop::Cancelled);
    }
    if !usable_email(run.email) {
        return Err(Stop::Email("the account's email is not one a page takes"));
    }
    let end = desk
        .now()
        .checked_add(run.within)
        .ok_or_else(|| Stop::Desk("this Mac's sign-in deadline is invalid".into()))?;
    let result = match desk.open(run.authorize_url, run.within) {
        Ok(()) => steps(run, desk, end),
        Err(error) => Err(error.into()),
    };
    if matches!(result, Err(Stop::WrongAccount | Stop::UnknownSession)) {
        desk.forget();
    }
    desk.close();
    result
}

/// The page reader's helpers, shared by every script: which controls are usable, a button's
/// text, the email field, the code field (one field, or one box per character), and a set
/// through the field's own value setter with the events a page's framework listens for.
const CONTROLS_JS: &str = r#"
  const shown = (el) => { const r = el.getBoundingClientRect(); return r.width > 0 && r.height > 0; };
  const usable = (el) => Boolean(el) && !el.disabled && !el.hidden && el.type !== "hidden" &&
    el.getAttribute("aria-disabled") !== "true" && shown(el);
  const labelOf = (el) => String(el.innerText || el.value || "").replace(/\s+/g, " ").trim();
  const enabledButtons = () => Array.from(document.querySelectorAll("button, [role='button'], input[type='submit']")).filter(usable);
  const typed = (el) => usable(el) && !el.readOnly;
  const emailField = () => Array.from(document.querySelectorAll("input[type='email'], input[name='email' i], input[autocomplete='email' i]")).filter(typed)[0] || null;
  const codeFields = () => {
    const boxes = Array.from(document.querySelectorAll("input[maxlength='1']")).filter(typed);
    if (boxes.length >= 4) return boxes;
    const one = Array.from(document.querySelectorAll("input[autocomplete='one-time-code' i], input[name*='code' i], input[id*='code' i], input[placeholder*='code' i], input[aria-label*='code' i], input[name*='otp' i], input[inputmode='numeric' i]"))
      .filter(el => typed(el) && !["email", "password", "checkbox", "radio", "submit", "button"].includes(String(el.type || "").toLowerCase()));
    return one.length ? [one[0]] : [];
  };
  const setValue = (el, value) => {
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value").set;
    setter.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
    el.dispatchEvent(new Event("change", { bubbles: true }));
  };
  const humanCheck = () => {
    if (document.querySelector("textarea[name='h-captcha-response'], iframe[src*='hcaptcha.com']")) return "hCaptcha";
    if (document.querySelector("textarea[name='g-recaptcha-response'], iframe[src*='recaptcha']")) return "reCAPTCHA";
    if (document.querySelector("input[name='cf-turnstile-response'], iframe[src*='challenges.cloudflare.com']")) return "Turnstile";
    return null;
  };
  const challenged = () => {
    const title = String(document.title || "");
    const text = String(document.body && document.body.innerText || "");
    return botCheckOf(title, text, botCheckMarkers()) || botCheckTitles.includes(title.trim().toLowerCase()) || blockPageOf(text);
  };
"#;

/// What every script starts with: the hosts whose `https` documents are this flow's own
/// (`pinned`), the address of the document the script runs in (`here`), and whether that
/// document is one of the flow's (`own`). A script reads no control and acts on nothing in a
/// document that is not.
fn own_js(pinned: &[&str]) -> Option<String> {
    let hosts = aterm_json::to_string(&Value::Array(
        pinned
            .iter()
            .map(|host| Value::String((*host).to_owned()))
            .collect(),
    ))
    .ok()?;
    Some(format!(
        r#"
  const pinned = {hosts};
  const here = String(location.href);
  const own = location.protocol === "https:" && pinned.includes(String(location.hostname).toLowerCase());
"#
    ))
}

/// Reads the page: of a document that is not the flow's own, its address alone; of one that
/// is, its address, its title, the start of its text, Cloudflare's marks and, unless the page
/// is Cloudflare's check or block page, its enabled buttons' texts and whether its code field
/// holds a value. It sets and presses nothing.
fn facts_script(pinned: &[&str]) -> Option<String> {
    Some(format!(
        r#"(() => {{
{own}
  if (!own) return JSON.stringify({{ at: here, foreign: true }});
{bot}{controls}
  const out = {{
    at: here,
    title: String(document.title || ""),
    text: String(document.body && document.body.innerText || "").slice(0, 6000),
    markers: botCheckMarkers(),
    buttons: [],
    disabled: [],
    check: null,
    code: null,
  }};
  if (challenged()) return JSON.stringify(out);
  out.buttons = enabledButtons().map(labelOf).filter(Boolean).slice(0, 40);
  out.disabled = Array.from(document.querySelectorAll("button, [role='button'], input[type='submit']"))
    .filter(el => shown(el) && !usable(el)).map(labelOf).filter(Boolean).slice(0, 10);
  out.check = humanCheck();
  const code = codeFields();
  if (code.length) out.code = code.every(f => String(f.value || "").length > 0) ? "filled" : "empty";
  return JSON.stringify(out);
}})()"#,
        own = own_js(pinned)?,
        bot = super::headless_oauth::BOT_CHECK_JS,
        controls = CONTROLS_JS,
    ))
}

/// Reads the login code a sign-in link's page shows, the shape of its text and its address:
/// the one run of six digits in it (digits a page shows apart, by spaces or hyphens, read
/// together), and for each of its first words its length, or for a word with a digit in it
/// how many characters it has. Nothing is read from a document that is not the flow's own or
/// from Cloudflare's check, and nothing is pressed.
fn link_code_script(pinned: &[&str]) -> Option<String> {
    Some(format!(
        r#"(() => {{
{own}
  const out = {{ at: here, code: null, shape: "" }};
  if (!own) {{ out.shape = "foreign"; return JSON.stringify(out); }}
{bot}{controls}
  if (challenged()) {{ out.shape = "challenge"; return JSON.stringify(out); }}
  const text = String(document.body && document.body.innerText || "");
  out.shape = text.split(/\s+/).filter(Boolean).slice(0, 60)
    .map(w => /[0-9]/.test(w) ? "<" + w.length + ">" : String(w.length)).join(" ");
  const six = (from) => Array.from(new Set((from.match(/[0-9]+/g) || []).filter(r => r.length === 6)));
  let found = six(text);
  if (!found.length) found = six(text.replace(/([0-9])[ \t\r\n -]+(?=[0-9])/g, "$1"));
  if (found.length === 1) out.code = found[0];
  return JSON.stringify(out);
}})()"#,
        own = own_js(pinned)?,
        bot = super::headless_oauth::BOT_CHECK_JS,
        controls = CONTROLS_JS,
    ))
}

/// What a [`link_code_script`] answer holds: the code, the text's shape, and the address of
/// the document that was read.
struct LinkRead {
    code: Option<zeroize::Zeroizing<String>>,
    shape: String,
    at: String,
}

/// The [`LinkRead`] an answer holds; an answer that is none holds nothing.
fn parse_link_code(answer: &str) -> LinkRead {
    let value = aterm_json::from_str::<Value>(answer).unwrap_or(Value::Null);
    let text = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    LinkRead {
        code: value
            .get("code")
            .and_then(Value::as_str)
            .map(|code| zeroize::Zeroizing::new(code.to_owned())),
        shape: text("shape"),
        at: text("at"),
    }
}

/// `text` as a script's string literal.
fn literal(text: &str) -> Option<String> {
    aterm_json::to_string(&Value::String(text.to_owned())).ok()
}

/// Presses the first enabled button whose whole text is `button`'s, in the document at `at`
/// and in no other, unless the page is Cloudflare's: `Continue with email` only while the
/// email field holds `email`, `Verify email address` only while the code field holds a value.
/// It answers what it did and the address of the document it ran in ([`parse_act`]).
fn press_script(button: Button, email: &str, at: &str, pinned: &[&str]) -> Option<String> {
    let title = literal(&button.title().to_ascii_lowercase())?;
    let email = match button {
        Button::ContinueWithEmail => literal(&email.to_ascii_lowercase())?,
        Button::VerifyEmailAddress | Button::Authorize => "null".to_owned(),
    };
    let code = button == Button::VerifyEmailAddress;
    Some(format!(
        r#"(() => {{
{own}
  const answer = (did) => JSON.stringify({{ at: here, did }});
  if (!own) return answer("foreign");
  if (here !== {expected}) return answer("moved");
{bot}{controls}
  if (challenged()) return answer("unready");
  const title = {title}, email = {email}, code = {code};
  const hit = enabledButtons().find(b => labelOf(b).toLowerCase() === title);
  if (!hit) return answer("absent");
  if (email !== null) {{
    const field = emailField();
    if (!field || String(field.value || "").trim().toLowerCase() !== email) return answer("unready");
  }}
  if (code) {{
    const fields = codeFields();
    if (!fields.length || !fields.every(f => String(f.value || "").length > 0)) return answer("unready");
  }}
  hit.click();
  return answer("pressed");
}})()"#,
        own = own_js(pinned)?,
        expected = literal(at)?,
        bot = super::headless_oauth::BOT_CHECK_JS,
        controls = CONTROLS_JS,
    ))
}

/// Sets `field` of the document at `at`, and of no other, to `value` and reads it back: an
/// email as it stands, case apart; a code without the spaces or hyphens a page may show it
/// with. It answers what it did and the address of the document it ran in ([`parse_act`]).
fn fill_script(field: Field, value: &str, at: &str, pinned: &[&str]) -> Option<String> {
    let value = literal(value)?;
    let body = match field {
        Field::Email => {
            r#"
  const field = emailField();
  if (!field) return answer("absent");
  const held = () => String(field.value || "").trim().toLowerCase();
  if (held() !== value.toLowerCase()) setValue(field, value);
  return answer(held() === value.toLowerCase() ? "filled" : "untaken");"#
        }
        Field::Code => {
            r#"
  const fields = codeFields();
  if (!fields.length) return answer("absent");
  if (fields.length === 1) setValue(fields[0], value);
  else Array.from(value).slice(0, fields.length).forEach((ch, i) => setValue(fields[i], ch));
  const held = fields.map(f => String(f.value || "")).join("").replace(/[\s-]/g, "");
  return answer(held === value ? "filled" : "untaken");"#
        }
    };
    Some(format!(
        r#"(() => {{
{own}
  const answer = (did) => JSON.stringify({{ at: here, did }});
  if (!own) return answer("foreign");
  if (here !== {expected}) return answer("moved");
{bot}{controls}
  if (challenged()) return answer("unready");
  const value = {value};{body}
}})()"#,
        own = own_js(pinned)?,
        expected = literal(at)?,
        bot = super::headless_oauth::BOT_CHECK_JS,
        controls = CONTROLS_JS,
    ))
}

/// What a press or a set answered: what it did, and the address of the document it ran in.
fn parse_act(answer: &str) -> Option<(String, String)> {
    let value: Value = aterm_json::from_str(answer).ok()?;
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
    Some((text("did")?, text("at")?))
}

/// The [`Facts`] a read's answer holds: the address is the one the document itself gave, and
/// `active` the one the web view gave.
fn parse_facts(answer: &str, active: &str) -> Result<Facts, DeskError> {
    let unread = || DeskError::Unread("the page's answer was not a page read".into());
    let value: Value = aterm_json::from_str(answer).map_err(|_| unread())?;
    let text = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(unread)
    };
    let list = |key: &str| match value.get(key) {
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().map(str::to_owned).ok_or_else(unread))
            .collect::<Result<Vec<_>, _>>(),
        _ => Err(unread()),
    };
    let at = text("at")?;
    // The empty page a web view starts on has no address of a document's own.
    let url = if at == "about:blank" {
        String::new()
    } else {
        at
    };
    if value.get("foreign") == Some(&Value::Bool(true)) {
        return Ok(Facts {
            url,
            active: active.to_owned(),
            ..Facts::default()
        });
    }
    Ok(Facts {
        url,
        active: active.to_owned(),
        load_failed: None,
        title: text("title")?,
        text: text("text")?,
        markers: list("markers")?
            .iter()
            .filter_map(|word| Marker::of(word))
            .collect(),
        buttons: list("buttons")?,
        disabled: match value.get("disabled") {
            Some(Value::Array(_)) => list("disabled")?,
            _ => Vec::new(),
        },
        check: value
            .get("check")
            .and_then(Value::as_str)
            .map(|name| super::one_line(name, 16)),
        code: match value.get("code") {
            Some(Value::Null) | None => None,
            Some(held) => Some(held.as_str() == Some("filled")),
        },
    })
}

/// How long the window's host has to say it is open.
const READY_WITHIN: Duration = Duration::from_secs(30);

/// How long one request to the window's host may take.
const ASK_WITHIN: Duration = Duration::from_secs(20);

/// How long a closed window's host is waited for: it lingers so the page's cookies reach the
/// account's store before the next window opens on it.
const END_WITHIN: Duration = Duration::from_secs(8);

/// How long the host has to answer the request that closes its window, before it is waited
/// for and ended as one that ignored it.
const CLOSE_WITHIN: Duration = Duration::from_secs(5);

/// How long the last lines of a host whose pipe has closed are read for
/// ([`Host::said_closed`]).
const LAST_LINES_WITHIN: Duration = Duration::from_secs(2);

/// How long a sign-in link's page must have stood somewhere other than the link before it
/// is taken to have gone on by itself ([`Linked::Moved`]): a page on its way to showing the
/// code is not one.
const MOVED_SETTLE: Duration = Duration::from_secs(5);

/// A navigation's error codes that say nothing failed: `NSURLErrorCancelled` (another
/// navigation took its place) and WebKit's frame-load-interrupted (the host refused it).
const QUIET_LOAD_ERRORS: [i64; 2] = [-999, 102];

/// The window's host process and its pipes.
struct Host {
    child: aterm_uds::spawnfd::Child,
    stdin: Option<std::process::ChildStdin>,
    lines: mpsc::Receiver<wire::Line>,
    next: u64,
    /// The error code of a navigation of the window's page that failed and that no read has
    /// reported yet.
    failed: Option<i64>,
    /// How many navigations of the window's page the host refused.
    refused: usize,
    /// How long its answer to the closing request, and then its own end, are waited for
    /// ([`CLOSE_WITHIN`], [`END_WITHIN`]).
    ends: (Duration, Duration),
}

impl Host {
    /// Starts `exe`'s window host, tells it what to open on its standard input (`open`: the
    /// title names the account, so it is in no argument), and waits until it says the window
    /// is open.
    fn start(exe: &Path, open: &wire::Open, within: Duration) -> Result<Self, DeskError> {
        Self::start_ending(exe, open, within, (CLOSE_WITHIN, END_WITHIN))
    }

    /// [`Self::start`] with its own bounds on the host's end.
    fn start_ending(
        exe: &Path,
        open: &wire::Open,
        within: Duration,
        ends: (Duration, Duration),
    ) -> Result<Self, DeskError> {
        use aterm_uds::spawnfd::{Stdio, Tether as _};
        let failed = |why: &str| DeskError::NoWindow(why.to_owned());
        let mut command = aterm_uds::spawnfd::command(exe);
        command
            .arg0("aterm")
            .arg(aterm_signin_window::VERB)
            .env_clear()
            .envs(super::open_url::child_env(&|name| std::env::var_os(name)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // The window ends with the sign-in that opened it, however that ends.
        command.tether();
        let mut child = command
            .spawn()
            .map_err(|_| failed("its host could not be started"))?;
        let stdin = child.stdin.take();
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(failed("its host has no answer pipe"));
        };
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let Ok(line) = wire::parse_line(&line) else {
                    continue;
                };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let mut host = Self {
            child,
            stdin,
            lines,
            next: 0,
            failed: None,
            refused: 0,
            ends,
        };
        let mut first = wire::open_line(open);
        first.push('\n');
        if !host.write(first.as_bytes()) {
            host.end();
            return Err(failed("its host took no first line"));
        }
        let deadline = Instant::now() + within.min(READY_WITHIN);
        loop {
            match host.event_until(deadline) {
                Ok(name) if name == wire::READY => return Ok(host),
                Ok(_) => {}
                Err(error) => {
                    host.end();
                    // A host that ended, or said nothing, before its window opened never
                    // had a window for a person to close.
                    return Err(match error {
                        DeskError::Gone | DeskError::Ended => {
                            failed("its host ended before the window opened")
                        }
                        DeskError::Unread(why) => DeskError::NoWindow(why),
                        other => other,
                    });
                }
            }
        }
    }

    /// Writes `bytes` to the host's standard input; whether they went.
    fn write(&mut self, bytes: &[u8]) -> bool {
        self.stdin
            .as_mut()
            .is_some_and(|stdin| stdin.write_all(bytes).and_then(|()| stdin.flush()).is_ok())
    }

    /// What an event line says of the window, where it ends the flow.
    fn ended_by(name: &str) -> Option<DeskError> {
        match name {
            wire::CLOSED => Some(DeskError::Gone),
            wire::NO_DESKTOP => Some(DeskError::NoDesktop),
            _ => None,
        }
    }

    /// Keeps what an event that does not end the flow says: a navigation that failed, with
    /// its code, and one the host refused.
    fn note(&mut self, name: &str, code: Option<i64>) {
        match name {
            wire::LOAD_FAILED => {
                let code = code.unwrap_or(0);
                if !QUIET_LOAD_ERRORS.contains(&code) {
                    self.failed = Some(code);
                }
            }
            wire::REFUSED => self.refused += 1,
            _ => {}
        }
    }

    /// The next event before `deadline`, replies apart.
    fn event_until(&mut self, deadline: Instant) -> Result<String, DeskError> {
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(wire::Line::Event { name, code, .. }) => {
                    if let Some(error) = Self::ended_by(&name) {
                        return Err(error);
                    }
                    self.note(&name, code);
                    return Ok(name);
                }
                Ok(wire::Line::Reply { .. }) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(DeskError::Unread("it did not open in time".into()));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(DeskError::Ended),
            }
        }
    }

    /// Sends the request `of` makes with a new id and answers its reply: an evaluation's
    /// value and the page's address.
    fn ask(
        &mut self,
        of: impl FnOnce(u64) -> wire::Request,
    ) -> Result<(Option<String>, Option<String>), DeskError> {
        self.ask_within(of, ASK_WITHIN)
    }

    /// [`Self::ask`] with its own bound on the reply.
    fn ask_within(
        &mut self,
        of: impl FnOnce(u64) -> wire::Request,
        within: Duration,
    ) -> Result<(Option<String>, Option<String>), DeskError> {
        self.next += 1;
        let id = self.next;
        let mut line = zeroize::Zeroizing::new(wire::request_line(&of(id)));
        line.push('\n');
        if !self.write(line.as_bytes()) {
            // A window a person closed says so before its host ends: that line outranks the
            // pipe that closed after it.
            return Err(self.said_closed().unwrap_or(DeskError::Ended));
        }
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(wire::Line::Reply { id: of, answer }) if of == id => {
                    return answer.map_err(|why| {
                        if why == "the window is closed" {
                            DeskError::Gone
                        } else {
                            DeskError::Unread(super::one_line(&why, 120))
                        }
                    });
                }
                Ok(wire::Line::Reply { .. }) => {}
                Ok(wire::Line::Event { name, code, .. }) => {
                    if let Some(error) = Self::ended_by(&name) {
                        return Err(error);
                    }
                    self.note(&name, code);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(DeskError::Unread(
                        "the sign-in window did not answer in time".into(),
                    ));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(DeskError::Ended),
            }
        }
    }

    /// What the host's last lines say ended the window, where one does: a host whose pipe
    /// has closed is at its end, and what it wrote before that is read to its end, for at
    /// most [`LAST_LINES_WITHIN`].
    fn said_closed(&mut self) -> Option<DeskError> {
        let deadline = Instant::now() + LAST_LINES_WITHIN;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(wire::Line::Event { name, code, .. }) => {
                    if let Some(error) = Self::ended_by(&name) {
                        return Some(error);
                    }
                    self.note(&name, code);
                }
                Ok(wire::Line::Reply { .. }) => {}
                Err(_) => return None,
            }
        }
    }

    /// Closes the window and waits for its host, which lingers a few seconds for the page's
    /// cookies; a host that outlives [`END_WITHIN`] is ended and reaped.
    fn end(&mut self) {
        let (close_within, end_within) = self.ends;
        let _ = self.ask_within(|id| wire::Request::Close { id }, close_within);
        self.stdin = None;
        let deadline = Instant::now() + end_within;
        while Instant::now() < deadline {
            if !matches!(self.child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.terminate();
        let _ = self.child.wait();
    }
}

/// The machine's one sign-in window at a time: the lock file beside the accounts store that
/// the flow holds from before its window opens until it has closed. A sign-in holds the
/// machine's sign-in lock as well, but a token's page opens after that lock is released
/// (under its account's own), so this, taken by the process that opens the window, is what
/// keeps two windows from standing side by side. `None` where there is no store to keep the
/// lock in: nothing else on this Mac opens a window then either.
///
/// It waits while another window is open, looking every [`TURN_EVERY`] for at most
/// [`TURN_WITHIN`], which no sign-in in a window outlasts.
///
/// # Errors
/// [`Stop::Busy`] where the other window stayed open, [`Stop::Cancelled`] where `cancelled`
/// said so first, and [`Stop::Desk`] where the lock could not be taken at all.
fn window_turn(
    store: Option<&Path>,
    cancelled: &dyn Fn() -> bool,
    (within, every): (Duration, Duration),
) -> Result<Option<super::accounts::Lock>, Stop> {
    let Some(store) = store else {
        return Ok(None);
    };
    let unusable = || Stop::Desk("the sign-in window's lock could not be taken".into());
    aterm_types::fs_restricted::ensure_private_dir(store).map_err(|_| unusable())?;
    let path = store.join(WINDOW_LOCK_FILE);
    let end = Instant::now() + within;
    loop {
        if cancelled() {
            return Err(Stop::Cancelled);
        }
        match super::accounts::lock_at_within(&path, &[]) {
            Ok(Some(lock)) => return Ok(Some(lock)),
            Ok(None) => {}
            Err(_) => return Err(unusable()),
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Stop::Busy);
        }
        std::thread::sleep(left.min(every));
    }
}

/// The lock file, in the accounts store, of the machine's one sign-in window ([`window_turn`]).
const WINDOW_LOCK_FILE: &str = "signin-window.lock";

/// The variable naming a file this sign-in's progress lines and each read's summary are
/// appended to, for a person finding out why a sign-in stopped: where the page was (its host
/// and path), its title, its buttons' texts and whether it had a code field; for a page with
/// none of the sign-in's buttons, the start of its text; for a sign-in link's page, the shape
/// of its text. An address in them is written `<address>` and, in a page's text, a word that
/// holds a digit `<n>`. Never a code, a link or a query.
pub const TRACE_ENV: &str = "ATERM_SIGN_IN_TRACE";

/// The data store's identifier for the account named `account` (its provider and its email):
/// a UUID (version 8) made of the SHA-256 of that name, the same in every process that signs
/// the account in or makes its token. It is the account's email and not its `ac-` key, whose
/// keyer only some processes hold: the opener and the token's capture must name one store.
pub(super) fn store_id(account: &str) -> String {
    let mut digest = aterm_digest::Sha256::new();
    digest.update(b"aterm sign-in window store\n");
    digest.update(account.trim().to_ascii_lowercase().as_bytes());
    let hash = digest.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02X}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// The file, under the accounts store's `records`, that says a sign-in in the window signed
/// the data store `store` in as its account.
fn marker_of(records: &Path, store: &str) -> PathBuf {
    records.join("signin-window").join(store)
}

/// Takes back the record that the window's store for `email` holds that account's session,
/// where a sign-in made in it turned out to be another account's: the next consent page
/// there is authorized only where it names the account, and one that names nobody ends with
/// the store's session forgotten ([`Desk::forget`]).
pub(super) fn disown_store(records: &Path, email: &str) {
    let _ = std::fs::remove_file(marker_of(records, &claude_store(email)));
}

/// The data store of the Claude account whose email is `email`, however the email is spaced
/// or cased: every caller that names an account's store names it through this.
fn claude_store(email: &str) -> String {
    store_id(&format!("claude:{}", email.trim()))
}

/// The desk that is aterm's own sign-in window, driven through its host process.
pub(super) struct Window<'a> {
    host: Option<Host>,
    /// The `aterm` executable that hosts the window.
    exe: PathBuf,
    title: String,
    store: String,
    /// The file that records that this flow signed the store in as the account.
    marker: Option<PathBuf>,
    mailbox: Option<&'a MailboxConfig>,
    email: &'a str,
    /// The flow whose pages this desk reads and acts in ([`Self::for_flow`]); without one it
    /// reads nothing and acts in nothing.
    flow: Option<&'a MailboxFlow>,
    /// The sign-in link the aside page was sent to, so it is sent there once.
    linked: Option<zeroize::Zeroizing<String>>,
    /// Since when the aside page has stood somewhere other than its link.
    moved_at: Option<Instant>,
    /// How many refused navigations the trace has said.
    refusals_said: usize,
    seen_otps: BTreeSet<String>,
    scope: Option<&'a super::account_work::Scope>,
    trace: Option<PathBuf>,
    /// The read whose page's text the trace last held, so each page's is written once.
    traced_text: Option<String>,
}

impl<'a> Window<'a> {
    /// A desk for one sign-in as `email` in a window `exe` hosts, reading `mailbox` for its
    /// code where this desk can (a mailbox only a headless browser reads, webmail alone, is
    /// none here), with its store's records under `records`.
    pub(super) fn new(
        exe: PathBuf,
        records: Option<&Path>,
        mailbox: Option<&'a MailboxConfig>,
        email: &'a str,
        scope: Option<&'a super::account_work::Scope>,
    ) -> Self {
        let store = claude_store(email);
        Self {
            host: None,
            exe,
            title: format!("aterm \u{b7} signing in Claude {email}"),
            marker: records.map(|dir| marker_of(dir, &store)),
            store,
            mailbox: mailbox.filter(|config| config.window_reads(Some(email))),
            email,
            flow: None,
            linked: None,
            moved_at: None,
            refusals_said: 0,
            seen_otps: BTreeSet::new(),
            scope,
            trace: std::env::var_os(TRACE_ENV)
                .filter(|path| !path.is_empty())
                .map(PathBuf::from)
                .filter(|path| path.is_absolute()),
            traced_text: None,
        }
    }

    /// This desk for `flow`: the documents it reads and acts in are that flow's own, and its
    /// window's pages go only where that flow's may.
    pub(super) fn for_flow(mut self, flow: &'a MailboxFlow) -> Self {
        self.flow = Some(flow);
        self
    }

    /// The hosts whose documents are the flow's own; none without a flow.
    fn pinned(&self) -> &'static [&'static str] {
        self.flow.map_or(&[], MailboxFlow::provider_hosts)
    }

    /// Whether a document at `at` is the flow's own.
    fn admits(&self, at: &str) -> bool {
        self.flow.is_some_and(|flow| flow.admits_provider_url(at))
    }

    fn host(&mut self) -> Result<&mut Host, DeskError> {
        self.host.as_mut().ok_or(DeskError::Ended)
    }

    /// Evaluates `script` in the page: its answer and the address the web view is at.
    fn eval(&mut self, script: Option<String>) -> Result<(String, String), DeskError> {
        let script =
            script.ok_or_else(|| DeskError::Unread("the page's script is unusable".into()))?;
        let (value, url) = self.host()?.ask(|id| wire::Request::Eval {
            id,
            script,
            aside: false,
        })?;
        Ok((value.unwrap_or_default(), url.unwrap_or_default()))
    }

    /// Appends `line` to the trace file, where one is named.
    fn trace(&self, line: &str) {
        if let Some(path) = &self.trace
            && let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
        {
            let _ = writeln!(file, "{line}");
        }
    }
}

/// The start of a page's `text` for the trace: its first words on one line, each address
/// written `<address>` and each word that holds a digit `<n>`, so no code and no address is
/// in it.
fn traced_text(text: &str) -> String {
    text.split_whitespace()
        .take(70)
        .map(|word| {
            if word.contains('@') {
                "<address>"
            } else if word.bytes().any(|byte| byte.is_ascii_digit()) {
                "<n>"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `text` for the trace: one line, each address in it written `<address>`.
fn traced(text: &str) -> String {
    super::one_line(text, 160)
        .split(' ')
        .map(|word| {
            if word.contains('@') {
                "<address>"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

impl Desk for Window<'_> {
    fn open(&mut self, url: &str, within: Duration) -> Result<(), DeskError> {
        let open = wire::Open {
            title: self.title.clone(),
            store: Some(self.store.clone()),
            hosts: self
                .flow
                .map_or(&[][..], MailboxFlow::navigation_hosts)
                .iter()
                .map(|host| (*host).to_owned())
                .collect(),
            loopback: self.flow.and_then(MailboxFlow::loopback_port),
        };
        let mut host = Host::start(&self.exe, &open, within)?;
        let url = url.to_owned();
        let loaded = host.ask(|id| wire::Request::Load {
            id,
            url,
            aside: false,
        });
        self.host = Some(host);
        loaded.map(|_| ())
    }

    fn facts(&mut self) -> Result<Facts, DeskError> {
        let (answer, active) = self.eval(facts_script(self.pinned()))?;
        let mut facts = parse_facts(&answer, &active)?;
        let (failed, refused) = self
            .host
            .as_mut()
            .map_or((None, 0), |host| (host.failed.take(), host.refused));
        facts.load_failed = failed;
        if refused > self.refusals_said {
            self.refusals_said = refused;
            self.trace(
                "the page was sent outside the sign-in's pages; the window refused, and it \
                 stands where it was",
            );
        }
        if self.trace.is_some() {
            let line = format!(
                "read at {} title={:?} buttons={:?} disabled={:?} check={:?} code={:?} text={} \
                 marks={} load_failed={:?}",
                super::headless_oauth::url_place(&facts.url),
                traced(&facts.title),
                facts.buttons.iter().map(|b| traced(b)).collect::<Vec<_>>(),
                facts.disabled.iter().map(|b| traced(b)).collect::<Vec<_>>(),
                facts.check,
                facts.code,
                facts.text.len(),
                facts.markers.len(),
                facts.load_failed,
            );
            // A page with none of the flow's buttons says in its own words why it has none:
            // the start of its text, once per page, with each address and each word that
            // holds a digit masked.
            let stuck = facts.code.is_none()
                && !facts.text.is_empty()
                && !facts.buttons.iter().any(|shown| {
                    [
                        Button::ContinueWithEmail,
                        Button::VerifyEmailAddress,
                        Button::Authorize,
                    ]
                    .iter()
                    .any(|button| button.is(shown))
                });
            if stuck && self.traced_text.as_deref() != Some(line.as_str()) {
                self.trace(&format!("its text: {}", traced_text(&facts.text)));
                self.traced_text = Some(line.clone());
            }
            self.trace(&line);
        }
        Ok(facts)
    }

    fn press(&mut self, button: Button, email: &str, at: &str) -> Result<bool, DeskError> {
        if !self.admits(at) {
            return Ok(false);
        }
        let (answer, _) = self.eval(press_script(button, email, at, self.pinned()))?;
        let (did, was_at) = parse_act(&answer)
            .ok_or_else(|| DeskError::Unread("the page's answer was not a press's".into()))?;
        self.trace(&format!("press {}: {did}", button.title()));
        Ok(did == "pressed" && was_at == at)
    }

    fn fill(&mut self, field: Field, value: &str, at: &str) -> Result<Filled, DeskError> {
        let usable = match field {
            Field::Code => usable_code(value),
            Field::Email => usable_email(value),
        };
        if !usable {
            return Err(DeskError::Unread("the value to set is unusable".into()));
        }
        if !self.admits(at) {
            return Ok(Filled::Moved);
        }
        let (answer, _) = self.eval(fill_script(field, value, at, self.pinned()))?;
        let (did, was_at) = parse_act(&answer)
            .ok_or_else(|| DeskError::Unread("the page's answer was not a set's".into()))?;
        self.trace(&format!("fill {field:?}: {did}"));
        Ok(match did.as_str() {
            "filled" if was_at == at => Filled::Kept,
            "untaken" if was_at == at => Filled::NotKept,
            "absent" if was_at == at => Filled::NoField,
            _ => Filled::Moved,
        })
    }

    fn go(&mut self, url: &str) -> Result<(), DeskError> {
        let url = url.to_owned();
        self.host()?
            .ask(|id| wire::Request::Load {
                id,
                url,
                aside: false,
            })
            .map(|_| ())
    }

    fn link_code(&mut self, link: &str, within: Duration) -> Result<Linked, DeskError> {
        // The link is opened aside once: a second request would spend a link that signs in.
        if self.linked.as_ref().map(|sent| sent.as_str()) != Some(link) {
            let url = link.to_owned();
            self.host()?.ask(|id| wire::Request::Load {
                id,
                url,
                aside: true,
            })?;
            self.linked = Some(zeroize::Zeroizing::new(link.to_owned()));
            self.moved_at = None;
        }
        let place = super::headless_oauth::url_place(link);
        let end = Instant::now() + within;
        loop {
            let script = link_code_script(self.pinned())
                .ok_or_else(|| DeskError::Unread("the page's script is unusable".into()))?;
            let (answer, _) = self.host()?.ask(|id| wire::Request::Eval {
                id,
                script,
                aside: true,
            })?;
            let answer = zeroize::Zeroizing::new(answer.unwrap_or_default());
            let read = parse_link_code(&answer);
            // Only a document of the flow's own is read for a code, and only where the link
            // itself led.
            let ours = self.admits(&read.at);
            let at_link = ours && super::headless_oauth::url_place(&read.at) == place;
            let now = Instant::now();
            let found = if at_link {
                self.moved_at = None;
                read.code.map(Linked::Code)
            } else if ours {
                let since = *self.moved_at.get_or_insert(now);
                (now.saturating_duration_since(since) >= MOVED_SETTLE).then_some(Linked::Moved)
            } else {
                None
            };
            let left = end.saturating_duration_since(now);
            if found.is_some() || left.is_zero() {
                let found = found.unwrap_or(Linked::Waiting);
                self.trace(&format!(
                    "the sign-in link's page: {} (its text's shape: {})",
                    match &found {
                        Linked::Code(_) => "a login code",
                        Linked::Waiting => "no login code yet",
                        Linked::Moved => "it went on by itself",
                    },
                    read.shape
                ));
                return Ok(found);
            }
            std::thread::sleep(left.min(TICK));
        }
    }

    fn forget(&mut self) {
        if let Some(host) = self.host.as_mut() {
            let _ = host.ask(|id| wire::Request::Forget { id });
        }
        if let Some(marker) = &self.marker {
            let _ = std::fs::remove_file(marker);
        }
        self.trace("forgot the window's session for this account");
    }

    fn close(&mut self) {
        if let Some(mut host) = self.host.take() {
            host.end();
        }
    }

    fn store_is_the_accounts(&self) -> bool {
        self.marker.as_deref().is_some_and(|marker| {
            std::fs::read_to_string(marker)
                .is_ok_and(|held| super::accounts::email_matches(held.trim(), self.email))
        })
    }

    fn note_signed_in(&mut self) {
        if let Some(marker) = &self.marker
            && let (Some(dir), Some(name)) = (
                marker.parent(),
                marker.file_name().and_then(std::ffi::OsStr::to_str),
            )
            && aterm_types::fs_restricted::ensure_private_dir(dir).is_ok()
        {
            let _ = aterm_uds::private_file::write_private(
                dir,
                name,
                self.email.to_ascii_lowercase().as_bytes(),
            );
        }
    }

    fn has_mailbox(&self) -> bool {
        self.mailbox.is_some()
    }

    fn mailbox(&mut self, within: Duration) -> Result<MailboxCredential, &'static str> {
        let config = self
            .mailbox
            .ok_or("no mailbox is configured for this account")?;
        let read = super::headless_oauth::poll_mailbox_config_for_credential_with_seen_otps(
            config,
            Some(self.email),
            within,
            &mut self.seen_otps,
        );
        if self.trace.is_some() {
            self.trace(match &read {
                Ok(MailboxCredential::Otp(_)) => "mailbox: a login code",
                Ok(MailboxCredential::MagicLink(_)) => "mailbox: a sign-in link",
                Err(why) => why,
            });
        }
        read
    }

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn pause(&mut self, wait: Duration) {
        std::thread::sleep(wait);
    }

    fn cancelled(&self) -> bool {
        self.scope
            .is_some_and(super::account_work::Scope::cancelled)
    }

    fn say(&mut self, line: &str) {
        eprintln!("aterm: Claude sign-in window: {line}");
        self.trace(line);
    }
}

/// Signs in at `authorize_url` as `email` in a window of aterm's own and answers how its
/// callback reached, or reaches, the waiting sign-in. `scope` is the caller's account-work
/// scope, whose cancellation ends the wait; the opener process has none and passes `None`.
/// Whether a sign-in may open the window at all is its caller's to have established
/// ([`super::headless_oauth::visible_sign_in`]).
///
/// # Errors
/// Why the sign-in gave no callback, without the callback, a code or a link; a sign-in that
/// names no account's email is refused before anything is opened.
#[cfg_attr(
    test,
    expect(
        dead_code,
        reason = "a test's visible sign-in is substituted; no test opens a window"
    )
)]
pub(super) fn authorize(
    authorize_url: &str,
    email: Option<&str>,
    scope: Option<&super::account_work::Scope>,
) -> Result<VisibleDone, VisibleStop> {
    let Some(email) = email.map(str::trim).filter(|email| !email.is_empty()) else {
        return Err(VisibleStop::Failed(
            "the sign-in names no account email, and aterm authorizes only a named account".into(),
        ));
    };
    if scope.is_some_and(super::account_work::Scope::cancelled) {
        return Err(Stop::Cancelled.into());
    }
    let flow = MailboxFlow::new(authorize_url).map_err(|why| VisibleStop::Failed(why.into()))?;
    let Some(exe) = std::env::current_exe()
        .ok()
        .and_then(|exe| std::fs::canonicalize(exe).ok())
    else {
        return Err(VisibleStop::Failed(
            "aterm's own executable, which hosts the sign-in window, could not be found".into(),
        ));
    };
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let store = super::open_url::discover_store_dir()
        .or_else(|| aterm_types::dirs::state_dir().map(|state| state.join("accounts")));
    let mut mailbox =
        super::headless_oauth::discover_mailbox_config(home.as_deref(), store.as_deref());
    if let Some(config) = &mut mailbox {
        // The account's mail is read on this Mac. The fleet's mail relay asks another Mac for
        // the code, with the account's address: a sign-in in the window never does.
        config.read_on_this_mac_alone();
        let label = super::headless_oauth::infer_account_label_from_env();
        config.alias_label_to_email(label.as_deref(), Some(email));
        // The code this sign-in asks for is mailed after it starts. Dated from here, a code
        // mailed for an earlier sign-in is never taken, and a read made late in this one still
        // admits its own mail: a read with no start dates itself, each one later.
        if config.since.is_none() {
            config.since = aterm_time::unix_secs().and_then(|now| i64::try_from(now).ok());
        }
    }
    let mut desk =
        Window::new(exe, store.as_deref(), mailbox.as_ref(), email, scope).for_flow(&flow);
    // One sign-in window at a time on this Mac, a token's included: this one waits for the
    // other to close, and its own four minutes start when it opens.
    desk.trace("waiting for this Mac's one sign-in window");
    let _turn = window_turn(
        store.as_deref(),
        &|| scope.is_some_and(super::account_work::Scope::cancelled),
        (TURN_WITHIN, TURN_EVERY),
    )
    .inspect_err(|stop| desk.trace(&format!("ended: {stop}")))?;
    if let Some(path) = desk.trace.clone() {
        // A traced sign-in that panics says where: its opener's standard error reaches nobody
        // under a captured renewal.
        let earlier = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if let Some(place) = info.location()
                && let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(&path)
            {
                let _ = writeln!(file, "panicked at {}:{}", place.file(), place.line());
            }
            earlier(info);
        }));
    }
    let done = drive(
        &SignIn {
            authorize_url,
            flow: &flow,
            email,
            within: WITHIN,
        },
        &mut desk,
    );
    match &done {
        Ok(Done::Delivered) => desk.trace("ended: the callback reached the waiting sign-in"),
        Ok(Done::Callback(_)) => desk.trace("ended: a hosted callback to hand over"),
        Err(stop) => desk.trace(&format!("ended: {stop}")),
    }
    Ok(match done? {
        Done::Delivered => VisibleDone::Delivered,
        Done::Callback(callback) => VisibleDone::Callback(callback),
    })
}

#[cfg(test)]
#[path = "signin_window_tests.rs"]
mod tests;
