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

/// Why the install the Mac Setup tab runs did not finish
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
    /// ([`crate::items::developer::Holder::AppleWindow`]), or the root half's wait on that
    /// existing window ran out with it still open. aterm opens no Apple installer window.
    AppleWindow,
    /// Apple's own installer window, which the root half waited on, closed without installing
    /// them, with no installer running: a person pressed Not Now or Cancel there or dismissed
    /// Apple's own failure (each unlinks the placeholder), or quit the window before pressing
    /// Install (which leaves the placeholder aterm stamped beside it).
    WindowClosed,
    /// The tools already on this Mac refuse to build ([`Found::Refuses`]).
    Refuses,
    /// Tool state or live installer custody could not be verified ([`Found::Unknown`]).
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
            Self::Unread => Some(89),
            Self::Password | Self::Refuses | Self::NotOwnLogin | Self::LowData | Self::NotAdmin => {
                None
            }
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

    /// The plain reason the tab says after `Mac Setup did not finish:`.
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
            Self::Unread => "aterm could not verify the developer tools or their installer",
            Self::NotOwnLogin => "it runs only in your own Mac login",
            Self::LowData => "Low Data Mode is on, so the download waits",
            Self::NotAdmin => "only an administrator of this Mac can install them",
            Self::Other => "something unexpected went wrong",
        }
    }

    /// The tab's last line for this class, naming the reason without prescribing another
    /// install attempt. Low Data Mode says how to lift it
    /// ([`TAB_LOW_DATA`]), and an account that is no administrator's who must install them
    /// ([`TAB_NOT_ADMIN`]).
    #[must_use]
    pub fn line(self) -> String {
        match self {
            Self::LowData => TAB_LOW_DATA.to_owned(),
            Self::NotAdmin => TAB_NOT_ADMIN.to_owned(),
            _ => format!(
                "{}: {}.",
                aterm_types::macos_setup::COMMAND_LINE_TOOLS_NOT_FINISHED,
                self.reason()
            ),
        }
    }
}

/// The tab's last line while Low Data Mode holds the download back ([`Unfinished::LowData`]).
pub const TAB_LOW_DATA: &str = "Low Data Mode is on, so the download waits. Turn it off in \
                                System Settings \u{203a} Network; setup resumes automatically.";

/// The tab's last line in an account that is no administrator's ([`Unfinished::NotAdmin`]).
pub const TAB_NOT_ADMIN: &str = "The remaining Mac setup needs an administrator of this Mac; \
                                 this account is not one.";

/// The one line the tab says before macOS asks for the password, with the tools missing.
pub const TAB_INTRO: &str = "aterm is installing Apple's Command Line Tools for building software, \
                             turning on developer mode, and setting up its local network helper. \
                             macOS asks for your Mac password once.";

/// [`TAB_INTRO`] on a Mac that has the tools, where the elevation runs for its other root steps
/// ([`crate::root_steps::Owed`]).
pub const TAB_INTRO_SETUP: &str = "aterm is finishing the part of this Mac's setup only an \
                                   administrator can do: developer mode for debuggers, the Mac's \
                                   name, and its local network helper. macOS asks for your Mac \
                                   password once.";

/// The one line a run outside the tab says right before `sudo` asks for the Mac password, so the
/// prompt never comes bare after whatever the terminal showed last (a package install's output).
pub const INTRO: &str = "aterm init: installing Apple's Command Line Tools (git, cc, the macOS \
                         SDK), turning on developer mode and setting up the local network \
                         helper; sudo asks for your Mac password once";

/// [`INTRO`] on a Mac that has the tools.
pub const INTRO_SETUP: &str = "aterm init: finishing the administrator's part of setup \
                               (developer mode, the Mac's name, the local network helper); sudo \
                               asks for your Mac password once";

/// The password prompt (`sudo -p`), in the tab and outside it.
pub const TAB_PROMPT: &str = aterm_types::macos_setup::MAC_SETUP_PASSWORD_PROMPT;

/// The tab's last line once the tools are installed and the rest of the elevation ran; the
/// window then closes the tab unless it is its window's last (`aterm-gui`'s
/// `App::developer_tools_install_ended`).
pub const TAB_INSTALLED: &str =
    "Successfully completed: Apple's Command Line Tools are in place. This tab closes by itself.";

/// Where the tab's own words go (its terminal), and why its install did not finish, once the
/// item knows: what [`CommandLineTools::in_tab`] speaks through.
#[derive(Clone)]
pub struct Tab {
    say: Arc<dyn Fn(&str) + Send + Sync>,
    unfinished: Arc<Mutex<Option<Unfinished>>>,
    entered: Arc<std::sync::atomic::AtomicBool>,
}

impl Tab {
    /// A tab that writes its words with `say` (each as given: a line carries its own newline).
    #[must_use]
    pub fn new(say: Arc<dyn Fn(&str) + Send + Sync>) -> Self {
        Self {
            say,
            unfinished: Arc::default(),
            entered: Arc::default(),
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

    /// Preserve preprompt-only exit meaning even if an engine readback changes its diagnosis.
    pub(crate) fn exit(&self, why: Unfinished) -> u8 {
        let entered = self.entered.load(std::sync::atomic::Ordering::Relaxed);
        match why {
            Unfinished::NotOwnLogin if !entered => {
                aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_NOT_OWN_LOGIN
            }
            Unfinished::Busy | Unfinished::AppleWindow if !entered => {
                aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_INSTALLER_HELD
            }
            Unfinished::LowData if entered => 1,
            _ => why.tab_exit(),
        }
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
    /// System State's Mac Setup tab, when this run is its program: plain words, a
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
    /// What the elevation still owes this Mac beside the tools ([`crate::root_steps::owed`]),
    /// read as the probe runs: on a Mac that has the tools, a run that may ask still asks for the
    /// one password where anything is owed ([`Self::with_owed`]).
    owed: Owing,
    /// Whether this run's root half ran: what it left owed then goes to the journal, and the
    /// row's read-back says the tools alone.
    asked: std::sync::atomic::AtomicBool,
    root_plan: Mutex<RootPlan>,
}

/// The compiled root work admitted by the functional developer-tools probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootPlan {
    /// Missing tools: install them along with the independent root steps.
    WithTools,
    /// Preserve developer-tool selection and run only the independent root steps.
    StepsOnly,
}

/// The reader of what the elevation still owes ([`CommandLineTools::with_owed`]).
pub type Owing = Box<dyn Fn(&dyn System) -> Vec<crate::root_steps::Owed> + Send + Sync>;

/// `word` when it is a name the root half gives the Mac: a bare swarm label, `m` and a number
/// with no leading zero (`m42`), which is also a DNS label and a fleet name. Anything else,
/// `Mac-mini-M4`, `m42-studio` or `M42` included, is none: the root half takes no free text from
/// the person's account.
#[must_use]
pub fn plain_name(word: &str) -> Option<String> {
    aterm_types::machine_name::Number::parse(word)
        .ok()
        .map(|number| number.to_string())
        .filter(|canonical| canonical == word)
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
            owed: Box::new(|_| Vec::new()),
            asked: std::sync::atomic::AtomicBool::new(false),
            root_plan: Mutex::new(RootPlan::WithTools),
        }
    }

    /// This item reading what the elevation still owes this Mac beside the tools with `owed`
    /// ([`crate::root_steps::owed_here`] on a Mac): where anything is, a Mac that has the tools
    /// still gets the one password, from a run that may ask for it.
    #[must_use]
    pub fn with_owed(self, owed: Owing) -> Self {
        Self { owed, ..self }
    }

    /// This item naming the Mac by what `name` reads as its root half is about to run, where
    /// that is a [`plain_name`]; anything else (or none) names nothing.
    #[must_use]
    pub fn with_name(self, name: NameMac) -> Self {
        Self { name, ..self }
    }

    /// This item as the Mac Setup tab's program speaks it, through `tab`.
    #[must_use]
    pub fn in_tab(self, tab: Tab) -> Self {
        Self {
            tab: Some(tab),
            ..self
        }
    }

    /// This item in a run aterm decided ([`crate::init::Decider::Aterm`]: the follow-up of a
    /// person's own package install or update), held as `unbidden` says: it asks for the Mac
    /// password automatically once per account episode, sharing System State's Mac Setup tab's ask
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

    /// Record an explicit person attempt in the shared episode without withholding their retry.
    #[must_use]
    pub(crate) fn explicit(self, mut record: Unbidden) -> Self {
        record.explicit = true;
        Self {
            unbidden: Some(record),
            ..self
        }
    }

    /// The baseline the root half applies.
    #[must_use]
    pub fn baseline() -> String {
        Self::baseline_named(RootPlan::WithTools, None)
    }

    /// [`Self::baseline`], with the Mac's Computer Name, Local hostname and host name set to
    /// `name` where it is a [`plain_name`] (`--name`): the one password that installs the tools
    /// also gives the Mac the name its vault and fleet know it by. Every run of it turns on
    /// developer mode ([`crate::root_steps::DeveloperMode`]), puts the rule in place that names
    /// the Mac at an admission that comes later ([`crate::root_steps::NameRule`]), and installs
    /// the root LAN broker ([`crate::items::lan::LanBroker`]) before the tools' download, so the
    /// one password does every root step setup has. [`RootPlan::StepsOnly`] omits CLT
    /// installation and selection while retaining these independent steps.
    #[must_use]
    pub fn baseline_named(plan: RootPlan, name: Option<&str>) -> String {
        let mut text = format!(
            "schema = {}\n\n[developer]\ncommand_line_tools = {}\ndebugger = true\n\n\
             [lan]\nbroker = true\n\n[names]\nadmitted_name_rule = true\n",
            crate::SCHEMA,
            plan == RootPlan::WithTools
        );
        if let Some(name) = name.and_then(plain_name) {
            text.push_str(&format!(
                "computer_name = \"{name}\"\nlocal_host_name = \"{name}\"\nhost_name = \"{name}\"\n"
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
        self.command_for(self.root_plan())
    }

    fn root_plan(&self) -> RootPlan {
        *self
            .root_plan
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Root work is re-read before launch. Independent work never acquires installation
    /// authority from a later reading and no longer owed work needs no password episode;
    /// a new compiler failure is not missing tools.
    fn apply_plan(&self, sys: &dyn System) -> Result<Option<RootPlan>, (String, Unfinished)> {
        self.plan_after(sys, self.root_plan())
    }

    /// Re-read work within the plan this apply already admitted, even if another probe
    /// changes the item's shared plan while this run waits for the terminal.
    fn plan_after(
        &self,
        sys: &dyn System,
        ceiling: RootPlan,
    ) -> Result<Option<RootPlan>, (String, Unfinished)> {
        if ceiling == RootPlan::StepsOnly {
            return Ok((!(self.owed)(sys).is_empty()).then_some(RootPlan::StepsOnly));
        }
        let (why, unfinished) = match (self.found)(sys) {
            Found::Missing(_) => return Ok(Some(RootPlan::WithTools)),
            Found::Present(_) => {
                return Ok((!(self.owed)(sys).is_empty()).then_some(RootPlan::StepsOnly));
            }
            Found::NotHere(_) => return Ok(None),
            Found::Refuses(why) => (why, Unfinished::Refuses),
            Found::Unknown(why) => (why, Unfinished::Unread),
        };
        if (self.owed)(sys).is_empty() {
            Err((why, unfinished))
        } else {
            Ok(Some(RootPlan::StepsOnly))
        }
    }

    fn command_for(&self, plan: RootPlan) -> Cmd {
        let root = Cmd::new(SUDO)
            .args(["-k", "-p", TAB_PROMPT])
            .arg(self.aterm.as_os_str())
            .args(["machines", "here", "apply", "--plain"]);
        let root = if plan == RootPlan::StepsOnly {
            root.arg("--root-steps-only")
        } else {
            root
        };
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
    /// said, where the elevation owes nothing else ([`Self::with_owed`]): on a Mac that had the
    /// tools all along, a refused password is no success.
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
            Err(_)
                if matches!((self.found)(sys), Found::Present(_))
                    && (self.owed)(sys).is_empty()
                    && crate::items::lan::ready(sys).unwrap_or(false) =>
            {
                Ok(())
            }
            ended => ended,
        }
    }

    /// The line said right before `sudo` asks: [`TAB_INTRO`] or [`INTRO`] while the tools are
    /// missing, else the setup's own ([`TAB_INTRO_SETUP`], [`INTRO_SETUP`]).
    fn intro(in_tab: bool, plan: RootPlan) -> &'static str {
        match (plan == RootPlan::StepsOnly, in_tab) {
            (true, true) => TAB_INTRO_SETUP,
            (true, false) => INTRO_SETUP,
            (_, true) => TAB_INTRO,
            (_, false) => INTRO,
        }
    }
}

/// What holds back the ask of a run aterm decided ([`CommandLineTools::unbidden`]): this
/// account's record of aterm's own ask, which System State's Mac Setup tab reads and
/// writes too ([`aterm_types::clt_mark`]), the build that asks, and the person's choices in
/// System State that hold that tab ([`held_by`]), read from the setup records as the item's probe
/// runs and again right before its apply claims the ask, so a Stop pressed after `aterm init`
/// began, while the base layer or the vault's join ran, still holds it (principle 4).
pub struct Unbidden {
    /// The record ([`aterm_types::clt_mark::path`]); `None` where it has no place, which holds
    /// the ask as a record naming this build does.
    mark: Option<PathBuf>,
    /// The build that asks ([`aterm_stamp::build_id`]).
    build: String,
    /// An explicit person retry records its attempt but never borrows an automatic hold.
    explicit: bool,
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
            explicit: false,
            holds: Arc::new(holds),
        }
    }

    /// This account's record and this build, held as `records` say each time they are read
    /// ([`held_by`]).
    #[must_use]
    pub fn here(records: &'static dyn crate::init::Records) -> Self {
        Self::new(
            aterm_types::clt_mark::path(),
            aterm_stamp::build_id(),
            move || records.holds(),
        )
    }

    /// Why the person's choices hold the ask now, naming their step, as the setup records read
    /// at this call ([`held_by`]).
    fn held(&self) -> Option<String> {
        if self.explicit {
            return None;
        }
        held_by(&(self.holds)()).map(|why| {
            format!("aterm asks for no Mac password by itself because {why}: {PERSONS_STEP}")
        })
    }

    /// Why a run aterm decided asks no password by itself now, naming the person's step, or
    /// `None` when it may ask: the person's choices hold it ([`Self::held`]), an earlier attempt consumed
    /// the episode (its tab, or an earlier run aterm decided: [`ASK_ALREADY_SPENT`]), or the
    /// record has no place or cannot say whether it did ([`UNREAD_RECORD`]), as the record reads
    /// through `sys` ([`aterm_types::clt_mark::reading`]).
    fn withheld(&self, sys: &dyn System) -> Option<String> {
        if self.explicit {
            return None;
        }
        if let Some(why) = self.held() {
            return Some(why);
        }
        let Some(mark) = self.mark.as_deref() else {
            return Some(format!(
                "{UNREAD_RECORD}, which has no place in this account, so it asks for no Mac \
                 password by itself: {PERSONS_STEP}"
            ));
        };
        match aterm_types::clt_mark::reading(sys.clt_mark(), mark) {
            aterm_types::clt_mark::Asked::Available => None,
            aterm_types::clt_mark::Asked::Spent => {
                Some(format!("{ASK_ALREADY_SPENT}: {PERSONS_STEP}"))
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
        if self.explicit {
            aterm_types::clt_mark::explicit(sys.clt_mark(), mark, &self.build)
        } else {
            aterm_types::clt_mark::claim(sys.clt_mark(), mark, &self.build)
        }
    }

    /// Release this run's exact claim after the port proves sudo never started. An unreadable
    /// record retains the hold and reports the release failure in the journal.
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
/// Mac password, as they hold System State's own Mac Setup tab: their Stop of setup, or
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

/// The person's explicit retry routes when setup asks no password by itself. The terminal
/// route also covers owed root steps after the tools are installed.
pub const PERSONS_STEP: &str = "run `aterm init --only base.clt` in a terminal, or use \
                                Install Command Line Tools\u{2026} in System State when shown";

/// Why a run aterm decided asks no password where this build asked by itself already: its tab,
/// or an earlier run aterm decided ([`aterm_types::clt_mark`]).
pub const ASK_ALREADY_SPENT: &str = "aterm has already asked for your Mac password for setup; \
                                       updates do not ask again automatically";

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

/// Why pending root work cannot ask for a password from here.
pub const NO_TTY: &str = "the remaining Mac setup needs an administrator password, and this \
                          run has no terminal to ask it in";

/// Why the item leaves the tools to an install under way ([`held`]).
pub const INSTALLING: &str = "Apple's Command Line Tools are being installed now, by an install \
                              already under way";

/// Why the item asks nobody for a password in an account that is no administrator's.
pub const NOT_ADMIN: &str = "the remaining Mac setup needs an administrator of this Mac, and \
                             this account is not one";

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
/// unlinks it when its request ends), or nothing. Unread placeholder or process-table state
/// remains unknown unless independent evidence proves custody.
fn held(sys: &dyn System) -> crate::items::developer::Holder {
    crate::items::developer::holder(sys, crate::items::developer::placeholder_state(sys))
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
        let Some(plan) = self.apply_plan(sys).map_err(|(why, _)| why)? else {
            return Ok(());
        };
        if !sys.has_tty() {
            return Err(NO_TTY.to_owned());
        }
        // Persist the episode before sudo can ask. Explicit retries bypass its withholding
        // decision, but consume the same record so a later update never retries unsolicited.
        let mut claimed = false;
        let mut skipped = false;
        let mut plan_failure = None;
        // The root half runs in this terminal's foreground, so a Ctrl-C (which the
        // through-installer wait invites) or a closed terminal reaches this run too: held until
        // the root half has finished its step and aterm's own ask is kept or taken back, then
        // sent again, so the run still stops as it would have.
        let stops = sys.defer_stops();
        let ran = sys.run_tty(&self.command_for(plan), &mut |command| {
            // A vault question can own the terminal first. Re-read work under that lease,
            // then Stop and the current holds after those reads, before claiming the ask.
            let fresh = self.plan_after(sys, plan);
            if let Ok(Some(plan)) = &fresh {
                *command = self.command_for(*plan);
            }
            let admission = fresh.as_ref().ok().and_then(|plan| {
                plan.and_then(|plan| self.admission_before_ask(sys, plan))
            });
            if sys.stopped().is_some() {
                return Err(crate::port::RunFailed::Refused(
                    "setup was stopped before the root command started".into(),
                ));
            }
            if let Some(why) = self.unbidden.as_ref().and_then(Unbidden::held) {
                return Err(crate::port::RunFailed::Refused(why));
            }
            let plan = match fresh {
                Ok(Some(plan)) => plan,
                Ok(None) => {
                    skipped = true;
                    return Err(crate::port::RunFailed::Refused(
                        "the queued root work is no longer owed".into(),
                    ));
                }
                Err((why, _)) => {
                    plan_failure = Some(why.clone());
                    return Err(crate::port::RunFailed::Refused(why));
                }
            };
            if let Some((why, _)) = admission {
                plan_failure = Some(why.clone());
                return Err(crate::port::RunFailed::Refused(why));
            }
            if let Some(record) = &self.unbidden {
                match record.claim(sys) {
                    Ok(added) => {
                        claimed = added;
                        if !added && !record.explicit {
                            return Err(crate::port::RunFailed::Refused(format!(
                                "{ASK_ALREADY_SPENT}: {PERSONS_STEP}"
                            )));
                        }
                    }
                    Err(error) if record.explicit => {
                        aterm_log::warn!(
                            "aterm init: could not record the explicit administrator attempt: {error}"
                        );
                    }
                    Err(error) => {
                        return Err(crate::port::RunFailed::Refused(format!(
                            "{NOT_NOTED} ({error}): {PERSONS_STEP}"
                        )));
                    }
                }
            }
            (self.say)(&format!("{}\n", Self::intro(false, plan)));
            self.asked.store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        });
        if skipped {
            drop(stops);
            return Ok(());
        }
        if let Some(why) = plan_failure {
            drop(stops);
            return Err(why);
        }
        let ended = self.ended(sys, &ran, sys.stopped());
        // Only these port results prove sudo never existed. A password refusal, installer
        // failure, timeout or lost result may follow human interaction and keeps the episode.
        if claimed
            && matches!(
                ran,
                Err(crate::port::RunFailed::Spawn(_) | crate::port::RunFailed::Refused(_))
            )
            && let Some(record) = &self.unbidden
        {
            record.forget(sys);
        }
        drop(stops);
        match (ended, ran) {
            (Ok(()), _) => Ok(()),
            (Err(_), Err(e)) => Err(format!("administrator setup command failed: {e}")),
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
    /// the change. With the tools present, the change is the elevation's other root steps where
    /// any is owed ([`Self::with_owed`]) and this run may ask for the password now: in the tab,
    /// a run that may not says why as it would for the tools (Low Data Mode and an install under
    /// way aside, which hold only a download); outside it, the row reads the tools in place and
    /// names the steps left for setup's one password, so no pass blocks on them. Once this run's
    /// root half ran, its read-back says the tools alone: what that left owed is in the journal.
    /// A refusing or unanswered compiler also defers to confirmed installer custody; without
    /// that custody known independent root work may proceed once, while read-back retains
    /// the compiler's blocked or unknown classification.
    fn read(&self, sys: &dyn System) -> (Probe, Option<Unfinished>) {
        let found = (self.found)(sys);
        *self
            .root_plan
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = if matches!(&found, Found::Missing(_)) {
            RootPlan::WithTools
        } else {
            RootPlan::StepsOnly
        };
        let (from, unready) = match found {
            Found::Present(now) => return self.read_root_steps(sys, now),
            Found::Refuses(why) => (why, Some(Unfinished::Refuses)),
            Found::Unknown(why) => (why, Some(Unfinished::Unread)),
            Found::NotHere(why) => return (Probe::NotApplicable { why }, None),
            Found::Missing(from) => (from, None),
        };
        if let Some((probe, unfinished)) = Self::installer_hold(sys, &from, unready.is_none()) {
            return (probe, Some(unfinished));
        }
        if let Some(unfinished) = unready {
            let independent = self.read_root_steps(sys, from.clone());
            if matches!(&independent.0, Probe::Change { .. }) {
                return independent;
            }
            let probe = if unfinished == Unfinished::Refuses {
                Probe::Blocked { why: from }
            } else {
                Probe::Unusable { why: from }
            };
            return (probe, Some(unfinished));
        }
        if let Some(held) = self.held_back(sys, &from, true) {
            return held;
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

    /// The same current installer custody used by a probe and a queued tools installation.
    /// Unread custody holds only actual installation, not independent work beside an unread
    /// or refusing compiler.
    fn installer_hold(
        sys: &dyn System,
        from: &str,
        installation: bool,
    ) -> Option<(Probe, Unfinished)> {
        match held(sys) {
            crate::items::developer::Holder::Install => Some((
                Probe::Deferred {
                    why: format!("{from}; {INSTALLING}"),
                },
                Unfinished::Busy,
            )),
            crate::items::developer::Holder::AppleWindow => Some((
                Probe::Deferred {
                    why: format!("{from}; {APPLE_WINDOW_OPEN}"),
                },
                Unfinished::AppleWindow,
            )),
            crate::items::developer::Holder::Unread if installation => Some((
                Probe::Unusable {
                    why: format!("{from}; the current installer owner could not be read"),
                },
                Unfinished::Unread,
            )),
            crate::items::developer::Holder::Unread | crate::items::developer::Holder::Nothing => {
                None
            }
        }
    }

    /// Independent root work. An `Ok` here does not establish compiler health; callers
    /// with a refusing or unread compiler must retain that separate result.
    fn read_root_steps(&self, sys: &dyn System, now: String) -> (Probe, Option<Unfinished>) {
        if self.asked.load(std::sync::atomic::Ordering::Relaxed) {
            return (Probe::Ok { now }, None);
        }
        let owed = (self.owed)(sys);
        if owed.is_empty() || self.asked.load(std::sync::atomic::Ordering::Relaxed) {
            return (Probe::Ok { now }, None);
        }
        let steps = owed
            .iter()
            .map(|step| step.plain())
            .collect::<Vec<_>>()
            .join("; ");
        let from = format!("{now}; still owed: {steps}");
        match self.held_back(sys, &from, false) {
            Some(held) if self.tab.is_some() => held,
            Some(_) => (
                Probe::Ok {
                    now: format!("{now}; left for setup's one Mac password: {steps}"),
                },
                None,
            ),
            None => (
                Probe::Change {
                    from,
                    to: format!(
                        "done as root by `{} machines here apply --plain --root-steps-only` (one password prompt)",
                        Cmd::new(SUDO).arg(self.aterm.as_os_str()).display()
                    ),
                },
                None,
            ),
        }
    }

    /// Why this run may not ask for the password now, as the probe reads it (`from`, what it
    /// found): no person at this Mac's console (only a person there types the Mac password,
    /// never an agent's tab, a script or a remote shell), no terminal, Low Data Mode where the
    /// run would `download` the tools with no independent root work owed, an account that is
    /// no administrator's, or a run aterm decided whose ask is held ([`Unbidden::withheld`]).
    /// `None` where it may ask.
    fn held_back(
        &self,
        sys: &dyn System,
        from: &str,
        download: bool,
    ) -> Option<(Probe, Option<Unfinished>)> {
        if let Some((probe, why)) = self.live_eligibility(sys, from, download) {
            return Some((probe, Some(why)));
        }
        self.unbidden
            .as_ref()
            .and_then(|unbidden| unbidden.withheld(sys))
            .map(|why| {
                (
                    Probe::Deferred {
                        why: format!("{from}; {why}"),
                    },
                    Some(Unfinished::Other),
                )
            })
    }

    /// Current live admission, independent of whether a tab already claimed its opening.
    fn live_eligibility(
        &self,
        sys: &dyn System,
        from: &str,
        download: bool,
    ) -> Option<(Probe, Unfinished)> {
        let presence = crate::console::presence(sys);
        if let Some(person) = presence.needs_person() {
            return Some((
                Probe::Deferred {
                    why: format!("{from}; finishing Mac setup {person}"),
                },
                if matches!(
                    presence,
                    crate::console::Presence::OtherLogin | crate::console::Presence::NoConsole
                ) {
                    Unfinished::NotOwnLogin
                } else {
                    Unfinished::Other
                },
            ));
        }
        if !sys.has_tty() {
            return Some((
                Probe::Blocked {
                    why: format!("{from}; {NO_TTY}"),
                },
                Unfinished::Other,
            ));
        }
        // Independent root steps precede the retained download in the same password episode.
        // With none definitely owed, do not ask merely to wait for Low Data Mode to end.
        if download
            && let Some(why) = crate::items::developer::low_data(sys)
            && (self.owed)(sys).is_empty()
        {
            return Some((
                Probe::Blocked {
                    why: format!("{from}; {why}"),
                },
                Unfinished::LowData,
            ));
        }
        if sys.administrator() == Some(false) {
            return Some((
                Probe::Blocked {
                    why: format!("{from}; {NOT_ADMIN}"),
                },
                Unfinished::NotAdmin,
            ));
        }
        None
    }

    /// Reapply the probe's live gates under the terminal lease, bounded by the original plan.
    /// A tools installer and Low Data Mode do not hold independent root steps.
    fn admission_before_ask(
        &self,
        sys: &dyn System,
        plan: RootPlan,
    ) -> Option<(String, Unfinished)> {
        let download = plan == RootPlan::WithTools;
        let from = "the queued root work";
        let held = if download {
            Self::installer_hold(sys, from, true)
        } else {
            None
        }
        .or_else(|| self.live_eligibility(sys, from, download));
        held.map(|(probe, unfinished)| {
            let why = match probe {
                Probe::Deferred { why } | Probe::Blocked { why } | Probe::Unusable { why } => why,
                _ => unreachable!("an admission gate only returns a refusal"),
            };
            (why, unfinished)
        })
    }

    /// [`Item::apply`] as the tab's program: one plain line before `sudo` asks for the Mac
    /// password with the plain prompt, and the root half run `--plain`. Once that has started,
    /// this run waits for it to end, holding off a Ctrl-C or a closed tab (which reach the root
    /// half, which finishes its step), and says how it ended ([`Self::ended`]): installed when the
    /// tools are there, whatever the root half's exit, else its plain reason ([`Unfinished`]),
    /// never an argv or a path.
    fn apply_in_tab(&self, sys: &dyn System, tab: &Tab) -> Result<(), String> {
        let Some(plan) = self.apply_plan(sys).map_err(|(_, why)| tab.note(why))? else {
            return Ok(());
        };
        if !sys.has_tty() {
            return Err(tab.note(Unfinished::Other));
        }
        let mut skipped = false;
        let mut plan_failure = None;
        let stops = sys.catch_stops();
        let ran = sys.run_tty(&self.command_for(plan), &mut |command| {
            let fresh = self.plan_after(sys, plan);
            if let Ok(Some(plan)) = &fresh {
                *command = self.command_for(*plan);
            }
            let admission = fresh
                .as_ref()
                .ok()
                .and_then(|plan| plan.and_then(|plan| self.admission_before_ask(sys, plan)));
            if sys.stopped().is_some() {
                return Err(crate::port::RunFailed::Refused(
                    "setup was stopped before the root command started".into(),
                ));
            }
            if let Some(why) = self.unbidden.as_ref().and_then(Unbidden::held) {
                return Err(crate::port::RunFailed::Refused(why));
            }
            let plan = match fresh {
                Ok(Some(plan)) => plan,
                Ok(None) => {
                    skipped = true;
                    return Err(crate::port::RunFailed::Refused(
                        "the queued root work is no longer owed".into(),
                    ));
                }
                Err((why, unfinished)) => {
                    plan_failure = Some(unfinished);
                    return Err(crate::port::RunFailed::Refused(why));
                }
            };
            if let Some((why, unfinished)) = admission {
                plan_failure = Some(unfinished);
                return Err(crate::port::RunFailed::Refused(why));
            }
            tab.line(Self::intro(true, plan));
            self.asked.store(true, std::sync::atomic::Ordering::Relaxed);
            tab.entered
                .store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        });
        if skipped {
            drop(stops);
            return Ok(());
        }
        if let Some(why) = plan_failure {
            drop(stops);
            return Err(tab.note(why));
        }
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
    use std::path::Path;

    const ATERM: &str = "/Applications/aterm.app/Contents/MacOS/aterm";

    const CC: &str = "/Library/Developer/CommandLineTools/usr/bin/cc";

    /// The person's own run at this Mac's console (alice, 501), with a terminal to ask in.
    fn console() -> Fake {
        let sys = unread_console();
        sys.running(&[]);
        sys
    }

    fn unread_console() -> Fake {
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

    #[test]
    fn an_installation_plan_is_narrowed_when_tools_change_before_apply() {
        for (later, owed, suffix, failed) in [
            (
                Found::Missing("still missing".into()),
                false,
                Some(""),
                false,
            ),
            (Found::Present("Xcode works".into()), false, None, false),
            (
                Found::Present("Xcode works".into()),
                true,
                Some(" --root-steps-only"),
                false,
            ),
            (Found::Refuses("compiler refuses".into()), false, None, true),
            (Found::Unknown("compiler unread".into()), false, None, true),
            (Found::NotHere("not applicable".into()), false, None, false),
            (
                Found::Refuses("compiler refuses".into()),
                true,
                Some(" --root-steps-only"),
                false,
            ),
            (
                Found::Unknown("compiler unread".into()),
                true,
                Some(" --root-steps-only"),
                false,
            ),
        ] {
            let state = Arc::new(Mutex::new(Found::Missing("missing at plan".into())));
            let reader = state.clone();
            let item = CommandLineTools::new(
                Box::new(move |_| reader.lock().unwrap().clone()),
                ATERM.into(),
            )
            .with_owed(Box::new(move |_| {
                if owed {
                    vec![crate::root_steps::Owed::Debugger]
                } else {
                    Vec::new()
                }
            }))
            .saying(Arc::new(|_| {}));
            let sys = console();
            assert!(matches!(item.read(&sys).0, Probe::Change { .. }));
            *state.lock().unwrap() = later;
            let expected: Vec<String> = suffix
                .map(|suffix| format!("{}{suffix}", argv()))
                .into_iter()
                .collect();
            for command in &expected {
                sys.on(command, Ran::ok(""));
            }
            let result = item.apply(&sys, &ctx(&[]));
            assert_eq!(result.is_err(), failed, "{result:?}");
            assert_eq!(asked(&sys), expected);
        }
        // Losing tools after an independent plan cannot admit installation.
        let state = Arc::new(Mutex::new(Found::Present("Xcode works".into())));
        let reader = state.clone();
        let item = CommandLineTools::new(
            Box::new(move |_| reader.lock().unwrap().clone()),
            ATERM.into(),
        )
        .with_owed(Box::new(|_| vec![crate::root_steps::Owed::Debugger]))
        .saying(Arc::new(|_| {}));
        let sys = console();
        assert!(matches!(item.read(&sys).0, Probe::Change { .. }));
        *state.lock().unwrap() = Found::Missing("lost before apply".into());
        let root = format!("{} --root-steps-only", argv());
        sys.on(&root, Ran::ok(""));
        assert!(item.apply(&sys, &ctx(&[])).is_ok());
        assert_eq!(asked(&sys), [root]);

        // Another setup question can own stdin after apply has captured its plan. Change
        // the world only after that capture, while holding the real shared terminal lease.
        for tabbed in [false, true] {
            for (later, owed, suffix, failed) in [
                (
                    Found::Present("Xcode arrived while queued".into()),
                    true,
                    Some(" --root-steps-only"),
                    false,
                ),
                (Found::Present("already done".into()), false, None, false),
                (
                    Found::Missing("still missing".into()),
                    false,
                    Some(""),
                    false,
                ),
                (
                    Found::Refuses("compiler now refuses".into()),
                    true,
                    Some(" --root-steps-only"),
                    false,
                ),
                (
                    Found::Refuses("compiler now refuses".into()),
                    false,
                    None,
                    true,
                ),
                (
                    Found::Unknown("compiler now unread".into()),
                    true,
                    Some(" --root-steps-only"),
                    false,
                ),
                (
                    Found::Unknown("compiler now unread".into()),
                    false,
                    None,
                    true,
                ),
                (
                    Found::NotHere("no longer applicable".into()),
                    false,
                    None,
                    false,
                ),
            ] {
                let state = Arc::new(Mutex::new(Found::Missing("initially missing".into())));
                let reader = state.clone();
                let (captured, ready) = std::sync::mpsc::sync_channel(1);
                let reads = std::sync::atomic::AtomicUsize::new(0);
                let said = Arc::new(Mutex::new(String::new()));
                let sink = said.clone();
                let mut item = CommandLineTools::new(
                    Box::new(move |_| {
                        let found = reader.lock().unwrap().clone();
                        if reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1 {
                            captured.send(()).unwrap();
                        }
                        found
                    }),
                    ATERM.into(),
                )
                .with_owed(Box::new(move |_| {
                    if owed {
                        vec![crate::root_steps::Owed::Debugger]
                    } else {
                        Vec::new()
                    }
                }))
                .unbidden(Unbidden::new(Some(MARK.into()), BUILD.into(), || {
                    crate::init::Holds::NONE
                }))
                .saying(Arc::new(move |text| sink.lock().unwrap().push_str(text)));
                let tab = Tab::new({
                    let said = said.clone();
                    Arc::new(move |text| said.lock().unwrap().push_str(text))
                });
                if tabbed {
                    item = item.in_tab(tab.clone());
                }
                let sys = console();
                sys.on(&argv(), Ran::ok(""));
                sys.on(&format!("{} --root-steps-only", argv()), Ran::ok(""));
                assert!(matches!(item.read(&sys).0, Probe::Change { .. }));
                let result = std::thread::scope(|scope| {
                    let worker = crate::port::owning_terminal(|| {
                        let worker = scope.spawn(|| item.apply(&sys, &ctx(&[])));
                        ready
                            .recv_timeout(std::time::Duration::from_secs(60))
                            .expect("apply captured its pre-terminal plan");
                        assert!(asked(&sys).is_empty());
                        assert!(said.lock().unwrap().is_empty());
                        assert!(!marked(&sys));
                        *state.lock().unwrap() = later;
                        worker
                    });
                    worker.join().expect("the queued apply joins")
                });
                let expected: Vec<_> = suffix
                    .map(|suffix| format!("{}{suffix}", argv()))
                    .into_iter()
                    .collect();
                assert_eq!(asked(&sys), expected, "tabbed={tabbed}, {result:?}");
                assert_eq!(result.is_err(), failed, "tabbed={tabbed}, {result:?}");
                assert_eq!(marked(&sys), !tabbed && suffix.is_some());
                assert_eq!(
                    tab.entered.load(std::sync::atomic::Ordering::Relaxed),
                    tabbed && suffix.is_some()
                );
                if suffix.is_none() {
                    assert!(said.lock().unwrap().is_empty() || failed && tabbed);
                }
            }
        }

        // Admission can change while another question owns the real terminal lease too.
        // Capture the old plan before changing console ownership or installer custody.
        for tabbed in [false, true] {
            for (console_uid, installer, tools_arrive, code, withdraw_root) in [
                (Some(0), None, false, 7, false),
                (Some(502), None, false, 7, false),
                (None, Some("/usr/sbin/softwareupdate"), false, 8, false),
                (
                    None,
                    Some(
                        "/System/Library/CoreServices/Install Command Line Developer Tools.app/Contents/MacOS/Install Command Line Developer Tools",
                    ),
                    false,
                    8,
                    false,
                ),
                // A narrow plan owes only independent steps, which a tools installer cannot hold.
                (None, Some("/usr/sbin/softwareupdate"), true, 0, false),
                // A constrained download can enter for definite independent work, but
                // loses that exception if the work completes while the terminal is held.
                (None, None, false, 3, true),
            ] {
                let state = Arc::new(Mutex::new(Found::Missing("initially missing".into())));
                let reader = state.clone();
                let (captured, ready) = std::sync::mpsc::sync_channel(1);
                let reads = std::sync::atomic::AtomicUsize::new(0);
                let said = Arc::new(Mutex::new(String::new()));
                let sink = said.clone();
                let owed = Arc::new(std::sync::atomic::AtomicBool::new(true));
                let owe = owed.clone();
                let mut item = CommandLineTools::new(
                    Box::new(move |_| {
                        let found = reader.lock().unwrap().clone();
                        if reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1 {
                            captured.send(()).unwrap();
                        }
                        found
                    }),
                    ATERM.into(),
                )
                .with_owed(Box::new(move |_| {
                    if owe.load(std::sync::atomic::Ordering::SeqCst) {
                        vec![crate::root_steps::Owed::Debugger]
                    } else {
                        Vec::new()
                    }
                }))
                .unbidden(Unbidden::new(Some(MARK.into()), BUILD.into(), || {
                    crate::init::Holds::NONE
                }))
                .saying(Arc::new(move |text| sink.lock().unwrap().push_str(text)));
                let tab = Tab::new({
                    let said = said.clone();
                    Arc::new(move |text| said.lock().unwrap().push_str(text))
                });
                if tabbed {
                    item = item.in_tab(tab.clone());
                }
                let mut sys = console();
                if withdraw_root {
                    sys.low_data = Some(true);
                }
                sys.on(&argv(), Ran::ok(""));
                let root = format!("{} --root-steps-only", argv());
                sys.on(&root, Ran::ok(""));
                assert!(matches!(item.read(&sys).0, Probe::Change { .. }));
                let result = std::thread::scope(|scope| {
                    let worker = crate::port::owning_terminal(|| {
                        let worker = scope.spawn(|| item.apply(&sys, &ctx(&[])));
                        ready
                            .recv_timeout(std::time::Duration::from_secs(60))
                            .expect("apply captured its plan before the terminal lease");
                        assert!(asked(&sys).is_empty());
                        assert!(!marked(&sys));
                        assert!(said.lock().unwrap().is_empty());
                        if let Some(uid) = console_uid {
                            sys.stat_of(
                                "/dev/console",
                                Stat {
                                    uid,
                                    gid: 20,
                                    mode: 0o600,
                                    dir: false,
                                    symlink: false,
                                    len: 0,
                                },
                            );
                        }
                        if let Some(installer) = installer {
                            sys.file(crate::items::developer::PLACEHOLDER, b"")
                                .running(&["/sbin/launchd", installer]);
                        }
                        if tools_arrive {
                            *state.lock().unwrap() = Found::Present("tools arrived".into());
                        }
                        if withdraw_root {
                            owed.store(false, std::sync::atomic::Ordering::SeqCst);
                        }
                        worker
                    });
                    worker.join().expect("the queued apply joins")
                });
                if code == 0 {
                    assert!(result.is_ok(), "{result:?}");
                    assert_eq!(asked(&sys), [root]);
                } else {
                    assert!(result.is_err(), "{tabbed}: admission changed: {result:?}");
                    assert!(asked(&sys).is_empty(), "{tabbed}: {:?}", asked(&sys));
                    assert!(!marked(&sys));
                    assert!(!tab.entered.load(std::sync::atomic::Ordering::Relaxed));
                    assert!(said.lock().unwrap().is_empty());
                    if tabbed {
                        assert_eq!(tab.exit(tab.unfinished().unwrap()), code);
                    }
                }
            }
        }
    }

    #[test]
    fn independent_work_completed_before_apply_spends_no_password_episode() {
        use std::sync::atomic::{AtomicBool, Ordering};
        for found in [
            Found::Present("healthy".into()),
            Found::Refuses("broken".into()),
            Found::Unknown("unread".into()),
        ] {
            let owed = Arc::new(AtomicBool::new(true));
            let reader = owed.clone();
            let said = Arc::new(Mutex::new(String::new()));
            let sink = said.clone();
            let item = item(found)
                .with_owed(Box::new(move |_| {
                    if reader.load(Ordering::Relaxed) {
                        vec![crate::root_steps::Owed::Debugger]
                    } else {
                        Vec::new()
                    }
                }))
                .unbidden(Unbidden::new(Some(MARK.into()), BUILD.into(), || {
                    crate::init::Holds::NONE
                }))
                .saying(Arc::new(move |text| sink.lock().unwrap().push_str(text)));
            let sys = console();
            assert!(matches!(item.read(&sys).0, Probe::Change { .. }));
            owed.store(false, Ordering::Relaxed);
            assert!(item.apply(&sys, &ctx(&[])).is_ok());
            assert!(asked(&sys).is_empty());
            assert!(said.lock().unwrap().is_empty());
            assert!(!marked(&sys));
            // Skipping obsolete work leaves the one episode available for genuinely owed work.
            owed.store(true, Ordering::Relaxed);
            assert!(matches!(item.read(&sys).0, Probe::Change { .. }));
            let root = format!("{} --root-steps-only", argv());
            sys.on(&root, Ran::ok(""));
            assert!(item.apply(&sys, &ctx(&[])).is_ok());
            assert_eq!(asked(&sys), [root]);
            assert!(marked(&sys));
        }

        for tabbed in [false, true] {
            for still_owed in [false, true] {
                let owed = Arc::new(AtomicBool::new(true));
                let owing = owed.clone();
                let found = Arc::new(Mutex::new(Found::Present("healthy at plan".into())));
                let reader = found.clone();
                let (captured, ready) = std::sync::mpsc::sync_channel(1);
                let reads = std::sync::atomic::AtomicUsize::new(0);
                let said = Arc::new(Mutex::new(String::new()));
                let sink = said.clone();
                let mut item = CommandLineTools::new(
                    Box::new(move |_| reader.lock().unwrap().clone()),
                    ATERM.into(),
                )
                .with_owed(Box::new(move |_| {
                    let pending = owing.load(Ordering::SeqCst);
                    if reads.fetch_add(1, Ordering::SeqCst) == 1 {
                        captured.send(()).unwrap();
                    }
                    if pending {
                        vec![crate::root_steps::Owed::Debugger]
                    } else {
                        Vec::new()
                    }
                }))
                .unbidden(Unbidden::new(Some(MARK.into()), BUILD.into(), || {
                    crate::init::Holds::NONE
                }))
                .saying(Arc::new(move |text| sink.lock().unwrap().push_str(text)));
                let tab = Tab::new({
                    let said = said.clone();
                    Arc::new(move |text| said.lock().unwrap().push_str(text))
                });
                if tabbed {
                    item = item.in_tab(tab.clone());
                }
                let sys = console();
                let root = format!("{} --root-steps-only", argv());
                sys.on(&root, Ran::ok(""));
                assert!(matches!(item.read(&sys).0, Probe::Change { .. }));
                let result = std::thread::scope(|scope| {
                    let worker = crate::port::owning_terminal(|| {
                        let worker = scope.spawn(|| item.apply(&sys, &ctx(&[])));
                        ready
                            .recv_timeout(std::time::Duration::from_secs(60))
                            .expect("apply captured its independent work");
                        assert!(asked(&sys).is_empty());
                        assert!(said.lock().unwrap().is_empty());
                        assert!(!marked(&sys));
                        owed.store(still_owed, Ordering::SeqCst);
                        if still_owed {
                            *found.lock().unwrap() = Found::Missing("lost while queued".into());
                            // A concurrent probe can plan a new full installation. It cannot
                            // expand the independent command already waiting on this lease.
                            assert!(matches!(item.read(&sys).0, Probe::Change { .. }));
                            assert_eq!(item.root_plan(), RootPlan::WithTools);
                        }
                        worker
                    });
                    worker.join().expect("the queued independent apply joins")
                });
                assert!(result.is_ok(), "tabbed={tabbed}, {result:?}");
                let expected: Vec<_> = still_owed.then_some(root).into_iter().collect();
                assert_eq!(asked(&sys), expected);
                assert_eq!(marked(&sys), !tabbed && still_owed);
                assert_eq!(tab.entered.load(Ordering::Relaxed), tabbed && still_owed);
                assert_eq!(said.lock().unwrap().is_empty(), !still_owed);
            }
        }
    }

    #[test]
    fn completed_root_work_needs_no_terminal() {
        use std::sync::atomic::{AtomicBool, Ordering};
        for tabbed in [false, true] {
            let pending = Arc::new(AtomicBool::new(true));
            let reader = pending.clone();
            let mut item = item(Found::Present("healthy".into())).with_owed(Box::new(move |_| {
                if reader.load(Ordering::Relaxed) {
                    vec![crate::root_steps::Owed::Debugger]
                } else {
                    Vec::new()
                }
            }));
            let tab = Tab::new(Arc::new(|_| {}));
            if tabbed {
                item = item.in_tab(tab.clone());
            }
            let mut sys = console();
            assert!(matches!(item.read(&sys).0, Probe::Change { .. }));
            pending.store(false, Ordering::Relaxed);
            sys.tty = false;
            assert!(item.apply(&sys, &ctx(&[])).is_ok());
            assert!(asked(&sys).is_empty());
            assert_eq!(tab.unfinished(), None);
            pending.store(true, Ordering::Relaxed);
            assert!(item.apply(&sys, &ctx(&[])).is_err());
            assert!(asked(&sys).is_empty());
        }
    }

    #[test]
    fn independent_root_work_preserves_a_refusing_or_unread_compiler() {
        use crate::root_steps::Owed;
        for found in [
            Found::Refuses("broken compiler".into()),
            Found::Unknown("unread compiler".into()),
        ] {
            let mut away = console();
            away.tty = false;
            let held = item(found.clone()).with_owed(Box::new(|_| vec![Owed::Debugger]));
            assert!(!matches!(held.read(&away).0, Probe::Change { .. }));
            let _ = run(&away, &held, Mode::Apply);
            assert!(asked(&away).is_empty());
            let settled = item(found.clone());
            assert!(!matches!(settled.read(&console()).0, Probe::Change { .. }));
            let sys = console();
            let item = item(found.clone()).with_owed(Box::new(|_| {
                vec![Owed::Debugger, Owed::NameRule, Owed::LanBroker]
            }));
            let root = format!("{} --root-steps-only", argv());
            let marker = Path::new("/independent-root-step");
            sys.on(&root, Ran::ok("")).on_write(&root, marker, b"done");
            assert_eq!(run(&sys, &item, Mode::Plan).outcome, Outcome::Planned);
            // A successful independent root command is not a compiler repair.
            let _ = run(&sys, &item, Mode::Apply);
            assert_eq!(asked(&sys), std::slice::from_ref(&root));
            assert_eq!(sys.read_file(marker).unwrap(), b"done");
            assert!(sys.read_file(Path::new(CC)).is_err());
            let expected = match found {
                Found::Refuses(_) => matches!(item.read(&sys).0, Probe::Blocked { .. }),
                Found::Unknown(_) => matches!(item.read(&sys).0, Probe::Unusable { .. }),
                _ => unreachable!(),
            };
            assert!(expected);
            let _ = run(&sys, &item, Mode::Apply);
            assert_eq!(
                asked(&sys),
                [root],
                "independent work consumes only one root episode"
            );
        }
        let parsed =
            crate::baseline::parse(&CommandLineTools::baseline_named(RootPlan::StepsOnly, None))
                .unwrap();
        let developer = parsed.developer.unwrap();
        assert_eq!(developer.command_line_tools, Some(false));
        assert_eq!(developer.debugger, Some(true));
        assert_eq!(parsed.names.unwrap().admitted_name_rule, Some(true));
        assert_eq!(parsed.lan.unwrap().broker, Some(true));
    }

    /// On a Mac that has the tools, the elevation still runs where its other root steps are
    /// owed: a person's run at the console asks for the one password, saying the setup's own
    /// line, and its read-back reads the tools in place whatever the root half left owed; the
    /// root half's baseline turns on developer mode and puts the naming rule in place. A refused
    /// password there is no success. NEGATIVE CONTROLS: with nothing owed nothing is asked, and a
    /// run outside the tab that may not ask (no terminal) reads in place, naming what is left,
    /// and asks nothing.
    #[test]
    fn a_mac_with_the_tools_still_gets_the_one_password_for_what_is_owed() {
        use crate::root_steps::Owed;
        let present = || item(Found::Present("installed".into()));
        let owed_reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let owing = |item: CommandLineTools| {
            let owed_reads = Arc::clone(&owed_reads);
            item.with_owed(Box::new(move |_: &dyn System| {
                owed_reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                vec![Owed::Debugger, Owed::NameRule]
            }))
        };
        let said = Arc::new(Mutex::new(String::new()));
        let sink = said.clone();
        let item = owing(present()).saying(Arc::new(move |text: &str| {
            sink.lock().unwrap().push_str(text);
        }));
        let sys = console();
        assert_eq!(run(&sys, &item, Mode::Plan).outcome, Outcome::Planned);
        let root = format!("{} --root-steps-only", argv());
        sys.on(&root, Ran::ok(""));
        let reads_at_start = Arc::new(std::sync::atomic::AtomicUsize::new(usize::MAX));
        sys.tty_model({
            let owed_reads = Arc::clone(&owed_reads);
            let reads_at_start = Arc::clone(&reads_at_start);
            move |_, _| {
                reads_at_start.store(
                    owed_reads.load(std::sync::atomic::Ordering::Relaxed),
                    std::sync::atomic::Ordering::Relaxed,
                );
                None
            }
        });
        let row = run(&sys, &item, Mode::Apply);
        assert_eq!(row.outcome, Outcome::Changed, "{row:?} {:?}", sys.calls());
        assert_eq!(
            owed_reads.load(std::sync::atomic::Ordering::Relaxed),
            reads_at_start.load(std::sync::atomic::Ordering::Relaxed),
            "read-back does not re-probe root work after its command starts"
        );
        assert_eq!(
            asked(&sys),
            std::slice::from_ref(&root),
            "one password prompt: {:?}",
            sys.calls()
        );
        assert_eq!(*said.lock().unwrap(), format!("{INTRO_SETUP}\n"));
        let parsed = crate::baseline::parse(&CommandLineTools::baseline()).unwrap();
        assert_eq!(parsed.developer.and_then(|t| t.debugger), Some(true));
        assert_eq!(parsed.names.and_then(|t| t.admitted_name_rule), Some(true));
        assert_eq!(parsed.lan.and_then(|t| t.broker), Some(true));

        let refused = console();
        refused.on(&root, Ran::exit(1, "Sorry, try again."));
        let row = run(&refused, &owing(present()), Mode::Apply);
        assert_eq!(row.outcome, Outcome::Failed, "{row:?}");

        let settled = console();
        let row = run(&settled, &present(), Mode::Apply);
        assert_eq!(row.outcome, Outcome::Ok, "{row:?}");
        assert!(asked(&settled).is_empty(), "{:?}", settled.calls());

        let mut away = console();
        away.tty = false;
        let row = run(&away, &owing(present()), Mode::Apply);
        assert_eq!(row.outcome, Outcome::Ok, "{row:?}");
        assert!(
            format!("{row:?}").contains("left for setup's one Mac password"),
            "{row:?}"
        );
        assert!(asked(&away).is_empty(), "{:?}", away.calls());
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
        let parsed = crate::baseline::parse(&CommandLineTools::baseline_named(
            RootPlan::WithTools,
            Some("m42"),
        ))
        .unwrap();
        let names = parsed.names.expect("a [names] table");
        assert_eq!(names.computer_name.as_deref(), Some("m42"));
        assert_eq!(names.local_host_name.as_deref(), Some("m42"));
        assert_eq!(names.host_name.as_deref(), Some("m42"));
        assert_eq!(names.admitted_name_rule, Some(true));

        for word in ["Mac-mini-M4", "M42", "m42-studio", "m042", ""] {
            let sys = console();
            let owned = word.to_owned();
            let item = missing().with_name(Box::new(move || Some(owned.clone())));
            sys.on(&argv(), Ran::ok(""))
                .on_write(&argv(), std::path::Path::new(CC), b"");
            run(&sys, &item, Mode::Apply);
            assert_eq!(asked(&sys), [argv()], "{word:?}: {:?}", sys.calls());
            assert_eq!(
                CommandLineTools::baseline_named(RootPlan::WithTools, Some(word)),
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
                let said = Arc::clone(&said);
                let sys = Arc::clone(&sys);
                Arc::new(move |text: &str| {
                    assert!(
                        crate::port::terminal_free().is_none(),
                        "the prelude owns the terminal"
                    );
                    said.lock()
                        .unwrap()
                        .push((text.to_owned(), sys.calls().len()));
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
                [(format!("{INTRO}\n"), ran)],
                "said once under the same terminal lease immediately before sudo: {calls:?}"
            );
            let deferred = calls.iter().position(|call| call == "defer stops").unwrap();
            let restored = calls
                .iter()
                .position(|call| call == "stops restored")
                .unwrap();
            assert!(deferred < ran && ran < restored, "{calls:?}");
            assert_eq!(asked(&sys), std::slice::from_ref(&argv), "{calls:?}");
            assert!(
                !calls.iter().any(|call| call.contains("sudo -K")),
                "nothing cached to forget: {calls:?}"
            );
        }
        // A vault question may own the terminal first. The root pass must not print its
        // prelude, claim its episode or start sudo while that question still owns stdin.
        for stopped_while_queued in [false, true] {
            let sys = Arc::new(console());
            sys.on(&argv, Ran::ok(""));
            sys.on_write(&argv, std::path::Path::new(CC), b"");
            let (probed, ready) = std::sync::mpsc::sync_channel(1);
            let once = std::sync::atomic::AtomicBool::new(false);
            let said = Arc::new(Mutex::new(String::new()));
            let question_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let item = CommandLineTools::new(
                Box::new(move |sys| {
                    if !once.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        let _ = probed.send(());
                    }
                    if sys.read_file(std::path::Path::new(CC)).is_ok() {
                        Found::Present(CC.into())
                    } else {
                        Found::Missing("no developer directory".into())
                    }
                }),
                ATERM.into(),
            )
            .unbidden(Unbidden::new(Some(MARK.into()), BUILD.into(), || {
                crate::init::Holds::NONE
            }))
            .saying({
                let (said, question_done) = (Arc::clone(&said), Arc::clone(&question_done));
                Arc::new(move |text| {
                    assert!(question_done.load(std::sync::atomic::Ordering::SeqCst));
                    assert!(crate::port::terminal_free().is_none());
                    said.lock().unwrap().push_str(text);
                })
            });
            let row = std::thread::scope(|scope| {
                let worker = crate::port::owning_terminal(|| {
                    let worker = scope.spawn(|| run(&sys, &item, Mode::Apply));
                    ready
                        .recv_timeout(std::time::Duration::from_secs(60))
                        .expect("the root item probes");
                    assert!(said.lock().unwrap().is_empty());
                    assert!(asked(&sys).is_empty());
                    assert!(!marked(&sys));
                    if stopped_while_queued {
                        sys.stop_now();
                    }
                    question_done.store(true, std::sync::atomic::Ordering::SeqCst);
                    worker
                });
                worker.join().expect("the owned root pass joins")
            });
            if stopped_while_queued {
                assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
                assert!(asked(&sys).is_empty());
                assert!(said.lock().unwrap().is_empty());
                assert!(
                    !marked(&sys),
                    "no root child or password episode was started"
                );
            } else {
                assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
                assert_eq!(asked(&sys), std::slice::from_ref(&argv));
                assert!(marked(&sys));
                assert_eq!(*said.lock().unwrap(), format!("{INTRO}\n"));
            }
        }
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

    /// In Low Data Mode a download alone asks no password; definite independent root work
    /// can enter the original batch, which retains the download behind its own network gate.
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

        // An authorized built-in batch does its independent steps before retaining that
        // same root process while the tools' download waits for an unconstrained path.
        for step in [
            crate::root_steps::Owed::Debugger,
            crate::root_steps::Owed::NameRule,
            crate::root_steps::Owed::LanBroker,
        ] {
            for tabbed in [false, true] {
                let mut sys = console();
                sys.low_data = Some(true);
                let mut item = missing().with_owed(Box::new(move |_| vec![step]));
                let tab = Tab::new(Arc::new(|_| {}));
                if tabbed {
                    item = item.in_tab(tab.clone());
                }
                assert_eq!(run(&sys, &item, Mode::Plan).outcome, Outcome::Planned);
                sys.on(&argv(), Ran::ok(""))
                    .on_write(&argv(), std::path::Path::new(CC), b"");
                let row = run(&sys, &item, Mode::Apply);
                assert_eq!(row.outcome, Outcome::Changed, "{step:?}: {row:?}");
                assert_eq!(asked(&sys), [argv()], "one full batch: {:?}", sys.calls());
                assert_eq!(
                    tab.entered.load(std::sync::atomic::Ordering::Relaxed),
                    tabbed
                );
            }
        }

        // Unknown root state is no independent work that may admit the ask.
        let mut unknown = console();
        unknown.low_data = Some(true);
        unknown
            .on(
                "/usr/sbin/DevToolsSecurity -status",
                Ran::ok("Developer mode rights are in an unknown state.\n"),
            )
            .stat_fails(
                crate::root_steps::ADMITTED_NAME_RULE,
                std::io::ErrorKind::PermissionDenied,
            );
        let item = missing().with_owed(Box::new(crate::root_steps::owed));
        assert_eq!(run(&unknown, &item, Mode::Apply).outcome, Outcome::Blocked);
        assert!(asked(&unknown).is_empty());
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

    /// An existing repair may temporarily break a compiler that was already installed. Its
    /// custody defers both refusing and unanswered probes; losing custody restores the actual
    /// failure, and a later ready probe recovers the row without another password attempt.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_active_repair_defers_a_refusing_or_unanswered_compiler_until_it_recovers() {
        let apple = format!(
            "/System/Library/CoreServices/{}",
            aterm_types::macos_setup::APPLE_TOOLS_INSTALLER
        );
        for (found, unheld, unfinished) in [
            (
                Found::Refuses("compiler mid replacement".into()),
                Outcome::Blocked,
                Unfinished::Refuses,
            ),
            (
                Found::Unknown("compiler did not answer during replacement".into()),
                Outcome::Unknown,
                Unfinished::Unread,
            ),
        ] {
            for (owner, held) in [
                ("/usr/sbin/softwareupdate", Unfinished::Busy),
                ("/usr/sbin/installer", Unfinished::Busy),
                (apple.as_str(), Unfinished::AppleWindow),
            ] {
                let sys = Arc::new(console());
                sys.file(crate::items::developer::PLACEHOLDER, b"")
                    .running(&["/sbin/launchd", owner]);
                let (item, said, tab) = in_tab(&sys, found.clone());
                let waiting = run(&sys, &item, Mode::Apply);
                assert_eq!(waiting.outcome, Outcome::Deferred, "{owner}: {waiting:?}");
                assert!(deferred(&waiting));
                assert_eq!(tab.unfinished(), Some(held));
                assert!(asked(&sys).is_empty());
                assert!(said.lock().unwrap().is_empty());

                sys.running(&["/sbin/launchd"]);
                assert_eq!(run(&sys, &item, Mode::Apply).outcome, unheld);
                assert_eq!(tab.unfinished(), Some(unfinished));

                sys.file(CC, b"repaired compiler");
                assert_eq!(run(&sys, &item, Mode::Apply).outcome, Outcome::Ok);
                assert!(asked(&sys).is_empty());
                assert!(said.lock().unwrap().is_empty());
            }
            let sys = Arc::new(unread_console());
            let (item, said, tab) = in_tab(&sys, found);
            assert_eq!(run(&sys, &item, Mode::Apply).outcome, unheld);
            assert_eq!(tab.unfinished(), Some(unfinished));
            assert!(asked(&sys).is_empty());
            assert!(said.lock().unwrap().is_empty());
        }
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

    /// At the console with no terminal to ask in (no `/dev/tty`), pending work stays blocked,
    /// running nothing.
    #[test]
    fn without_a_terminal_pending_work_is_blocked() {
        let mut sys = console();
        sys.tty = false;
        let item = missing();
        for mode in [Mode::Plan, Mode::Apply] {
            let row = run(&sys, &item, mode);
            assert_eq!(row.outcome, Outcome::Blocked, "{mode:?}: {row:?}");
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

    #[test]
    fn unread_installer_custody_asks_nothing_and_recovers_on_a_later_read() {
        for placeholder in [false, true] {
            let sys = Arc::new(unread_console());
            if placeholder {
                sys.file(crate::items::developer::PLACEHOLDER, b"");
            }
            let item = missing();
            for mode in [Mode::Plan, Mode::Apply] {
                let row = run(&sys, &item, mode);
                assert_eq!(row.outcome, Outcome::Unknown, "{placeholder}: {row:?}");
                assert!(asked(&sys).is_empty(), "no password with unread custody");
            }
            let (tab_item, said, tab) =
                in_tab(&sys, Found::Missing("no developer directory".into()));
            assert_eq!(run(&sys, &tab_item, Mode::Apply).outcome, Outcome::Unknown);
            assert_eq!(tab.unfinished(), Some(Unfinished::Unread));
            assert!(said.lock().unwrap().is_empty());
            assert!(asked(&sys).is_empty());

            sys.running(&[]);
            assert_eq!(run(&sys, &item, Mode::Plan).outcome, Outcome::Planned);
            sys.on(&argv(), Ran::ok(""))
                .on_write(&argv(), std::path::Path::new(CC), b"");
            assert_eq!(run(&sys, &item, Mode::Apply).outcome, Outcome::Changed);
            assert_eq!(asked(&sys), [argv()], "one prompt after known idle");
        }
    }

    #[test]
    fn an_unread_placeholder_does_not_admit_an_install_and_can_recover() {
        use std::io::ErrorKind;
        let placeholder = crate::items::developer::PLACEHOLDER;
        for kind in [
            ErrorKind::PermissionDenied,
            ErrorKind::NotADirectory,
            ErrorKind::Other,
        ] {
            for processes in [vec![], vec!["/usr/sbin/softwareupdate"]] {
                let sys = Arc::new(console());
                sys.running(&processes).stat_fails(placeholder, kind);
                let (item, said, tab) =
                    in_tab(&sys, Found::Missing("no developer directory".into()));
                assert_eq!(run(&sys, &item, Mode::Apply).outcome, Outcome::Unknown);
                assert_eq!(tab.unfinished(), Some(Unfinished::Unread));
                assert!(asked(&sys).is_empty());
                assert!(said.lock().unwrap().is_empty());

                sys.running(&[])
                    .stat_fails(placeholder, ErrorKind::NotFound);
                sys.on(&argv(), Ran::ok(""))
                    .on_write(&argv(), std::path::Path::new(CC), b"");
                assert_eq!(run(&sys, &item, Mode::Apply).outcome, Outcome::Changed);
                assert_eq!(asked(&sys), [argv()]);
            }
        }
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
                let sys = console();
                sys.tty_model(|sys, _| {
                    sys.stop_now();
                    None
                });
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
                assert!(marked(&sys), "{decider:?} {status:?}: {calls:?}");
                assert!(
                    !calls.iter().any(|call| call == &format!("remove {MARK}")),
                    "the consumed episode survives stop delivery: {calls:?}"
                );
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
    fn marked(sys: &Fake) -> bool {
        aterm_types::clt_mark::spent(sys, std::path::Path::new(MARK))
    }

    /// In a run aterm decided (the follow-up of a person's own package install or update), the
    /// first run asks once, claiming its ask before `sudo`; later automatic runs and updates
    /// preserve that spent ask after a person's cancellation. The shared GUI claim holds too.
    /// NEGATIVE CONTROL: explicit retries remain available and record the first human attempt.
    #[test]
    fn automatic_setup_preserves_its_spent_ask_across_updates_and_a_person_can_retry() {
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
        assert!(marked(&sys));

        for mode in [Mode::Plan, Mode::Apply] {
            let row = run(&sys, &aterm, mode);
            assert_eq!(row.outcome, Outcome::Deferred, "{mode:?}: {row:?}");
            assert!(deferred(&row));
            let why = row.why.unwrap_or_default();
            assert!(
                why.contains(ASK_ALREADY_SPENT) && why.ends_with(PERSONS_STEP),
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
        assert_eq!(
            asked(&sys).len(),
            1,
            "an update must not ask for the password again"
        );
        assert_eq!(row.outcome, Outcome::Deferred, "{row:?}");
        assert!(marked(&sys));

        let person = console();
        person.on(&argv, Ran::exit(88, ""));
        let typed = crate::init::Decider::Person.compose(
            crate::init::Base {
                clt: Some(missing()),
                ..crate::init::Base::none()
            },
            || {
                Unbidden::new(Some(PathBuf::from(MARK)), BUILD.to_owned(), || {
                    panic!("an explicit retry never reads automatic holds")
                })
            },
        );
        let typed = typed.clt.as_ref().unwrap();
        for _ in 0..2 {
            assert_eq!(run(&person, typed, Mode::Apply).outcome, Outcome::Failed);
        }
        assert_eq!(asked(&person).len(), 2, "{:?}", person.calls());
        assert_eq!(
            run(&person, &decided(crate::init::Decider::Aterm), Mode::Apply).outcome,
            Outcome::Deferred
        );
        assert_eq!(
            asked(&person).len(),
            2,
            "explicit attempts must hold unsolicited retries"
        );
        assert_eq!(
            person
                .calls()
                .iter()
                .filter(|call| call.starts_with("write ") || call.starts_with("remove "))
                .count(),
            2,
            "the explicit retry revokes the first attempt's release capability: {:?}",
            person.calls()
        );
    }

    /// A run aterm decided asks for no password while the person's choices hold System State's
    /// own Mac Setup tab (their Stop, setup records that cannot be read, a newer
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
                    && !why.contains(ASK_ALREADY_SPENT)
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
                assert!(marked(&sys));
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
                    why.contains(ASK_ALREADY_SPENT) && why.ends_with(PERSONS_STEP),
                    "{why}"
                );
                assert!(asked(&sys).is_empty(), "{calls:?}");
                assert!(marked(&sys), "the tab's line stays: {calls:?}");
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
                    marked(&sys),
                    "a failed install retains its consumed episode: {calls:?}"
                );
            }
        }
    }

    /// A pending automatic claim must not release a later explicit person's real attempt,
    /// even when the automatic sudo was proven never started afterward.
    #[test]
    fn an_explicit_attempt_survives_an_older_automatic_no_start_result() {
        use std::sync::atomic::{AtomicBool, Ordering};
        if !aterm_uds::test_binary::isolated_lifecycle(
            "init_clt::tests::an_explicit_attempt_survives_an_older_automatic_no_start_result",
        ) {
            return;
        }
        let sys = Arc::new(console());
        let automatic_entered = Arc::new(AtomicBool::new(false));
        let explicit_ran = Arc::new(AtomicBool::new(false));
        sys.tty_model({
            let automatic_entered = Arc::clone(&automatic_entered);
            move |_, _| {
                if automatic_entered.swap(true, Ordering::SeqCst) {
                    Some(Ran::exit(1, ""))
                } else {
                    None // A's real unscripted Spawn failure, after its claim.
                }
            }
        });
        let item = CommandLineTools::new(
            Box::new({
                let (sys, automatic_entered, explicit_ran) = (
                    Arc::clone(&sys),
                    Arc::clone(&automatic_entered),
                    Arc::clone(&explicit_ran),
                );
                move |probe| {
                    // `ended` reads back after A released the terminal, before it releases its
                    // no-start claim. B must acquire the real terminal here, never recursively
                    // from A's terminal-command callback, which still owns that mutex.
                    if automatic_entered.load(Ordering::SeqCst)
                        && !explicit_ran.swap(true, Ordering::SeqCst)
                    {
                        let person = decided(crate::init::Decider::Person);
                        assert_eq!(run(&sys, &person, Mode::Apply).outcome, Outcome::Failed);
                    }
                    if probe.read_file(std::path::Path::new(CC)).is_ok() {
                        Found::Present(CC.into())
                    } else {
                        Found::Missing("no developer directory".into())
                    }
                }
            }),
            ATERM.into(),
        )
        .saying(Arc::new(|_: &str| {}));
        let automatic = crate::init::Decider::Aterm
            .compose(
                crate::init::Base {
                    clt: Some(item),
                    ..crate::init::Base::none()
                },
                || {
                    Unbidden::new(Some(PathBuf::from(MARK)), BUILD.to_owned(), || {
                        crate::init::Holds::NONE
                    })
                },
            )
            .clt
            .expect("base.clt");
        assert_eq!(run(&sys, &automatic, Mode::Apply).outcome, Outcome::Failed);
        assert!(explicit_ran.load(Ordering::SeqCst));
        assert!(marked(&sys));
        assert_eq!(
            run(&sys, &decided(crate::init::Decider::Aterm), Mode::Apply).outcome,
            Outcome::Deferred
        );
        assert_eq!(
            asked(&sys).len(),
            2,
            "no third unsolicited attempt: {:?}",
            sys.calls()
        );
    }

    /// All outcomes after sudo may have asked a person and retain the episode. Only a proven
    /// failure to start releases this exact claim; a lost post-start result does not.
    #[test]
    fn a_password_attempt_stays_spent_and_only_proven_no_start_releases_it() {
        use crate::port::RunFailed;
        for (reply, keeps) in [
            (Some(Ok(Ran::exit(82, ""))), true),
            (Some(Ok(Ran::exit(83, ""))), true),
            (Some(Ok(Ran::exit(1, ""))), true),
            (Some(Ok(Ran::exit(85, ""))), true),
            (Some(Ok(Ran::exit(88, ""))), true),
            (
                Some(Err(RunFailed::AfterStart("lost wait result".into()))),
                true,
            ),
            (
                Some(Err(RunFailed::TimedOut(std::time::Duration::from_secs(60)))),
                true,
            ),
            (Some(Err(RunFailed::Spawn("exec failed".into()))), false),
            (
                Some(Err(RunFailed::Refused("before child start".into()))),
                false,
            ),
            (None, false), // The port's actual unscripted Spawn failure.
        ] {
            let sys = console();
            if let Some(reply) = reply {
                sys.on_result(&argv(), reply);
            }
            let aterm = decided(crate::init::Decider::Aterm);
            let row = run(&sys, &aterm, Mode::Apply);
            assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
            assert_eq!(marked(&sys), keeps, "{:?}", sys.calls());
            assert_eq!(
                run(&sys, &aterm, Mode::Plan).outcome,
                if keeps {
                    Outcome::Deferred
                } else {
                    Outcome::Planned
                }
            );
            if keeps {
                let attempts = asked(&sys);
                let next = decided(crate::init::Decider::Aterm);
                assert_eq!(run(&sys, &next, Mode::Apply).outcome, Outcome::Deferred);
                assert_eq!(asked(&sys), attempts);
            }
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
            (89, Unfinished::Unread),
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
            Some(90),
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
            let fake = console();
            if stopped {
                fake.tty_model(|sys, _| {
                    sys.stop_now();
                    None
                });
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
            for after in [
                Unfinished::LowData,
                Unfinished::NotOwnLogin,
                Unfinished::Busy,
                Unfinished::AppleWindow,
            ] {
                assert_eq!(
                    tab.exit(after),
                    1,
                    "a post-sudo {after:?} diagnosis cannot release the episode"
                );
            }
            assert_eq!(said.lock().unwrap().len(), 1, "the intro alone: {status:?}");
        }

        // Completed tools cannot hide a failed root helper transaction. This
        // follows the actual tab producer through its installed-tools fallback.
        let mut fake = console();
        fake.running_app = Some("/Applications/aterm.app".into());
        let sys = Arc::new(fake);
        let image = aterm_uds::lan::install::test_image(0x0100_000c, &[b"this build"]);
        sys.file("/Applications/aterm.app/Contents/MacOS/aterm", &image)
            .file(aterm_uds::lan::IMAGE_EXECUTABLE, &image)
            .file(
                aterm_uds::lan::PLIST_PATH,
                aterm_uds::lan::install::plist_text(Path::new("/Applications/aterm.app"))
                    .as_bytes(),
            )
            .on(
                "/bin/launchctl print system/systems.alab.aterm-lan",
                Ran::exit(113, "Could not find service"),
            );
        let (item, _, tab) = in_tab(&sys, Found::Missing("no developer directory".into()));
        sys.on(
            &argv(),
            Ran::exit(i32::from(Unfinished::Other.code().unwrap()), ""),
        )
        .on_write(&argv(), Path::new(CC), b"");
        let row = run(&sys, &item, Mode::Apply);
        assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
        assert_eq!(tab.unfinished(), Some(Unfinished::Other));
    }

    #[test]
    fn a_successful_installer_still_needs_a_working_compiler_and_can_recover() {
        for (unready, unfinished) in [
            (
                Found::Refuses("SDK cannot link".into()),
                Unfinished::Refuses,
            ),
            (
                Found::Unknown("compiler did not answer".into()),
                Unfinished::Unread,
            ),
        ] {
            let sys = Arc::new(console());
            let (mut item, _, tab) = in_tab(&sys, Found::Missing("not installed".into()));
            item.found = Box::new(move |sys: &dyn System| match sys.read_file(Path::new(CC)) {
                Ok(bytes) if bytes == b"ready" => Found::Present("compiler links".into()),
                Ok(_) => unready.clone(),
                Err(_) => Found::Missing("not installed".into()),
            });
            sys.on(&argv(), Ran::ok(""))
                .on_write(&argv(), Path::new(CC), b"unready");

            let row = run(&sys, &item, Mode::Apply);
            assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
            assert_eq!(tab.unfinished(), Some(unfinished));
            assert_eq!(asked(&sys), [argv()]);

            // Installing the files and exiting successfully is no compiler health proof.
            let row = run(&sys, &item, Mode::Apply);
            assert!(
                matches!(row.outcome, Outcome::Blocked | Outcome::Unknown),
                "{row:?}"
            );
            assert_eq!(tab.unfinished(), Some(unfinished));
            assert_eq!(
                asked(&sys),
                [argv()],
                "an unready read-back starts no second prompt"
            );

            sys.file(CC, b"ready");
            let row = run(&sys, &item, Mode::Apply);
            assert_eq!(row.outcome, Outcome::Ok, "{row:?}");
            assert_eq!(asked(&sys), [argv()], "recovery needs no new root episode");
        }
    }

    /// A stop the person sent while the root half ran (a Ctrl-C in the tab, which reaches the
    /// root half too) leaves the tab's run waiting for it: the root half finishes its step, and
    /// tools that arrived anyway end the tab on installed, whatever its exit said.
    #[test]
    fn in_the_tab_a_stop_waits_for_the_root_half_and_tools_that_arrived_are_installed() {
        let fake = console();
        fake.tty_model(|sys, _| {
            sys.stop_now();
            None
        });
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

    /// Each root code identifies only its own class and avoids generic process failures.
    /// The tab preserves the distinct codes used by its parent for download waits and stops.
    #[test]
    fn root_codes_are_distinct_and_tab_exit_codes_preserve_their_classes() {
        let mut codes = Vec::new();
        for why in Unfinished::ALL {
            match why {
                Unfinished::LowData => {
                    assert_eq!(
                        why.tab_exit(),
                        aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_LOW_DATA
                    );
                }
                Unfinished::NotAdmin => {
                    assert_eq!(
                        why.tab_exit(),
                        aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_NOT_ADMIN
                    );
                }
                Unfinished::Stopped | Unfinished::WindowClosed => {
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
                    assert_eq!(why.tab_exit(), 1, "{why:?}");
                }
            }
            if let Some(code) = why.code() {
                assert!(![0, 1, 2, 101, 126, 127].contains(&code), "{why:?}");
                assert!(!codes.contains(&code), "{why:?}");
                codes.push(code);
            }
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
