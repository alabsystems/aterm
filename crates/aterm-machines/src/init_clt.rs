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
    /// Another install of them runs: `softwareupdate`, `installer` or Apple's installer window
    /// holds the placeholder (the window while it or the placeholder is younger than
    /// [`aterm_types::macos_setup::APPLE_WINDOW_INSTALL_BOUND`]), or a live run of the root half
    /// stamped it.
    Busy,
    /// A stop ended the install before the tools arrived: a signal the tab's run caught (a
    /// person's Ctrl-C, at `sudo`'s password prompt or during the install), or the root half's
    /// own stop.
    Stopped,
    /// Apple's own installer window, opened where the catalog lists none, did not install them
    /// in time.
    AppleWindow,
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
    pub const ALL: [Self; 14] = [
        Self::Password,
        Self::Catalog,
        Self::NoneOffered,
        Self::Download,
        Self::Installer,
        Self::Busy,
        Self::Stopped,
        Self::AppleWindow,
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
            Self::Password
            | Self::Refuses
            | Self::Unread
            | Self::NotOwnLogin
            | Self::LowData
            | Self::NotAdmin => None,
        }
    }

    /// What the tab's program exits with once it said this class: Low Data Mode, an account
    /// that is no administrator's and a stop have their own codes
    /// ([`aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_LOW_DATA`],
    /// [`aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_NOT_ADMIN`],
    /// [`aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_STOPPED`]), so the window spends no
    /// automatic opening on the first, offers no Install… for the second, and keeps the third's
    /// opening spent, a person's decline; every other class is 1.
    #[must_use]
    pub const fn tab_exit(self) -> u8 {
        match self {
            Self::LowData => aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_LOW_DATA,
            Self::NotAdmin => aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_NOT_ADMIN,
            Self::Stopped => aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_STOPPED,
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
            Self::AppleWindow => "Apple's installer window did not install them",
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

/// The tab's password prompt (`sudo -p`).
pub const TAB_PROMPT: &str = "Mac password: ";

/// The tab's last line once the tools are installed.
pub const TAB_INSTALLED: &str = "Successfully completed: Apple's Command Line Tools are installed. \
                                 You can close this tab.";

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
}

impl CommandLineTools {
    /// The item over `found`.
    #[must_use]
    pub fn new(found: ProbeClt, aterm: PathBuf) -> Self {
        Self {
            found,
            aterm,
            tab: None,
        }
    }

    /// This item as the Command Line Tools tab's program speaks it, through `tab`.
    #[must_use]
    pub fn in_tab(self, tab: Tab) -> Self {
        Self {
            tab: Some(tab),
            ..self
        }
    }

    /// The one-table baseline the root half applies.
    #[must_use]
    pub fn baseline() -> String {
        format!(
            "schema = {}\n\n[developer]\ncommand_line_tools = true\n",
            crate::SCHEMA
        )
    }

    /// The argv the apply runs on the person's terminal: the root half `--plain`, which applies
    /// [`Self::baseline`] built in and reads no file the person's account could write; in the
    /// tab, with the plain prompt.
    #[must_use]
    pub fn command(&self) -> Cmd {
        let sudo = Cmd::new(SUDO);
        let sudo = if self.tab.is_some() {
            sudo.args(["-p", TAB_PROMPT])
        } else {
            sudo
        };
        sudo.arg(self.aterm.as_os_str())
            .args(["machines", "here", "apply", "--plain"])
    }
}

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

/// Why the item leaves the tools to an install under way ([`installing`]).
pub const INSTALLING: &str = "Apple's Command Line Tools are being installed now, by an install \
                              already under way";

/// Why the item asks nobody for a password in an account that is no administrator's.
pub const NOT_ADMIN: &str = "only an administrator of this Mac can install them, and this \
                             account is not one: an administrator runs `aterm init --only \
                             base.clt` in their own login";

/// Whether an install of the tools runs now, by the predicate the window's page and the root
/// half share ([`aterm_types::macos_setup::tools_install_running`],
/// [`crate::items::developer::installer_runs`]): the on-demand placeholder, held by
/// `softwareupdate`, `installer` or Apple's installer window while it or the placeholder is
/// younger than [`aterm_types::macos_setup::APPLE_WINDOW_INSTALL_BOUND`] (an age that cannot be
/// read included), or stamped by a run of the root half that is still alive
/// ([`crate::items::developer::placeholder_held_by_a_live_run`]: one between its steps, or
/// waiting on Apple's installer window after a person closed it), which alone may hold or replace
/// it. A process table that cannot be read is no install here (the root half then says whether
/// one holds the placeholder).
fn installing(sys: &dyn System) -> bool {
    sys.stat(std::path::Path::new(crate::items::developer::PLACEHOLDER))
        .is_ok()
        && (crate::items::developer::installer_runs(sys) == Some(true)
            || crate::items::developer::placeholder_held_by_a_live_run(sys))
}

/// Whether `row` is this item's, deferred: to an install under way, or to a person at this Mac's
/// console. Neither is this run's to finish, so an apply or a plan completes with it.
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
        // The root half runs in this terminal's foreground, so a Ctrl-C (which the
        // through-installer wait invites) or a closed terminal reaches this run too: held until
        // `sudo` has forgotten the password it cached, then sent again, so the run still stops
        // as it would have.
        let stops = sys.defer_stops();
        let ran = sys.run_tty(&self.command());
        // The root half is over: `sudo` forgets the password it cached, on a person's own run
        // as in the tab ([`Self::apply_in_tab`]).
        let _ = sys.run(&Cmd::new(SUDO).arg("-K"));
        drop(stops);
        match ran {
            Ok(code) => Unfinished::after_root(code).map_err(|why| why.reason().to_owned()),
            Err(e) => Err(format!("`sudo` did not start: {e}")),
        }
    }
}

impl CommandLineTools {
    /// What the probe reads, and why the tab's install would not finish when it reads short of
    /// a change: the tools present; else, with them missing, an install already under way
    /// (deferred to it, asking nothing), a run with no person at this Mac's console (deferred to
    /// one: only a person there types the Mac password, never an agent's tab, a script or a
    /// remote shell), no terminal to ask in, Low Data Mode, or an account that is no
    /// administrator's (each blocked, asking nothing); else the change.
    fn read(&self, sys: &dyn System) -> (Probe, Option<Unfinished>) {
        let from = match (self.found)(sys) {
            Found::Present(now) => return (Probe::Ok { now }, None),
            Found::Refuses(why) => return (Probe::Blocked { why }, Some(Unfinished::Refuses)),
            Found::Unknown(why) => return (Probe::Unusable { why }, Some(Unfinished::Unread)),
            Found::NotHere(why) => return (Probe::NotApplicable { why }, None),
            Found::Missing(from) => from,
        };
        if installing(sys) {
            return (
                Probe::Deferred {
                    why: format!("{from}; {INSTALLING}"),
                },
                Some(Unfinished::Busy),
            );
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
    /// half, which finishes its step), drops `sudo`'s cached password, and says how it ended:
    /// installed when the tools are there, whatever the root half's exit, else its plain reason
    /// ([`Unfinished`]), never an argv or a path.
    fn apply_in_tab(&self, sys: &dyn System, tab: &Tab) -> Result<(), String> {
        if !sys.has_tty() {
            return Err(tab.note(Unfinished::Other));
        }
        let stops = sys.catch_stops();
        tab.line(TAB_INTRO);
        let ran = sys.run_tty(&self.command());
        let stopped = sys.stopped();
        // Still held: a second Ctrl-C now cannot end the run before `sudo` forgets the password.
        let _ = sys.run(&Cmd::new(SUDO).arg("-K"));
        drop(stops);
        // A stop this run caught is the person's, whatever `sudo` or the root half then exited
        // with: a Ctrl-C at the password prompt ends `sudo` on its own 1, which alone would read
        // as a password macOS refused.
        let ended = match ran {
            Ok(code) if code != Some(0) && stopped.is_some() => Err(Unfinished::Stopped),
            Ok(code) => Unfinished::after_root(code),
            Err(_) => Err(Unfinished::Other),
        };
        match ended {
            Ok(()) => Ok(()),
            Err(_) if matches!((self.found)(sys), Found::Present(_)) => Ok(()),
            Err(why) => Err(tab.note(why)),
        }
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

        let argv = format!("tty /usr/bin/sudo {ATERM} machines here apply --plain");
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

    /// A person's own `aterm init` at the console (no tab) installs through `sudo` too, and the
    /// password `sudo` cached goes once the root half ends, whether it finished or not (design
    /// §6.3: `sudo -K` runs afterwards).
    #[test]
    fn a_persons_own_run_drops_sudos_cached_password_after_the_root_half() {
        let argv = format!("tty /usr/bin/sudo {ATERM} machines here apply --plain");
        for (ran, outcome) in [
            (Ran::ok(""), Outcome::Changed),
            (Ran::exit(82, ""), Outcome::Failed),
        ] {
            let sys = console();
            let installs = ran.success();
            sys.on(&argv, ran);
            if installs {
                sys.on_write(&argv, std::path::Path::new(CC), b"");
            }
            let row = run(&sys, &missing(), Mode::Apply);
            assert_eq!(row.outcome, outcome, "{row:?}");
            let calls = sys.calls();
            let at = |call: &str| {
                calls
                    .iter()
                    .position(|seen| seen == call)
                    .unwrap_or_else(|| panic!("no {call:?}: {calls:?}"))
            };
            assert!(
                at(&argv) < at("/usr/bin/sudo -K"),
                "`sudo -K` after the root half on a person's own run: {calls:?}"
            );
        }
    }

    /// The root half's failure says why in plain words on a person's own run too, never the argv
    /// it ran.
    #[test]
    fn a_person_s_own_run_says_why_in_plain_words_never_the_argv() {
        let sys = console();
        sys.on(
            &format!("tty /usr/bin/sudo {ATERM} machines here apply --plain"),
            Ran::exit(82, ""),
        );
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
    /// package `installer`, Apple's installer window while it or the placeholder is younger than
    /// [`aterm_types::macos_setup::APPLE_WINDOW_INSTALL_BOUND`], or a live run of the root half),
    /// the item asks no password and fails nothing: it is deferred to that install. A placeholder
    /// left behind beside other programs, one merely called `installer` and Apple's window left
    /// open past that bound beside a placeholder as old included, is no install.
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

        // Apple's installer window beside the placeholder is the install a person started there
        // while the window, or the placeholder its press created, is younger than its bound (an
        // age nothing can read included): deferred, asking no password. So a press in a window
        // opened hours before defers too. One left open past the bound beside a placeholder as
        // old waits for a press beside a placeholder an ended install left, for days perhaps:
        // no install, so it never defers `aterm init` for good.
        let apple = "/System/Library/CoreServices/Install Command Line Developer Tools.app/\
                     Contents/MacOS/Install Command Line Developer Tools";
        let placeholder = crate::items::developer::PLACEHOLDER;
        let bound = aterm_types::macos_setup::APPLE_WINDOW_INSTALL_BOUND.as_secs();
        let window = |age: Option<u64>, changed: Option<u64>| {
            let sys = console();
            sys.file(placeholder, b"")
                .running(&["/sbin/launchd", apple]);
            if let Some(age) = age {
                sys.born(Fake::PIDS + 1, sys.now - age);
            }
            if let Some(changed) = changed {
                sys.changed_at(placeholder, sys.now - changed);
            }
            sys
        };
        for (age, changed) in [
            (Some(60), Some(bound)),
            (Some(bound - 1), Some(bound)),
            (None, Some(bound)),
            (Some(bound), Some(60)),
            (Some(bound), Some(bound - 1)),
            (Some(bound), None),
        ] {
            let young = window(age, changed);
            let row = run(&young, &missing(), Mode::Apply);
            assert_eq!(
                row.outcome,
                Outcome::Deferred,
                "{age:?} {changed:?}: {row:?}"
            );
            assert!(
                row.why
                    .as_deref()
                    .is_some_and(|why| why.ends_with(INSTALLING)),
                "{age:?} {changed:?}: {row:?}"
            );
            assert!(
                asked(&young).is_empty(),
                "{age:?} {changed:?}: {:?}",
                young.calls()
            );
        }
        assert_eq!(
            run(&window(Some(bound), Some(bound)), &missing(), Mode::Plan).outcome,
            Outcome::Planned
        );

        // A run of the root half that is still alive holds it, though no installer process
        // runs this moment (it waits on Apple's installer window a person closed): a second
        // `aterm init` defers to it, asking no password.
        let held = console();
        held.file(crate::items::developer::PLACEHOLDER, b"aterm 4242 1000\n")
            .running(&["/sbin/launchd"])
            .born(4242, 1000);
        let row = run(&held, &missing(), Mode::Apply);
        assert_eq!(row.outcome, Outcome::Deferred, "{row:?}");
        assert!(asked(&held).is_empty(), "{:?}", held.calls());
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

    fn tab_argv() -> String {
        format!("tty /usr/bin/sudo -p 'Mac password: ' {ATERM} machines here apply --plain")
    }

    /// In the tab, the apply says its one intro line right before `sudo` asks for the Mac
    /// password with the plain prompt, runs the root half `--plain`, holds off a stop while it
    /// runs, and drops `sudo`'s cached password once it ended; outside it the argv keeps
    /// `sudo`'s own prompt (pinned by
    /// `missing_tools_plan_the_machines_here_argv_and_apply_runs_it_on_the_terminal`).
    #[test]
    fn in_the_tab_the_intro_comes_right_before_sudo_asks_with_the_plain_prompt() {
        let sys = Arc::new(console());
        let (item, said, tab) = in_tab(&sys, Found::Missing("no developer directory".into()));
        let argv = tab_argv();
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
        assert!(ran < at("/usr/bin/sudo -K"), "{calls:?}");
        assert!(
            at("/usr/bin/sudo -K") < at("stops restored"),
            "stops are held until `sudo` forgot the password: {calls:?}"
        );
        assert_eq!(tab.unfinished(), None);
    }

    /// A person's own `aterm init` at the console runs the root half in its own terminal's
    /// foreground, so a Ctrl-C there (which the through-installer wait invites: "Ctrl-C stops
    /// waiting") reaches this run too. It is held until `sudo -K` has run, then sent again: the
    /// password `sudo` cached never outlives the run, and the run still stops as it would have.
    #[test]
    fn a_persons_own_run_holds_a_stop_until_sudo_forgets_the_password() {
        let argv = format!("tty /usr/bin/sudo {ATERM} machines here apply --plain");
        let mut sys = console();
        sys.stop_after = Some(0);
        sys.on(&argv, Ran::ok(""))
            .on_write(&argv, std::path::Path::new(CC), b"");
        let _ = run(&sys, &missing(), Mode::Apply);
        let calls = sys.calls();
        let at = |call: &str| calls.iter().position(|seen| seen == call);
        let ran = at(&argv).expect("the root half ran");
        let forgot = at("/usr/bin/sudo -K").expect("sudo -K ran");
        assert!(
            at("defer stops").is_some_and(|held| held < ran),
            "no stop is held around the root half: {calls:?}"
        );
        assert!(
            at("stops restored").is_some_and(|restored| restored > forgot),
            "stops are restored before sudo -K: {calls:?}"
        );
        assert!(
            calls
                .iter()
                .position(|seen| seen.starts_with("stop sent again"))
                .is_some_and(|sent| sent > forgot),
            "the stop is sent again once sudo -K ran: {calls:?}"
        );
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
            Some(88),
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
                &tab_argv(),
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
        let argv = tab_argv();
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
                Unfinished::Stopped => {
                    assert_eq!(
                        why.line(),
                        format!(
                            "The install did not finish: {reason}. Install\u{2026} on System State tries again."
                        ),
                    );
                    assert_eq!(
                        why.tab_exit(),
                        aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_STOPPED
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
        assert_eq!(codes.len(), 8);
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
