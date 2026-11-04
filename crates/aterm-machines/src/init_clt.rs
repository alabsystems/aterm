// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `base.clt` item for Apple's Command Line Tools ([`crate::items::developer`]).

use crate::items::{Ctx, Item};
use crate::model::Probe;
use crate::port::{Cmd, System};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

/// The item's id, as `aterm init --only` and every remedy that names it spell it.
pub const ID: &str = "base.clt";

/// What the C toolchain probe found, in the item's terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// The tools build a C program.
    Present(String),
    /// No developer directory holds a compiler.
    Missing(String),
    /// A compiler is installed and refuses to build.
    Refuses(String),
    /// The probe could not tell (a compiler that did not answer in time).
    Unknown(String),
    /// Not a question here (not macOS).
    NotHere(String),
}

/// The probe [`CommandLineTools`] reads.
pub type ProbeClt = Box<dyn Fn(&dyn System) -> Found + Send + Sync>;

/// The reader of the swarm `m<N>` label the vault's roster gave this Mac at admission
/// ([`CommandLineTools::with_name`]), `None` before admission: read as the root half is about to
/// run, so the label rides the one password wherever the roster gave it by then.
pub type NameMac = Box<dyn Fn() -> Option<String> + Send + Sync>;

/// Why the install the Command Line Tools tab runs did not finish
/// ([`aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB`]): the plain words the tab ends on
/// ([`Unfinished::reason`]) and, for what the root half finds, the exit code `aterm machines here
/// apply --plain` says it with ([`Unfinished::code`]), so the root half prints nothing of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unfinished {
    /// `sudo` refused: the Mac password was not accepted (sudo's own exit 1, which the root half
    /// never uses).
    Password,
    /// Apple's software catalog could not be read.
    Catalog,
    /// Apple's catalog offers no Command Line Tools for this Mac.
    NoneOffered,
    /// The download from Apple did not finish.
    Download,
    /// Apple's installer refused them: `softwareupdate -i` failed after its download, or exited
    /// 0 and installed nothing.
    Installer,
    /// Another install of them runs: `softwareupdate` or `installer` holds the placeholder, or
    /// a live run of the root half stamped it ([`crate::items::developer::Holder::Install`]).
    Busy,
    /// A stop ended the install before the tools arrived: a signal the tab's run caught (a
    /// person's Ctrl-C, at `sudo`'s password prompt or during the install), or the root half's
    /// own stop.
    Stopped,
    /// Apple's own installer window has not installed them: it was open when the run began,
    /// with the placeholder beside it or not, which defers to it
    /// ([`crate::items::developer::Holder::AppleWindow`]), or the root half's wait on it (the one
    /// it opened where the catalog lists none, or the one already open) ran out with it still
    /// open.
    AppleWindow,
    /// Apple's own installer window, which the root half waited on, closed without installing
    /// them, with no installer running: a person pressed Not Now or Cancel there or dismissed
    /// Apple's own failure (each unlinks the placeholder), or quit the window before pressing
    /// Install (which leaves the placeholder aterm stamped beside it).
    WindowClosed,
    /// The tools already on this Mac refuse to build ([`Found::Refuses`]).
    Refuses,
    /// Whether they are installed could not be told ([`Found::Unknown`]).
    Unread,
    /// The run is not in the person's own Mac login.
    NotOwnLogin,
    /// The network this Mac uses is in Low Data Mode, so the download waits: nothing was asked.
    LowData,
    /// This account is not one of the Mac's administrators, whom alone macOS lets install the
    /// tools: nothing was asked.
    NotAdmin,
    /// Anything else.
    Other,
}

impl Unfinished {
    /// Every class, in code order.
    pub const ALL: [Self; 15] = [
        Self::Password,
        Self::Catalog,
        Self::NoneOffered,
        Self::Download,
        Self::Installer,
        Self::Busy,
        Self::Stopped,
        Self::AppleWindow,
        Self::WindowClosed,
        Self::Refuses,
        Self::Unread,
        Self::NotOwnLogin,
        Self::LowData,
        Self::NotAdmin,
        Self::Other,
    ];

    /// The exit code `aterm machines here apply --plain` says this class with: the root half's
    /// classes only (never 0, `sudo`'s own 1, a usage 2, a panic's 101 or a failed exec's 126 and
    /// 127). `None` for what only the tab's own run finds.
    #[must_use]
    pub const fn code(self) -> Option<u8> {
        match self {
            Self::Catalog => Some(80),
            Self::NoneOffered => Some(81),
            Self::Download => Some(82),
            Self::Installer => Some(83),
            Self::Busy => Some(84),
            Self::Stopped => Some(85),
            Self::AppleWindow => Some(86),
            Self::Other => Some(87),
            Self::WindowClosed => Some(88),
            Self::Password
            | Self::Refuses
            | Self::Unread
            | Self::NotOwnLogin
            | Self::LowData
            | Self::NotAdmin => None,
        }
    }

    /// What the tab's program exits with once it said this class: Low Data Mode, an account
    /// that is no administrator's, a stop and Apple's window closed without installing them have
    /// their own codes ([`aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_LOW_DATA`],
    /// [`aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_NOT_ADMIN`],
    /// [`aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_STOPPED`],
    /// [`aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_WINDOW_CLOSED`]), so the window spends
    /// no automatic opening on the first, offers no Install… for the second, and keeps the last
    /// two's opening spent, each a person's press; every other class is 1.
    #[must_use]
    pub const fn tab_exit(self) -> u8 {
        match self {
            Self::LowData => aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_LOW_DATA,
            Self::NotAdmin => aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_NOT_ADMIN,
            Self::Stopped => aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_STOPPED,
            Self::WindowClosed => aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_WINDOW_CLOSED,
            _ => 1,
        }
    }

    /// Whether an ask of aterm's own that ended for this reason stays spent, as System State's
    /// window reads the tab's exit ([`Self::tab_exit`]): a stop (a person's Ctrl-C), Apple's
    /// window closed without the tools (a person's press there) and an account that is no
    /// administrator's keep it, as a closed tab does; every other class installed nothing and no
    /// person declined it (a password macOS refused, a broken download, a machine's failure, Low
    /// Data Mode), so it takes the ask back and the next ask of that build goes ahead.
    #[must_use]
    pub const fn keeps_the_ask(self) -> bool {
        matches!(self, Self::Stopped | Self::WindowClosed | Self::NotAdmin)
    }

    /// How `sudo <aterm> machines here apply --plain` ended (`code`, `None` on a signal): done
    /// on 0, `sudo`'s own refusal on 1, a root class on its code, else [`Self::Other`].
    ///
    /// # Errors
    /// The class the exit names.
    pub fn after_root(code: Option<i32>) -> Result<(), Self> {
        match code {
            Some(0) => Ok(()),
            Some(1) => Err(Self::Password),
            Some(code) => Err(Self::ALL
                .into_iter()
                .find(|class| class.code().map(i32::from) == Some(code))
                .unwrap_or(Self::Other)),
            None => Err(Self::Other),
        }
    }

    /// The plain reason the tab says after `The install did not finish:`.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Password => "macOS did not accept the Mac password",
            Self::Catalog => "Apple's software catalog could not be reached",
            Self::NoneOffered => {
                "Apple's software catalog offers no Command Line Tools for this Mac"
            }
            Self::Download => "the download from Apple did not finish",
            Self::Installer => "Apple's installer refused to install them",
            Self::Busy => "another install of the Command Line Tools is running",
            Self::Stopped => "it was stopped",
            Self::AppleWindow => {
                "Apple's installer window has not installed them (press Install there, or quit it)"
            }
            Self::WindowClosed => "Apple's installer window closed without installing them",
            Self::Refuses => "the developer tools already on this Mac do not work",
            Self::Unread => "aterm could not tell whether they are installed",
            Self::NotOwnLogin => "it runs only in your own Mac login",
            Self::LowData => "Low Data Mode is on, so the download waits",
            Self::NotAdmin => "only an administrator of this Mac can install them",
            Self::Other => "something unexpected went wrong",
        }
    }

    /// The tab's last line for this class:
    /// `The install did not finish: <reason>. Install… on System State tries again.`, except
    /// where pressing Install… again would only say the same: Low Data Mode says how to lift it
    /// ([`TAB_LOW_DATA`]), and an account that is no administrator's who must install them
    /// ([`TAB_NOT_ADMIN`]).
    #[must_use]
    pub fn line(self) -> String {
        match self {
            Self::LowData => TAB_LOW_DATA.to_owned(),
            Self::NotAdmin => TAB_NOT_ADMIN.to_owned(),
            _ => format!(
                "{}: {}. {}",
                aterm_types::macos_setup::COMMAND_LINE_TOOLS_NOT_FINISHED,
                self.reason(),
                aterm_types::macos_setup::COMMAND_LINE_TOOLS_TRY_AGAIN
            ),
        }
    }
}

/// The tab's last line while Low Data Mode holds the download back ([`Unfinished::LowData`]).
pub const TAB_LOW_DATA: &str = "Low Data Mode is on, so the download waits. Turn it off in \
                                System Settings \u{203a} Network, then press Install\u{2026} \
                                again.";

/// The tab's last line in an account that is no administrator's ([`Unfinished::NotAdmin`]).
pub const TAB_NOT_ADMIN: &str = "Only an administrator of this Mac can install Apple's Command \
                                 Line Tools. Ask one to install them from their own login.";

/// The one line the tab says before macOS asks for the password.
pub const TAB_INTRO: &str = "aterm is installing Apple's Command Line Tools, which git and your \
                             GitHub repositories need. macOS asks for your Mac password once.";

/// The one line a run outside the tab says right before `sudo` asks for the Mac password, so the
/// prompt never comes bare after whatever the terminal showed last (a package install's output).
pub const INTRO: &str = "aterm init: installing Apple's Command Line Tools (git, cc, the macOS \
                         SDK); sudo asks for your Mac password once";

/// The password prompt (`sudo -p`), in the tab and outside it.
pub const TAB_PROMPT: &str = "Mac password: ";

/// The tab's last line once the tools are installed; the window then closes the tab unless it is
/// its window's last (`aterm-gui`'s `App::developer_tools_install_ended`).
pub const TAB_INSTALLED: &str =
    "Successfully completed: Apple's Command Line Tools are installed. This tab closes by itself.";

/// Where the tab's own words go (its terminal), and why its install did not finish, once the
/// item knows: what [`CommandLineTools::in_tab`] speaks through.
#[derive(Clone)]
pub struct Tab {
    say: Arc<dyn Fn(&str) + Send + Sync>,
    unfinished: Arc<Mutex<Option<Unfinished>>>,
}

impl Tab {
    /// A tab that writes its words with `say` (each as given: a line carries its own newline).
    #[must_use]
    pub fn new(say: Arc<dyn Fn(&str) + Send + Sync>) -> Self {
        Self {
            say,
            unfinished: Arc::default(),
        }
    }

    /// Write `text` as given.
    pub fn write(&self, text: &str) {
        (self.say)(text);
    }

    /// Write `text` as one line.
    pub fn line(&self, text: &str) {
        (self.say)(&format!("{text}\n"));
    }

    /// Note `why` the install did not finish, returning its plain reason.
    pub fn note(&self, why: Unfinished) -> String {
        *self
            .unfinished
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(why);
        why.reason().to_owned()
    }

    /// Why the install did not finish, if the item said.
    #[must_use]
    pub fn unfinished(&self) -> Option<Unfinished> {
        *self
            .unfinished
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// `base.clt`: installs Apple's Command Line Tools by the root path that exists.
pub struct CommandLineTools {
    found: ProbeClt,
    /// The `aterm` that `sudo` runs (this executable).
    aterm: PathBuf,
    /// System State's Command Line Tools tab, when this run is its program: plain words, a
    /// plain password prompt, and the root half's `--plain` exit codes.
    tab: Option<Tab>,
    /// What holds the ask back in a run aterm decided ([`Self::unbidden`]); `None` in a person's
    /// run, which asks each time.
    unbidden: Option<Unbidden>,
    /// Where a run outside the tab says its one line before `sudo` asks ([`INTRO`]): this run's
    /// standard error.
    say: Arc<dyn Fn(&str) + Send + Sync>,
    /// The swarm `m<N>` label the vault's roster gave this Mac at admission, where it is known
    /// by the time the root half runs: the root half names the Mac by it under the same password
    /// ([`plain_name`], [`Self::baseline_named`]).
    name: NameMac,
}

/// `word` when it is a name the root half gives the Mac: a bare swarm label, `m` and a number
/// with no leading zero (`m42`), which is also a DNS label and a fleet name. Anything else,
/// `Mac-mini-M4`, `m42-studio` or `M42` included, is none: the root half takes no free text from
/// the person's account.
#[must_use]
pub fn plain_name(word: &str) -> Option<String> {
    aterm_secrets::swarm_roster::leading_m_label(word)
        .filter(|(_, canonical)| canonical == word)
        .map(|(_, canonical)| canonical)
}

impl CommandLineTools {
    /// The item over `found`.
    #[must_use]
    pub fn new(found: ProbeClt, aterm: PathBuf) -> Self {
        Self {
            found,
            aterm,
            tab: None,
            unbidden: None,
            say: Arc::new(|text: &str| {
                use std::io::Write as _;
                let _ = std::io::stderr().lock().write_all(text.as_bytes());
            }),
            name: Box::new(|| None),
        }
    }

    /// This item naming the Mac by what `name` reads as its root half is about to run, where
    /// that is a [`plain_name`]; anything else (or none) names nothing.
    #[must_use]
    pub fn with_name(self, name: NameMac) -> Self {
        Self { name, ..self }
    }

    /// This item as the Command Line Tools tab's program speaks it, through `tab`.
    #[must_use]
    pub fn in_tab(self, tab: Tab) -> Self {
        Self {
            tab: Some(tab),
            ..self
        }
    }

    /// This item in a run aterm decided ([`crate::init::Decider::Aterm`]: the follow-up of a
    /// person's own package install or update), held as `unbidden` says: it asks for the Mac
    /// password at most once per build, sharing System State's Command Line Tools tab's ask
    /// ([`aterm_types::clt_mark`]), and never while the person's choices hold that tab, as read
    /// when its probe runs and again right before it claims the ask; where it asks nothing it is
    /// deferred, naming the person's step ([`PERSONS_STEP`]), and an ask a Stop or another asker
    /// took between its probe and its apply fails in those words.
    #[must_use]
    pub fn unbidden(self, unbidden: Unbidden) -> Self {
        Self {
            unbidden: Some(unbidden),
            ..self
        }
    }

    /// This item saying its line before `sudo` through `say` (a test's).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn saying(self, say: Arc<dyn Fn(&str) + Send + Sync>) -> Self {
        Self { say, ..self }
    }

    /// The one-table baseline the root half applies.
    #[must_use]
    pub fn baseline() -> String {
        Self::baseline_named(None)
    }

    /// [`Self::baseline`], with the Mac's Computer Name and Local hostname set to `name` where
    /// it is a [`plain_name`] (`--name`): the one password that installs the tools also gives
    /// the Mac the name its vault and fleet know it by.
    #[must_use]
    pub fn baseline_named(name: Option<&str>) -> String {
        let mut text = format!(
            "schema = {}\n\n[developer]\ncommand_line_tools = true\n",
            crate::SCHEMA
        );
        if let Some(name) = name.and_then(plain_name) {
            text.push_str(&format!(
                "\n[names]\ncomputer_name = \"{name}\"\nlocal_host_name = \"{name}\"\n"
            ));
        }
        text
    }

    /// The argv the apply runs on the person's terminal: the root half `--plain`, which applies
    /// [`Self::baseline`] built in and reads no file the person's account could write, under
    /// `sudo -k` with the plain prompt ([`TAB_PROMPT`]). `-k` with a command makes `sudo` ignore
    /// the credentials it cached for this terminal and cache none (sudo(8) 1.9.17 on macOS 27.0),
    /// so a password typed for an earlier `sudo` never authorizes the root half by itself, none is
    /// kept once it ends, and the person's own cached `sudo` stays as it was. Where the roster
    /// gave this Mac its label by now ([`Self::with_name`]), `--name` carries it.
    #[must_use]
    pub fn command(&self) -> Cmd {
        let root = Cmd::new(SUDO)
            .args(["-k", "-p", TAB_PROMPT])
            .arg(self.aterm.as_os_str())
            .args(["machines", "here", "apply", "--plain"]);
        match (self.name)().as_deref().and_then(plain_name) {
            Some(name) => root.args(["--name", name.as_str()]),
            None => root,
        }
    }

    /// How the root half's run (`ran`) ended, as the tab's program and a run outside it both
    /// read it: a stop this run caught (`stopped`) is the person's, whatever `sudo` or the root
    /// half then exited with (a Ctrl-C at the password prompt ends `sudo` on its own 1, which
    /// alone would read as a password macOS refused); else the root half's exit
    /// ([`Unfinished::after_root`]); and tools that arrived anyway are installed, whatever it
    /// said.
    fn ended(
        &self,
        sys: &dyn System,
        ran: &Result<Option<i32>, crate::port::RunFailed>,
        stopped: Option<i32>,
    ) -> Result<(), Unfinished> {
        let ended = match ran {
            Ok(code) if *code != Some(0) && stopped.is_some() => Err(Unfinished::Stopped),
            Ok(code) => Unfinished::after_root(*code),
            Err(_) => Err(Unfinished::Other),
        };
        match ended {
            Err(_) if matches!((self.found)(sys), Found::Present(_)) => Ok(()),
            ended => ended,
        }
    }
}

/// What holds back the ask of a run aterm decided ([`CommandLineTools::unbidden`]): this
/// account's record of aterm's own ask, which System State's Command Line Tools tab reads and
/// writes too ([`aterm_types::clt_mark`]), the build that asks, and the person's choices in
/// System State that hold that tab ([`held_by`]), read from the setup records as the item's probe
/// runs and again right before its apply claims the ask, so a Stop pressed after `aterm init`
/// began, while the base layer or the vault's join ran, still holds it (principle 4).
pub struct Unbidden {
    /// The record ([`aterm_types::clt_mark::path`]); `None` where it has no place, which holds
    /// the ask as a record naming this build does.
    mark: Option<PathBuf>,
    /// The build that asks ([`aterm_types::version::build_id`]).
    build: String,
    /// The person's choices now, as the setup records say ([`crate::init::Records::holds`]).
    holds: Arc<dyn Fn() -> crate::init::Holds + Send + Sync>,
}

impl Unbidden {
    /// The record at `mark` and `build`, held as `holds` reads the person's choices each time
    /// it is called.
    #[must_use]
    pub fn new(
        mark: Option<PathBuf>,
        build: String,
        holds: impl Fn() -> crate::init::Holds + Send + Sync + 'static,
    ) -> Self {
        Self {
            mark,
            build,
            holds: Arc::new(holds),
        }
    }

    /// This account's record and this build, held as `records` say each time they are read
    /// ([`held_by`]).
    #[must_use]
    pub fn here(records: &'static dyn crate::init::Records) -> Self {
        Self::new(
            aterm_types::clt_mark::path(),
            aterm_types::version::build_id(),
            move || records.holds(),
        )
    }

    /// Why the person's choices hold the ask now, naming their step, as the setup records read
    /// at this call ([`held_by`]).
    fn held(&self) -> Option<String> {
        held_by(&(self.holds)()).map(|why| {
            format!("aterm asks for no Mac password by itself because {why}: {PERSONS_STEP}")
        })
    }

    /// Why a run aterm decided asks no password by itself now, naming the person's step, or
    /// `None` when it may ask: the person's choices hold it ([`Self::held`]), this build asked by
    /// itself already (its tab, or an earlier run aterm decided: [`ASKED_ON_THIS_BUILD`]), or the
    /// record has no place or cannot say whether it did ([`UNREAD_RECORD`]), as the record reads
    /// through `sys` ([`aterm_types::clt_mark::reading`]).
    fn withheld(&self, sys: &dyn System) -> Option<String> {
        if let Some(why) = self.held() {
            return Some(why);
        }
        let Some(mark) = self.mark.as_deref() else {
            return Some(format!(
                "{UNREAD_RECORD}, which has no place in this account, so it asks for no Mac \
                 password by itself: {PERSONS_STEP}"
            ));
        };
        match aterm_types::clt_mark::reading(sys.clt_mark(), mark, &self.build) {
            aterm_types::clt_mark::Asked::NotNamed => None,
            aterm_types::clt_mark::Asked::Named => {
                Some(format!("{ASKED_ON_THIS_BUILD}: {PERSONS_STEP}"))
            }
            aterm_types::clt_mark::Asked::Unread(error) => Some(format!(
                "{UNREAD_RECORD}, {} ({error}), so it asks for no Mac password by itself: \
                 {PERSONS_STEP}",
                mark.display()
            )),
        }
    }

    /// Claim this build's ask in the record, right before `sudo` asks
    /// ([`aterm_types::clt_mark::claim`]): `true` when this run added its line, `false` when
    /// another asker of this build (System State's tab) named it first.
    ///
    /// # Errors
    /// No place for the record, and the claim's own.
    fn claim(&self, sys: &dyn System) -> std::io::Result<bool> {
        let mark = self
            .mark
            .as_deref()
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;
        aterm_types::clt_mark::claim(sys.clt_mark(), mark, &self.build)
    }

    /// Take this build's ask back, which this run's claim added: it installed nothing, and no
    /// person declined it, so the next run of this build, or its tab, asks again. A record that
    /// cannot be rewritten keeps it, and says so in the log.
    fn forget(&self, sys: &dyn System) {
        if let Some(mark) = &self.mark
            && let Err(error) = aterm_types::clt_mark::forget(sys.clt_mark(), mark, &self.build)
        {
            aterm_log::warn!(
                "aterm init: the record of aterm's Command Line Tools ask was not taken back: {error}"
            );
        }
    }
}

/// Why the person's choices in System State hold a run aterm decided back from asking for the
/// Mac password, as they hold System State's own Command Line Tools tab: their Stop of setup, or
/// records that cannot be read now (which hold every group, [`crate::init::Holds::stopped`]), or
/// records a newer aterm saved ([`NEWER`]). `None` when they hold nothing of it: a group turned
/// off holds no tab.
#[must_use]
pub fn held_by(holds: &crate::init::Holds) -> Option<String> {
    if holds.newer {
        Some(NEWER.to_owned())
    } else {
        holds.stopped.clone()
    }
}

/// The person's own ways to install the tools, which a run aterm decided names where it asks no
/// password by itself: System State's card, or an `aterm init` they type, each asking once.
pub const PERSONS_STEP: &str = "press Install Command Line Tools\u{2026} on System State, or run \
                                `aterm init --only base.clt` in a terminal";

/// Why a run aterm decided asks no password where this build asked by itself already: its tab,
/// or an earlier run aterm decided ([`aterm_types::clt_mark`]).
pub const ASKED_ON_THIS_BUILD: &str = "aterm asks for your Mac password for them by itself once \
                                       per build, and this build has asked";

/// Why a run aterm decided asks no password where the record of its own ask has no place, or
/// cannot say whether this build asked ([`aterm_types::clt_mark::Asked::Unread`]): the row goes on
/// to name where, and why.
pub const UNREAD_RECORD: &str = "aterm cannot read its record of asking for them";

/// Why a run aterm decided asks no password where a newer aterm's setup records stand
/// ([`held_by`]).
pub const NEWER: &str = "a newer aterm set this Mac up";

/// Why a run aterm decided asked no password after all: its claim of the ask in the record failed
/// (no hold on the record's lock, or a write that failed), so a later run could not tell that it
/// asked.
pub const NOT_NOTED: &str = "aterm could not note that it asked, so it asks for no Mac password \
                             by itself";

/// `sudo`.
const SUDO: &str = "/usr/bin/sudo";

/// Where `aterm init` places the item.
#[must_use]
pub fn place() -> crate::engine::Place {
    crate::engine::Place {
        layer: aterm_converge::Layer::Base,
        class: aterm_converge::Class::Elevate,
        requires: aterm_converge::Requires::Account,
        remedy: aterm_converge::Remedy::Init {
            only: ID.to_owned(),
        },
    }
}

/// Whether the `--only` word `word` names the item ([`aterm_converge::selects`]).
#[must_use]
pub fn selected(word: &str) -> bool {
    aterm_converge::selects(word, ID, "developer", Some(aterm_converge::Layer::Base))
}

/// Why the item cannot install from here.
pub const NO_TTY: &str = "installing the Command Line Tools asks for your password, and this \
                          run has no terminal to ask it in: run `aterm init --only base.clt` \
                          in a terminal";

/// Why the item leaves the tools to an install under way ([`held`]).
pub const INSTALLING: &str = "Apple's Command Line Tools are being installed now, by an install \
                              already under way";

/// Why the item asks nobody for a password in an account that is no administrator's.
pub const NOT_ADMIN: &str = "only an administrator of this Mac can install them, and this \
                             account is not one: an administrator runs `aterm init --only \
                             base.clt` in their own login";

/// Why the item leaves the tools to Apple's installer window open for them
/// ([`crate::items::developer::Holder::AppleWindow`]), naming the person's step there.
pub const APPLE_WINDOW_OPEN: &str = "Apple's installer window is open for them: finish in it \
                                     (press Install there), or quit it so aterm can install them";

/// What holds an install of the tools now, with the on-demand placeholder there or not as one
/// `stat` finds it, by the rule the window's page and the root half share
/// ([`crate::items::developer::holder`]): an install under way (`softwareupdate` or `installer`
/// beside the placeholder, or a live run of the root half that stamped it, which alone may hold
/// or replace it), Apple's installer window at any age with the placeholder beside it or not (its
/// own flow, pressed or not, which makes the placeholder as it opens unless one is there, and
/// unlinks it when its request ends), or nothing. A process table that cannot be read is no
/// install here (the root half then says whether one holds a placeholder there).
fn held(sys: &dyn System) -> crate::items::developer::Holder {
    let placeholder = sys
        .stat(std::path::Path::new(crate::items::developer::PLACEHOLDER))
        .is_ok();
    crate::items::developer::holder(sys, placeholder)
}

/// Whether `row` is this item's, deferred: to an install under way, to Apple's installer window
/// open for them, or to a person at this Mac's console. None is this run's to finish, so an
/// apply or a plan completes with it.
#[must_use]
pub fn deferred(row: &aterm_converge::ItemReport) -> bool {
    row.id == ID && row.outcome == aterm_converge::Outcome::Deferred
}

impl Item for CommandLineTools {
    fn id(&self) -> String {
        ID.to_owned()
    }

    fn family(&self) -> &'static str {
        "developer"
    }

    fn title(&self) -> String {
        "Apple's Command Line Tools (cc, git, the macOS SDK)".to_owned()
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        let (probe, why) = self.read(sys);
        if let (Some(tab), Some(why)) = (&self.tab, why) {
            tab.note(why);
        }
        probe
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        if let Some(tab) = &self.tab {
            return self.apply_in_tab(sys, tab);
        }
        if !sys.has_tty() {
            return Err(NO_TTY.to_owned());
        }
        // In a run aterm decided, the person's choices are read once more and aterm's own ask is
        // claimed right before `sudo` asks: a Stop pressed since the probe holds it, neither
        // System State's tab nor a later run of this build asks again by itself, and of this run
        // and a tab of this build landing at once only the one whose claim added the line asks.
        // An ask it cannot claim is not made. The engine gives an apply no deferral of its own,
        // so each of these reads as the row's failure in the deferral's words, naming the
        // person's step.
        if let Some(unbidden) = &self.unbidden {
            if let Some(why) = unbidden.held() {
                return Err(why);
            }
            match unbidden.claim(sys) {
                Ok(true) => {}
                Ok(false) => return Err(format!("{ASKED_ON_THIS_BUILD}: {PERSONS_STEP}")),
                Err(error) => return Err(format!("{NOT_NOTED} ({error}): {PERSONS_STEP}")),
            }
        }
        (self.say)(&format!("{INTRO}\n"));
        // The root half runs in this terminal's foreground, so a Ctrl-C (which the
        // through-installer wait invites) or a closed terminal reaches this run too: held until
        // the root half has finished its step and aterm's own ask is kept or taken back, then
        // sent again, so the run still stops as it would have.
        let stops = sys.defer_stops();
        let ran = sys.run_tty(&self.command());
        let ended = self.ended(sys, &ran, sys.stopped());
        if let Some(unbidden) = &self.unbidden
            && ended.is_err_and(|why| !why.keeps_the_ask())
        {
            unbidden.forget(sys);
        }
        drop(stops);
        match (ended, ran) {
            (Ok(()), _) => Ok(()),
            (Err(_), Err(e)) => Err(format!("`sudo` did not start: {e}")),
            (Err(why), Ok(_)) => Err(why.reason().to_owned()),
        }
    }
}

impl CommandLineTools {
    /// What the probe reads, and why the tab's install would not finish when it reads short of
    /// a change: the tools present; else, with them missing, an install already under way or
    /// Apple's installer window open for them (deferred to it, asking nothing; the window's row
    /// names the person's step there, and the rest of the run goes on), a run with no person at
    /// this Mac's console (deferred to one: only a person there types the Mac password, never an
    /// agent's tab, a script or a remote shell), no terminal to ask in, Low Data Mode, or an
    /// account that is no administrator's (each blocked, asking nothing), or, in a run aterm
    /// decided, the person's Stop as the setup records read now, this build's ask spent, or a
    /// record that cannot say ([`Unbidden::withheld`]: deferred, naming the person's step); else
    /// the change.
    fn read(&self, sys: &dyn System) -> (Probe, Option<Unfinished>) {
        let from = match (self.found)(sys) {
            Found::Present(now) => return (Probe::Ok { now }, None),
            Found::Refuses(why) => return (Probe::Blocked { why }, Some(Unfinished::Refuses)),
            Found::Unknown(why) => return (Probe::Unusable { why }, Some(Unfinished::Unread)),
            Found::NotHere(why) => return (Probe::NotApplicable { why }, None),
            Found::Missing(from) => from,
        };
        match held(sys) {
            crate::items::developer::Holder::Install => {
                return (
                    Probe::Deferred {
                        why: format!("{from}; {INSTALLING}"),
                    },
                    Some(Unfinished::Busy),
                );
            }
            crate::items::developer::Holder::AppleWindow => {
                return (
                    Probe::Deferred {
                        why: format!("{from}; {APPLE_WINDOW_OPEN}"),
                    },
                    Some(Unfinished::AppleWindow),
                );
            }
            crate::items::developer::Holder::Nothing | crate::items::developer::Holder::Unread => {}
        }
        let presence = crate::console::presence(sys);
        if let Some(person) = presence.needs_person() {
            return (
                Probe::Deferred {
                    why: format!("{from}; installing them {person}"),
                },
                Some(Unfinished::Other),
            );
        }
        if !sys.has_tty() {
            return (
                Probe::Blocked {
                    why: format!("{from}; {NO_TTY}"),
                },
                Some(Unfinished::Other),
            );
        }
        // Asked before the password: the download waits for a network without Low Data Mode.
        if let Some(why) = crate::items::developer::low_data(sys) {
            return (
                Probe::Blocked {
                    why: format!("{from}; {why}"),
                },
                Some(Unfinished::LowData),
            );
        }
        if sys.administrator() == Some(false) {
            return (
                Probe::Blocked {
                    why: format!("{from}; {NOT_ADMIN}"),
                },
                Some(Unfinished::NotAdmin),
            );
        }
        if let Some(why) = self
            .unbidden
            .as_ref()
            .and_then(|unbidden| unbidden.withheld(sys))
        {
            return (
                Probe::Deferred {
                    why: format!("{from}; {why}"),
                },
                Some(Unfinished::Other),
            );
        }
        (
            Probe::Change {
                from,
                to: format!(
                    "installed as root by `{} machines here apply --plain` (one password prompt)",
                    Cmd::new(SUDO).arg(self.aterm.as_os_str()).display()
                ),
            },
            None,
        )
    }

    /// [`Item::apply`] as the tab's program: one plain line before `sudo` asks for the Mac
    /// password with the plain prompt, and the root half run `--plain`. Once that has started,
    /// this run waits for it to end, holding off a Ctrl-C or a closed tab (which reach the root
    /// half, which finishes its step), and says how it ended ([`Self::ended`]): installed when the
    /// tools are there, whatever the root half's exit, else its plain reason ([`Unfinished`]),
    /// never an argv or a path.
    fn apply_in_tab(&self, sys: &dyn System, tab: &Tab) -> Result<(), String> {
        if !sys.has_tty() {
            return Err(tab.note(Unfinished::Other));
        }
        let stops = sys.catch_stops();
        tab.line(TAB_INTRO);
        let ran = sys.run_tty(&self.command());
        let stopped = sys.stopped();
        drop(stops);
        self.ended(sys, &ran, stopped).map_err(|why| tab.note(why))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::bind;
    use crate::items::defaults::tests::ctx;
    use crate::model::{Mode, Outcome};
    use crate::port::{Fake, Ran, Stat};
    use aterm_converge::{Class, Layer, NoJournal, Scope};

    const ATERM: &str = "/Applications/aterm.app/Contents/MacOS/aterm";

    const CC: &str = "/Library/Developer/CommandLineTools/usr/bin/cc";

    /// The person's own run at this Mac's console (alice, 501), with a terminal to ask in.
    fn console() -> Fake {
        let mut sys = Fake::root();
        sys.euid = 501;
        sys.tty = true;
        sys.stat_of(
            "/dev/console",
            Stat {
                uid: 501,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: 0,
            },
        );
        sys
    }

    /// The item over a probe that finds the tools only once [`CC`] is there, its line before
    /// `sudo` said to nobody (a test that reads it says it through [`CommandLineTools::saying`]).
    fn item(found: Found) -> CommandLineTools {
        CommandLineTools::new(
            Box::new(move |sys: &dyn System| {
                if sys.read_file(std::path::Path::new(CC)).is_ok() {
                    Found::Present(CC.into())
                } else {
                    found.clone()
                }
            }),
            PathBuf::from(ATERM),
        )
        .saying(Arc::new(|_: &str| {}))
    }

    /// The root half's argv on the terminal, in the tab and outside it alike.
    fn argv() -> String {
        format!("tty /usr/bin/sudo -k -p 'Mac password: ' {ATERM} machines here apply --plain")
    }

    fn missing() -> CommandLineTools {
        item(Found::Missing("no developer directory".into()))
    }

    fn run(sys: &Fake, item: &CommandLineTools, mode: Mode) -> aterm_converge::ItemReport {
        let ctx = ctx(&[]);
        let bound = bind([(item as &dyn Item, place())], sys, &ctx);
        let mut rows =
            aterm_converge::run_scoped(&bound, mode, Scope::EVERY, &mut NoJournal, &mut |_| {})
                .unwrap();
        rows.remove(0)
    }

    fn asked(sys: &Fake) -> Vec<String> {
        sys.calls()
            .into_iter()
            .filter(|call| call.starts_with("tty "))
            .collect()
    }

    /// The tools missing, a person's own run at the console plans the root half `--plain`, and
    /// its apply runs it on the terminal: one password prompt, and no baseline file written for
    /// root to read (the root half applies its own).
    #[test]
    fn missing_tools_plan_the_machines_here_argv_and_apply_runs_it_on_the_terminal() {
        let sys = console();
        let item = missing();

        let row = run(&sys, &item, Mode::Plan);
        assert_eq!(row.outcome, Outcome::Planned, "{row:?}");
        assert_eq!(
            row.to.as_deref(),
            Some(
                "installed as root by `/usr/bin/sudo /Applications/aterm.app/Contents/MacOS/\
                 aterm machines here apply --plain` (one password prompt)"
            ),
            "{row:?}"
        );
        assert_eq!(row.layer, Some(Layer::Base));
        assert_eq!(row.class, Some(Class::Elevate));
        assert!(
            asked(&sys).is_empty(),
            "a plan runs nothing: {:?}",
            sys.calls()
        );

        let argv = argv();
        sys.on(&argv, Ran::ok(""))
            .on_write(&argv, std::path::Path::new(CC), b"");
        let row = run(&sys, &item, Mode::Apply);
        assert_eq!(row.outcome, Outcome::Changed, "{row:?} {:?}", sys.calls());
        let calls = sys.calls();
        assert_eq!(asked(&sys), [argv], "one password prompt: {calls:?}");
        assert!(
            !calls
                .iter()
                .any(|call| call.starts_with("write ") || call.starts_with("scratch")),
            "nothing written for the root half to read: {calls:?}"
        );
        let parsed = crate::baseline::parse(&CommandLineTools::baseline()).unwrap();
        assert_eq!(
            parsed.developer.and_then(|table| table.command_line_tools),
            Some(true)
        );
    }

    /// A swarm label known when the root half runs rides the same prompt: the one argv gains
    /// `--name`, and the root half's baseline names the Mac by it. Free text is never passed
    /// (NEGATIVE CONTROL: `Mac-mini-M4`, `M42`, `m42-studio`, `m042` and none leave the argv and
    /// the baseline as they are without a name).
    #[test]
    fn a_known_swarm_label_rides_the_one_password_prompt() {
        let sys = console();
        let item = missing().with_name(Box::new(|| Some("m42".to_owned())));
        let named = format!("{} --name m42", argv());
        sys.on(&named, Ran::ok(""))
            .on_write(&named, std::path::Path::new(CC), b"");
        let row = run(&sys, &item, Mode::Apply);
        assert_eq!(row.outcome, Outcome::Changed, "{row:?} {:?}", sys.calls());
        assert_eq!(
            asked(&sys),
            [named],
            "one password prompt: {:?}",
            sys.calls()
        );
        let parsed =
            crate::baseline::parse(&CommandLineTools::baseline_named(Some("m42"))).unwrap();
        let names = parsed.names.expect("a [names] table");
        assert_eq!(names.computer_name.as_deref(), Some("m42"));
        assert_eq!(names.local_host_name.as_deref(), Some("m42"));
        assert_eq!(names.host_name, None);

        for word in ["Mac-mini-M4", "M42", "m42-studio", "m042", ""] {
            let sys = console();
            let owned = word.to_owned();
            let item = missing().with_name(Box::new(move || Some(owned.clone())));
            sys.on(&argv(), Ran::ok(""))
                .on_write(&argv(), std::path::Path::new(CC), b"");
            run(&sys, &item, Mode::Apply);
            assert_eq!(asked(&sys), [argv()], "{word:?}: {:?}", sys.calls());
            assert_eq!(
                CommandLineTools::baseline_named(Some(word)),
                CommandLineTools::baseline(),
                "{word:?}"
            );
        }
    }

    /// A run outside the tab says its one line right before `sudo` asks, whatever ran on the
    /// terminal before it, and asks with the plain prompt under `sudo -k` (design §6.3): a
    /// password cached for an earlier `sudo` never authorizes the root half, and none is kept
    /// once it ends, so no `sudo -K` follows it, whether the install finished or not.
    #[test]
    fn a_run_outside_the_tab_says_why_right_before_sudo_asks_afresh() {
        let argv = argv();
        for (ran, outcome) in [
            (Ran::ok(""), Outcome::Changed),
            (Ran::exit(82, ""), Outcome::Failed),
        ] {
            let sys = Arc::new(console());
            let said = Arc::new(Mutex::new(Vec::new()));
            let item = missing().saying({
                let (said, sys) = (Arc::clone(&said), Arc::clone(&sys));
                Arc::new(move |text: &str| {
                    said.lock()
                        .unwrap()
                        .push((sys.calls().len(), text.to_owned()));
                })
            });
            let installs = ran.success();
            sys.on(&argv, ran);
            if installs {
                sys.on_write(&argv, std::path::Path::new(CC), b"");
            }
            let row = run(&sys, &item, Mode::Apply);
            assert_eq!(row.outcome, outcome, "{row:?}");
            let calls = sys.calls();
            let ran = calls
                .iter()
                .position(|seen| *seen == argv)
                .unwrap_or_else(|| panic!("no root half: {calls:?}"));
            assert_eq!(
                *said.lock().unwrap(),
                [(ran - 1, format!("{INTRO}\n"))],
                "said once, right before the stops are held and sudo asks: {calls:?}"
            );
            assert_eq!(calls[ran - 1], "defer stops", "{calls:?}");
            assert_eq!(asked(&sys), std::slice::from_ref(&argv), "{calls:?}");
            assert!(
                !calls.iter().any(|call| call.contains("sudo -K")),
                "nothing cached to forget: {calls:?}"
            );
        }
        assert!(
            missing()
                .command()
                .display()
                .starts_with("/usr/bin/sudo -k -p 'Mac password: ' "),
            "{}",
            missing().command().display()
        );
    }

    /// The root half's failure says why in plain words on a person's own run too, never the argv
    /// it ran.
    #[test]
    fn a_person_s_own_run_says_why_in_plain_words_never_the_argv() {
        let sys = console();
        sys.on(&argv(), Ran::exit(82, ""));
        let row = run(&sys, &missing(), Mode::Apply);
        assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
        assert_eq!(row.why.as_deref(), Some(Unfinished::Download.reason()));
    }

    /// In Low Data Mode the item asks no password and runs nothing: the download waits, and
    /// the row says how to go on; on a network without it the same Mac plans the install.
    #[test]
    fn low_data_mode_asks_no_password_and_says_how_to_go_on() {
        let mut sys = console();
        sys.low_data = Some(true);
        let item = missing();
        for mode in [Mode::Plan, Mode::Apply] {
            let row = run(&sys, &item, mode);
            assert_eq!(row.outcome, Outcome::Blocked, "{mode:?}: {row:?}");
            let why = row.why.unwrap_or_default();
            assert!(
                why.starts_with("no developer directory; Low Data Mode is on"),
                "{why}"
            );
        }
        assert!(asked(&sys).is_empty(), "{:?}", sys.calls());
        sys.low_data = Some(false);
        assert_eq!(run(&sys, &item, Mode::Plan).outcome, Outcome::Planned);
    }

    /// An account that is not one of the Mac's administrators asks for no password (macOS's
    /// `sudo` would refuse it, which read as a wrong password): the row says only an
    /// administrator can install them, and the tab says so as its own class.
    #[test]
    fn an_account_that_is_no_administrator_asks_no_password() {
        let mut sys = console();
        sys.admin = Some(false);
        let row = run(&sys, &missing(), Mode::Apply);
        assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
        assert!(
            row.why
                .as_deref()
                .is_some_and(|why| why.ends_with(NOT_ADMIN)),
            "{row:?}"
        );
        assert!(asked(&sys).is_empty(), "{:?}", sys.calls());

        let sys = Arc::new(sys);
        let (item, said, tab) = in_tab(&sys, Found::Missing("no developer directory".into()));
        let _ = run(&sys, &item, Mode::Apply);
        assert_eq!(tab.unfinished(), Some(Unfinished::NotAdmin));
        assert!(
            said.lock().unwrap().is_empty(),
            "nothing said before the end"
        );
        assert!(asked(&sys).is_empty(), "{:?}", sys.calls());

        let mut sys = console();
        sys.admin = Some(true);
        assert_eq!(run(&sys, &missing(), Mode::Plan).outcome, Outcome::Planned);
    }

    /// With an install of the tools under way (the placeholder held by `softwareupdate`, Apple's
    /// package `installer`, or a live run of the root half), the item asks no password and fails
    /// nothing: it is deferred to that install. A placeholder left behind beside other programs,
    /// one merely called `installer` included, is no install.
    #[test]
    fn an_install_under_way_asks_no_password_and_fails_nothing() {
        for installer in ["/usr/sbin/softwareupdate", "/usr/sbin/installer"] {
            let sys = console();
            sys.file(crate::items::developer::PLACEHOLDER, b"")
                .running(&["/sbin/launchd", installer]);
            let row = run(&sys, &missing(), Mode::Apply);
            assert_eq!(row.outcome, Outcome::Deferred, "{installer}: {row:?}");
            assert!(deferred(&row));
            assert!(
                row.why
                    .as_deref()
                    .is_some_and(|why| why.ends_with(INSTALLING))
            );
            assert!(asked(&sys).is_empty(), "{:?}", sys.calls());
        }

        let stale = console();
        stale
            .file(crate::items::developer::PLACEHOLDER, b"")
            .running(&["/sbin/launchd", "/tmp/installer"]);
        assert_eq!(
            run(&stale, &missing(), Mode::Plan).outcome,
            Outcome::Planned
        );

        // A run of the root half that is still alive holds it, though no installer process
        // runs this moment (it waits on Apple's installer window): a second `aterm init` defers
        // to it, asking no password.
        let held = console();
        held.file(crate::items::developer::PLACEHOLDER, b"aterm 4242 1000\n")
            .running(&["/sbin/launchd"])
            .born(4242, 1000);
        let row = run(&held, &missing(), Mode::Apply);
        assert_eq!(row.outcome, Outcome::Deferred, "{row:?}");
        assert!(asked(&held).is_empty(), "{:?}", held.calls());
    }

    /// Apple's installer window is Apple's own flow, at any age, with the placeholder beside it
    /// (it made the placeholder as it opened, before any press, so a press hours later leaves no
    /// trace aterm reads) or with none (it opened beside another install's, which has ended
    /// since): the item asks no password and fails nothing, deferred to that window with the
    /// person's step there named (finish in it, or quit it so aterm can install them), and the
    /// tab says it as the window's class.
    /// NEGATIVE CONTROL: the placeholder with no window beside it (left behind) plans the install.
    #[test]
    fn apples_window_defers_at_any_age_with_or_without_the_placeholder() {
        let apple = "/System/Library/CoreServices/Install Command Line Developer Tools.app/\
                     Contents/MacOS/Install Command Line Developer Tools";
        let placeholder = crate::items::developer::PLACEHOLDER;
        let day = 24 * 60 * 60;
        for beside in [true, false] {
            for age in [Some(60), Some(3 * 60 * 60), Some(10 * day), None] {
                let sys = Arc::new(console());
                if beside {
                    sys.file(placeholder, b"");
                }
                sys.running(&["/sbin/launchd", apple]);
                if let Some(age) = age {
                    sys.born(Fake::PIDS + 1, sys.now - age);
                }
                let row = run(&sys, &missing(), Mode::Apply);
                assert_eq!(row.outcome, Outcome::Deferred, "{beside} {age:?}: {row:?}");
                assert!(deferred(&row), "{beside} {age:?}");
                assert!(
                    row.why
                        .as_deref()
                        .is_some_and(|why| why.ends_with(APPLE_WINDOW_OPEN)),
                    "{beside} {age:?}: {row:?}"
                );
                assert!(
                    asked(&sys).is_empty(),
                    "{beside} {age:?}: {:?}",
                    sys.calls()
                );
                let (item, said, tab) =
                    in_tab(&sys, Found::Missing("no developer directory".into()));
                assert_eq!(
                    run(&sys, &item, Mode::Apply).outcome,
                    Outcome::Deferred,
                    "{beside} {age:?}"
                );
                assert_eq!(
                    tab.unfinished(),
                    Some(Unfinished::AppleWindow),
                    "{beside} {age:?}"
                );
                assert!(said.lock().unwrap().is_empty(), "{beside} {age:?}");
                assert!(
                    asked(&sys).is_empty(),
                    "{beside} {age:?}: {:?}",
                    sys.calls()
                );
            }
        }

        let left = console();
        left.file(placeholder, b"").running(&["/sbin/launchd"]);
        assert_eq!(run(&left, &missing(), Mode::Plan).outcome, Outcome::Planned);
    }

    /// A process the kernel names no executable for (one whose file a replacement deleted while
    /// it ran) is none of Apple's installers: beside a stale placeholder whose stamped run is
    /// gone, `aterm init` plans the install, as the root half then takes the placeholder's place.
    /// NEGATIVE CONTROL: with `softwareupdate` named beside it, the item defers to that install.
    #[test]
    fn a_process_the_kernel_names_no_file_for_defers_nothing() {
        let unnamed = crate::port::Process {
            pid: Fake::PIDS + 7,
            ppid: crate::port::Process::LAUNCHD,
            terminal: false,
            exe: None,
            comm: "deleted".into(),
        };
        for (beside, outcome) in [
            ("/sbin/launchd", Outcome::Planned),
            ("/usr/sbin/softwareupdate", Outcome::Deferred),
        ] {
            let sys = console();
            sys.file(crate::items::developer::PLACEHOLDER, b"aterm 4242 1000\n")
                .running(&["/sbin/launchd", beside])
                .process(unnamed.clone(), None);
            assert_eq!(
                run(&sys, &missing(), Mode::Plan).outcome,
                outcome,
                "{beside}"
            );
        }
    }

    /// Only a person at this Mac's console types the Mac password: an agent's tab (a terminal
    /// a coding agent started), a run with no terminal, and a remote shell each defer the tools,
    /// asking nothing, so the run fails nothing for them.
    #[test]
    fn off_the_console_the_tools_wait_for_a_person_and_nothing_is_asked() {
        let mut agent = console();
        agent.agent = Some("Claude Code");
        let mut script = console();
        script.terminal = false;
        script.tty = false;
        let mut remote = console();
        remote.remote_login = true;
        for (name, sys) in [("agent", agent), ("script", script), ("remote", remote)] {
            for mode in [Mode::Plan, Mode::Apply] {
                let row = run(&sys, &missing(), mode);
                assert_eq!(row.outcome, Outcome::Deferred, "{name} {mode:?}: {row:?}");
                assert!(deferred(&row), "{name}");
                assert!(
                    row.why
                        .as_deref()
                        .is_some_and(|why| why.contains("a person at this Mac's console")),
                    "{name}: {row:?}"
                );
            }
            assert!(asked(&sys).is_empty(), "{name}: {:?}", sys.calls());
        }
    }

    /// At the console with no terminal to ask in (no `/dev/tty`), the item is blocked on
    /// `aterm init --only base.clt` in a terminal, running nothing.
    #[test]
    fn without_a_terminal_the_item_is_blocked_on_init_only_base_clt() {
        let mut sys = console();
        sys.tty = false;
        let item = missing();
        for mode in [Mode::Plan, Mode::Apply] {
            let row = run(&sys, &item, mode);
            assert_eq!(row.outcome, Outcome::Blocked, "{mode:?}: {row:?}");
            assert!(
                row.why
                    .as_deref()
                    .is_some_and(|why| why.contains("`aterm init --only base.clt`")),
                "{row:?}"
            );
        }
        assert!(asked(&sys).is_empty(), "{:?}", sys.calls());
    }

    #[test]
    fn the_probe_maps_each_finding() {
        let sys = console();
        for (found, outcome) in [
            (Found::Present("cc".into()), Outcome::Ok),
            (Found::Refuses("license".into()), Outcome::Blocked),
            (Found::Unknown("no answer".into()), Outcome::Unknown),
            (Found::NotHere("not macOS".into()), Outcome::NotApplicable),
        ] {
            let row = run(&sys, &item(found.clone()), Mode::Apply);
            assert_eq!(row.outcome, outcome, "{found:?}: {row:?}");
        }
        assert!(asked(&sys).is_empty(), "{:?}", sys.calls());
    }

    type TabMessages = Arc<Mutex<Vec<(usize, String)>>>;

    /// The tab's item over a probe that finds the tools only once [`CC`] is there, speaking
    /// into `said`, each entry with how many calls `sys` had seen when it was said.
    fn in_tab(sys: &Arc<Fake>, found: Found) -> (CommandLineTools, TabMessages, Tab) {
        let said = Arc::new(Mutex::new(Vec::new()));
        let tab = Tab::new({
            let (said, sys) = (Arc::clone(&said), Arc::clone(sys));
            Arc::new(move |text: &str| {
                said.lock()
                    .unwrap()
                    .push((sys.calls().len(), text.to_owned()));
            })
        });
        (item(found).in_tab(tab.clone()), said, tab)
    }

    /// In the tab, the apply says its one intro line right before `sudo` asks for the Mac
    /// password with the plain prompt under `sudo -k`, as a run outside it does, runs the root
    /// half `--plain`, and holds off a stop until it ended.
    #[test]
    fn in_the_tab_the_intro_comes_right_before_sudo_asks_with_the_plain_prompt() {
        let sys = Arc::new(console());
        let (item, said, tab) = in_tab(&sys, Found::Missing("no developer directory".into()));
        let argv = argv();
        sys.on(&argv, Ran::ok(""))
            .on_write(&argv, std::path::Path::new(CC), b"");
        let row = run(&sys, &item, Mode::Apply);
        assert_eq!(row.outcome, Outcome::Changed, "{row:?} {:?}", sys.calls());
        let calls = sys.calls();
        let at = |call: &str| {
            calls
                .iter()
                .position(|c| c == call)
                .unwrap_or_else(|| panic!("{call}: {calls:?}"))
        };
        let ran = at(&argv);
        assert_eq!(
            *said.lock().unwrap(),
            [(ran, format!("{TAB_INTRO}\n"))],
            "said once, right before sudo ({ran}): {calls:?}"
        );
        assert!(at("catch stops") < ran, "{calls:?}");
        assert!(
            ran < at("stops restored"),
            "stops are held until the root half ended: {calls:?}"
        );
        assert!(
            !calls.iter().any(|call| call.contains("sudo -K")),
            "nothing cached to forget: {calls:?}"
        );
        assert_eq!(tab.unfinished(), None);
    }

    /// A run outside the tab, a person's own `aterm init` at the console and a run aterm decided
    /// alike, runs the root half in the person's terminal's foreground, so a Ctrl-C there (which
    /// the through-installer wait invites: "Ctrl-C stops waiting") reaches this run too. It is
    /// held until the root half ended and, in a run aterm decided, its ask was kept or taken back,
    /// then sent again: the run still stops as it would have, and the record says what the stop
    /// left it. A person's run writes no record at all.
    #[test]
    fn a_run_on_the_persons_terminal_holds_a_stop_until_the_root_half_ended_and_the_ask_settled() {
        let argv = argv();
        for decider in [crate::init::Decider::Person, crate::init::Decider::Aterm] {
            for status in [Some(0), Some(82)] {
                let mut sys = console();
                sys.stop_after = Some(0);
                sys.on(
                    &argv,
                    Ran {
                        status,
                        ..Ran::default()
                    },
                );
                let installs = status == Some(0);
                if installs {
                    sys.on_write(&argv, std::path::Path::new(CC), b"");
                }
                let row = run(&sys, &decided(decider), Mode::Apply);
                let calls = sys.calls();
                let at = |call: &str| calls.iter().position(|seen| seen == call);
                let ran = at(&argv).expect("the root half ran");
                let restored = at("stops restored").expect("stops restored");
                assert!(
                    at("defer stops").is_some_and(|held| held < ran),
                    "{decider:?}: no stop is held around the root half: {calls:?}"
                );
                assert!(restored > ran, "{decider:?}: {calls:?}");
                assert!(
                    calls
                        .iter()
                        .position(|seen| seen.starts_with("stop sent again"))
                        .is_some_and(|sent| sent > restored),
                    "{decider:?}: the stop is sent again once the run settled: {calls:?}"
                );
                // A stop during an install that did not finish is the person's: the ask stays
                // spent.
                assert_eq!(
                    row.outcome,
                    if installs {
                        Outcome::Changed
                    } else {
                        Outcome::Failed
                    },
                    "{decider:?} {status:?}: {row:?}"
                );
                if !installs {
                    assert_eq!(row.why.as_deref(), Some(Unfinished::Stopped.reason()));
                }
                if decider == crate::init::Decider::Aterm {
                    assert!(marked(&sys, BUILD), "{status:?}: {calls:?}");
                    assert!(
                        !calls.iter().any(|call| call == &format!("remove {MARK}")),
                        "{calls:?}"
                    );
                } else {
                    assert!(
                        !calls
                            .iter()
                            .any(|call| call.starts_with("write ") || call.starts_with("remove ")),
                        "a person's run writes no record: {calls:?}"
                    );
                }
            }
        }
    }

    /// This account's record of aterm's own ask, in the test world.
    const MARK: &str = "/Users//alice/Library/Application Support/aterm/command-line-tools-tab";

    /// The build that asks.
    const BUILD: &str = "0.9.0+gabc1234";

    /// The missing tools' item as `decider`'s run composes it, over [`MARK`] and [`BUILD`], the
    /// person's choices read through `holds` each time the item reads them; a person's run reads
    /// neither.
    fn decided_with(
        decider: crate::init::Decider,
        holds: impl Fn() -> crate::init::Holds + Send + Sync + 'static,
    ) -> CommandLineTools {
        decider
            .compose(
                crate::init::Base {
                    clt: Some(missing()),
                    ..crate::init::Base::none()
                },
                || Unbidden::new(Some(PathBuf::from(MARK)), BUILD.to_owned(), holds),
            )
            .clt
            .expect("base.clt")
    }

    /// [`decided_with`] where the person's choices hold nothing.
    fn decided(decider: crate::init::Decider) -> CommandLineTools {
        decided_with(decider, || crate::init::Holds::NONE)
    }

    /// The person's Stop of setup in System State.
    fn stop() -> crate::init::Holds {
        crate::init::Holds {
            stopped: Some("you stopped setup in System State".to_owned()),
            ..crate::init::Holds::NONE
        }
    }

    /// Whether the record names `build`.
    fn marked(sys: &Fake, build: &str) -> bool {
        aterm_types::clt_mark::opened_on_build(sys, std::path::Path::new(MARK), build)
    }

    /// In a run aterm decided (the follow-up of a person's own package install or update), the
    /// first run of a build asks once, claiming the build in the record before `sudo` asks; a
    /// second run of that build asks nothing, deferred naming the person's step, and so does a
    /// run after System State's own tab asked on this build; an update's build asks once more,
    /// keeping the earlier build's line.
    /// NEGATIVE CONTROL: a person's own run asks every time, writing no record.
    #[test]
    fn a_run_aterm_decided_asks_once_per_build_and_a_persons_run_every_time() {
        let sys = console();
        let argv = argv();
        // Apple's window closed without the tools: the person's own press there, which keeps
        // the ask spent.
        sys.on(&argv, Ran::exit(88, ""));
        let aterm = decided(crate::init::Decider::Aterm);

        let row = run(&sys, &aterm, Mode::Apply);
        assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
        let calls = sys.calls();
        let claimed = calls
            .iter()
            .position(|call| *call == format!("write {MARK} 600"))
            .unwrap_or_else(|| panic!("the ask is claimed: {calls:?}"));
        let ran = calls.iter().position(|call| *call == argv).unwrap();
        assert!(claimed < ran, "claimed before sudo asks: {calls:?}");
        assert_eq!(asked(&sys), std::slice::from_ref(&argv));
        assert!(marked(&sys, BUILD));

        for mode in [Mode::Plan, Mode::Apply] {
            let row = run(&sys, &aterm, mode);
            assert_eq!(row.outcome, Outcome::Deferred, "{mode:?}: {row:?}");
            assert!(deferred(&row));
            let why = row.why.unwrap_or_default();
            assert!(
                why.contains(ASKED_ON_THIS_BUILD) && why.ends_with(PERSONS_STEP),
                "{why}"
            );
        }
        assert_eq!(asked(&sys).len(), 1, "asked once: {:?}", sys.calls());

        // The tab's own ask on this build, claimed by the window, holds the follow-up's too.
        let tabbed = console();
        assert!(aterm_types::clt_mark::claim(&tabbed, std::path::Path::new(MARK), BUILD).unwrap());
        let row = run(&tabbed, &aterm, Mode::Apply);
        assert_eq!(row.outcome, Outcome::Deferred, "{row:?}");
        assert!(asked(&tabbed).is_empty(), "{:?}", tabbed.calls());

        let update = crate::init::Decider::Aterm.compose(
            crate::init::Base {
                clt: Some(missing()),
                ..crate::init::Base::none()
            },
            || {
                Unbidden::new(
                    Some(PathBuf::from(MARK)),
                    "0.9.1+gdef5678".to_owned(),
                    || crate::init::Holds::NONE,
                )
            },
        );
        let row = run(&sys, update.clt.as_ref().unwrap(), Mode::Apply);
        assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
        assert_eq!(asked(&sys).len(), 2, "an update's build asks once more");
        assert!(marked(&sys, "0.9.1+gdef5678") && marked(&sys, BUILD));

        let person = console();
        person.on(&argv, Ran::exit(88, ""));
        let typed = crate::init::Decider::Person.compose(
            crate::init::Base {
                clt: Some(missing()),
                ..crate::init::Base::none()
            },
            || panic!("a person's run reads no record and no holds"),
        );
        let typed = typed.clt.as_ref().unwrap();
        assert!(aterm_types::clt_mark::claim(&person, std::path::Path::new(MARK), BUILD).unwrap());
        for _ in 0..2 {
            assert_eq!(run(&person, typed, Mode::Apply).outcome, Outcome::Failed);
        }
        assert_eq!(asked(&person).len(), 2, "{:?}", person.calls());
        assert_eq!(
            person
                .calls()
                .iter()
                .filter(|call| call.starts_with("write ") || call.starts_with("remove "))
                .count(),
            1,
            "only the test's own record write: {:?}",
            person.calls()
        );
    }

    /// A run aterm decided asks for no password while the person's choices hold System State's
    /// own Command Line Tools tab (their Stop, setup records that cannot be read, a newer
    /// aterm's records) or where the record has no place or cannot be read: deferred, naming why
    /// and the person's step, writing nothing; a record that cannot say is named as such, never
    /// as this build's ask. A group turned off holds no tab, so it holds nothing here.
    #[test]
    fn a_run_aterm_decided_asks_nothing_under_a_stop() {
        let newer = crate::init::Holds {
            newer: true,
            ..crate::init::Holds::NONE
        };
        assert_eq!(held_by(&crate::init::Holds::NONE), None);
        assert_eq!(held_by(&newer).as_deref(), Some(NEWER));
        assert!(held_by(&stop()).is_some());
        let sys = console();
        sys.on(&argv(), Ran::exit(88, ""));
        let row = run(
            &sys,
            &decided_with(crate::init::Decider::Aterm, stop),
            Mode::Apply,
        );
        assert_eq!(row.outcome, Outcome::Deferred, "{row:?}");
        let why = row.why.unwrap_or_default();
        assert!(
            why.contains("because you stopped setup in System State")
                && why.ends_with(PERSONS_STEP),
            "{why}"
        );

        let nowhere = crate::init::Decider::Aterm.compose(
            crate::init::Base {
                clt: Some(missing()),
                ..crate::init::Base::none()
            },
            || Unbidden::new(None, BUILD.to_owned(), || crate::init::Holds::NONE),
        );
        let unread = console();
        unread.link(MARK, "/Users//alice/elsewhere");
        for (sys, item) in [
            (&sys, nowhere.clt.as_ref().unwrap()),
            (&unread, &decided(crate::init::Decider::Aterm)),
        ] {
            let row = run(sys, item, Mode::Apply);
            assert_eq!(row.outcome, Outcome::Deferred, "{row:?}");
            let why = row.why.unwrap_or_default();
            assert!(
                why.contains(UNREAD_RECORD)
                    && !why.contains(ASKED_ON_THIS_BUILD)
                    && why.ends_with(PERSONS_STEP),
                "{why}"
            );
        }
        let why = run(&unread, &decided(crate::init::Decider::Aterm), Mode::Plan)
            .why
            .unwrap_or_default();
        assert!(why.contains(MARK), "names the record it cannot read: {why}");

        for sys in [&sys, &unread] {
            assert!(asked(sys).is_empty(), "{:?}", sys.calls());
            assert!(
                !sys.calls()
                    .iter()
                    .any(|call| call.starts_with("write ") || call.starts_with("remove ")),
                "{:?}",
                sys.calls()
            );
        }
    }

    /// The person's Stop holds a run aterm decided whenever it is pressed before `sudo` asks:
    /// composing the run reads none of their choices, its probe reads them, so a Stop recorded
    /// after the run began (while the base layer or the vault's join ran) defers it, and its apply
    /// reads them once more right before it claims the ask, so a Stop recorded between the probe
    /// and the apply asks nothing either (the row fails in the Stop's words, naming the person's
    /// step). Neither asks or writes the record.
    /// NEGATIVE CONTROL: with no Stop at either read, the same run asks once.
    #[test]
    fn a_stop_pressed_after_the_run_began_holds_its_ask() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let reads = Arc::new(AtomicUsize::new(0));
        let now = Arc::new(Mutex::new(crate::init::Holds::NONE));
        let item = decided_with(crate::init::Decider::Aterm, {
            let (reads, now) = (Arc::clone(&reads), Arc::clone(&now));
            move || {
                reads.fetch_add(1, Ordering::SeqCst);
                now.lock().unwrap().clone()
            }
        });
        assert_eq!(reads.load(Ordering::SeqCst), 0, "composing reads no holds");
        *now.lock().unwrap() = stop();
        let sys = console();
        let row = run(&sys, &item, Mode::Apply);
        assert_eq!(row.outcome, Outcome::Deferred, "{row:?}");
        assert!(
            row.why
                .as_deref()
                .is_some_and(|why| why.contains("because you stopped setup in System State")),
            "{row:?}"
        );
        assert!(reads.load(Ordering::SeqCst) > 0);

        for stop_at_apply in [true, false] {
            let reads = Arc::new(AtomicUsize::new(0));
            let item = decided_with(crate::init::Decider::Aterm, {
                let reads = Arc::clone(&reads);
                move || {
                    // The probe's read finds no Stop; the apply's, the read after it, finds one.
                    if reads.fetch_add(1, Ordering::SeqCst) > 0 && stop_at_apply {
                        stop()
                    } else {
                        crate::init::Holds::NONE
                    }
                }
            });
            let sys = console();
            sys.on(&argv(), Ran::exit(88, ""));
            let row = run(&sys, &item, Mode::Apply);
            assert_eq!(row.outcome, Outcome::Failed, "{stop_at_apply}: {row:?}");
            assert_eq!(reads.load(Ordering::SeqCst), 2, "{stop_at_apply}");
            let writes = sys
                .calls()
                .into_iter()
                .filter(|call| call.starts_with("write ") || call.starts_with("remove "))
                .count();
            if stop_at_apply {
                let why = row.why.unwrap_or_default();
                assert!(
                    why.contains("because you stopped setup in System State")
                        && why.ends_with(PERSONS_STEP),
                    "{why}"
                );
                assert!(asked(&sys).is_empty(), "{:?}", sys.calls());
                assert_eq!(writes, 0, "{:?}", sys.calls());
            } else {
                assert_eq!(asked(&sys).len(), 1, "{:?}", sys.calls());
                assert!(marked(&sys, BUILD));
            }
        }
    }

    /// A run aterm decided and System State's tab of the same build claim one record: where the
    /// tab's landing claimed the build after this run's probe read the record, this run's claim
    /// right before `sudo` loses, so it asks nothing (the row fails naming this build's ask and
    /// the person's step) and never takes back the tab's line, which it did not add.
    /// NEGATIVE CONTROL: with the record left alone, the same run's claim wins and it asks.
    #[test]
    fn a_run_whose_claim_lost_to_the_tab_asks_nothing_and_keeps_the_tabs_line() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        for tab_lands in [true, false] {
            let sys = Arc::new(console());
            sys.on(&argv(), Ran::exit(82, ""));
            let reads = Arc::new(AtomicUsize::new(0));
            let item = decided_with(crate::init::Decider::Aterm, {
                let (reads, sys) = (Arc::clone(&reads), Arc::clone(&sys));
                move || {
                    // The tab lands between this run's probe and its apply: the apply's read of the
                    // holds, the last before its claim, is when.
                    if reads.fetch_add(1, Ordering::SeqCst) == 1 && tab_lands {
                        assert!(
                            aterm_types::clt_mark::claim(&*sys, std::path::Path::new(MARK), BUILD)
                                .unwrap()
                        );
                    }
                    crate::init::Holds::NONE
                }
            });
            let row = run(&sys, &item, Mode::Apply);
            assert_eq!(row.outcome, Outcome::Failed, "{tab_lands}: {row:?}");
            let calls = sys.calls();
            if tab_lands {
                let why = row.why.unwrap_or_default();
                assert!(
                    why.contains(ASKED_ON_THIS_BUILD) && why.ends_with(PERSONS_STEP),
                    "{why}"
                );
                assert!(asked(&sys).is_empty(), "{calls:?}");
                assert!(marked(&sys, BUILD), "the tab's line stays: {calls:?}");
                assert_eq!(
                    calls
                        .iter()
                        .filter(|call| call.starts_with("write ") || call.starts_with("remove "))
                        .collect::<Vec<_>>(),
                    [&format!("write {MARK} 600")],
                    "the tab's write alone: {calls:?}"
                );
            } else {
                assert_eq!(asked(&sys).len(), 1, "{calls:?}");
                assert!(
                    !marked(&sys, BUILD),
                    "a failed install takes its own line back: {calls:?}"
                );
            }
        }
    }

    /// A run aterm decided that installed nothing, where no person declined it (a password macOS
    /// refused, a broken download, a machine's failure), takes its build's line back, so the next
    /// run of that build asks again; a stop the person sent, Apple's window closed without them
    /// and the root half's own stop keep it, as the window reads the tab's exits
    /// ([`Unfinished::keeps_the_ask`]).
    #[test]
    fn a_failed_install_takes_the_ask_back_and_a_decline_keeps_it() {
        for (status, stop, keeps) in [
            (82, false, false),
            (83, false, false),
            (1, false, false),
            (1, true, true),
            (85, false, true),
            (88, false, true),
        ] {
            let mut sys = console();
            if stop {
                sys.stop_after = Some(0);
            }
            sys.on(&argv(), Ran::exit(status, ""));
            let aterm = decided(crate::init::Decider::Aterm);
            let row = run(&sys, &aterm, Mode::Apply);
            assert_eq!(row.outcome, Outcome::Failed, "{status}: {row:?}");
            assert_eq!(
                marked(&sys, BUILD),
                keeps,
                "{status} {stop}: {:?}",
                sys.calls()
            );
            let calls = sys.calls();
            if !keeps {
                let at = |call: &str| calls.iter().position(|seen| seen == call).unwrap();
                assert!(
                    at(&argv()) < at(&format!("remove {MARK}"))
                        && at(&format!("remove {MARK}")) < at("stops restored"),
                    "taken back once the root half ended, before a stop is sent again: {calls:?}"
                );
                assert_eq!(
                    run(&sys, &aterm, Mode::Plan).outcome,
                    Outcome::Planned,
                    "the next run of the build asks again"
                );
            }
        }
    }

    /// The asks that stay spent are the tab's exits the window keeps its opening on.
    #[test]
    fn the_asks_that_stay_spent_are_the_tab_exits_the_window_keeps() {
        for why in Unfinished::ALL {
            assert_eq!(
                why.keeps_the_ask(),
                [
                    aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_STOPPED,
                    aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_WINDOW_CLOSED,
                    aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_NOT_ADMIN,
                ]
                .contains(&why.tab_exit()),
                "{why:?}"
            );
        }
    }

    /// `sudo`'s exit says why the tab's install did not finish: 0 done, `sudo`'s own 1 the Mac
    /// password, each root class's code that class, a signal after a stop the person sent
    /// stopped, and anything else (a usage 2, a panic's 101, a failed exec's 126 or 127, a
    /// signal nobody sent) the other class. The apply's error is the plain reason alone, never
    /// the argv or a path.
    #[test]
    fn the_root_halfs_exit_says_why_the_tab_install_did_not_finish() {
        assert_eq!(Unfinished::after_root(Some(0)), Ok(()));
        assert_eq!(Unfinished::after_root(Some(1)), Err(Unfinished::Password));
        for (code, why) in [
            (80, Unfinished::Catalog),
            (81, Unfinished::NoneOffered),
            (82, Unfinished::Download),
            (83, Unfinished::Installer),
            (84, Unfinished::Busy),
            (85, Unfinished::Stopped),
            (86, Unfinished::AppleWindow),
            (87, Unfinished::Other),
            (88, Unfinished::WindowClosed),
        ] {
            assert_eq!(why.code(), Some(code), "{why:?}");
            assert_eq!(
                Unfinished::after_root(Some(i32::from(code))),
                Err(why),
                "{code}"
            );
        }
        for code in [
            Some(2),
            Some(79),
            Some(89),
            Some(101),
            Some(126),
            Some(127),
            Some(-1),
            None,
        ] {
            assert_eq!(
                Unfinished::after_root(code),
                Err(Unfinished::Other),
                "{code:?}"
            );
        }

        for (status, stopped, why) in [
            (Some(1), false, Unfinished::Password),
            (Some(84), false, Unfinished::Busy),
            (Some(82), false, Unfinished::Download),
            (None, false, Unfinished::Other),
            (None, true, Unfinished::Stopped),
            // A Ctrl-C at `sudo`'s password prompt: `sudo` ends on its own 1.
            (Some(1), true, Unfinished::Stopped),
            (Some(82), true, Unfinished::Stopped),
        ] {
            let mut fake = console();
            if stopped {
                fake.stop_after = Some(0);
            }
            let sys = Arc::new(fake);
            let (item, said, tab) = in_tab(&sys, Found::Missing("no developer directory".into()));
            sys.on(
                &argv(),
                Ran {
                    status,
                    ..Ran::default()
                },
            );
            let row = run(&sys, &item, Mode::Apply);
            assert_eq!(row.outcome, Outcome::Failed, "{status:?}: {row:?}");
            assert_eq!(row.why.as_deref(), Some(why.reason()), "{status:?}");
            assert_eq!(tab.unfinished(), Some(why), "{status:?}");
            assert_eq!(said.lock().unwrap().len(), 1, "the intro alone: {status:?}");
        }
    }

    /// A stop the person sent while the root half ran (a Ctrl-C in the tab, which reaches the
    /// root half too) leaves the tab's run waiting for it: the root half finishes its step, and
    /// tools that arrived anyway end the tab on installed, whatever its exit said.
    #[test]
    fn in_the_tab_a_stop_waits_for_the_root_half_and_tools_that_arrived_are_installed() {
        let mut fake = console();
        fake.stop_after = Some(0);
        let sys = Arc::new(fake);
        let (item, _, tab) = in_tab(&sys, Found::Missing("no developer directory".into()));
        let argv = argv();
        sys.on(
            &argv,
            Ran {
                status: None,
                ..Ran::default()
            },
        )
        .on_write(&argv, std::path::Path::new(CC), b"");
        let row = run(&sys, &item, Mode::Apply);
        assert_eq!(row.outcome, Outcome::Changed, "{row:?} {:?}", sys.calls());
        assert_eq!(tab.unfinished(), None);
    }

    /// Every reason is a plain clause a person reads in the tab (no argv, path, item id or
    /// engine word); each line is `The install did not finish: <reason>. Install… on System
    /// State tries again.` except Low Data Mode's and a non-administrator's, which say what
    /// lifts them, with their own tab exit; and each root code is its class's own, clear of the
    /// codes `sudo`, a usage error, a panic and a failed exec end on.
    #[test]
    fn each_reason_is_plain_words_and_each_root_code_is_its_own() {
        let mut codes = Vec::new();
        for why in Unfinished::ALL {
            let reason = why.reason();
            for word in [
                "/",
                "`",
                "base.clt",
                "baseline",
                "journal",
                "fleet",
                "sudo",
                "root",
                "exit",
                "argv",
                "aterm machines",
            ] {
                assert!(!reason.contains(word), "{why:?}: {reason}");
                assert!(!why.line().contains(word), "{why:?}: {}", why.line());
            }
            assert!(!reason.ends_with('.'), "{why:?}");
            match why {
                Unfinished::LowData => {
                    assert_eq!(why.line(), TAB_LOW_DATA);
                    assert_eq!(
                        why.tab_exit(),
                        aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_LOW_DATA
                    );
                }
                Unfinished::NotAdmin => {
                    assert_eq!(why.line(), TAB_NOT_ADMIN);
                    assert_eq!(
                        why.tab_exit(),
                        aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_NOT_ADMIN
                    );
                }
                Unfinished::Stopped | Unfinished::WindowClosed => {
                    assert_eq!(
                        why.line(),
                        format!(
                            "The install did not finish: {reason}. Install\u{2026} on System State tries again."
                        ),
                    );
                    assert_eq!(
                        why.tab_exit(),
                        if why == Unfinished::Stopped {
                            aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_STOPPED
                        } else {
                            aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_WINDOW_CLOSED
                        }
                    );
                }
                _ => {
                    assert_eq!(
                        why.line(),
                        format!(
                            "The install did not finish: {reason}. Install\u{2026} on System State tries again."
                        ),
                    );
                    assert_eq!(why.tab_exit(), 1, "{why:?}");
                }
            }
            if let Some(code) = why.code() {
                assert!(![0, 1, 2, 101, 126, 127].contains(&code), "{why:?}");
                assert!(!codes.contains(&code), "{why:?}");
                codes.push(code);
            }
        }
        assert_eq!(codes.len(), 9);
        assert!(TAB_LOW_DATA.contains("Low Data Mode") && TAB_LOW_DATA.contains("Network"));
        assert!(TAB_NOT_ADMIN.contains("administrator"));
        for line in [TAB_INTRO, TAB_INSTALLED] {
            assert!(!line.contains('/') && !line.contains('`'), "{line}");
        }
    }

    /// In the tab, what the probe finds that no install fixes is noted as its class: tools that
    /// refuse to build, a probe that could not tell, and no terminal to ask in. Nothing runs.
    #[test]
    fn in_the_tab_the_probe_notes_what_no_install_fixes() {
        for (found, why) in [
            (Found::Refuses("license".into()), Unfinished::Refuses),
            (Found::Unknown("no answer".into()), Unfinished::Unread),
            (
                Found::Missing("no developer directory".into()),
                Unfinished::Other,
            ),
        ] {
            let mut fake = console();
            fake.tty = false;
            let sys = Arc::new(fake);
            let (item, said, tab) = in_tab(&sys, found.clone());
            let row = run(&sys, &item, Mode::Apply);
            assert!(
                row.outcome.is_failure() || row.outcome == Outcome::Blocked,
                "{row:?}"
            );
            assert_eq!(tab.unfinished(), Some(why), "{found:?}");
            assert!(said.lock().unwrap().is_empty());
            assert!(asked(&sys).is_empty(), "{:?}", sys.calls());
        }
    }
}
