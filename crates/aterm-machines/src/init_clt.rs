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

/// The id of the read-only row of what setup's one elevation still owes beside the tools
/// ([`RootSteps`]): `--only base.clt` selects it too.
pub const ROOT_STEPS_ID: &str = "base.clt.root-steps";

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

/// The reader of generated OS names from the canonical number and nickname
/// ([`CommandLineTools::with_name`]), `None` before admission: read as the root half is about to
/// run, so that same naming choice rides the one password.
pub type NameMac = Box<dyn Fn() -> Option<aterm_types::machine_name::GeneratedNames> + Send + Sync>;

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
    /// The root half ran and the tools are there, but a root step beside them (developer mode,
    /// the Mac's name rule, the LAN broker) still reads owed ([`RootSteps`]).
    StepsOwed,
    /// Anything else.
    Other,
}

impl Unfinished {
    /// Every class, in code order.
    pub const ALL: [Self; 16] = [
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
        Self::StepsOwed,
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
            Self::Password
            | Self::Refuses
            | Self::NotOwnLogin
            | Self::LowData
            | Self::NotAdmin
            | Self::StepsOwed => None,
        }
    }

    /// What the tab's program exits with once it said this class: Low Data Mode, an account
    /// that is no administrator's, a stop and Apple's window closed without installing them have
    /// their own codes ([`aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_LOW_DATA`],
    /// [`aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_NOT_ADMIN`],
    /// [`aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_STOPPED`],
    /// [`aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_WINDOW_CLOSED`]), so the window gives
    /// the first's attempt back (it asked nothing, and the mode's end wakes it), offers no
    /// Install… for the second, records the third as a person's decline
    /// ([`aterm_types::clt_mark::End::Declined`]) and says where the fourth ended; every other
    /// class is 1. The window records the fourth and every 1 as unfinished, asked again by
    /// itself on the record's schedule: Apple's window gives no word of which button closed it
    /// (Not Now, Cancel, Done after its own failure, or a quit at logout), so its close is never
    /// read as a decline.
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
            Self::StepsOwed => {
                "developer mode, the Mac's name rule or the local network helper is still not set up"
            }
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

/// The root half's flag (`machines here apply --plain`) that leaves developer mode as it reads:
/// its baseline then carries no `[developer] debugger` ([`CommandLineTools::leaves_developer_mode`]).
/// It only narrows what the root half does.
pub const LEAVE_DEVELOPER_MODE: &str = "--leave-developer-mode";

/// The tab's last line once the tools read in place and no root step beside them reads owed, each
/// read back after the root half ran; the window then closes the tab unless it is its window's
/// last (`aterm-gui`'s `App::developer_tools_install_ended`).
pub const TAB_INSTALLED: &str = "Successfully completed: Apple's Command Line Tools and the \
                                 administrator's part of setup are in place. This tab closes by \
                                 itself.";

/// Clear the admitted tab's screen and scrollback after its go line was received.
pub(crate) const TAB_CLEAR: &str = "\x1b[H\x1b[2J\x1b[3J";

/// The same landing handshake for every explicit root-operation tab, before any elevation.
pub(crate) fn await_tab_go(sys: &dyn System, out: &Tab) -> bool {
    let go = sys.ask_kept("");
    if !matches!(&go, Ok(Some(line))
        if line.bytes() == aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_GO.as_bytes())
    {
        return false;
    }
    drop(go);
    out.write(TAB_CLEAR);
    true
}

/// The nonroot half of the fixed naming-permission tab. It carries no naming target.
pub(crate) fn name_rule_tab_entry(sys: &dyn System) -> u8 {
    let out = Tab::new(Arc::new(|text: &str| {
        use std::io::Write as _;
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(text.as_bytes());
        let _ = stdout.flush();
    }));
    if !await_tab_go(sys, &out) {
        return aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_NOT_STARTED;
    }
    let result = if crate::sessions::is_live_account() {
        std::env::current_exe()
            .map_err(|_| Unfinished::Other)
            .and_then(|aterm| {
                let item = CommandLineTools::new(
                    Box::new(|_| Found::Unknown("naming permission only".into())),
                    aterm,
                );
                *item
                    .root_plan
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = RootPlan::NameRuleOnly;
                item.apply_in_tab(sys, &out)
                    .map_err(|_| out.unfinished().unwrap_or(Unfinished::Other))
            })
    } else {
        Err(Unfinished::NotOwnLogin)
    };
    match result {
        Ok(()) => {
            out.line("Naming permission is ready.");
            0
        }
        Err(why) => {
            out.line(&format!(
                "Naming permission did not finish: {}.",
                why.reason()
            ));
            out.exit(why)
        }
    }
}

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
    /// The window that opened the Mac Setup tab read developer mode off as the person's own
    /// choice ([`Self::leaving_developer_mode`]).
    developer_mode_theirs: bool,
    /// The root work the last probe admitted ([`Self::read`]), which an apply re-reads within
    /// ([`Self::plan_after`]).
    root_plan: Mutex<RootPlan>,
}

/// The compiled root work admitted by the functional developer-tools probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootPlan {
    /// Missing tools: install them along with the independent root steps.
    WithTools,
    /// Preserve developer-tool selection and run only the independent root steps.
    StepsOnly,
    /// An explicit name-menu repair: only the existing naming permission, with no OS targets.
    NameRuleOnly,
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
            developer_mode_theirs: false,
            root_plan: Mutex::new(RootPlan::WithTools),
        }
    }

    /// This item as the Mac Setup tab's program of a window that read developer mode off as the
    /// person's own choice (`theirs`: they turned it off in aterm's Settings ▸ Security, or
    /// something they ran turned it off after aterm saw it on;
    /// [`aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_LEAVE_DEVELOPER_MODE`]): it is never
    /// owed, and the root half leaves it ([`Self::leaves_developer_mode`]).
    #[must_use]
    pub fn leaving_developer_mode(self, theirs: bool) -> Self {
        Self {
            developer_mode_theirs: theirs,
            ..self
        }
    }

    /// Whether this run leaves developer mode as it reads, owing nothing for it and telling the
    /// root half to leave it (`--leave-developer-mode`): a run nobody typed (the Mac Setup tab, or
    /// a run aterm decided) where the account's record says setup's elevation finished once
    /// ([`aterm_types::clt_mark::Record::finished`]: developer mode reading off since was turned
    /// off by someone after setup turned it on), or where the tab's window read it off as the
    /// person's choice ([`Self::leaving_developer_mode`]). A person's own typed `aterm init` sets
    /// it up whatever it reads: their run is their yes.
    fn leaves_developer_mode(&self, sys: &dyn System) -> bool {
        let typed = self.tab.is_none() && self.unbidden.as_ref().is_none_or(|u| u.explicit);
        !typed
            && (self.developer_mode_theirs
                || self
                    .unbidden
                    .as_ref()
                    .is_some_and(|unbidden| unbidden.finished(sys)))
    }

    /// [`Self::command_for`] `plan`, telling the root half to leave developer mode where this
    /// run does ([`Self::leaves_developer_mode`]).
    fn root_command(&self, sys: &dyn System, plan: RootPlan) -> Cmd {
        let command = self.command_for(plan);
        if self.leaves_developer_mode(sys) {
            command.arg(LEAVE_DEVELOPER_MODE)
        } else {
            command
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
    /// that is a typed canonical target; none names nothing.
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
    /// password automatically only where the account's record lets an ask go now (no other asker
    /// runs, no person declined the last ask, and its next try is due), sharing System State's
    /// Mac Setup tab's record ([`aterm_types::clt_mark`]), and never while the person's choices
    /// hold that tab, as read
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
    /// `name` (typed `--name` and optional `--nickname`): the one password that installs the tools
    /// also gives the Mac the name its vault and fleet know it by. Every run of it turns on
    /// developer mode ([`crate::root_steps::DeveloperMode`]), puts the rule in place that names
    /// the Mac at an admission that comes later ([`crate::root_steps::NameRule`]), and installs
    /// the root LAN broker ([`crate::items::lan::LanBroker`]) before the tools' download, so the
    /// root transaction also disables Spotlight data-volume indexing in
    /// `controller::commands`, so one password does every root step setup has.
    /// [`RootPlan::StepsOnly`] omits CLT
    /// installation and selection while retaining these independent steps.
    /// [`RootPlan::NameRuleOnly`] contains only the naming permission and no OS targets.
    #[must_use]
    pub fn baseline_named(
        plan: RootPlan,
        name: Option<&aterm_types::machine_name::GeneratedNames>,
    ) -> String {
        if plan == RootPlan::NameRuleOnly {
            return format!(
                "schema = {}\n\n[names]\nadmitted_name_rule = true\n",
                crate::SCHEMA
            );
        }
        let mut text = format!(
            "schema = {}\n\n[developer]\ncommand_line_tools = {}\ndebugger = true\n\n\
             [lan]\nbroker = true\n\n[names]\nadmitted_name_rule = true\n",
            crate::SCHEMA,
            plan == RootPlan::WithTools
        );
        if let Some(name) = name {
            text.push_str(&format!(
                "computer_name = \"{}\"\nlocal_host_name = \"{}\"\nhost_name = \"{}\"\n",
                name.computer_name(),
                name.host_name(),
                name.host_name(),
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
    /// knows its canonical name by now ([`Self::with_name`]), `--name`/`--nickname` carry it.
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

    fn is_explicit(&self) -> bool {
        self.unbidden.as_ref().is_some_and(|record| record.explicit)
    }

    /// What the elevation still owes this Mac beside the tools ([`Self::with_owed`]), less
    /// developer mode where this run leaves it ([`Self::leaves_developer_mode`]). Only the
    /// person's setup request may renew an old or unread naming permission; automatic runs
    /// retain the original metadata inventory and durable episode policy.
    fn owed_now(&self, sys: &dyn System) -> Vec<crate::root_steps::Owed> {
        let mut owed = (self.owed)(sys);
        if self.leaves_developer_mode(sys) {
            owed.retain(|step| *step != crate::root_steps::Owed::Debugger);
        }
        if self.is_explicit()
            && sys.macos()
            && sys
                .identity()
                .is_ok_and(|identity| identity.user.uid == sys.euid() && identity.user.uid != 0)
        {
            owed.retain(|step| *step != crate::root_steps::Owed::NameRule);
            if !matches!(crate::root_steps::name_rule_current(sys), Ok(true)) {
                owed.push(crate::root_steps::Owed::NameRule);
            }
        }
        owed
    }

    fn independent_plan(&self, owed: &[crate::root_steps::Owed]) -> Option<RootPlan> {
        if owed.is_empty() {
            None
        } else if self.is_explicit() && owed == [crate::root_steps::Owed::NameRule] {
            Some(RootPlan::NameRuleOnly)
        } else {
            Some(RootPlan::StepsOnly)
        }
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
        if ceiling == RootPlan::NameRuleOnly {
            return Ok((!self.is_explicit()
                || self
                    .owed_now(sys)
                    .contains(&crate::root_steps::Owed::NameRule))
            .then_some(RootPlan::NameRuleOnly));
        }
        if ceiling == RootPlan::StepsOnly {
            return Ok(self.independent_plan(&self.owed_now(sys)));
        }
        let (why, unfinished) = match (self.found)(sys) {
            Found::Missing(_) => return Ok(Some(RootPlan::WithTools)),
            Found::Present(_) => {
                return Ok(self.independent_plan(&self.owed_now(sys)));
            }
            Found::NotHere(_) => return Ok(None),
            Found::Refuses(why) => (why, Unfinished::Refuses),
            Found::Unknown(why) => (why, Unfinished::Unread),
        };
        let owed = self.owed_now(sys);
        if owed.is_empty() {
            Err((why, unfinished))
        } else {
            Ok(self.independent_plan(&owed))
        }
    }

    fn command_for(&self, plan: RootPlan) -> Cmd {
        let root = Cmd::new(SUDO)
            .args(["-k", "-p", TAB_PROMPT])
            .arg(self.aterm.as_os_str())
            .args(["machines", "here", "apply", "--plain"]);
        let root = match plan {
            RootPlan::WithTools => root,
            RootPlan::StepsOnly => root.arg("--root-steps-only"),
            RootPlan::NameRuleOnly => return root.arg("--name-rule-only"),
        };
        let Some(name) = (self.name)() else {
            return root;
        };
        let root = root.args(["--name", &name.number().to_string()]);
        match name.nickname() {
            Some(nickname) => root.args(["--nickname", nickname.as_str()]),
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
                if self.root_plan() != RootPlan::NameRuleOnly
                    && matches!((self.found)(sys), Found::Present(_))
                    && self.owed_now(sys).is_empty()
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
        if plan == RootPlan::NameRuleOnly {
            return "aterm is updating only its machine-naming permission. macOS asks for your Mac password once.";
        }
        match (plan != RootPlan::WithTools, in_tab) {
            (true, true) => TAB_INTRO_SETUP,
            (true, false) => INTRO_SETUP,
            (_, true) => TAB_INTRO,
            (_, false) => INTRO,
        }
    }
}

/// What holds back the ask of a run aterm decided ([`CommandLineTools::unbidden`]): this
/// account's record of setup's administrator ask, which System State's Mac Setup tab reads and
/// writes too ([`aterm_types::clt_mark`]), the process that asks, and the person's choices in
/// System State that hold that tab ([`held_by`]), read from the setup records as the item's probe
/// runs and again right before its apply claims the ask, so a Stop pressed after `aterm init`
/// began, while the base layer or the vault's join ran, still holds it (principle 4).
pub struct Unbidden {
    /// The record ([`aterm_types::clt_mark::path`]); `None` where it has no place, which holds
    /// the automatic ask.
    mark: Option<PathBuf>,
    /// This process, which asks ([`aterm_types::clt_mark::Asker::this_process`]); `None` where
    /// its birth cannot be read, which holds the automatic ask (the record could not tell a later
    /// asker whether it still runs).
    asker: Option<aterm_types::clt_mark::Asker>,
    /// An explicit person retry records its attempt but never borrows an automatic hold.
    explicit: bool,
    /// The person's choices now, as the setup records say ([`crate::init::Records::holds`]).
    holds: Arc<dyn Fn() -> crate::init::Holds + Send + Sync>,
    /// This run's claim of the episode ([`Self::claim`]), which only its end replaces.
    claim: Mutex<Option<aterm_types::clt_mark::Claim>>,
}

impl Unbidden {
    /// The record at `mark` and the process `asker` that asks, held as `holds` reads the
    /// person's choices each time it is called.
    #[must_use]
    pub fn new(
        mark: Option<PathBuf>,
        asker: Option<aterm_types::clt_mark::Asker>,
        holds: impl Fn() -> crate::init::Holds + Send + Sync + 'static,
    ) -> Self {
        Self {
            mark,
            asker,
            explicit: false,
            holds: Arc::new(holds),
            claim: Mutex::new(None),
        }
    }

    /// This account's record and this process of this build, held as `records` say each time
    /// they are read ([`held_by`]).
    #[must_use]
    pub fn here(records: &'static dyn crate::init::Records) -> Self {
        Self::new(
            aterm_types::clt_mark::path(),
            aterm_types::clt_mark::Asker::this_process(&aterm_stamp::build_id()),
            move || records.holds(),
        )
    }

    /// Whether the record says setup's elevation finished on this account once
    /// ([`aterm_types::clt_mark::Record::finished`]), as it reads through `sys`; `false` where it
    /// has no place or cannot be read.
    fn finished(&self, sys: &dyn System) -> bool {
        self.mark.as_deref().is_some_and(|mark| {
            aterm_types::clt_mark::record(sys.clt_mark(), mark, sys.now()).finished
        })
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
    /// `None` when it may ask: the person's choices hold it ([`Self::held`]), the record says no
    /// automatic ask goes now (another asker asks, the last attempt's next try is not due yet,
    /// a finished one's pause has not passed, or a person declined), or the record has no place
    /// or cannot say ([`UNREAD_RECORD`]), as the record reads through `sys` at `sys`'s clock
    /// ([`aterm_types::clt_mark::reading`]).
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
        if self.asker.is_none() {
            return Some(format!("{NOT_NOTED}: {PERSONS_STEP}"));
        }
        let now = sys.now();
        match aterm_types::clt_mark::reading(sys.clt_mark(), mark, now) {
            asked if asked.may_ask(now) => None,
            aterm_types::clt_mark::Asked::Asking => {
                Some(format!("{ASKING_ELSEWHERE}: {PERSONS_STEP}"))
            }
            aterm_types::clt_mark::Asked::Declined { due, .. } => Some(format!(
                "{ASK_DECLINED}; it asks again by itself in about {} days: {PERSONS_STEP}",
                due.saturating_sub(now).div_ceil(86_400)
            )),
            aterm_types::clt_mark::Asked::Unfinished { due, .. }
            | aterm_types::clt_mark::Asked::Done { due } => Some(format!(
                "{ASKS_AGAIN} in about {} min: {PERSONS_STEP}",
                due.saturating_sub(now).div_ceil(60)
            )),
            aterm_types::clt_mark::Asked::Available => None,
            aterm_types::clt_mark::Asked::Unread(error) => Some(format!(
                "{UNREAD_RECORD}, {} ({error}), so it asks for no Mac password by itself: \
                 {PERSONS_STEP}",
                mark.display()
            )),
        }
    }

    /// Claim the episode right before `sudo` asks: an automatic run's claim
    /// ([`aterm_types::clt_mark::claim`]) only where the record lets one go now, a person's
    /// ([`aterm_types::clt_mark::explicit`]) whatever it says. `true` when this run holds it.
    ///
    /// # Errors
    /// No place for the record, no readable birth of this process, and the claim's own.
    fn claim(&self, sys: &dyn System) -> std::io::Result<bool> {
        let mark = self
            .mark
            .as_deref()
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;
        let asker = self
            .asker
            .as_ref()
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;
        let claim = if self.explicit {
            Some(aterm_types::clt_mark::explicit(
                sys.clt_mark(),
                mark,
                asker,
                sys.now(),
            )?)
        } else {
            aterm_types::clt_mark::claim(sys.clt_mark(), mark, asker, sys.now())?
        };
        let held = claim.is_some();
        *self.claim.lock().unwrap_or_else(PoisonError::into_inner) = claim;
        Ok(held)
    }

    /// Record how this run's claimed attempt ended ([`aterm_types::clt_mark::end`]); nothing
    /// where it claimed none. A record that cannot be written keeps its `asking` line, which
    /// reads as cut off once this run ends: asked again by itself, as an interruption is.
    fn end(&self, sys: &dyn System, how: aterm_types::clt_mark::End) {
        let claim = self
            .claim
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let (Some(mark), Some(claim)) = (&self.mark, claim)
            && let Err(error) =
                aterm_types::clt_mark::end(sys.clt_mark(), mark, &claim, how, sys.now())
        {
            aterm_log::warn!(
                "aterm init: how aterm's ask for the Mac password ended was not recorded: {error}"
            );
        }
    }
}

/// How the attempt that ran the root half (`ran`, which `ended` reads) ends in the record, read
/// as the `CltEpisode` model's terminal exit ([`aterm_types::clt_mark::term_end`]): only a port
/// result that proves `sudo` never started gives the attempt back; done only where the read-back
/// after it (`settled`) finds the tools in place and no root step owed; a stop is the person's
/// decline only when the run caught a key at this terminal (`caught`, `SIGINT` or `SIGQUIT`),
/// while a hangup, a logout's `SIGTERM` or a stop the root helper reported with none caught here
/// is an interruption; anything else, a refused or untyped password, Apple's window closed
/// without the tools (whose button nothing tells) and a root half that exited 0 but left a step
/// owed included, is unfinished and asked again by itself on the record's doubling schedule.
fn episode_end(
    ran: &Result<Option<i32>, crate::port::RunFailed>,
    ended: Result<(), Unfinished>,
    settled: bool,
    caught: Option<i32>,
) -> aterm_types::clt_mark::End {
    use aterm_types::clt_mark::{Stop, TermExit, term_end};
    term_end(match (ran, ended) {
        (Err(crate::port::RunFailed::Spawn(_) | crate::port::RunFailed::Refused(_)), _) => {
            TermExit::NotStarted
        }
        (_, Ok(())) => TermExit::Finished { settled },
        (_, Err(Unfinished::Stopped)) => TermExit::Unfinished {
            stop: Some(match caught {
                Some(libc::SIGINT | libc::SIGQUIT) => Stop::Person,
                _ => Stop::Interruption,
            }),
        },
        (_, Err(_)) => TermExit::Unfinished { stop: None },
    })
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

/// Why a run aterm decided asks no password where a person declined setup's last ask (a Stop
/// or Ctrl-C at the password prompt, the Mac Setup tab closed while aterm ran on): no automatic
/// ask comes until its long pause passes ([`aterm_types::clt_mark::DECLINE_AGAIN`]) or their own
/// yes ([`aterm_types::clt_mark::Asked::Declined`]).
pub const ASK_DECLINED: &str = "you declined aterm's last ask for your Mac password for setup, so \
                                it asks for none by itself for now";

/// Why a run aterm decided asks no password while another asker's attempt runs: System State's
/// Mac Setup tab, or another run aterm decided ([`aterm_types::clt_mark::Asked::Asking`]).
pub const ASKING_ELSEWHERE: &str = "another aterm is asking for your Mac password for setup now";

/// Why a run aterm decided asks no password before the record's next try is due: the last
/// attempt did not finish, or finished, and aterm asks again by itself on the record's schedule
/// ([`aterm_types::clt_mark::ASK_AGAIN`]); the row goes on to say when.
pub const ASKS_AGAIN: &str = "aterm asked for your Mac password for setup a short while ago and \
                              asks again by itself";

/// Why a run aterm decided asks no password where the record of its own ask has no place, or
/// cannot say whether this build asked ([`aterm_types::clt_mark::Asked::Unread`]): the row goes on
/// to name where, and why.
pub const UNREAD_RECORD: &str = "aterm cannot read its record of asking for them";

/// Why a run aterm decided asks no password where a newer aterm's setup records stand
/// ([`held_by`]).
pub const NEWER: &str = "a newer aterm set this Mac up";

/// Why a run aterm decided asked no password after all: its claim of the ask in the record failed
/// (no hold on the record's lock, a write that failed, or no readable birth of this process), so
/// a later run could not tell that it asked, or whether it still runs.
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

/// Whether `row` is this item's, or [`RootSteps`]', deferred: to an install under way, to
/// Apple's installer window open for them, to a person at this Mac's console, or to setup's one
/// Mac password, which this run may not ask for (or asked for, leaving a step owed). None is this
/// run's to finish, so an apply or a plan completes with it, and `--check` reads it as not in
/// place.
#[must_use]
pub fn deferred(row: &aterm_converge::ItemReport) -> bool {
    (row.id == ID || row.id == ROOT_STEPS_ID) && row.outcome == aterm_converge::Outcome::Deferred
}

/// `steps` in plain words, `; ` between them.
fn plain_steps(steps: &[crate::root_steps::Owed]) -> String {
    steps
        .iter()
        .map(|step| step.plain())
        .collect::<Vec<_>>()
        .join("; ")
}

/// `base.clt.root-steps`: what setup's one elevation still owes this Mac beside the tools
/// (developer mode, the Mac's name rule, the LAN broker: [`crate::root_steps::owed`]), read fresh
/// after [`CommandLineTools`]' own row, whose one Mac password does them. It never asks and
/// changes nothing: in place where nothing is owed, else deferred, naming each step owed and who
/// does it (that row's password, which this run may not ask for and why, or which ran and left
/// it owed), so neither a run that may not ask nor a root half that left a step undone reads in
/// place. In the Mac Setup tab a step still owed after its password is the tab's unfinished
/// end ([`Unfinished::StepsOwed`]), unless the tools' own row said why first.
pub struct RootSteps<'a>(&'a CommandLineTools);

impl Item for RootSteps<'_> {
    fn id(&self) -> String {
        ROOT_STEPS_ID.to_owned()
    }

    fn family(&self) -> &'static str {
        "developer"
    }

    fn title(&self) -> String {
        "the administrator's part of setup (developer mode, the Mac's name rule, the local network \
         helper)"
            .to_owned()
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        let clt = self.0;
        let owed = clt.owed_now(sys);
        if owed.is_empty() {
            return Probe::Ok {
                now: "nothing owed".to_owned(),
            };
        }
        let steps = plain_steps(&owed);
        if clt.asked.load(std::sync::atomic::Ordering::Relaxed) {
            if let Some(tab) = &clt.tab
                && tab.unfinished().is_none()
            {
                tab.note(Unfinished::StepsOwed);
            }
            return Probe::Deferred {
                why: format!("still owed after this run's Mac password: {steps}"),
            };
        }
        let from = format!("still owed: {steps}");
        let why = match clt.held_back(sys, &from, false) {
            Some((Probe::Deferred { why } | Probe::Blocked { why }, _)) => why,
            Some(_) | None => format!("{from}; {ID}'s one Mac password does them"),
        };
        Probe::Deferred { why }
    }

    fn apply(&self, _sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        Err(format!(
            "{ROOT_STEPS_ID} reads only; {ID}'s one Mac password does its steps"
        ))
    }
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
        // The record is claimed before sudo can ask, once this run owns the terminal (a vault
        // question can own it first): the queued root work, Stop and the automatic ask's holds
        // are read again then, immediately before the root episode starts. Explicit retries
        // bypass its withholding decision, but claim the same record, so its end decides the next
        // automatic ask. A refusal here starts no child and keeps its own words; work no longer
        // owed by then asks nothing and is no failure.
        let mut refused: Option<String> = None;
        let mut skipped = false;
        let mut ran_plan = plan;
        // The root half runs in this terminal's foreground, so a Ctrl-C (which the
        // through-installer wait invites) or a closed terminal reaches this run too: held until
        // the root half has finished its step and aterm's own ask is kept or taken back, then
        // sent again, so the run still stops as it would have.
        let stops = sys.defer_stops();
        let ran = sys.run_tty(&self.root_command(sys, plan), &mut |command| {
            // Re-read work under the terminal lease, bounded by the plan this apply admitted.
            let fresh = self.plan_after(sys, plan);
            if let Ok(Some(plan)) = &fresh {
                *command = self.root_command(sys, *plan);
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
                refused = Some(why.clone());
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
                    refused = Some(why.clone());
                    return Err(crate::port::RunFailed::Refused(why));
                }
            };
            if let Some((why, _)) = admission {
                refused = Some(why.clone());
                return Err(crate::port::RunFailed::Refused(why));
            }
            ran_plan = plan;
            *self.root_plan.lock().unwrap_or_else(PoisonError::into_inner) = plan;
            if let Some(record) = &self.unbidden {
                let why = match record.claim(sys) {
                    Ok(true) => None,
                    Ok(false) => Some(record.withheld(sys).unwrap_or_else(|| {
                        format!("{ASKING_ELSEWHERE}: {PERSONS_STEP}")
                    })),
                    Err(error) if record.explicit => {
                        aterm_log::warn!(
                            "aterm init: could not record the explicit administrator attempt: {error}"
                        );
                        None
                    }
                    Err(error) => Some(format!("{NOT_NOTED} ({error}): {PERSONS_STEP}")),
                };
                if let Some(why) = why {
                    refused = Some(why.clone());
                    return Err(crate::port::RunFailed::Refused(why));
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
        if let Some(why) = refused {
            drop(stops);
            return Err(why);
        }
        let caught = sys.stopped();
        let ended = self.ended(sys, &ran, caught);
        if let Some(record) = &self.unbidden {
            // Done only as read back now: no root step owed, and the tools in place where the
            // root half was to install them (a steps-only or naming-permission run leaves a
            // refusing or unread compiler as it was, which no password mends).
            let settled = self.owed_now(sys).is_empty()
                && (ran_plan != RootPlan::WithTools
                    || matches!((self.found)(sys), Found::Present(_)));
            record.end(sys, episode_end(&ran, ended, settled, caught));
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
    /// decided, the person's Stop as the setup records read now, or a record that says no
    /// automatic ask goes now or cannot say ([`Unbidden::withheld`]: deferred, naming the
    /// person's step); else the change. With the tools there, whether they build or refuse to,
    /// or a compiler that did not answer, the change is the elevation's other root steps where
    /// any is owed ([`Self::with_owed`]) and this run may ask for the password now
    /// ([`Self::read_owed`]): the root steps never wait on the compiler's verdict, and run alone
    /// ([`RootPlan::StepsOnly`]), installing and selecting no tools. A refusing or unanswered
    /// compiler first defers to confirmed installer custody ([`Self::installer_hold`]). Once this
    /// run's root half ran, its read-back says the tools alone: what is still owed is
    /// [`RootSteps`]' row.
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
            Found::Present(now) => {
                let found = now.clone();
                return self.read_owed(sys, (Probe::Ok { now }, None), &found);
            }
            Found::Refuses(why) => (why, Some(Unfinished::Refuses)),
            Found::Unknown(why) => (why, Some(Unfinished::Unread)),
            Found::NotHere(why) => return (Probe::NotApplicable { why }, None),
            Found::Missing(from) => (from, None),
        };
        if let Some((probe, unfinished)) = Self::installer_hold(sys, &from, unready.is_none()) {
            return (probe, Some(unfinished));
        }
        if let Some(unfinished) = unready {
            let verdict = if unfinished == Unfinished::Refuses {
                Probe::Blocked { why: from.clone() }
            } else {
                Probe::Unusable { why: from.clone() }
            };
            return self.read_owed(sys, (verdict, Some(unfinished)), &from);
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

    /// [`Self::read`] where a compiler is there (`found`, what the probe read of it), whose own
    /// reading is `verdict`: the change for the elevation's other root steps where any is owed
    /// and this run may ask for the password now, else `verdict`. In the tab a run that may not
    /// ask says why ([`Self::held_back`]), as it would for the tools (Low Data Mode and an
    /// install under way aside, which hold only a download); outside it the row says the tools'
    /// verdict alone, and [`RootSteps`]' row names what is left and who does it, so a step owed
    /// never reads in place. Once this run's root half ran, its read-back is `verdict`.
    fn read_owed(
        &self,
        sys: &dyn System,
        verdict: (Probe, Option<Unfinished>),
        found: &str,
    ) -> (Probe, Option<Unfinished>) {
        if self.asked.load(std::sync::atomic::Ordering::Relaxed) {
            return verdict;
        }
        let owed = self.owed_now(sys);
        if owed.is_empty() {
            return verdict;
        }
        let plan = self.independent_plan(&owed).expect("nonempty root work");
        *self
            .root_plan
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = plan;
        let from = format!("{found}; still owed: {}", plain_steps(&owed));
        // The flag the root half will run with, as `command_for` spells it for `plan`.
        let only = if plan == RootPlan::NameRuleOnly {
            "--name-rule-only"
        } else {
            "--root-steps-only"
        };
        match self.held_back(sys, &from, false) {
            Some(held) if self.tab.is_some() => held,
            Some(_) => verdict,
            None => (
                Probe::Change {
                    from,
                    to: format!(
                        "done as root by `{} machines here apply --plain {only}` (one password prompt)",
                        Cmd::new(SUDO).arg(self.aterm.as_os_str()).display()
                    ),
                },
                None,
            ),
        }
    }

    /// The read-only row of what the elevation still owes this Mac beside the tools
    /// ([`RootSteps`]), reading what this item reads.
    #[must_use]
    pub fn steps(&self) -> RootSteps<'_> {
        RootSteps(self)
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
            && self.owed_now(sys).is_empty()
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
        let ran = sys.run_tty(&self.root_command(sys, plan), &mut |command| {
            let fresh = self.plan_after(sys, plan);
            if let Ok(Some(plan)) = &fresh {
                *command = self.root_command(sys, *plan);
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
            *self
                .root_plan
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = plan;
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
#[path = "init_clt_tests.rs"]
mod tests;
