// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Selectable groups and durable plan types for the local Mac workstation profile.

/// The default ALab vault on the local network, advertised through mDNS.
pub const DEFAULT_VAULT_URL: &str = "http://alab-secrets.local:7484/v1/alab";

/// Every compiled-in ALab vault URL a client probes and joins across ([`DEFAULT_VAULT_URL`] first).
pub const DEFAULT_VAULT_URLS: &[&str] = &[DEFAULT_VAULT_URL];

/// Report row name for setup's Secrets stage.
pub const SECRETS_VAULT_ROW: &str = "Secrets vault";

/// Report row name indicating whether the vault subscriber runs.
pub const SECRETS_SUBSCRIBER_ROW: &str = "Secrets subscriber";

/// Independently selectable part of the workstation profile, with discriminants as bit positions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Setting {
    /// Persistent and active Caps Lock modifier mappings.
    CapsLock = 0,
    /// Saved and active scroll direction.
    NaturalScrolling = 1,
    /// Internal and Bluetooth trackpad scrolling behavior.
    TrackpadScrolling = 2,
    /// Desktop icons and wallpaper clicks.
    Desktop = 3,
    /// Desktop, Stage Manager, and Notification Center widgets.
    Widgets = 4,
    /// Dock auto-hide, pins, extras, recent applications and minimization.
    Dock = 5,
    /// Durable Messages account sign-out.
    Messages = 6,
    /// Durable FaceTime account sign-out.
    FaceTime = 7,
    /// Keep rustup's offline documentation and Rust build folders out of Spotlight.
    Spotlight = 8,
    /// Guide the person through the app's required macOS grants.
    Permissions = 9,
    /// Join and continuously synchronize the credentials vault.
    Secrets = 10,
    /// Authenticate GitHub and clone the credential's own repositories (no forks or archived ones).
    Repositories = 11,
}

impl Setting {
    /// Every checklist row, in presentation order.
    pub const ALL: [Self; 12] = [
        Self::CapsLock,
        Self::NaturalScrolling,
        Self::TrackpadScrolling,
        Self::Desktop,
        Self::Widgets,
        Self::Dock,
        Self::Messages,
        Self::FaceTime,
        Self::Spotlight,
        Self::Permissions,
        Self::Secrets,
        Self::Repositories,
    ];

    /// Whether this row belongs to the original macOS preference/account profile.
    #[must_use]
    pub const fn is_workstation(self) -> bool {
        (self as u8) < 8
    }

    /// The complete effect described by this checklist row.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::CapsLock => "Use Caps Lock as Control",
            Self::NaturalScrolling => "Turn off natural scrolling",
            Self::TrackpadScrolling => {
                "Enable vertical, horizontal and momentum trackpad scrolling"
            }
            Self::Desktop => "Show desktop icons; keep windows in place when clicking wallpaper",
            Self::Widgets => {
                "Hide desktop and Stage Manager widgets; clear Notification Center widgets once aterm has Full Disk Access"
            }
            Self::Dock => {
                "Dock: automatically hide; only aterm, System Settings and Safari; no stacks or recents; minimize into app icons"
            }
            Self::Messages => "Sign out of Messages",
            Self::FaceTime => "Sign out of FaceTime",
            Self::Spotlight => "Keep Rust offline documentation and build folders out of Spotlight",
            Self::Permissions => "Set up aterm's macOS permissions",
            Self::Secrets => "Sync credentials from the secrets vault",
            Self::Repositories => "Set up GitHub and clone your repositories into home",
        }
    }
}

impl Setting {
    /// The page group this setting is shown in.
    #[must_use]
    pub const fn group(self) -> Group {
        match self {
            Self::Permissions => Group::Access,
            Self::CapsLock | Self::NaturalScrolling | Self::TrackpadScrolling => Group::Input,
            Self::Desktop | Self::Widgets | Self::Dock => Group::Desktop,
            Self::Messages | Self::FaceTime => Group::Accounts,
            Self::Spotlight | Self::Secrets | Self::Repositories => Group::Developer,
        }
    }

    /// The short name a System State row shows for this setting.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::CapsLock => "Caps Lock key",
            Self::NaturalScrolling => "Natural scrolling",
            Self::TrackpadScrolling => "Trackpad scrolling",
            Self::Desktop => "Desktop",
            Self::Widgets => "Widgets",
            Self::Dock => "Dock",
            Self::Messages => "Messages",
            Self::FaceTime => "FaceTime",
            Self::Spotlight => "Rust in Spotlight",
            Self::Permissions => "Permissions",
            Self::Secrets => "Secrets vault",
            Self::Repositories => "GitHub repositories",
        }
    }

    /// What aterm wants this setting to read, in the words a row shows.
    #[must_use]
    pub const fn wanted(self) -> &'static str {
        match self {
            Self::CapsLock => "Control",
            Self::NaturalScrolling => "Off",
            Self::TrackpadScrolling => "All on",
            Self::Desktop => "Icons shown",
            Self::Widgets => "Hidden",
            Self::Dock => "3 apps",
            Self::Messages | Self::FaceTime => "Signed out",
            Self::Spotlight => "Kept out",
            Self::Permissions => "On",
            Self::Secrets => "In sync",
            Self::Repositories => "All",
        }
    }
}

/// Every checklist row, also available as [`Setting::ALL`].
pub const ALL: [Setting; 12] = Setting::ALL;

/// One card on the System State page. Presentation only: groups are never
/// persisted, and a group's intent is the bits of its settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Group {
    /// The macOS grants aterm itself needs: disk, developer tools, debugger, network,
    /// accessibility. Shown as "Permissions".
    Access,
    /// Caps Lock, scrolling direction and trackpad gestures.
    Input,
    /// Desktop icons, widgets and the Dock.
    Desktop,
    /// Messages and FaceTime accounts.
    Accounts,
    /// Rust in Spotlight, the secrets vault and GitHub repositories.
    Developer,
}

impl Group {
    /// Every group, in page order.
    pub const ALL: [Self; 5] = [
        Self::Access,
        Self::Input,
        Self::Desktop,
        Self::Accounts,
        Self::Developer,
    ];

    /// The card's title.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Access => "Permissions",
            Self::Input => "Keyboard & Trackpad",
            Self::Desktop => "Desktop & Dock",
            Self::Accounts => "Messages & FaceTime",
            Self::Developer => "Workspace",
        }
    }

    /// The settings this group's switch governs, in row order.
    #[must_use]
    pub const fn settings(self) -> &'static [Setting] {
        match self {
            Self::Access => &[Setting::Permissions],
            Self::Input => &[
                Setting::CapsLock,
                Setting::NaturalScrolling,
                Setting::TrackpadScrolling,
            ],
            Self::Desktop => &[Setting::Desktop, Setting::Widgets, Setting::Dock],
            Self::Accounts => &[Setting::Messages, Setting::FaceTime],
            Self::Developer => &[Setting::Spotlight, Setting::Secrets, Setting::Repositories],
        }
    }
}

/// One macOS grant shown in the Access group. All of them ride the
/// [`Setting::Permissions`] bit: the group is one intent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Access {
    /// Full Disk Access for aterm (System Settings ▸ Privacy & Security).
    FullDiskAccess,
    /// aterm's switch in the Developer Tools list.
    DeveloperTools,
    /// macOS developer mode (`DevToolsSecurity`): debugging without a password.
    Debugger,
    /// Local Network access for aterm and what runs in it.
    LocalNetwork,
    /// Accessibility, which the Messages and FaceTime sign-outs need.
    Accessibility,
}

impl Access {
    /// Every access row, in page order.
    pub const ALL: [Self; 5] = [
        Self::FullDiskAccess,
        Self::DeveloperTools,
        Self::Debugger,
        Self::LocalNetwork,
        Self::Accessibility,
    ];

    /// The row's name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::FullDiskAccess => "Full Disk Access",
            Self::DeveloperTools => "Developer Tools",
            Self::Debugger => "Debugger access",
            Self::LocalNetwork => "Local Network",
            Self::Accessibility => "Accessibility",
        }
    }

    /// What aterm wants the grant to read.
    #[must_use]
    pub const fn wanted(self) -> &'static str {
        match self {
            Self::FullDiskAccess
            | Self::DeveloperTools
            | Self::Debugger
            | Self::LocalNetwork
            | Self::Accessibility => "On",
        }
    }

    /// Whether this grant is needed for `selection`. Accessibility is needed only
    /// while an account sign-out is managed; otherwise its row reads Not needed.
    #[must_use]
    pub const fn needed_for(self, selection: Selection) -> bool {
        match self {
            Self::Accessibility => {
                selection.contains(Setting::Messages) || selection.contains(Setting::FaceTime)
            }
            _ => true,
        }
    }
}

/// One kind of credential that aterm keeps in step across a person's Macs, each a row in the
/// Secrets group's "Your other Macs" (`docs/DESIGN-init-on-by-default-2026-10-25.md` §8.5, §8.7).
/// Every kind rides [`Setting::Secrets`] (no plan bit of its own, so an older aterm reads every
/// plan and a rollback does not freeze setup); a row stands apart only to say where that kind
/// stands and to hold its own off switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SyncKind {
    /// The other Macs found on this network or in the fleet book.
    Peers,
    /// The git identity (`aterm-identity`, or the GitHub account).
    Identity,
    /// The aterm fleet (`aterm-fleet-code`).
    Fleet,
    /// The AI account roster (logins stay on each Mac).
    Accounts,
    /// Shared data (Commons), within its admission boundary.
    SharedData,
    /// This Mac's name from the inventory (ALab only).
    Name,
}

impl SyncKind {
    /// Every kind, in page order.
    pub const ALL: [Self; 6] = [
        Self::Peers,
        Self::Identity,
        Self::Fleet,
        Self::Accounts,
        Self::SharedData,
        Self::Name,
    ];

    /// The row's stable human label.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Peers => "Your other Macs",
            Self::Identity => "Git identity",
            Self::Fleet => "Fleet",
            Self::Accounts => "AI accounts",
            Self::SharedData => "Shared data",
            Self::Name => "Name",
        }
    }
}

/// What a System State row is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Subject {
    /// One selectable setting.
    Setting(Setting),
    /// One grant in the Access group.
    Access(Access),
    /// One kind of credential synced across the person's Macs. A stub that rides
    /// [`Setting::Secrets`] and reads as not-applicable until SYNC-1 fills its behaviour; it is
    /// not enumerated by [`Subject::all`], so no Sync row is built in shipped code yet.
    Sync(SyncKind),
}

impl Subject {
    /// This reading describes the selected vault or credentials it supplied.
    #[must_use]
    pub const fn uses_vault_source(self) -> bool {
        matches!(
            self,
            Self::Setting(Setting::Secrets | Setting::Repositories) | Self::Sync(_)
        )
    }

    /// The page group the row sits in. A Sync row follows [`Setting::Secrets`].
    #[must_use]
    pub const fn group(self) -> Group {
        match self {
            Self::Setting(setting) => setting.group(),
            Self::Access(_) => Group::Access,
            Self::Sync(_) => Setting::Secrets.group(),
        }
    }

    /// The setting bit whose selection manages this row. A Sync row rides
    /// [`Setting::Secrets`]: no plan bit of its own.
    #[must_use]
    pub const fn setting(self) -> Setting {
        match self {
            Self::Setting(setting) => setting,
            Self::Access(_) => Setting::Permissions,
            Self::Sync(_) => Setting::Secrets,
        }
    }

    /// The row's name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Setting(setting) => setting.name(),
            Self::Access(access) => access.name(),
            Self::Sync(kind) => kind.name(),
        }
    }

    /// What aterm wants the row to read.
    #[must_use]
    pub const fn wanted(self) -> &'static str {
        match self {
            Self::Setting(setting) => setting.wanted(),
            Self::Access(access) => access.wanted(),
            Self::Sync(_) => Setting::Secrets.wanted(),
        }
    }

    /// Every row the page can show, in page order: each group's settings, with
    /// the Access group's grants in place of its one setting.
    #[must_use]
    pub fn all() -> Vec<Self> {
        let mut out = Vec::with_capacity(Setting::ALL.len() + Access::ALL.len());
        for group in Group::ALL {
            if group == Group::Access {
                out.extend(Access::ALL.into_iter().map(Self::Access));
            } else {
                out.extend(group.settings().iter().copied().map(Self::Setting));
            }
        }
        out
    }
}

/// Where one row stands, read without changing anything: a converge outcome
/// reduced once for display (`aterm_machines::init_observe::standing`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Standing {
    /// The Mac already reads as wanted.
    InPlace,
    /// The Mac reads differently from what aterm wants.
    Differs,
    /// macOS waits for a person: a switch, a dialog, an approval.
    NeedsYou,
    /// Waiting on something else that will happen by itself.
    Waiting,
    /// Written; macOS takes it up at the next login.
    AtLogin,
    /// The last attempt failed, or the item cannot be changed here.
    Failed,
    /// Could not be read. Never shown as a value.
    Unknown,
    /// Does not apply on this Mac.
    NotNeeded,
    /// macOS tells aterm nothing of it, and nothing in this launch can read it (Local Network,
    /// whose switch macOS does not report to an app): shown as `—` with its note, and settled,
    /// since no step aterm or a person takes now gives the page more to say.
    Untold,
}

impl Standing {
    /// Whether the row counts as in place for the page's "N of M" count.
    #[must_use]
    pub const fn settled(self) -> bool {
        matches!(
            self,
            Self::InPlace | Self::AtLogin | Self::NotNeeded | Self::Untold
        )
    }

    /// Whether the row needs a person, or failed: the only states that may
    /// light the tab's attention indicator.
    #[must_use]
    pub const fn attention(self) -> bool {
        matches!(self, Self::NeedsYou | Self::Failed)
    }
}

/// What can end a failure (design §3.6), decided where the failure is found and carried to
/// System State's page, its menu line, the tab's diamond and the setup run's retry loop alike,
/// so none of them says a step waits that only a person can finish.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FailureKind {
    /// A retry can help: a network blip, a vault that did not answer, a busy lock, a process
    /// that exited oddly. While aterm still tries it again by itself the page is calm and
    /// counts it as waiting; once those tries are spent it failed. A failure nobody classified
    /// is one of these, never a person's.
    Transient,
    /// Only a person's act ends it: a GitHub token refused or missing, a vault password to
    /// retype, an approval only an admin Mac gives, a switch only the person flips. It needs
    /// them at once, and no pass retries it. A reading says it as [`Standing::NeedsYou`], with
    /// its remedy in the row's note, and the run's row for it parks the run.
    NeedsPerson,
}

impl FailureKind {
    /// Whether aterm tries a failure of this kind again by itself.
    #[must_use]
    pub const fn retried(self) -> bool {
        matches!(self, Self::Transient)
    }

    /// The kind of failure a setup step's row `status` reports, `None` for a status that is no
    /// failure. `error` and `failed` name no kind, so they are [`Self::Transient`]: retried
    /// while tries remain, then failed. A step only a person finishes is said by its row's
    /// reading ([`Standing::NeedsYou`]), never by a step status.
    #[must_use]
    pub fn of_status(status: &str) -> Option<Self> {
        match status {
            "error" | "failed" => Some(Self::Transient),
            _ => None,
        }
    }
}

/// What a failed setup step's row says, in System State's words, while aterm still tries it
/// again by itself on its own pause ([`FailureKind::Transient`], a Secrets vault try): it waits
/// for that try whatever the run's retry loop does.
pub const TRIES_AGAIN_BY_ITSELF: &str = "aterm tries again by itself.";

/// What a failed Secrets vault try's row says once two tries in a row failed: its tries on a
/// pause are spent, so it waits, calm, for a change of this Mac's network or vault, which tries
/// it again by itself ([`FailureKind::Transient`]).
pub const TRIED_TWICE: &str =
    "Two tries did not take, so aterm tries again once this Mac's network or vault changes.";

/// The Local Network row's note when nothing says whether aterm may use the local network
/// ([`Standing::Untold`]): macOS does not tell apps (design §3.3).
pub const LOCAL_NETWORK_UNTOLD: &str = "macOS does not tell apps whether it is on";

/// The Local Network row's note once the window raised macOS's question in this approval and
/// nothing since says how it was answered ([`Standing::Untold`]).
pub const LOCAL_NETWORK_ASKED: &str = "macOS asked you, and does not tell apps your answer";

/// The GitHub repositories row's note while Apple's Command Line Tools, which every clone runs,
/// are not installed: a person's remedy (`init_observe::Wait::CommandLineTools`), which System
/// State's page starts in a terminal tab of its own (design §3.3).
pub const COMMAND_LINE_TOOLS_MISSING: &str = "Install Apple's Command Line Tools";

/// The note of a workstation row the person changed after aterm set it and read it back
/// (design §6.9): no automatic pass sets it again, so the row reads as the person left it, with
/// no chip saying it will change, until Restore, turning its group off and on, or Set up now.
pub const CHANGED_BY_YOU: &str = "Changed by you";

/// The Secrets vault row's value on a member Mac whose sync the person turned off, which is
/// also what the row wants then (design §4: an explicit opt-out is the row's wanted state), so
/// its quiet Turn On Sync turns it back on.
pub const SYNC_OFF: &str = "Sync off";

/// The Secrets vault row's value while `[swarm] enabled = false` turns sharing with the person's
/// other Macs off: their choice, so also what the row wants (design §4), whatever this Mac's
/// membership reads. It is not [`SYNC_OFF`], whose Turn On Sync turns on only the vault's sync,
/// which the switch would hold off all the same.
pub const SHARING_OFF: &str = "Sharing off";

/// The one note of the Secrets vault row at [`SHARING_OFF`], naming the setting as the page names
/// a setting: the person's choice, said calmly, with nothing to change.
pub const SHARING_OFF_NOTE: &str =
    "You turned sharing with your other Macs off in aterm.toml ([swarm]).";

/// The placeholder that makes `softwareupdate -l` list Apple's Command Line Tools: an install
/// through `softwareupdate` keeps it while it runs, and Apple's installer window creates it when
/// it opens ([`APPLE_TOOLS_INSTALLER`]). It can outlive a crashed installer, so System State also
/// requires a live installer process beside it before saying Installing (design §3.3).
pub const COMMAND_LINE_TOOLS_INSTALLING: &str =
    "/tmp/.com.apple.dt.CommandLineTools.installondemand.in-progress";

/// The environment variable System State's Command Line Tools tab runs `aterm init --only
/// base.clt` under, as the tab's own program (design §3.3). That run does nothing until the
/// window types [`COMMAND_LINE_TOOLS_TAB_GO`] (once the tab has landed), then speaks to a person
/// in plain words: one line before macOS asks for the Mac password, a plain password prompt, and
/// one line saying how it ended. It exits 0 once it said the tools are installed and 1 once it
/// said why the install did not finish (Low Data Mode, an account that may not install them, a
/// stop and Apple's window closed without installing them each with a code of its own); any
/// other end said nothing, and the window ends the tab with [`COMMAND_LINE_TOOLS_NOT_FINISHED`]
/// and [`COMMAND_LINE_TOOLS_TRY_AGAIN`].
pub const COMMAND_LINE_TOOLS_TAB: &str = "ATERM_COMMAND_LINE_TOOLS_TAB";

/// The line the window types into the Command Line Tools tab once it landed, which starts the
/// install ([`COMMAND_LINE_TOOLS_TAB`]): nothing runs before the window's landing check passes.
pub const COMMAND_LINE_TOOLS_TAB_GO: &str = "install";

/// How the Command Line Tools tab says its install did not finish, before its plain reason:
/// `The install did not finish: <reason>. Install… on System State tries again.`
pub const COMMAND_LINE_TOOLS_NOT_FINISHED: &str = "The install did not finish";

/// The Command Line Tools tab's last words when its install did not finish: what tries again.
pub const COMMAND_LINE_TOOLS_TRY_AGAIN: &str = "Install\u{2026} on System State tries again.";

/// What the Command Line Tools tab's program exits with when the window's go line
/// ([`COMMAND_LINE_TOOLS_TAB_GO`]) never came, or came behind other typing: it started nothing and
/// said nothing (neither 0 nor 1, which say it printed its last line), so the window ends the tab
/// with its own line and takes back an opening the tab spent, which installed nothing and which
/// no person declined.
pub const COMMAND_LINE_TOOLS_TAB_NOT_STARTED: u8 = 2;

/// What the Command Line Tools tab's program exits with once it said Low Data Mode holds the
/// download back ([`COMMAND_LINE_TOOLS_TAB`]): the window spends no automatic opening on it, and
/// the GitHub repositories row waits, calmly, for a network without Low Data Mode.
pub const COMMAND_LINE_TOOLS_TAB_LOW_DATA: u8 = 3;

/// What the Command Line Tools tab's program exits with once it said this account is not one of
/// the Mac's administrators, whom alone macOS lets install the tools: the GitHub repositories row
/// says so, and offers no Install… that would only say it again.
pub const COMMAND_LINE_TOOLS_TAB_NOT_ADMIN: u8 = 4;

/// What the Command Line Tools tab's program exits with once it said a stop ended its install
/// before the tools arrived: a Ctrl-C in the tab, at the password prompt or during the install.
/// A person declined: the window keeps the account's opening spent, as it does for a closed
/// tab, and no launch reopens it by itself.
pub const COMMAND_LINE_TOOLS_TAB_STOPPED: u8 = 5;

/// What the Command Line Tools tab's program exits with once it said Apple's installer window
/// closed without installing them: the window the program waited on (opened where Apple's
/// catalog lists none, or already open) went with no installer running. A person pressed Not
/// Now, Cancel, or Done on Apple's own failure there, each of which unlinks the placeholder, or
/// quit the window before pressing Install, which leaves a placeholder aterm's root half stamped
/// beside it ([`APPLE_TOOLS_INSTALLER`]), so the window keeps the account's opening spent, as
/// for a stop, and Install… (or `aterm init`) is the way on.
pub const COMMAND_LINE_TOOLS_TAB_WINDOW_CLOSED: u8 = 6;

/// The tail of Apple's own installer app's executable (`Install Command Line Developer Tools`,
/// which `xcode-select --install` and the `/usr/bin` stubs open). Read from its arm64e code on
/// macOS 27.0 (`-[IHAppDelegate createInstallMarkerFile]` and `finishInstallRequest`): the app
/// creates the on-demand placeholder ([`COMMAND_LINE_TOOLS_INSTALLING`]) with
/// `O_WRONLY | O_CREAT | O_EXCL` as the tools are requested of it (by `xcode-select --install`
/// or a stub), which opens its window, before any press, so it never touches a placeholder
/// already there (another install's: it then makes none of its own); it unlinks that path when
/// its request ends (installed, cancelled, or handed to Xcode), then quits, and at its exit or
/// abort when it created the file. A quit before Install is pressed (its menu's Quit, the
/// Dock's, a logout: `applicationShouldTerminate:` lets it go until then) ends no request, so a
/// placeholder another made beside it stays. So its window, with the placeholder beside it or
/// not, is Apple's own flow at any age, waiting for a press or running the install a press
/// started ([`tools_held_by`]): nothing aterm reads tells the two apart, and neither may be taken
/// over.
pub const APPLE_TOOLS_INSTALLER: &str =
    "Install Command Line Developer Tools.app/Contents/MacOS/Install Command Line Developer Tools";

/// What a running process is to an install of Apple's Command Line Tools, read from its
/// executable's path alone: never run, and never by name, so a program merely called
/// `installer` or `softwareupdate` is none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolsInstaller {
    /// One of Apple's command-line installers, `softwareupdate` or `installer`, which hold the
    /// placeholder while they install (aterm's own root install, Homebrew's, a script's). Their
    /// paths are named only where aterm's root tools may be (aterm-machines, and the window's
    /// installer probe's closed comparison): the caller's `is_install_process` says it.
    CommandLine,
    /// Apple's own installer app ([`APPLE_TOOLS_INSTALLER`]).
    AppleWindow,
}

/// [`ToolsInstaller`] for the process whose executable is `exe`: only an absolute path, one
/// `is_install_process` (the caller's exact comparison with Apple's command-line installers)
/// names, or one that ends in [`APPLE_TOOLS_INSTALLER`].
#[must_use]
pub fn tools_installer(
    exe: &std::path::Path,
    is_install_process: impl Fn(&std::path::Path) -> bool,
) -> Option<ToolsInstaller> {
    if !exe.is_absolute() {
        None
    } else if is_install_process(exe) {
        Some(ToolsInstaller::CommandLine)
    } else if exe.ends_with(APPLE_TOOLS_INSTALLER) {
        Some(ToolsInstaller::AppleWindow)
    } else {
        None
    }
}

/// Which of Apple's installers holds an install of the Command Line Tools now, over `exes` (each
/// running process's executable, `None` where the kernel names none): one of Apple's
/// command-line installers beside the on-demand placeholder ([`COMMAND_LINE_TOOLS_INSTALLING`],
/// there when `placeholder`), whatever its age; else Apple's installer window, at any age, with
/// the placeholder beside it or not; else nothing ([`tools_installer`]). A command-line installer
/// counts only beside the placeholder, which it keeps while it installs the tools (with none
/// there, `softwareupdate` lists or installs something else). A placeholder left behind counts
/// as well: the placeholder is what makes `softwareupdate` list the tools at all, so beside any
/// placeholder it may install them (a person's `softwareupdate -ia`), and the reading lasts only
/// while that installer runs. The window counts by itself: it
/// makes the placeholder as it opens unless one is there already (another install's, which may
/// end before the window does), and its request ends only with the window (it unlinks the path
/// and quits, or is quit, [`APPLE_TOOLS_INSTALLER`]), so a window listed is Apple's own flow,
/// pressed or not.
/// A process the kernel names no executable for (its file deleted, an app replaced while it
/// ran) is none of them: Apple's installers live at fixed paths on the sealed system volume, which
/// nothing deletes. The one rule the window's page, `aterm init` and the root half share: a
/// command-line installer beside the placeholder is an install under way (the root half says
/// another install runs, `aterm init` defers to it, the page reads Installing), Apple's window is
/// where the person finishes (the root half waits on it, never replacing a placeholder beside it,
/// making one where none is there, or running `softwareupdate`; `aterm init` defers to it naming
/// that step; the page reads it as Apple's window), and a placeholder beside neither is no
/// install anywhere (a run of aterm's root half that stamped it aside:
/// [`placeholder_held_by_a_live_run`]).
#[must_use]
pub fn tools_held_by(
    placeholder: bool,
    exes: impl IntoIterator<Item = Option<impl AsRef<std::path::Path>>>,
    is_install_process: impl Fn(&std::path::Path) -> bool,
) -> Option<ToolsInstaller> {
    let mut held = None;
    for exe in exes.into_iter().flatten() {
        match tools_installer(exe.as_ref(), &is_install_process) {
            Some(ToolsInstaller::CommandLine) if placeholder => {
                return Some(ToolsInstaller::CommandLine);
            }
            Some(ToolsInstaller::AppleWindow) => held = Some(ToolsInstaller::AppleWindow),
            Some(ToolsInstaller::CommandLine) | None => {}
        }
    }
    held
}

/// The longest stamp a run of the root half writes into the placeholder it holds
/// ([`placeholder_stamp`]); a longer file is no run's.
pub const PLACEHOLDER_STAMP_MAX: usize = 64;

/// What a run of aterm's root half writes into the on-demand placeholder
/// ([`COMMAND_LINE_TOOLS_INSTALLING`]) it holds: `aterm <pid> <start>`, its process and when that
/// started (seconds since the epoch, `0` where it could not be read), so `aterm init`, the root
/// half and the window tell a placeholder a live run holds (between its steps, or waiting on
/// Apple's installer window, with no installer process running) from one left behind
/// ([`placeholder_held_by_a_live_run`]).
#[must_use]
pub fn placeholder_stamp(pid: u32, started: u64) -> String {
    format!("aterm {pid} {started}\n")
}

/// Whether the placeholder whose contents are `stamp` is held by a run of aterm's that is alive
/// now: its stamp ([`placeholder_stamp`]) names a process `started` reads alive from that same
/// start. A start of `0` (one its run could not read) matches no live process, whatever its pid
/// now names. The one reader `aterm init`, the root half and the window's page share; each reads
/// the file itself (root's own, no longer than [`PLACEHOLDER_STAMP_MAX`]).
#[must_use]
pub fn placeholder_held_by_a_live_run(stamp: &[u8], started: impl Fn(u32) -> Option<u64>) -> bool {
    let Ok(text) = std::str::from_utf8(stamp) else {
        return false;
    };
    let mut words = text.split_whitespace();
    let (Some("aterm"), Some(pid), Some(start), None) = (
        words.next(),
        words.next().and_then(|word| word.parse::<u32>().ok()),
        words.next().and_then(|word| word.parse::<u64>().ok()),
        words.next(),
    ) else {
        return false;
    };
    start != 0 && started(pid) == Some(start)
}

/// One read-only observation of one row. `now` is `None` when nothing could be
/// read; a guessed value is never shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    /// What the row is about.
    pub subject: Subject,
    /// Where it stands.
    pub standing: Standing,
    /// The current value in the row's words, when it was read.
    pub now: Option<String>,
    /// What the row reads after the next login, where that differs from the value live now
    /// (`now`, marked [`until_login`]): the saved preference macOS loads then, in the row's
    /// words, which a row that will change shows as its from-side (design §3.3). `None` for
    /// a row whose live and saved values agree, and where the saved one names no word.
    pub after_login: Option<String>,
    /// The wanted value in the row's words.
    pub wanted: String,
    /// A short note in a person's words, shown under the row (an approval code,
    /// what a waiting row waits on).
    pub note: Option<String>,
    /// Why the row stands as it does, in its probe's words: shown under a row that
    /// needs a person, is not needed, was opted out of or failed (a row that
    /// differs or waits shows its note alone).
    pub detail: Option<String>,
}

impl Observation {
    /// An observation with no reading yet.
    #[must_use]
    pub fn unknown(subject: Subject) -> Self {
        Self {
            subject,
            standing: Standing::Unknown,
            now: None,
            after_login: None,
            wanted: subject.wanted().to_owned(),
            note: None,
            detail: None,
        }
    }
}

/// A row's current value while what is live differs from the saved preference macOS loads
/// at the next login (design §3.3): the live value, marked "until login", which a row that is
/// not changing shows. The row stands as the Mac will after that login
/// ([`Observation::after_login`]), so it is no change to make where its group is left as is,
/// and a row that will change shows that value, unmarked, as its from-side.
#[must_use]
pub fn until_login(live: &str) -> String {
    format!("{live}{UNTIL_LOGIN}")
}

/// The live value in a value [`until_login`] marked, or `None` for a value it did not mark.
#[must_use]
pub fn live_until_login(value: &str) -> Option<&str> {
    value.strip_suffix(UNTIL_LOGIN)
}

/// The mark [`until_login`] puts after a live value.
const UNTIL_LOGIN: &str = " · until login";

/// How a Restore left the saved preferences of one System State row (design §3.4): the
/// page names rows, never the preference keys `aterm doctor` lists. Worst last, so a row
/// of several keys takes its worst key's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PutBack {
    /// As it was, and in use.
    Back,
    /// As it was saved; in use from the next login.
    AtLogin,
    /// Its value from before aterm was never read (macOS hid it), so it was left as it is.
    Unknown,
    /// Its saved value is in another app's container, which macOS opens to aterm only
    /// once aterm has Full Disk Access: it waits for a Restore with the grant.
    Waits,
    /// It could not be put back, or macOS did not keep it.
    Failed,
}

/// What a Restore did, by System State row (design §3.4).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RestoreOutcome {
    /// Each row whose saved preferences it reached, once, in page order, as its worst key
    /// came out; `None` gathers keys no row of this build writes (an older aterm's backup).
    pub rows: Vec<(Option<Setting>, PutBack)>,
    /// The change journal refused a record of a change it made, which went ahead.
    pub journal_refused: bool,
}

impl RestoreOutcome {
    /// The outcome of keys `keys`, each with its row and how it came out: one entry per
    /// row, in page order, the unnamed keys last.
    #[must_use]
    pub fn of(keys: impl IntoIterator<Item = (Option<Setting>, PutBack)>) -> Self {
        let mut rows: Vec<(Option<Setting>, PutBack)> = Vec::new();
        for (setting, put_back) in keys {
            match rows.iter_mut().find(|(row, _)| *row == setting) {
                Some((_, worst)) => *worst = (*worst).max(put_back),
                None => rows.push((setting, put_back)),
            }
        }
        let place = |setting: &Option<Setting>| {
            setting.map_or(Setting::ALL.len(), |setting| {
                Setting::ALL
                    .iter()
                    .position(|each| *each == setting)
                    .unwrap_or(Setting::ALL.len())
            })
        };
        rows.sort_by_key(|(setting, _)| place(setting));
        Self {
            rows,
            journal_refused: false,
        }
    }
}

/// What a setup pass did to System State's rows that the page says in its own words (design
/// §3.4, §3.6), where `aterm doctor` lists the preference keys, keyboards and processes behind
/// them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PassOutcome {
    /// What the pass's rollback of the changes it could not confirm put back, by row; `None`
    /// when none ran.
    pub rolled_back: Option<RestoreOutcome>,
    /// Each row a change failed for, or could not be confirmed for, once each, in page order.
    pub unfinished: Vec<(Setting, Unfinished)>,
}

/// How a change a pass made to one row did not finish.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Unfinished {
    /// It could not be read back, so aterm cannot say whether it took.
    Unconfirmed,
    /// It did not take.
    Failed,
}

impl PassOutcome {
    /// Note that a change of `setting` did not finish as `how` says, once per row and how.
    pub fn unfinished(&mut self, setting: Setting, how: Unfinished) {
        if !self.unfinished.contains(&(setting, how)) {
            self.unfinished.push((setting, how));
            self.unfinished.sort_by_key(|(setting, how)| {
                (Setting::ALL.iter().position(|s| s == setting), *how)
            });
        }
    }

    /// Add what a later part of the same pass did (its follow-up, a later stage).
    pub fn merge(&mut self, later: Self) {
        for (setting, how) in later.unfinished {
            self.unfinished(setting, how);
        }
        if let Some(later) = later.rolled_back {
            self.rolled_back = Some(match self.rolled_back.take() {
                None => later,
                Some(earlier) => {
                    let mut both = RestoreOutcome::of(earlier.rows.into_iter().chain(later.rows));
                    both.journal_refused = earlier.journal_refused || later.journal_refused;
                    both
                }
            });
        }
    }
}

/// Bitmask of selected settings, one bit per [`Setting`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection(u16);

impl Default for Selection {
    fn default() -> Self {
        Self::all()
    }
}

impl Selection {
    const KNOWN: u16 = (1 << Setting::ALL.len()) - 1;

    /// Select every setting (the initial offer's default).
    #[must_use]
    pub const fn all() -> Self {
        Self(Self::KNOWN)
    }

    /// Select nothing.
    #[must_use]
    pub const fn none() -> Self {
        Self(0)
    }

    /// The original preference/account profile, excluding independent setup stages.
    #[must_use]
    pub const fn workstation(self) -> Self {
        Self(self.0 & 0xff)
    }

    /// Whether a setting is selected.
    #[must_use]
    pub const fn contains(self, setting: Setting) -> bool {
        self.0 & (1 << setting as u8) != 0
    }

    /// Change one checkbox without changing any other setting.
    pub const fn set(&mut self, setting: Setting, selected: bool) {
        let bit = 1 << setting as u8;
        if selected {
            self.0 |= bit;
        } else {
            self.0 &= !bit;
        }
    }

    /// Every setting this selection or `other` selects.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether applying this selection requires no OS work.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The stable representation saved in aterm's automatic setup plan.
    #[must_use]
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// Decode a plan, refusing bits no setting owns.
    #[must_use]
    pub const fn from_bits(bits: u16) -> Option<Self> {
        if bits & !Self::KNOWN == 0 {
            Some(Self(bits))
        } else {
            None
        }
    }
}

/// The base plan every Mac sets up where nobody chose one
/// (`docs/DESIGN-init-on-by-default-2026-10-25.md` §8.1): aterm's own Permissions, the Secrets
/// vault lane (discovery and the Sync rows ride it) and Rust in Spotlight, which changes this
/// login's own rustup (its profile set to minimal, each toolchain's `rust-docs` set aside and
/// removed) and keeps aterm's build caches out of Spotlight. The rest is the ALab profile (the
/// workstation preferences, Dock, Repositories and the Messages and FaceTime sign-outs), which
/// aterm admits by itself only on an ALab Mac.
pub const BASE_PLAN: Selection = {
    let mut selection = Selection::none();
    selection.set(Setting::Permissions, true);
    selection.set(Setting::Secrets, true);
    selection.set(Setting::Spotlight, true);
    selection
};

/// An Access row's note while no person asked for macOS's grants: setup goes on and finishes
/// without it, and the page's one press asks for the whole batch (System State's offer).
pub const ACCESS_OFFERED: &str =
    "macOS needs you to answer this one, so aterm offers it. Nothing else waits for it.";

/// Persisted first-launch setup plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The exact checked rows.
    pub selection: Selection,
    /// A vault URL or local path accepted by the secrets client.
    pub vault_url: String,
}

impl Default for Plan {
    fn default() -> Self {
        Self::from(Selection::all())
    }
}

impl From<Selection> for Plan {
    fn from(selection: Selection) -> Self {
        Self {
            selection,
            vault_url: DEFAULT_VAULT_URL.into(),
        }
    }
}

/// What became of one access step in one approved setup revision. The window's
/// access lanes append these to [`access_record::FILE`]; the setup worker's
/// Permissions stage reads them to know which rows were handed to macOS this
/// revision, so a row waiting on a person is never retried.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AccessOutcome {
    /// macOS's own step (a System Settings page, a dialog) was put in front of the person.
    Handed,
    /// The person answered aterm's panel with Not now, or cancelled macOS's dialog.
    Declined,
    /// The step was started without anything to show (the Local Network trigger).
    Triggered,
    /// The grant read on.
    Granted,
}

impl AccessOutcome {
    /// The record's word for this outcome.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Handed => "handed",
            Self::Declined => "declined",
            Self::Triggered => "triggered",
            Self::Granted => "granted",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        [Self::Handed, Self::Declined, Self::Triggered, Self::Granted]
            .into_iter()
            .find(|outcome| outcome.word() == word)
    }
}

impl Access {
    /// The record's word for this row.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::FullDiskAccess => "full-disk-access",
            Self::DeveloperTools => "developer-tools",
            Self::Debugger => "debugger",
            Self::LocalNetwork => "local-network",
            Self::Accessibility => "accessibility",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|access| access.word() == word)
    }
}

/// How many of macOS's own dialogs or pages this process has put on screen and
/// not yet seen end. The setup worker's prompts and the window's access
/// hand-overs each hold a [`MacosDialog`] while theirs is up, and an automatic
/// step starts only when none is up (`MacosDialog::when_none_up`), so it never
/// stacks a second one (`docs/DESIGN-system-state-2026-10-21.md` §6).
static MACOS_DIALOGS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

static MACOS_DIALOG_WAITER: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static MACOS_DIALOG_WAITER_READY: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();

/// Register what wakes a caller that timed out waiting for another macOS
/// dialog, once the last dialog ends. Immediate slot probes do not wait and
/// never subscribe. The first registration wins.
pub fn on_macos_dialog_waiter_ready(ready: fn()) {
    let _ = MACOS_DIALOG_WAITER_READY.set(ready);
}

#[cfg(not(target_family = "wasm"))]
fn arm_macos_dialog_waiter() {
    // This arm/check and the last drop's count transition/consume share one
    // SeqCst order: neither side can miss the other across the two atomics.
    MACOS_DIALOG_WAITER.store(true, std::sync::atomic::Ordering::SeqCst);
    notify_macos_dialog_waiter();
}

fn notify_macos_dialog_waiter() {
    if MACOS_DIALOGS.load(std::sync::atomic::Ordering::SeqCst) == 0
        && MACOS_DIALOG_WAITER.swap(false, std::sync::atomic::Ordering::SeqCst)
        && let Some(ready) = MACOS_DIALOG_WAITER_READY.get()
    {
        ready();
    }
}

/// Whether a macOS dialog or page this process started is still up.
#[must_use]
pub fn macos_dialog_up() -> bool {
    MACOS_DIALOGS.load(std::sync::atomic::Ordering::Acquire) != 0
}

/// Held while one macOS dialog or page this process started is up.
#[must_use = "the dialog counts as up only while the guard lives"]
pub struct MacosDialog(());

impl MacosDialog {
    /// Count one macOS dialog as up until the guard drops.
    pub fn up() -> Self {
        MACOS_DIALOGS.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Self(())
    }
}

/// The wait needs a clock, which a browser's wasm has none of (`Instant::now` panics there).
#[cfg(not(target_family = "wasm"))]
impl MacosDialog {
    /// Wait while another macOS dialog this process started is up, then count this
    /// one up. `limit` bounds the wait as a hang detector, never a latency budget: a
    /// person answers a dialog in their own time, so past it this returns `None` and
    /// the caller leaves its step for a later pass rather than stack a second dialog.
    #[must_use = "the dialog counts as up only while the guard lives"]
    pub fn when_none_up(limit: std::time::Duration) -> Option<Self> {
        let deadline = std::time::Instant::now().checked_add(limit);
        loop {
            // One step from none up to this one: two callers that both saw none
            // cannot both put a dialog up.
            if MACOS_DIALOGS
                .compare_exchange(
                    0,
                    1,
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Acquire,
                )
                .is_ok()
            {
                return Some(Self(()));
            }
            if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
                if !limit.is_zero() {
                    arm_macos_dialog_waiter();
                }
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }
}

/// Whether System State is the visible view of a focused window with a person at this Mac
/// (input within the last minute, the screen unlocked): the only time a setup run may put the
/// Accounts stage's Accessibility and Automation prompts, or its sign-outs, on screen
/// (`docs/DESIGN-system-state-2026-10-21.md` §6, `docs/DESIGN-init-on-by-default-2026-10-25.md`
/// §6.4). The window publishes it ([`set_system_state_front`]); a process with no window never
/// has it, so its setup run leaves those steps for a window.
static SYSTEM_STATE_FRONT: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Called once each time System State comes to the front: the setup worker
/// registers its wake, so a run parked on the page resumes there.
static SYSTEM_STATE_SHOWN: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();

/// Whether System State is the visible view of a focused window now.
#[must_use]
pub fn system_state_front() -> bool {
    SYSTEM_STATE_FRONT.load(std::sync::atomic::Ordering::Acquire)
}

/// The window's report of whether System State is the visible view of a focused
/// window. Cheap enough for every tick; it calls the registered watcher
/// ([`on_system_state_front`]) only when the page comes to the front.
pub fn set_system_state_front(front: bool) {
    let was = SYSTEM_STATE_FRONT.swap(front, std::sync::atomic::Ordering::AcqRel);
    if front
        && !was
        && let Some(shown) = SYSTEM_STATE_SHOWN.get()
    {
        shown();
    }
}

/// Register what runs when System State comes to the front. The first
/// registration wins: the process has one setup worker.
pub fn on_system_state_front(shown: fn()) {
    let _ = SYSTEM_STATE_SHOWN.set(shown);
}

/// The grants a person opted out of, which System State honors as the row's
/// wanted state (`docs/DESIGN-system-state-2026-10-21.md` §4): a `[privacy]`
/// switch off (`enabled`, `check` or `notice`), or debugger access turned off. The
/// window publishes them; the setup worker's readings and its Permissions stage
/// read them. A panel's Not now is the access record's `declined`, not an opt-out.
pub mod opt_out {
    use super::Access;

    /// Why the person opted a grant out, in a sentence the row's description
    /// shows, and which readings that sentence is true of.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum OptOut {
        /// True whatever the grant reads: a `[privacy]` switch is off.
        Always(&'static str),
        /// True only of a grant that reads off ("You turned debugger access
        /// off…"): a grant that reads on, or could not be read, keeps its own
        /// reading and its own words.
        WhileOff(&'static str),
    }

    impl OptOut {
        /// The sentence, where it is true of a grant that reads off (`reads_off`)
        /// or not; `None` where it is not.
        #[must_use]
        pub const fn holds(self, reads_off: bool) -> Option<&'static str> {
            match self {
                Self::Always(why) => Some(why),
                Self::WhileOff(why) if reads_off => Some(why),
                Self::WhileOff(_) => None,
            }
        }
    }

    static OPTED_OUT: std::sync::Mutex<[Option<OptOut>; 5]> = std::sync::Mutex::new([None; 5]);

    const fn slot(access: Access) -> usize {
        match access {
            Access::FullDiskAccess => 0,
            Access::DeveloperTools => 1,
            Access::Debugger => 2,
            Access::LocalNetwork => 3,
            Access::Accessibility => 4,
        }
    }

    /// Record why the person opted `access` out, or `None` when they did not.
    pub fn set(access: Access, why: Option<OptOut>) {
        if let Ok(mut opted) = OPTED_OUT.lock() {
            opted[slot(access)] = why;
        }
    }

    /// Why the person opted `access` out, if they did.
    #[must_use]
    pub fn get(access: Access) -> Option<OptOut> {
        OPTED_OUT.lock().ok().and_then(|opted| opted[slot(access)])
    }
}

/// How long a setup step waits for another macOS dialog to go before it leaves its
/// own for a later pass: a hang detector (`MacosDialog::when_none_up`).
pub const DIALOG_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// How long macOS's Local Network question counts as up once aterm raised it. macOS reports
/// neither the alert nor its answer, so this is a bound, not a reading: the Commons runtime that
/// asks by itself holds the dialog slot this long from its first LAN use. System State's Local
/// Network step holds its own alert until the answer is heard, or until the person has been at
/// the Mac this long, counted from their first input after the ask and paused while they are
/// away (`aterm-gui`'s `system_state_access`), so no page of the window's comes up beside an
/// alert nobody has seen (`docs/DESIGN-system-state-2026-10-21.md` §6).
pub const LOCAL_NETWORK_QUESTION_HOLD: std::time::Duration = std::time::Duration::from_secs(30);

/// Whether this process's LAN was handed to the Commons runtime. System State's Local Network
/// step hands it over at its trigger, the moment it asked macOS the question under the window's
/// dialog slot (`docs/DESIGN-system-state-2026-10-21.md` §6): Commons opens no LAN socket before
/// that, unless other evidence says macOS asked aterm already or the step will not run.
pub mod lan_first_use {
    // `aterm_time`'s clock, which a browser's wasm has too (`std::time::Instant::now` panics
    // there).
    use aterm_time::{Duration, Instant};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, OnceLock, PoisonError};

    static HANDED: OnceLock<u64> = OnceLock::new();

    /// How long a use of the LAN waits for System State's Local Network step, from the first time
    /// a use in this process found the step still to come ([`waits_for_step`]); `aterm-gui`'s
    /// permission walk waits as long for it (`permission_walk::LOCAL_NETWORK_WAIT`). The step asks
    /// only while System State is the visible view of a focused window, so a window whose System
    /// State stays behind a terminal tab would otherwise hold every first LAN use for good: past
    /// this, a use goes through this process's door ([`install_door`]), and macOS asks its own
    /// question once. Where a door is installed (a window) only time with a person at the Mac
    /// counts toward it ([`PRESENT_GAP`]): macOS lets no LAN traffic through before a person
    /// answers its question, so a use held while nobody is there loses nothing. A process with no
    /// door (the node job) counts the wall clock, and past this its uses go.
    pub const STEP_WAIT: Duration = Duration::from_secs(60);

    /// The most of the time between two looks that counts toward [`STEP_WAIT`] where a door is
    /// installed, and only with a person read present at both looks: a look reads a person
    /// present for an input within the last minute, so a longer gap may hide an absence.
    const PRESENT_GAP: Duration = Duration::from_secs(60);

    /// The time counted toward [`STEP_WAIT`] in this process, and the last look that counted it.
    struct Waited {
        counted: Duration,
        /// When the last look was, and whether it read a person present.
        last: Option<(Instant, bool)>,
    }

    impl Waited {
        const fn new() -> Self {
            Self {
                counted: Duration::ZERO,
                last: None,
            }
        }

        /// Count the time since the last look at `now`, `present` being whether this look reads
        /// a person at the Mac: the whole of it where `gap` is `None` (no presence is read), else
        /// at most `gap` of it, and only with the person present at both looks. The total.
        fn count(&mut self, now: Instant, present: bool, gap: Option<Duration>) -> Duration {
            self.counted = self.peek(now, present, gap);
            self.last = Some((now, present));
            self.counted
        }

        /// What [`Self::count`] would total at `now`, counting nothing.
        fn peek(&self, now: Instant, present: bool, gap: Option<Duration>) -> Duration {
            match self.last {
                Some((at, true)) if present => {
                    let since = now.saturating_duration_since(at);
                    self.counted
                        .saturating_add(gap.map_or(since, |gap| since.min(gap)))
                }
                _ => self.counted,
            }
        }
    }

    /// What this process's uses counted toward [`STEP_WAIT`] ([`waits_for_step`]).
    static WAITED: Mutex<Waited> = Mutex::new(Waited::new());

    /// A reader of something this process sees now, from any thread.
    type Reader = Box<dyn Fn() -> bool + Send + Sync>;
    type DoorReaders<'a> = (&'a dyn Fn() -> bool, &'a dyn Fn() -> bool);

    /// The door a window's first LAN uses go through once they waited [`STEP_WAIT`] for System
    /// State's step ([`install_door`]).
    struct InstalledDoor {
        /// A person is at this Mac now: input within the last minute and the screen unlocked.
        present: Reader,
        /// The window's quiet moment: an aterm window has keyboard focus, and no key went to aterm
        /// for the steps' key rest, so no key on its way lands in macOS's question.
        quiet: Reader,
    }

    static DOOR: OnceLock<InstalledDoor> = OnceLock::new();

    /// This process's reader of whether System State's Local Network step is still to come for
    /// it ([`install_step_to_come`]).
    static STEP_TO_COME: OnceLock<Reader> = OnceLock::new();

    /// This process's reader of whether `[swarm] enabled` leaves sharing on ([`install_sharing`]).
    static SHARING: OnceLock<Reader> = OnceLock::new();

    /// A use of this process asked macOS through the door ([`note_asked`]).
    static ASKED: AtomicBool = AtomicBool::new(false);

    /// This process's reader of whether another process of this app on this Mac (the vault
    /// subscriber, a background job) already put macOS's Local Network question to aterm
    /// ([`install_asked_elsewhere`]).
    static ASKED_ELSEWHERE: OnceLock<Reader> = OnceLock::new();

    /// When [`asked`] last read [`ASKED_ELSEWHERE`].
    static ELSEWHERE_LOOKED: Mutex<Option<Instant>> = Mutex::new(None);

    /// How often [`asked`] reads [`ASKED_ELSEWHERE`] at most: a file read, and uses ask often.
    const ELSEWHERE_LOOK: Duration = Duration::from_secs(2);

    /// What runs the moment this process's LAN stops being held for System State's step
    /// ([`on_open`]).
    static OPENED: OnceLock<fn()> = OnceLock::new();

    /// The hold on the dialog slot the door took for the question a use raised
    /// ([`hold_door_slot`]): [`super::LOCAL_NETWORK_QUESTION_HOLD`] of the person's time, during
    /// which the use whose door pass took it may hold it through its own LAN use
    /// ([`take_door_slot`]) and give it back.
    struct Hold<G> {
        /// The slot, while the hold has it.
        slot: Option<G>,
        /// The door pass that took it: the count of door passes so far.
        pass: u64,
        /// The question's hold ran out: a slot given back now goes at once.
        over: bool,
    }

    impl<G> Hold<G> {
        const fn new() -> Self {
            Self {
                slot: None,
                pass: 0,
                over: true,
            }
        }

        /// A door pass took `slot` for the question its use raises: the hold starts. Its pass,
        /// and what an earlier hold still had (to let go).
        #[cfg(any(test, not(target_family = "wasm")))]
        fn start(&mut self, slot: G) -> (u64, Option<G>) {
            self.pass = self.pass.wrapping_add(1);
            self.over = false;
            (self.pass, self.slot.replace(slot))
        }

        /// The slot, for the use whose door pass `pass` took it, while the hold has it.
        fn take(&mut self, pass: u64) -> Option<G> {
            if pass == self.pass {
                self.slot.take()
            } else {
                None
            }
        }

        /// The slot the use of `pass` took over, given back: the hold keeps it while the
        /// question's hold lasts; past it (or for another pass) it goes, returned to let go.
        fn give_back(&mut self, pass: u64, slot: G) -> Option<G> {
            if pass == self.pass && !self.over && self.slot.is_none() {
                self.slot = Some(slot);
                None
            } else {
                Some(slot)
            }
        }

        /// The question's hold of `pass` ran out: what it still has, returned to let go.
        #[cfg(any(test, not(target_family = "wasm")))]
        fn end(&mut self, pass: u64) -> Option<G> {
            if pass != self.pass {
                return None;
            }
            self.over = true;
            self.slot.take()
        }
    }

    /// This process's door hold ([`Hold`]).
    static HELD: Mutex<Hold<super::MacosDialog>> = Mutex::new(Hold::new());

    /// The door pass that took this process's dialog slot for the question its use raised
    /// ([`lan_use`]): only that use may take the slot over ([`take_door_slot`]).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct DoorPass(u64);

    /// The door's slot, taken over by the use whose door pass took it ([`take_door_slot`]): held
    /// while this lives. Let go within the question's hold, it goes back to that hold, which
    /// keeps it until [`super::LOCAL_NETWORK_QUESTION_HOLD`] of the person's time after the ask;
    /// past it, it goes.
    #[must_use = "the slot is held only while this lives"]
    pub struct DoorSlot {
        slot: Option<super::MacosDialog>,
        pass: u64,
    }

    impl Drop for DoorSlot {
        fn drop(&mut self) {
            if let Some(slot) = self.slot.take() {
                // Let go after the lock: the last dialog's drop may wake a waiter.
                let goes = HELD
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .give_back(self.pass, slot);
                drop(goes);
            }
        }
    }

    /// What a use of the LAN finds now ([`lan_use`]).
    #[derive(Debug, PartialEq, Eq)]
    pub enum LanUse {
        /// The use waits: sharing is off, or System State's Local Network step is still to come
        /// and the wait for it holds the use.
        Waits,
        /// The use goes, with the door pass that took this process's dialog slot for the
        /// question it raises where it went through the door ([`take_door_slot`]).
        Goes(Option<DoorPass>),
    }

    /// Where this process's LAN stands for a use held for System State's step, read without
    /// counting a look or taking a slot ([`door`]).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Door {
        /// Nothing holds a use for the step: it handed the LAN over, a use asked through the
        /// door, or the step is not to come.
        Open,
        /// The step is still to come and the wait for it holds a use for at least this much more
        /// of the person's time ([`STEP_WAIT`]).
        Waits(Duration),
        /// The wait for the step ran out: a use goes through the door at the person's next quiet
        /// moment with this process's dialog slot free ([`install_door`]).
        Spent,
    }

    /// Register what runs the moment this process's LAN stops being held for System State's
    /// step: the step hands it over ([`hand_over`]), a use first asks through the door
    /// ([`note_asked`]), or a look finds the wait for the step ran out ([`waits_for_step`]). The
    /// setup worker registers its wake, so a Secrets join parked on the step goes on then. The
    /// first registration wins.
    pub fn on_open(opened: fn()) {
        let _ = OPENED.set(opened);
    }

    /// Run what [`on_open`] registered.
    fn tell_open() {
        if let Some(opened) = OPENED.get() {
            opened();
        }
    }

    /// Hand this process's LAN to the Commons runtime: the step of approved revision `revision`
    /// asked macOS. The first hand-over wins; a later one changes nothing.
    pub fn hand_over(revision: u64) {
        if HANDED.set(revision).is_ok() {
            tell_open();
        }
    }

    /// The revision whose Local Network step handed this process's LAN over, if one did.
    #[must_use]
    pub fn handed() -> Option<u64> {
        HANDED.get().copied()
    }

    /// Install this process's reader of whether System State's Local Network step is still to
    /// come for it: true while the step is meant to raise macOS's question first and has not
    /// yet, by the rule the Commons runtime's own gate waits on. A window that runs first-run
    /// setup installs it, since its System State runs the step. The first reader installed wins.
    pub fn install_step_to_come(reader: impl Fn() -> bool + Send + Sync + 'static) {
        let _ = STEP_TO_COME.set(Box::new(reader));
    }

    /// Install this process's door, through which its first LAN uses go once they waited
    /// [`STEP_WAIT`] for System State's step ([`may_use`]): `present` reads a person at this Mac
    /// (input within the last minute, the screen unlocked), which is also the only time the wait
    /// counts, and `quiet` the window's quiet moment (an aterm window has keyboard focus and no
    /// key went to aterm for the steps' key rest). Past the wait a use goes only with both true
    /// and this process's dialog slot free, which it takes for the question's hold; the Commons
    /// gate goes through the same door ([`waits_for_step`]), and once one use asked, every other
    /// follows its answer ([`asked`]). A window that runs first-run setup installs it. The first
    /// door installed wins.
    pub fn install_door(
        present: impl Fn() -> bool + Send + Sync + 'static,
        quiet: impl Fn() -> bool + Send + Sync + 'static,
    ) {
        let _ = DOOR.set(InstalledDoor {
            present: Box::new(present),
            quiet: Box::new(quiet),
        });
    }

    /// Install this process's reader of whether `[swarm] enabled` leaves sharing on: while it
    /// reads off (or cannot be read), no automatic LAN use goes ([`may_use`]), whatever System
    /// State's step did (`docs/DESIGN-alab-swarm-2026-10-27.md` §9). A person's own action (a
    /// phrase typed, a command run) does not ask it. The first reader installed wins.
    pub fn install_sharing(reader: impl Fn() -> bool + Send + Sync + 'static) {
        let _ = SHARING.set(Box::new(reader));
    }

    /// Whether `[swarm] enabled` leaves sharing on, as this process's installed reader says; on
    /// where none is installed.
    #[must_use]
    pub fn sharing_on() -> bool {
        SHARING.get().is_none_or(|on| on())
    }

    /// Install this process's reader of whether another process of this app on this Mac (the
    /// vault subscriber, a background job with no window) already put macOS's Local Network
    /// question to aterm: [`asked`] reads it, at most every [`ELSEWHERE_LOOK`], and once it says
    /// so this process counts the question asked. A window that runs first-run setup installs
    /// it. The first reader installed wins.
    pub fn install_asked_elsewhere(reader: impl Fn() -> bool + Send + Sync + 'static) {
        let _ = ASKED_ELSEWHERE.set(Box::new(reader));
    }

    /// Whether a LAN use of this process went through the door and raised macOS's Local Network
    /// question by itself ([`note_asked`]), or another process of this app on this Mac did
    /// ([`install_asked_elsewhere`]): every later use follows its answer, raising no question of
    /// its own (macOS asks each app once).
    #[must_use]
    pub fn asked() -> bool {
        if ASKED.load(Ordering::Acquire) {
            return true;
        }
        let Some(elsewhere) = ASKED_ELSEWHERE.get() else {
            return false;
        };
        let now = Instant::now();
        let due = elsewhere_due(
            &mut ELSEWHERE_LOOKED
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
            now,
        );
        if due && elsewhere() {
            note_asked();
            return true;
        }
        false
    }

    /// Whether [`asked`] reads [`ASKED_ELSEWHERE`] at `now`, `looked` being when it last did,
    /// which a read due notes (pure but for that).
    fn elsewhere_due(looked: &mut Option<Instant>, now: Instant) -> bool {
        if looked.is_some_and(|at| now.saturating_duration_since(at) < ELSEWHERE_LOOK) {
            return false;
        }
        *looked = Some(now);
        true
    }

    /// Note that a LAN use of this process went through the door ([`install_door`]): the Commons
    /// gate that asked by itself past [`STEP_WAIT`], or a use [`may_use`] let through. The first
    /// note tells [`on_open`].
    pub fn note_asked() {
        if !ASKED.swap(true, Ordering::AcqRel) {
            tell_open();
        }
    }

    /// Take over the dialog slot the door took for the question a use raised, for the use whose
    /// door pass took it (`pass`, from [`lan_use`]) and that holds the slot through its own LAN
    /// use (the Secrets stage's vault join): the slot is held while the [`DoorSlot`] lives, and
    /// goes back to the door's hold when it goes, so the question keeps its hold
    /// ([`super::LOCAL_NETWORK_QUESTION_HOLD`] of the person's time after the ask). `None` where
    /// the door's hold has no slot of that pass: it ran out, or another use's pass took it.
    #[must_use]
    pub fn take_door_slot(pass: DoorPass) -> Option<DoorSlot> {
        let slot = HELD
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take(pass.0)?;
        Some(DoorSlot {
            slot: Some(slot),
            pass: pass.0,
        })
    }

    /// Whether an automatic use of the LAN by this process (a probe, a sweep, a browse, a join)
    /// may go now without putting macOS's Local Network question on screen out of turn: sharing
    /// is on ([`install_sharing`]), and the step handed the LAN over, a use already asked through
    /// the door ([`asked`]), the installed reader says the step is not still to come, or the uses
    /// waited [`STEP_WAIT`] for it ([`waits_for_step`]) and this use goes through the door: where
    /// a door is installed (a window), a person present in the window's quiet moment and this
    /// process's dialog slot free, which the use takes for the question's hold. In a process that
    /// installed no reader (a command, a window that runs no first-run setup) nothing waits on a
    /// step; in one with a reader and no door (the node job) a use goes once the wait ran out.
    /// Ask it right before the use, and only for a use that is due: a pass through the door
    /// spends the door on that use.
    #[must_use]
    pub fn may_use() -> bool {
        matches!(lan_use(), LanUse::Goes(_))
    }

    /// [`may_use`], with the door pass that took this process's dialog slot for the question the
    /// use raises where it went through the door: a use that holds the slot through its own LAN
    /// use takes it over with that pass ([`take_door_slot`]).
    pub fn lan_use() -> LanUse {
        lan_use_sharing(sharing_on())
    }

    /// [`lan_use`] for a use that reads `[swarm] enabled` itself and says so where it holds the
    /// use (the Secrets stage's join, whose row then names the switch): the step's rule alone.
    pub fn step_use() -> LanUse {
        lan_use_sharing(true)
    }

    /// [`lan_use`] with `sharing` read.
    fn lan_use_sharing(sharing: bool) -> LanUse {
        let mut pass = None;
        let goes = may_use_with(
            sharing,
            handed().is_some() || asked(),
            STEP_TO_COME
                .get()
                .map(|reader| &**reader as &dyn Fn() -> bool),
            waits_for_step,
            || {
                through_the_door().is_some_and(|through| {
                    pass = through;
                    true
                })
            },
        );
        if goes {
            LanUse::Goes(pass)
        } else {
            LanUse::Waits
        }
    }

    /// Where this process's LAN stands for a use held for System State's step, read without
    /// counting a look or taking a slot: open where nothing holds a use for the step, else what
    /// is left of [`STEP_WAIT`] counted at this moment, or spent. The setup worker's park reads it
    /// for a Secrets join that waits on the step: a change is a new fact, and what is left arms
    /// its one timer, at the earliest moment the wait could run out.
    #[must_use]
    pub fn door() -> Door {
        if handed().is_some() || asked() || STEP_TO_COME.get().is_none_or(|to_come| !to_come()) {
            return Door::Open;
        }
        let present = DOOR.get().is_none_or(|door| (door.present)());
        let counted = WAITED.lock().unwrap_or_else(PoisonError::into_inner).peek(
            Instant::now(),
            present,
            DOOR.get().map(|_| PRESENT_GAP),
        );
        door_of(counted)
    }

    /// [`Door`] of what the uses counted toward [`STEP_WAIT`] (pure), the step still to come.
    fn door_of(counted: Duration) -> Door {
        STEP_WAIT
            .checked_sub(counted)
            .filter(|left| !left.is_zero())
            .map_or(Door::Spent, Door::Waits)
    }

    /// Whether a use that finds System State's Local Network step still to come (`to_come`)
    /// waits for it now: within [`STEP_WAIT`] counted from the first time a use in this process
    /// found it so, which the first such call notes, counting only time with a person present
    /// where a door is installed; past it, while the door's person or quiet moment is missing
    /// ([`install_door`]). A use that finds the step not to come waits for nothing and notes
    /// nothing. The Commons runtime's gate waits by this too, so it goes through the same door.
    /// The look that spends the wait tells [`on_open`].
    #[must_use]
    pub fn waits_for_step(to_come: bool) -> bool {
        let door = DOOR.get().map(|door| {
            (
                &*door.present as &dyn Fn() -> bool,
                &*door.quiet as &dyn Fn() -> bool,
            )
        });
        let mut waited = WAITED.lock().unwrap_or_else(PoisonError::into_inner);
        let before = waited.counted;
        let waits = waits_with(to_come, &mut waited, Instant::now(), door);
        let spent = spent_now(before, waited.counted);
        drop(waited);
        if spent {
            tell_open();
        }
        waits
    }

    /// Whether a look that counted `before` → `after` toward [`STEP_WAIT`] is the one that spent
    /// it (pure).
    fn spent_now(before: Duration, after: Duration) -> bool {
        before < STEP_WAIT && after >= STEP_WAIT
    }

    /// [`waits_for_step`] at `now`, with `waited` what the uses counted so far and `door` the
    /// installed door's readers of a person present and of the window's quiet moment (pure but
    /// for counting).
    fn waits_with(
        to_come: bool,
        waited: &mut Waited,
        now: Instant,
        door: Option<DoorReaders<'_>>,
    ) -> bool {
        if !to_come {
            return false;
        }
        let present = door.is_none_or(|(present, _)| present());
        let counted = waited.count(now, present, door.map(|_| PRESENT_GAP));
        counted < STEP_WAIT || door.is_some_and(|(_, quiet)| !present || !quiet())
    }

    /// [`may_use`] of what it read (pure but for `waits` and `door`): `sharing`, whether the LAN
    /// was handed over or a use already asked (`handed`), the reader, if one was installed,
    /// `waits`, whether a step still to come holds the use now ([`waits_for_step`]), and `door`,
    /// whether the use, past that wait, goes through the door ([`through_the_door`]); each asked
    /// only where the one before it leaves the use waiting.
    fn may_use_with(
        sharing: bool,
        handed: bool,
        step_to_come: Option<&dyn Fn() -> bool>,
        waits: impl FnOnce(bool) -> bool,
        door: impl FnOnce() -> bool,
    ) -> bool {
        sharing
            && (handed || step_to_come.is_none_or(|to_come| !to_come() || (!waits(true) && door())))
    }

    /// Whether a use past [`STEP_WAIT`] goes through this process's door, and with which pass:
    /// with no door installed, at once and with none; with one, only where this process's dialog
    /// slot is free, which it takes for the question's hold ([`through_the_door_with`]). `None`:
    /// the use waits at the door.
    fn through_the_door() -> Option<Option<DoorPass>> {
        #[cfg(not(target_family = "wasm"))]
        {
            through_the_door_with(
                DOOR.get().is_some(),
                || super::MacosDialog::when_none_up(std::time::Duration::ZERO),
                note_asked,
                hold_door_slot,
            )
        }
        // A browser's wasm has no macOS question to ask, and no clock to wait with.
        #[cfg(target_family = "wasm")]
        {
            Some(None)
        }
    }

    /// [`through_the_door`] (pure): where a `door` is installed, take the dialog `slot`, note the
    /// ask (`asked`) and `hold` the slot for the question, which gives the pass; a slot another
    /// macOS question holds keeps the use waiting (`None`). With no door the use goes with no pass.
    fn through_the_door_with<G, T>(
        door: bool,
        slot: impl FnOnce() -> Option<G>,
        asked: impl FnOnce(),
        hold: impl FnOnce(G) -> T,
    ) -> Option<Option<T>> {
        if !door {
            return Some(None);
        }
        let slot = slot()?;
        asked();
        Some(Some(hold(slot)))
    }

    /// How often the door's question hold looks for a person at the Mac ([`hold_door_slot`]).
    #[cfg(not(target_family = "wasm"))]
    const HOLD_LOOK: std::time::Duration = std::time::Duration::from_secs(1);

    /// Hold the slot the door took, from a thread of its own, until the person has had
    /// [`super::LOCAL_NETWORK_QUESTION_HOLD`] with the question, counted as System State's step
    /// counts its own alert's hold: the time between two looks with the person present at both,
    /// at most [`PRESENT_GAP`] of it ([`held_out`]). macOS says neither when its question closes
    /// nor how it was answered, so the next macOS question of this process waits the hold out.
    /// The use whose door pass this is may take the slot over meanwhile and give it back
    /// ([`take_door_slot`]). A hold that cannot start lets the slot go at once.
    #[cfg(not(target_family = "wasm"))]
    fn hold_door_slot(slot: super::MacosDialog) -> DoorPass {
        let (pass, earlier) = HELD
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .start(slot);
        drop(earlier);
        let spawned = std::thread::Builder::new()
            .name("lan-first-use-question-hold".into())
            .spawn(move || {
                let mut waited = Waited::new();
                let present = || DOOR.get().map(|door| &*door.present as &dyn Fn() -> bool);
                while !held_out(&mut waited, Instant::now(), present()) {
                    std::thread::sleep(HOLD_LOOK);
                }
                end_hold(pass);
            });
        if spawned.is_err() {
            end_hold(pass);
        }
        DoorPass(pass)
    }

    /// Whether the door's question hold has run out at a look at `now`: the person, as the door's
    /// `present` reader reads them (every look where no door is installed), has had
    /// [`super::LOCAL_NETWORK_QUESTION_HOLD`] with the question, counted in `waited`.
    #[cfg(any(test, not(target_family = "wasm")))]
    fn held_out(waited: &mut Waited, now: Instant, present: Option<&dyn Fn() -> bool>) -> bool {
        let here = present.is_none_or(|present| present());
        waited.count(now, here, present.map(|_| PRESENT_GAP)) >= super::LOCAL_NETWORK_QUESTION_HOLD
    }

    /// End the door's question hold of `pass`, letting go the slot it still has.
    #[cfg(not(target_family = "wasm"))]
    fn end_hold(pass: u64) {
        let goes = HELD
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .end(pass);
        drop(goes);
    }

    #[cfg(test)]
    mod tests {
        use super::Waited;
        use std::time::{Duration, Instant};

        /// One test: the hand-over and a use's ask are process-wide, so a second test could see
        /// this one's. Each tells the setup worker's watcher once, the moment the LAN opens: a
        /// Secrets join parked on System State's step goes on then.
        #[test]
        fn lan_first_use_the_first_hand_over_wins() {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static OPENED: AtomicUsize = AtomicUsize::new(0);
            fn opened() {
                OPENED.fetch_add(1, Ordering::SeqCst);
            }
            super::on_open(opened);
            assert!(!super::asked(), "nothing asked yet");
            super::note_asked();
            assert_eq!(
                OPENED.load(Ordering::SeqCst),
                1,
                "a use asked: the LAN opened"
            );
            super::note_asked();
            assert_eq!(OPENED.load(Ordering::SeqCst), 1, "told once");
            assert_eq!(super::handed(), None, "nothing handed over yet");
            super::hand_over(8_401);
            assert_eq!(super::handed(), Some(8_401));
            assert_eq!(OPENED.load(Ordering::SeqCst), 2, "the hand-over is told");
            super::hand_over(8_402);
            assert_eq!(super::handed(), Some(8_401), "the first hand-over wins");
            assert_eq!(OPENED.load(Ordering::SeqCst), 2, "and only it is told");
        }

        /// A use went through the door, which took this process's dialog slot for the question it
        /// raised. The Secrets stage's join takes it over with its own door pass and returns at
        /// once (its probe found no vault here, or the join failed fast): the slot goes back to
        /// the question's hold, which keeps it until the hold runs out, so no System Settings page
        /// of the walk comes up beside the question. Another use's pass takes nothing, and a slot
        /// given back once the hold ran out goes at once.
        #[test]
        fn a_slot_taken_from_the_door_keeps_the_questions_hold() {
            struct Slot<'a>(&'a std::cell::Cell<u32>);
            impl Drop for Slot<'_> {
                fn drop(&mut self) {
                    self.0.set(self.0.get() + 1);
                }
            }
            let gone = std::cell::Cell::new(0);
            let mut hold = super::Hold::new();
            let (pass, earlier) = hold.start(Slot(&gone));
            assert!(earlier.is_none());
            assert!(
                hold.take(pass.wrapping_add(1)).is_none(),
                "another use's pass takes nothing"
            );
            let joined = hold
                .take(pass)
                .expect("the join's own pass takes the slot over");
            assert!(hold.give_back(pass, joined).is_none());
            assert_eq!(
                gone.get(),
                0,
                "the join returned at once: the question keeps its hold"
            );
            drop(hold.end(pass));
            assert_eq!(gone.get(), 1, "the question's hold ran out: the slot goes");
            let (pass, _) = hold.start(Slot(&gone));
            let joined = hold.take(pass).expect("taken over");
            assert!(
                hold.end(pass).is_none(),
                "the hold ran out while the join held the slot"
            );
            drop(hold.give_back(pass, joined));
            assert_eq!(
                gone.get(),
                2,
                "given back past the hold, the slot goes at once"
            );
        }

        /// The door's question hold counts the person's time with the question, as System State's
        /// step counts its own alert's: hours away count nothing, and it runs out once they have
        /// had [`super::super::LOCAL_NETWORK_QUESTION_HOLD`] with it. A process with no door
        /// counts every second.
        #[test]
        fn the_doors_question_hold_counts_only_the_persons_time() {
            let t0 = Instant::now();
            let at = |seconds: u64| t0 + Duration::from_secs(seconds);
            let (away, here) = (|| false, || true);
            let mut waited = Waited::new();
            assert!(!super::held_out(&mut waited, at(0), Some(&here)));
            assert!(!super::held_out(&mut waited, at(10), Some(&here)));
            for seconds in (11..=7_200).step_by(5) {
                assert!(
                    !super::held_out(&mut waited, at(seconds), Some(&away)),
                    "away at {seconds} s: the question keeps its hold"
                );
            }
            assert!(!super::held_out(&mut waited, at(7_205), Some(&here)));
            assert!(!super::held_out(&mut waited, at(7_220), Some(&here)));
            assert!(
                super::held_out(&mut waited, at(7_225), Some(&here)),
                "ten seconds before the person left and twenty after they came back"
            );
            let mut wall = Waited::new();
            assert!(!super::held_out(&mut wall, at(0), None));
            assert!(
                super::held_out(&mut wall, at(30), None),
                "no door: the wall clock"
            );
        }

        /// Another process of this app on this Mac (the vault subscriber) putting macOS's
        /// question to aterm is read at most every two seconds: uses ask often.
        #[test]
        fn another_process_ask_is_read_at_most_every_two_seconds() {
            let t0 = Instant::now();
            let mut looked = None;
            assert!(super::elsewhere_due(&mut looked, t0));
            assert!(!super::elsewhere_due(
                &mut looked,
                t0 + Duration::from_secs(1)
            ));
            assert!(super::elsewhere_due(
                &mut looked,
                t0 + Duration::from_secs(2)
            ));
            assert!(!super::elsewhere_due(
                &mut looked,
                t0 + Duration::from_secs(3)
            ));
        }

        /// The park of a Secrets join waiting on System State's step reads where the LAN stands
        /// without counting a look: what is left of the wait arms its one timer, and the look
        /// that spends the wait is the one that tells the setup worker.
        #[test]
        fn the_door_reads_what_is_left_of_the_wait_and_tells_its_spending_once() {
            assert_eq!(
                super::door_of(Duration::ZERO),
                super::Door::Waits(super::STEP_WAIT)
            );
            assert_eq!(
                super::door_of(Duration::from_millis(59_500)),
                super::Door::Waits(Duration::from_millis(500))
            );
            assert_eq!(super::door_of(super::STEP_WAIT), super::Door::Spent);
            assert_eq!(super::door_of(Duration::from_secs(600)), super::Door::Spent);
            let second = Duration::from_secs(1);
            assert!(super::spent_now(
                super::STEP_WAIT - second,
                super::STEP_WAIT
            ));
            assert!(!super::spent_now(
                super::STEP_WAIT,
                super::STEP_WAIT + second
            ));
            assert!(!super::spent_now(second, second * 2));
            let t0 = Instant::now();
            let mut waited = Waited::new();
            waited.count(t0, true, None);
            assert_eq!(
                waited.peek(t0 + Duration::from_secs(10), true, None),
                Duration::from_secs(10)
            );
            assert_eq!(waited.counted, Duration::ZERO, "a peek counts nothing");
            assert_eq!(
                waited.peek(t0 + Duration::from_secs(10), false, None),
                Duration::ZERO
            );
        }

        /// A use with the door's slot `free` or held by another macOS question: whether it went
        /// through, and whether it noted the ask.
        fn door(free: bool) -> (bool, bool) {
            let asked = std::cell::Cell::new(false);
            let went =
                super::through_the_door_with(true, || free.then_some(()), || asked.set(true), drop)
                    .is_some();
            (went, asked.get())
        }

        /// A LAN use waits only while an installed reader says System State's Local Network step
        /// is still to come, the step has not handed the LAN over, and the wait for it has not run
        /// out: a process with no reader waits on nothing, and the hand-over opens it whatever the
        /// reader says.
        #[test]
        fn lan_first_use_waits_only_for_a_step_still_to_come() {
            let to_come = || true;
            let not_to_come = || false;
            let waits = |to_come: bool| to_come;
            let open = || true;
            assert!(
                super::may_use_with(true, false, None, waits, open),
                "no reader: no step waits"
            );
            assert!(
                !super::may_use_with(true, false, Some(&to_come), waits, open),
                "the step is still to come and asks first"
            );
            assert!(super::may_use_with(
                true,
                false,
                Some(&not_to_come),
                waits,
                open
            ));
            assert!(
                super::may_use_with(true, true, Some(&to_come), waits, open),
                "the step handed the LAN over"
            );
            assert!(
                super::may_use_with(true, false, Some(&to_come), |_| false, open),
                "the wait for the step ran out"
            );
        }

        /// System State open behind a terminal tab never reads frontmost, so its Local Network
        /// step never asks and never hands the LAN over: in a process with no door (the node
        /// job), Commons and the `_alab._tcp` browse wait for it [`super::STEP_WAIT`] from the
        /// first wait at most, on the wall clock, and then go, macOS asking its own question once.
        /// A use that finds no step to come notes no wait.
        #[test]
        fn the_local_network_step_behind_a_terminal_tab_never_holds_the_lan_for_good() {
            let mut waited = Waited::new();
            let t0 = Instant::now();
            let at = |seconds: u64| t0 + Duration::from_secs(seconds);
            assert!(
                !super::waits_with(false, &mut waited, t0, None),
                "no step to come"
            );
            assert!(waited.last.is_none(), "and no wait noted");
            let to_come = || true;
            let open = || true;
            let mut may_use = |seconds: u64| {
                super::may_use_with(
                    true,
                    false,
                    Some(&to_come),
                    |to_come| super::waits_with(to_come, &mut waited, at(seconds), None),
                    open,
                )
            };
            assert!(!may_use(5), "the first use waits for the step");
            assert!(
                !may_use(64),
                "a later use waits only for what is left of the first's wait"
            );
            assert!(may_use(65), "the step's wait ran out from the first wait");
            for minute in 2..=120 {
                assert!(may_use(5 + 60 * minute), "two hours behind a terminal tab");
            }
        }

        /// Past [`super::STEP_WAIT`] a window's LAN use (the `_alab._tcp` browse, the
        /// Secrets stage's join, Tab Control's sweep, the fleet's heartbeat, facts push and
        /// glance) goes only through the door the Commons gate goes through: never while another
        /// macOS question of this process holds its dialog slot (the walk's System Settings page,
        /// the debugger's password dialog, the Command Line Tools' installer), so macOS's Local
        /// Network question never comes up beside one.
        #[test]
        fn a_lan_use_past_the_step_wait_waits_for_another_macos_question() {
            let to_come = || true;
            let past = |_| false;
            assert!(
                !super::may_use_with(true, false, Some(&to_come), past, || door(false).0),
                "another macOS question holds the slot: the use waits"
            );
            assert_eq!(door(false), (false, false), "and notes no ask");
            assert!(
                super::may_use_with(true, false, Some(&to_come), past, || door(true).0),
                "the slot is free: the use goes"
            );
            assert_eq!(
                door(true),
                (true, true),
                "taking the slot and noting the ask"
            );
            assert_eq!(
                super::through_the_door_with(false, || None::<()>, || {}, drop),
                Some(None),
                "no door (the node job): past the wait a use goes as it did, with no pass"
            );
        }

        /// Past [`super::STEP_WAIT`] a window's use waits for a person at the Mac in the window's
        /// quiet moment (keys at rest), as the Commons gate's look does: macOS's question comes up
        /// with the person there and no key on its way into it.
        #[test]
        fn a_lan_use_past_the_step_wait_waits_for_a_person_in_a_quiet_moment() {
            let t0 = Instant::now();
            let at = |seconds: u64| t0 + Duration::from_secs(seconds);
            for (present, quiet, waits) in [
                (true, true, false),
                (true, false, true),
                (false, true, true),
                (false, false, true),
            ] {
                let mut waited = Waited::new();
                let here = || true;
                let still = || true;
                assert!(super::waits_with(
                    true,
                    &mut waited,
                    at(0),
                    Some((&here, &still))
                ));
                assert!(super::waits_with(
                    true,
                    &mut waited,
                    at(30),
                    Some((&here, &still))
                ));
                let (now_present, now_quiet) = (move || present, move || quiet);
                assert_eq!(
                    super::waits_with(true, &mut waited, at(60), Some((&now_present, &now_quiet))),
                    waits,
                    "present {present}, quiet {quiet}"
                );
            }
        }

        /// In a window only the time a person is at the Mac counts toward
        /// [`super::STEP_WAIT`]: hours away from it (or with its screen locked) count nothing, a
        /// gap longer than a minute counts a minute at most, and the wait runs out only after a
        /// minute with the person there.
        #[test]
        fn away_time_does_not_count_toward_the_step_wait() {
            let t0 = Instant::now();
            let at = |seconds: u64| t0 + Duration::from_secs(seconds);
            let mut waited = Waited::new();
            let away = || false;
            let here = || true;
            let quiet = || true;
            for seconds in (0..=7_200).step_by(10) {
                assert!(
                    super::waits_with(true, &mut waited, at(seconds), Some((&away, &quiet))),
                    "nobody at the Mac for two hours: still waiting at {seconds} s"
                );
            }
            assert_eq!(waited.counted, Duration::ZERO, "away time counts nothing");
            assert!(super::waits_with(
                true,
                &mut waited,
                at(7_210),
                Some((&here, &quiet))
            ));
            assert!(super::waits_with(
                true,
                &mut waited,
                at(7_215),
                Some((&here, &quiet))
            ));
            assert_eq!(
                waited.counted,
                Duration::from_secs(5),
                "the person's time counts"
            );
            assert!(
                !super::waits_with(true, &mut waited, at(7_815), Some((&here, &quiet))),
                "a minute with the person there: through the door"
            );
            assert_eq!(
                waited.counted,
                Duration::from_secs(65),
                "a gap of ten minutes counts a minute at most"
            );
        }

        /// One use goes through the door and asks; the others follow its answer, taking no slot
        /// of their own and raising no question of their own (macOS asks each app once), even
        /// while the first use's slot is still held.
        #[test]
        fn one_use_asks_and_the_others_follow_its_answer() {
            let to_come = || true;
            let past = |_| false;
            let asked = std::cell::Cell::new(false);
            let slot_free = std::cell::Cell::new(true);
            let take = || slot_free.replace(false).then_some(());
            let first = super::may_use_with(true, asked.get(), Some(&to_come), past, || {
                super::through_the_door_with(true, take, || asked.set(true), drop).is_some()
            });
            assert!(first && asked.get(), "the first use asks under the slot");
            for use_ in ["browse", "sweep", "heartbeat", "join"] {
                let taken = std::cell::Cell::new(false);
                assert!(
                    super::may_use_with(true, asked.get(), Some(&to_come), past, || {
                        taken.set(true);
                        false
                    }),
                    "{use_}: follows the first use's answer"
                );
                assert!(!taken.get(), "{use_}: no door of its own");
            }
        }

        /// With `[swarm] enabled = false` no automatic LAN use goes: not the `_alab._tcp`
        /// browse, the heartbeat, the glance, the node job's advert, nor Tab Control's sweep,
        /// whether or not a step is to come, the LAN was handed over or a use asked.
        #[test]
        fn swarm_off_holds_the_automatic_lan_uses() {
            let to_come = || true;
            let not_to_come = || false;
            for (handed, reader) in [
                (false, None),
                (false, Some(&not_to_come as &dyn Fn() -> bool)),
                (true, Some(&to_come as &dyn Fn() -> bool)),
                (false, Some(&to_come as &dyn Fn() -> bool)),
            ] {
                assert!(
                    !super::may_use_with(false, handed, reader, |_| false, || true),
                    "sharing off holds the use (handed {handed})"
                );
                assert!(
                    super::may_use_with(true, true, reader, |_| false, || true),
                    "sharing on: the use goes"
                );
            }
        }
    }
}

impl Drop for MacosDialog {
    fn drop(&mut self) {
        if MACOS_DIALOGS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst) == 1 {
            notify_macos_dialog_waiter();
        }
    }
}

/// The per-revision record of access steps, a text file in aterm's state
/// directory: one `<revision> <row> <outcome>` line per step that changed its row's outcome
/// (a repeat of the row's latest is not written again). Unknown lines are
/// skipped, so an older or newer aterm can share the file. [`access_record::append`] tells the
/// registered watcher ([`access_record::on_append`]) once each line is on disk; System State's
/// Access rows and a run parked on the Permissions stage wait for that.
pub mod access_record {
    use std::io::Write as _;
    use std::path::Path;

    use super::{Access, AccessOutcome};

    /// The composing application's loaded signing identity, hashed without a path.
    pub type CodeSubject = [u8; 32];

    static CODE_SUBJECT: std::sync::OnceLock<fn() -> Option<CodeSubject>> =
        std::sync::OnceLock::new();

    /// Install the process's identity reader. Uncomposed and headless test hosts
    /// have no identity unless their host injects one; no platform probe runs here.
    pub fn code_subject_from(read: fn() -> Option<CodeSubject>) {
        let _ = CODE_SUBJECT.set(read);
    }

    /// This process's code subject, or unknown. Unknown never matches a kept
    /// Local Network grant from another process.
    #[must_use]
    pub fn code_subject() -> Option<CodeSubject> {
        CODE_SUBJECT.get().and_then(|read| read())
    }

    /// The file's name inside aterm's state directory.
    pub const FILE: &str = "system-state-access";

    const LOCK_FILE: &str = ".system-state-access.lock";

    /// Lines kept when the file is rewritten: a few revisions of every row.
    const KEEP: usize = 64;

    /// Called once each outcome [`append`] was given is on disk: the setup worker registers its
    /// re-reading of the Access rows and its wake.
    static APPENDED: std::sync::OnceLock<fn(u64, Access, AccessOutcome)> =
        std::sync::OnceLock::new();

    /// Register what runs after each line is written, with the line's revision,
    /// row and outcome. The first registration wins: the process has one setup
    /// worker.
    pub fn on_append(appended: fn(u64, Access, AccessOutcome)) {
        let _ = APPENDED.set(appended);
    }

    /// Append one step's outcome for `revision`, keeping the file short: an outcome that is
    /// already its row's latest in `revision` and its Local Network code subject is not written again. Once it is on disk, the
    /// registered watcher ([`on_append`]) is told.
    ///
    /// # Errors
    /// The existing record cannot be read, or the state directory cannot be written.
    pub fn append(
        dir: &Path,
        revision: u64,
        access: Access,
        outcome: AccessOutcome,
    ) -> std::io::Result<()> {
        append_with(dir, revision, access, outcome, || {})
    }

    fn append_with(
        dir: &Path,
        revision: u64,
        access: Access,
        outcome: AccessOutcome,
        after_read: impl FnOnce(),
    ) -> std::io::Result<()> {
        append_for_subject(dir, revision, access, outcome, code_subject(), after_read)
    }

    pub(super) fn append_for_subject(
        dir: &Path,
        revision: u64,
        access: Access,
        outcome: AccessOutcome,
        subject: Option<CodeSubject>,
        after_read: impl FnOnce(),
    ) -> std::io::Result<()> {
        write(dir, revision, access, outcome, subject, after_read)?;
        if let Some(appended) = APPENDED.get() {
            appended(revision, access, outcome);
        }
        Ok(())
    }

    fn write(
        dir: &Path,
        revision: u64,
        access: Access,
        outcome: AccessOutcome,
        subject: Option<CodeSubject>,
        after_read: impl FnOnce(),
    ) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let _lock = WriterLock::take(dir)?;
        let path = dir.join(FILE);
        let previous = match std::fs::read_to_string(&path) {
            Ok(previous) => previous,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error),
        };
        let mut lines: Vec<String> = previous.lines().map(str::to_owned).collect();
        after_read();
        // Every reader takes the latest applicable line per row, so a repeat (each new process that judges
        // the same step again) adds nothing it reads, and would only push older steps out of
        // the kept lines: a revision's Local Network step, its first, would be asked again.
        let latest = previous.lines().rev().find_map(|line| {
            let step = parse_step(line)?;
            (step.revision == revision
                && step.access == access
                && (access != Access::LocalNetwork || step.subject == subject))
                .then_some(step.outcome)
        });
        if latest == Some(outcome) {
            return Ok(());
        }
        let mut line = format!("{revision} {} {}", access.word(), outcome.word());
        if access == Access::LocalNetwork
            && let Some(subject) = subject
        {
            line.push(' ');
            line.push_str(&aterm_codec::hex::encode_lower(&subject));
        }
        lines.push(line);
        let start = lines.len().saturating_sub(KEEP);
        let staging = aterm_tempfile::Builder::new()
            .prefix(".system-state-access-")
            .tempdir_in(dir)?;
        let staged = staging.path().join(FILE);
        {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600);
            }
            let mut file = options.open(&staged)?;
            for line in &lines[start..] {
                writeln!(file, "{line}")?;
            }
            // Ordered ahead of the rename: a power loss leaves the old file or
            // the new one, never a torn one, and a lost last step is read again
            // from macOS on the next look. A loss costs a repeat, so a volume
            // that refuses both ordered flushes (a network home whose server
            // takes no `F_FULLFSYNC`) takes a plain `fsync` rather than no
            // record at all, which would ask every step again each launch.
            aterm_durable::ordered_or_fsync(&file)?;
        }
        std::fs::rename(&staged, &path)
    }

    /// Held across the whole read/merge/publication, including by independent
    /// writers in this process. The lock's name is never renamed or removed.
    struct WriterLock(std::fs::File);

    impl WriterLock {
        fn take(dir: &Path) -> std::io::Result<Self> {
            let mut options = std::fs::OpenOptions::new();
            options.read(true).write(true).create(true).truncate(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options
                    .mode(0o600)
                    .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS);
            }
            let file: std::fs::File = options.open(dir.join(LOCK_FILE))?;
            if !file.metadata()?.is_file() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "the access record lock is not a regular file",
                ));
            }
            #[expect(
                clippy::disallowed_methods,
                reason = "the access record's cross-process write guard; Drop releases its file lock"
            )]
            file.lock()?;
            Ok(Self(file))
        }
    }

    impl Drop for WriterLock {
        fn drop(&mut self) {
            let _ = self.0.unlock();
        }
    }

    /// Applicable outcomes for `revision`, oldest first. Local Network events
    /// require a known current code subject matching the recorded subject.
    #[must_use]
    pub fn read(dir: &Path, revision: u64) -> Vec<(Access, AccessOutcome)> {
        read_for_subject(dir, revision, code_subject())
    }

    pub(super) fn read_for_subject(
        dir: &Path,
        revision: u64,
        subject: Option<CodeSubject>,
    ) -> Vec<(Access, AccessOutcome)> {
        outcomes(
            &std::fs::read_to_string(dir.join(FILE)).unwrap_or_default(),
            revision,
            subject,
        )
        .collect()
    }

    /// The outcomes the record's `text` holds for `revision`, oldest first.
    fn outcomes(
        text: &str,
        revision: u64,
        subject: Option<CodeSubject>,
    ) -> impl DoubleEndedIterator<Item = (Access, AccessOutcome)> + '_ {
        text.lines().filter_map(move |line| {
            let (at, access, outcome) = parse_line(line, subject)?;
            (at == revision).then_some((access, outcome))
        })
    }

    struct Step {
        revision: u64,
        access: Access,
        outcome: AccessOutcome,
        subject: Option<CodeSubject>,
    }

    fn parse_step(line: &str) -> Option<Step> {
        let mut words = line.split_whitespace();
        let at = words
            .next()
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))?
            .parse::<u64>()
            .ok()?;
        let access = Access::parse(words.next()?)?;
        let outcome = AccessOutcome::parse(words.next()?)?;
        let subject = match words.next() {
            Some(encoded) if access == Access::LocalNetwork && encoded.len() == 64 => Some(
                aterm_codec::hex::decode_vec(encoded.as_bytes())?
                    .try_into()
                    .ok()?,
            ),
            Some(_) => return None,
            None => None,
        };
        words.next().is_none().then_some(Step {
            revision: at,
            access,
            outcome,
            subject,
        })
    }

    /// Local Network events require the current loaded identity. Legacy unbound
    /// events remain in history but cannot establish another application's grant.
    fn parse_line(
        line: &str,
        subject: Option<CodeSubject>,
    ) -> Option<(u64, Access, AccessOutcome)> {
        let step = parse_step(line)?;
        if step.access == Access::LocalNetwork && (subject.is_none() || step.subject != subject) {
            return None;
        }
        Some((step.revision, step.access, step.outcome))
    }

    /// The latest outcome recorded for `access` in `revision`.
    #[must_use]
    pub fn latest(dir: &Path, revision: u64, access: Access) -> Option<AccessOutcome> {
        latest_for_subject(dir, revision, access, code_subject())
    }

    pub(super) fn latest_for_subject(
        dir: &Path,
        revision: u64,
        access: Access,
        subject: Option<CodeSubject>,
    ) -> Option<AccessOutcome> {
        read_for_subject(dir, revision, subject)
            .into_iter()
            .rev()
            .find_map(|(row, outcome)| (row == access).then_some(outcome))
    }

    /// The last outcome the record holds for `access` in any revision, revision 0 included: the
    /// file's last line for that row. Read like [`read`], so a line this build does not know (an
    /// unknown row, outcome or fields) is skipped. Local Network events require
    /// the current loaded code subject, including when reading an older revision.
    #[must_use]
    pub fn latest_of_any_revision(dir: &Path, access: Access) -> Option<AccessOutcome> {
        latest_of_any_revision_for_subject(dir, access, code_subject())
    }

    fn latest_of_any_revision_for_subject(
        dir: &Path,
        access: Access,
        subject: Option<CodeSubject>,
    ) -> Option<AccessOutcome> {
        let text = std::fs::read_to_string(dir.join(FILE)).unwrap_or_default();
        text.lines().rev().find_map(|line| {
            let (_, row, outcome) = parse_line(line, subject)?;
            (row == access).then_some(outcome)
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::mpsc;
        use std::time::Duration;

        const TEST_SUBJECT: CodeSubject = [0x54; 32];

        fn append(
            dir: &Path,
            revision: u64,
            access: Access,
            outcome: AccessOutcome,
        ) -> std::io::Result<()> {
            append_with(dir, revision, access, outcome, || {})
        }

        fn append_with(
            dir: &Path,
            revision: u64,
            access: Access,
            outcome: AccessOutcome,
            after_read: impl FnOnce(),
        ) -> std::io::Result<()> {
            super::append_for_subject(
                dir,
                revision,
                access,
                outcome,
                Some(TEST_SUBJECT),
                after_read,
            )
        }

        fn read(dir: &Path, revision: u64) -> Vec<(Access, AccessOutcome)> {
            super::read_for_subject(dir, revision, Some(TEST_SUBJECT))
        }

        fn latest(dir: &Path, revision: u64, access: Access) -> Option<AccessOutcome> {
            super::latest_for_subject(dir, revision, access, Some(TEST_SUBJECT))
        }

        fn latest_of_any_revision(dir: &Path, access: Access) -> Option<AccessOutcome> {
            super::latest_of_any_revision_for_subject(dir, access, Some(TEST_SUBJECT))
        }

        #[test]
        fn local_network_events_do_not_cross_code_subjects_or_reuse_unbound_grants() {
            let dir = aterm_tempfile::tempdir().unwrap();
            let root = dir.path();
            let release = Some([0x11; 32]);
            let dev = Some([0x22; 32]);
            std::fs::write(root.join(FILE), "9 local-network granted\n").unwrap();
            assert_eq!(super::read_for_subject(root, 9, dev), []);
            super::append_for_subject(
                root,
                9,
                Access::LocalNetwork,
                AccessOutcome::Granted,
                release,
                || {},
            )
            .unwrap();
            assert_eq!(
                super::read_for_subject(root, 9, release),
                [(Access::LocalNetwork, AccessOutcome::Granted)]
            );
            assert_eq!(super::read_for_subject(root, 9, dev), []);
            assert_eq!(super::read_for_subject(root, 9, None), []);
            super::append_for_subject(
                root,
                9,
                Access::LocalNetwork,
                AccessOutcome::Triggered,
                dev,
                || {},
            )
            .unwrap();
            assert_eq!(
                super::latest_for_subject(root, 9, Access::LocalNetwork, release),
                Some(AccessOutcome::Granted)
            );
            assert_eq!(
                super::latest_of_any_revision_for_subject(root, Access::LocalNetwork, dev),
                Some(AccessOutcome::Triggered)
            );
            let before = std::fs::read(root.join(FILE)).unwrap();
            super::append_for_subject(
                root,
                9,
                Access::LocalNetwork,
                AccessOutcome::Granted,
                release,
                || {},
            )
            .unwrap();
            assert_eq!(
                std::fs::read(root.join(FILE)).unwrap(),
                before,
                "another subject's newer outcome must not defeat deduplication"
            );
        }

        #[test]
        fn access_record_an_unreadable_history_is_preserved_until_it_can_be_read() {
            let dir = aterm_tempfile::tempdir().unwrap();
            let root = dir.path();
            let revision = 8_176;
            append(
                root,
                revision,
                Access::DeveloperTools,
                AccessOutcome::Declined,
            )
            .unwrap();
            append(
                root,
                revision + 1,
                Access::FullDiskAccess,
                AccessOutcome::Granted,
            )
            .unwrap();
            let path = root.join(FILE);
            let readable = std::fs::read(&path).unwrap();
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(&[0xff])
                .unwrap();
            let unreadable = std::fs::read(&path).unwrap();
            assert_eq!(
                std::fs::read_to_string(&path).unwrap_err().kind(),
                std::io::ErrorKind::InvalidData
            );

            let result = append(
                root,
                revision,
                Access::LocalNetwork,
                AccessOutcome::Triggered,
            );
            assert_eq!(
                std::fs::read(&path).unwrap(),
                unreadable,
                "a failed history read must preserve the earlier answers"
            );
            assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);

            // Repair this test's damaged input, then the same append can succeed
            // while retaining both earlier outcomes and their separate revisions.
            std::fs::write(&path, readable).unwrap();
            append(
                root,
                revision,
                Access::LocalNetwork,
                AccessOutcome::Triggered,
            )
            .unwrap();
            assert_eq!(
                read(root, revision),
                [
                    (Access::DeveloperTools, AccessOutcome::Declined),
                    (Access::LocalNetwork, AccessOutcome::Triggered),
                ]
            );
            assert_eq!(
                latest(root, revision + 1, Access::FullDiskAccess),
                Some(AccessOutcome::Granted)
            );
        }

        #[test]
        fn access_record_write_failure_releases_its_lock_and_removes_its_staging() {
            let dir = aterm_tempfile::tempdir().unwrap();
            let root = dir.path();
            assert!(
                append_with(
                    root,
                    8_175,
                    Access::Debugger,
                    AccessOutcome::Granted,
                    || {
                        std::fs::create_dir(root.join(FILE)).unwrap();
                    }
                )
                .is_err()
            );
            let remaining: Vec<_> = std::fs::read_dir(root)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            assert_eq!(
                remaining.len(),
                2,
                "a failed publication kept private staging"
            );
            assert!(remaining.contains(&std::ffi::OsString::from(FILE)));
            assert!(remaining.contains(&std::ffi::OsString::from(LOCK_FILE)));
            let probe = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(root.join(LOCK_FILE))
                .unwrap();
            #[expect(
                clippy::disallowed_methods,
                reason = "an independent handle verifies the failed publication released its writer lock"
            )]
            probe.try_lock().unwrap();
            probe.unlock().unwrap();
            std::fs::remove_dir(root.join(FILE)).unwrap();
            append(root, 8_175, Access::Debugger, AccessOutcome::Granted).unwrap();
            assert_eq!(
                latest(root, 8_175, Access::Debugger),
                Some(AccessOutcome::Granted)
            );
        }

        #[test]
        fn access_record_concurrent_writers_keep_independent_outcomes_and_the_latest_decline() {
            let dir = aterm_tempfile::tempdir().unwrap();
            let root = dir.path();
            let revision = 8_173;
            let other_revision = revision + 1;
            append(
                root,
                revision,
                Access::FullDiskAccess,
                AccessOutcome::Handed,
            )
            .unwrap();
            append(
                root,
                other_revision,
                Access::FullDiskAccess,
                AccessOutcome::Granted,
            )
            .unwrap();
            let (ready, read_ready) = mpsc::sync_channel(1);
            let (release, resume) = mpsc::sync_channel(1);
            let (first_done, first_result) = mpsc::sync_channel(1);
            let first_root = root.to_path_buf();
            let first = std::thread::spawn(move || {
                let result = append_with(
                    &first_root,
                    revision,
                    Access::Debugger,
                    AccessOutcome::Granted,
                    || {
                        // Coordinate the old unlocked path and the corrected locked path
                        // without waiting for a writer held behind our own lock.
                        let probe = std::fs::OpenOptions::new()
                            .read(true)
                            .write(true)
                            .create(true)
                            .truncate(false)
                            .open(first_root.join(LOCK_FILE))
                            .unwrap();
                        #[expect(
                            clippy::disallowed_methods,
                            reason = "an independently opened handle samples writer ownership only to schedule this regression"
                        )]
                        let unlocked = match probe.try_lock() {
                            Ok(()) => {
                                probe.unlock().unwrap();
                                true
                            }
                            Err(std::fs::TryLockError::WouldBlock) => false,
                            Err(std::fs::TryLockError::Error(error)) => panic!("{error}"),
                        };
                        ready.send(unlocked).unwrap();
                        resume.recv_timeout(Duration::from_secs(60)).unwrap();
                    },
                );
                first_done.send(result).unwrap();
            });
            let unlocked = read_ready.recv_timeout(Duration::from_secs(60)).unwrap();
            let (second_done, second_result) = mpsc::sync_channel(1);
            let second_root = root.to_path_buf();
            let second = std::thread::spawn(move || {
                second_done
                    .send(append(
                        &second_root,
                        revision,
                        Access::FullDiskAccess,
                        AccessOutcome::Declined,
                    ))
                    .unwrap();
            });
            let result = if unlocked {
                let result = second_result.recv_timeout(Duration::from_secs(60)).unwrap();
                release.send(()).unwrap();
                result
            } else {
                release.send(()).unwrap();
                second_result.recv_timeout(Duration::from_secs(60)).unwrap()
            };
            result.unwrap();
            first_result
                .recv_timeout(Duration::from_secs(60))
                .unwrap()
                .unwrap();
            first.join().unwrap();
            second.join().unwrap();

            let rows = read(root, revision);
            assert!(rows.contains(&(Access::Debugger, AccessOutcome::Granted)));
            assert!(rows.contains(&(Access::FullDiskAccess, AccessOutcome::Handed)));
            assert!(
                rows.contains(&(Access::FullDiskAccess, AccessOutcome::Declined)),
                "an independent append lost the newer Not now: {rows:?}"
            );
            assert_eq!(rows.len(), 3);
            assert_eq!(
                latest(root, revision, Access::FullDiskAccess),
                Some(AccessOutcome::Declined)
            );
            assert_eq!(
                read(root, other_revision),
                [(Access::FullDiskAccess, AccessOutcome::Granted)]
            );
        }

        /// The Commons runtime asks whether macOS ever asked aterm about Local Network: the row's
        /// last line in any revision, revision 0 (an older aterm's legacy run) included, past every
        /// line this build does not know.
        #[test]
        fn access_record_the_latest_of_any_revision_reads_every_revision_and_skips_unknown_lines() {
            let dir = aterm_tempfile::tempdir().unwrap();
            let root = dir.path();
            assert_eq!(latest_of_any_revision(root, Access::LocalNetwork), None);
            std::fs::write(root.join(FILE), "0 local-network triggered\n").unwrap();
            assert_eq!(
                latest_of_any_revision(root, Access::LocalNetwork),
                None,
                "an unbound legacy event establishes no current identity's access"
            );
            append(root, 0, Access::LocalNetwork, AccessOutcome::Triggered).unwrap();
            assert_eq!(
                latest_of_any_revision(root, Access::LocalNetwork),
                Some(AccessOutcome::Triggered)
            );
            std::fs::write(
                root.join(FILE),
                format!(
                    "0 local-network triggered\n\
                 7 local-network granted {}\n\
                 9 full-disk-access handed\n\
                 12 local-network asked-twice\n\
                 13 bluetooth triggered\n\
                 14 local-network declined later\n\
                 not-a-revision local-network declined\n",
                    aterm_codec::hex::encode_lower(&TEST_SUBJECT)
                ),
            )
            .unwrap();
            assert_eq!(
                latest_of_any_revision(root, Access::LocalNetwork),
                Some(AccessOutcome::Granted),
                "an older revision's line, past another row's and every unknown or newer line"
            );
            assert_eq!(
                latest_of_any_revision(root, Access::FullDiskAccess),
                Some(AccessOutcome::Handed)
            );
            assert_eq!(latest_of_any_revision(root, Access::Debugger), None);
            append(root, 3, Access::LocalNetwork, AccessOutcome::Triggered).unwrap();
            assert_eq!(
                latest_of_any_revision(root, Access::LocalNetwork),
                Some(AccessOutcome::Triggered),
                "the file's last line for the row, whatever its revision"
            );
        }
    }
}

/// The GitHub repositories row's short note when GitHub refuses the token the vault syncs (the
/// menu line's and `aterm doctor`'s, at most 52 characters): who renews it. The vault's
/// `gh_access_token` is in its `[everyone]` section, which only a vault admin writes
/// (`aterm secrets set`). System State's row says it whole, with the line they run.
pub const GITHUB_TOKEN_REFUSED: &str = "GitHub refuses your token; a vault admin renews it";

/// The one line a vault admin runs on their Mac, with a new GitHub token copied, to save it in the
/// vault for every Mac ([`GITHUB_TOKEN_REFUSED`]).
pub const GITHUB_TOKEN_COMMAND: &str = "pbpaste | aterm secrets set gh_access_token";

/// The Secrets vault row's value while this Mac asks ALab's members to let it in
/// (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.6 step 3).
pub const VAULT_ASKING: &str = "Asking ALab";

/// The Secrets vault row's note while automatic LAN admission is pending.
pub const VAULT_ASKING_NOTE: &str = "Joining ALab automatically; aterm keeps retrying";

/// The Secrets vault row's value once a member let this Mac in, until the vault records it.
pub const VAULT_ALLOWED: &str = "Allowed";

/// The Secrets vault row's note while it reads [`VAULT_ALLOWED`]: nothing is the person's to do.
pub const VAULT_ALLOWED_NOTE: &str = "The vault records this Mac by itself";

/// A stage of a setup pass as its row reaches System State and `aterm doctor`: the name the row
/// carries and the settings whose rows the stage changes. A stage row says what its stage waits
/// on (parked until Check again, or resuming after aterm stopped), which the page says on those
/// settings' rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StageRow {
    /// The row's name.
    pub name: &'static str,
    /// The settings whose rows the stage changes.
    pub settings: &'static [Setting],
}

impl StageRow {
    /// How the words of a stage row parked after it took over an hour end (init-on-by-default
    /// design §6.5): first why it stopped (`Took over an hour cloning GitHub repositories;`),
    /// then this. System State checks again by itself, a little later each time, so nobody has
    /// to: a stage that hangs again costs another hour, never aterm itself.
    pub const PARKED_TAIL: &str = "aterm checks again by itself.";

    /// How the words of a stage row parked after aterm stopped twice in it end (§6.5): first why
    /// (`Stopped twice while cloning GitHub repositories;`), then this. The park guards against
    /// a stage that stops aterm, every window with it, so nothing lifts it by itself: Check again
    /// does, when the person chooses to try it.
    pub const HELD_TAIL: &str = "it waits for Check again.";

    /// How the words of a stage row end that has run past the hang limit in the aterm still
    /// running it (init-on-by-default design §6.5): first what it is still doing (`Still cloning
    /// GitHub repositories after more than an hour;`), then this. It offers no Check again: the
    /// stage still runs there, so a try now could change nothing.
    pub const STILL_RUNNING_TAIL: &str = "it ends by itself or when aterm quits.";

    /// The words of a stage row resumed after aterm stopped in it.
    pub const RESUMING: &str = "Resuming after aterm stopped.";

    /// How the words of a Messages or FaceTime sign-out's row end while the Accounts stage is
    /// parked for want of a person at this Mac (init-on-by-default design §6.4): the pass that
    /// found nobody at System State only read the accounts, or a sign-out found nobody there
    /// as its clicks were to start. System State reads them by this, and looks for the person's
    /// return only then.
    pub const WAITS_FOR_A_PERSON: &str = "runs while you are at this Mac with System State on \
                                          screen: it opens Messages and FaceTime, and macOS may \
                                          ask for Accessibility";

    /// The stage whose row is named `name`, if one.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        STAGE_ROWS.into_iter().find(|stage| stage.name == name)
    }
}

/// The macOS preferences stage: input and desktop settings.
pub const WORKSTATION_STAGE: StageRow = StageRow {
    name: "Selected Mac settings",
    settings: &[
        Setting::CapsLock,
        Setting::NaturalScrolling,
        Setting::TrackpadScrolling,
        Setting::Desktop,
        Setting::Widgets,
        Setting::Dock,
    ],
};

/// The stage that keeps Rust's documentation out of Spotlight.
pub const SPOTLIGHT_STAGE: StageRow = StageRow {
    name: "Spotlight and Rust documentation",
    settings: &[Setting::Spotlight],
};

/// The stage that asks for aterm's permissions.
pub const PERMISSIONS_STAGE: StageRow = StageRow {
    name: "aterm permissions",
    settings: &[Setting::Permissions],
};

/// The stage that joins the Secrets vault.
pub const SECRETS_STAGE: StageRow = StageRow {
    name: SECRETS_VAULT_ROW,
    settings: &[Setting::Secrets],
};

/// The stage that clones the GitHub repositories.
pub const REPOSITORIES_STAGE: StageRow = StageRow {
    name: "GitHub repositories",
    settings: &[Setting::Repositories],
};

/// The stage that signs Messages and FaceTime out.
pub const ACCOUNTS_STAGE: StageRow = StageRow {
    name: "Messages and FaceTime sign-out",
    settings: &[Setting::Messages, Setting::FaceTime],
};

/// Every stage row, in the order a pass runs them.
pub const STAGE_ROWS: [StageRow; 6] = [
    WORKSTATION_STAGE,
    SPOTLIGHT_STAGE,
    PERMISSIONS_STAGE,
    SECRETS_STAGE,
    REPOSITORIES_STAGE,
    ACCOUNTS_STAGE,
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The tools are held by one of Apple's command-line installers (as the caller's exact
    /// comparison names them: a stand-in here) beside the placeholder, whatever else runs, else
    /// by Apple's own installer app, by absolute path, with the placeholder beside it or not: a
    /// placeholder beside another program, even one named like an installer, or beside a process
    /// the kernel names no executable for, is held by nothing, and a command-line installer with
    /// no placeholder installs something else.
    /// NEGATIVE CONTROL: the same processes with the installer's or the window's row taken out
    /// hold nothing, placeholder or not.
    #[test]
    fn the_tools_are_held_only_by_their_own_installers() {
        let cli = |exe: &std::path::Path| exe == std::path::Path::new("/stand-in/installs");
        let apple = "/System/Library/CoreServices/Install Command Line Developer Tools.app/\
                     Contents/MacOS/Install Command Line Developer Tools";
        let held = |placeholder: bool, exes: &[Option<&str>]| {
            tools_held_by(placeholder, exes.iter().copied(), cli)
        };
        let (line, window) = (
            Some(ToolsInstaller::CommandLine),
            Some(ToolsInstaller::AppleWindow),
        );
        assert_eq!(held(true, &[Some("/stand-in/installs")]), line);
        assert_eq!(held(true, &[Some("/bin/zsh"), Some(apple)]), window);
        assert_eq!(
            held(true, &[Some(apple), Some("/stand-in/installs")]),
            line,
            "an install under way outranks the window beside it"
        );
        assert_eq!(held(true, &[None, Some(apple)]), window, "{apple}");
        assert_eq!(held(true, &[None, Some("/stand-in/installs")]), line);
        assert_eq!(
            held(true, &[None, Some("/bin/zsh")]),
            None,
            "an unnamed row"
        );
        assert_eq!(held(true, &[Some("/bin/zsh")]), None, "the control");
        assert_eq!(held(true, &[]), None);
        assert_eq!(
            held(false, &[Some(apple)]),
            window,
            "the window with no placeholder beside it"
        );
        assert_eq!(
            held(false, &[Some("/stand-in/installs"), Some(apple)]),
            window,
            "a command-line installer counts only beside the placeholder"
        );
        assert_eq!(held(false, &[None, Some(apple)]), window);
        assert_eq!(held(false, &[Some("/stand-in/installs")]), None);
        assert_eq!(held(false, &[Some("/bin/zsh"), None]), None, "the control");
        assert_eq!(
            tools_installer(std::path::Path::new("/stand-in/installs"), cli),
            line
        );
        assert_eq!(tools_installer(std::path::Path::new(apple), cli), window);
        for other in [
            "/tmp/installs",
            "stand-in/installs",
            "/System/Library/CoreServices/Software Update.app/Contents/Resources/softwareupdated",
            "Install Command Line Developer Tools.app/Contents/MacOS/Install Command Line Developer Tools",
        ] {
            assert_eq!(held(true, &[Some(other)]), None, "{other}");
            assert_eq!(
                tools_installer(std::path::Path::new(other), cli),
                None,
                "{other}"
            );
        }
    }

    /// A placeholder is a live run's only when its stamp names a process alive from the start
    /// the stamp names: a gone process, a pid another process took since, a start of `0` (which
    /// matches nothing, whatever runs under the pid) and any other contents are no run's.
    #[test]
    fn a_placeholder_is_a_live_runs_only_from_the_start_its_stamp_names() {
        let alive = |pid: u32| (pid == 4242).then_some(1000);
        let stamp = placeholder_stamp(4242, 1000);
        assert_eq!(stamp, "aterm 4242 1000\n");
        assert!(placeholder_held_by_a_live_run(stamp.as_bytes(), alive));
        for other in [
            placeholder_stamp(4242, 999),
            placeholder_stamp(4243, 1000),
            placeholder_stamp(4242, 0),
            "aterm 4242\n".to_owned(),
            "aterm 4242 1000 extra\n".to_owned(),
            "apple 4242 1000\n".to_owned(),
            String::new(),
        ] {
            assert!(
                !placeholder_held_by_a_live_run(other.as_bytes(), alive),
                "{other:?}"
            );
        }
        assert!(!placeholder_held_by_a_live_run(
            placeholder_stamp(4242, 0).as_bytes(),
            |_| Some(0)
        ));
        assert!(stamp.len() <= PLACEHOLDER_STAMP_MAX);
        assert!(placeholder_stamp(u32::MAX, u64::MAX).len() <= PLACEHOLDER_STAMP_MAX);
    }

    /// A `Subject::Sync` row rides `Setting::Secrets` (its group, setting and wanted all follow
    /// it, and it uses the vault source) and is not enumerated by `all()`, so no Sync row is
    /// built in shipped code yet. Its six kinds are distinct and have distinct names. The
    /// negative control is `all()`'s zero Sync subjects: were the stub enumerated, this would
    /// fail.
    #[test]
    fn a_sync_row_is_a_stub_that_rides_secrets_and_stays_off_the_page() {
        for kind in SyncKind::ALL {
            let row = Subject::Sync(kind);
            assert_eq!(row.group(), Setting::Secrets.group(), "{kind:?}");
            assert_eq!(row.setting(), Setting::Secrets, "{kind:?}");
            assert_eq!(row.wanted(), Setting::Secrets.wanted(), "{kind:?}");
            assert_eq!(row.name(), kind.name(), "{kind:?}");
            assert!(row.uses_vault_source(), "{kind:?}");
        }
        // Not enumerated: no Sync row is built in shipped code.
        assert_eq!(
            Subject::all()
                .into_iter()
                .filter(|subject| matches!(subject, Subject::Sync(_)))
                .count(),
            0
        );
        // The six kinds are distinct, and so are their names.
        for (i, a) in SyncKind::ALL.iter().enumerate() {
            for b in &SyncKind::ALL[i + 1..] {
                assert_ne!(a, b);
                assert_ne!(a.name(), b.name(), "{a:?} vs {b:?}");
            }
        }
    }

    /// A Restore's outcome holds one entry per row, its worst key speaking for it, in page
    /// order with the keys of no row last.
    #[test]
    fn a_restore_outcome_is_one_entry_per_row_in_page_order_worst_first() {
        let outcome = RestoreOutcome::of([
            (None, PutBack::Failed),
            (Some(Setting::Dock), PutBack::Back),
            (Some(Setting::NaturalScrolling), PutBack::Back),
            (Some(Setting::NaturalScrolling), PutBack::AtLogin),
            (Some(Setting::Dock), PutBack::Waits),
            (Some(Setting::Dock), PutBack::Back),
            (Some(Setting::CapsLock), PutBack::AtLogin),
        ]);
        assert_eq!(
            outcome.rows,
            [
                (Some(Setting::CapsLock), PutBack::AtLogin),
                (Some(Setting::NaturalScrolling), PutBack::AtLogin),
                (Some(Setting::Dock), PutBack::Waits),
                (None, PutBack::Failed),
            ]
        );
        assert!(!outcome.journal_refused);
        assert_eq!(until_login("Control"), "Control · until login");
        assert_eq!(live_until_login(&until_login("Control")), Some("Control"));
        assert_eq!(live_until_login("Control"), None);
    }

    /// A pass's outcome names each row once per way its change did not finish, in page order,
    /// and a later part of the pass adds to it: its rollback joins the earlier one row by row.
    #[test]
    fn a_pass_outcome_names_each_row_once_and_takes_in_its_later_parts() {
        let mut outcome = PassOutcome::default();
        outcome.unfinished(Setting::Dock, Unfinished::Failed);
        outcome.unfinished(Setting::CapsLock, Unfinished::Unconfirmed);
        outcome.unfinished(Setting::Dock, Unfinished::Failed);
        outcome.unfinished(Setting::CapsLock, Unfinished::Failed);
        assert_eq!(
            outcome.unfinished,
            [
                (Setting::CapsLock, Unfinished::Unconfirmed),
                (Setting::CapsLock, Unfinished::Failed),
                (Setting::Dock, Unfinished::Failed),
            ]
        );
        outcome.rolled_back = Some(RestoreOutcome::of([(Some(Setting::Dock), PutBack::Back)]));
        let mut later = RestoreOutcome::of([(Some(Setting::Dock), PutBack::AtLogin)]);
        later.journal_refused = true;
        outcome.merge(PassOutcome {
            rolled_back: Some(later),
            unfinished: vec![(Setting::Widgets, Unfinished::Failed)],
        });
        let rolled_back = outcome.rolled_back.unwrap();
        assert_eq!(rolled_back.rows, [(Some(Setting::Dock), PutBack::AtLogin)]);
        assert!(rolled_back.journal_refused);
        assert_eq!(outcome.unfinished.len(), 4);
    }

    #[test]
    fn every_row_defaults_on_and_each_checkbox_is_independent() {
        assert_eq!(Selection::default(), Selection::all());
        for row in Setting::ALL {
            let mut selected = Selection::all();
            selected.set(row, false);
            for other in Setting::ALL {
                assert_eq!(selected.contains(other), other != row);
            }
            selected.set(row, true);
            assert_eq!(selected, Selection::all());
        }
    }

    #[test]
    fn plans_round_trip_and_unknown_groups_are_refused() {
        for bits in 0..=Selection::all().bits() {
            let selected = Selection::from_bits(bits).unwrap();
            assert_eq!(selected.bits(), bits);
            assert_eq!(selected.is_empty(), bits == 0);
        }
        assert_eq!(Selection::from_bits(1 << Setting::ALL.len()), None);
        assert_eq!(Selection::from_bits(u16::MAX), None);
    }

    #[test]
    fn every_setting_sits_in_exactly_the_group_that_lists_it() {
        for setting in Setting::ALL {
            let owners: Vec<_> = Group::ALL
                .into_iter()
                .filter(|group| group.settings().contains(&setting))
                .collect();
            assert_eq!(owners, vec![setting.group()], "{setting:?}");
        }
        let subjects = Subject::all();
        assert_eq!(subjects.len(), Setting::ALL.len() - 1 + Access::ALL.len());
        for subject in &subjects {
            assert_eq!(subject.group(), subject.setting().group());
            assert!(!subject.name().is_empty() && !subject.wanted().is_empty());
        }
        assert!(
            subjects
                .iter()
                .all(|subject| *subject != Subject::Setting(Setting::Permissions))
        );
    }

    #[test]
    fn accessibility_is_needed_only_while_a_sign_out_is_managed() {
        let mut selection = Selection::none();
        assert!(!Access::Accessibility.needed_for(selection));
        assert!(Access::FullDiskAccess.needed_for(selection));
        selection.set(Setting::FaceTime, true);
        assert!(Access::Accessibility.needed_for(selection));
    }

    #[test]
    fn only_settled_rows_count_and_only_people_and_failures_draw_attention() {
        for standing in [
            Standing::InPlace,
            Standing::Differs,
            Standing::NeedsYou,
            Standing::Waiting,
            Standing::AtLogin,
            Standing::Failed,
            Standing::Unknown,
            Standing::NotNeeded,
            Standing::Untold,
        ] {
            assert_eq!(
                standing.settled(),
                matches!(
                    standing,
                    Standing::InPlace | Standing::AtLogin | Standing::NotNeeded | Standing::Untold
                )
            );
            assert_eq!(
                standing.attention(),
                matches!(standing, Standing::NeedsYou | Standing::Failed)
            );
        }
        let unread = Observation::unknown(Subject::Access(Access::LocalNetwork));
        assert_eq!(unread.now, None);
        assert_eq!(unread.wanted, "On");
    }

    /// Every failed step's status names its kind, a status that names none is a retry's, and
    /// no status is ever a person's: a reading says that (design §3.6).
    #[test]
    fn a_step_status_names_its_failure_kind_and_never_a_persons() {
        assert_eq!(
            FailureKind::of_status("error"),
            Some(FailureKind::Transient)
        );
        assert_eq!(
            FailureKind::of_status("failed"),
            Some(FailureKind::Transient)
        );
        for calm in ["ok", "applied", "waiting", "blocked", "unverified", "noted"] {
            assert_eq!(FailureKind::of_status(calm), None, "{calm}");
        }
        assert!(FailureKind::Transient.retried());
        assert!(!FailureKind::NeedsPerson.retried());
    }

    #[test]
    fn the_original_workstation_bits_remain_separate_from_new_setup_stages() {
        let original = Selection::all().workstation();
        assert_eq!(original.bits(), 0xff);
        for setting in Setting::ALL {
            assert_eq!(original.contains(setting), setting.is_workstation());
        }
        assert_eq!(Plan::default().selection, Selection::all());
        assert_eq!(Plan::from(Selection::none()).selection, Selection::none());
    }

    #[test]
    fn the_access_record_keeps_each_revision_apart_and_skips_foreign_lines() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path();
        assert!(access_record::read(root, 7).is_empty());
        access_record::append(root, 7, Access::FullDiskAccess, AccessOutcome::Handed).unwrap();
        access_record::append(root, 8, Access::Debugger, AccessOutcome::Declined).unwrap();
        access_record::append(root, 7, Access::FullDiskAccess, AccessOutcome::Granted).unwrap();
        std::fs::write(
            root.join(access_record::FILE),
            std::fs::read_to_string(root.join(access_record::FILE)).unwrap()
                + "7 telepathy handed\nnot a line\n7 debugger handed extra\n",
        )
        .unwrap();
        assert_eq!(
            access_record::read(root, 7),
            vec![
                (Access::FullDiskAccess, AccessOutcome::Handed),
                (Access::FullDiskAccess, AccessOutcome::Granted),
            ]
        );
        assert_eq!(
            access_record::latest(root, 7, Access::FullDiskAccess),
            Some(AccessOutcome::Granted)
        );
        assert_eq!(access_record::latest(root, 7, Access::Debugger), None);
        assert_eq!(
            access_record::latest(root, 8, Access::Debugger),
            Some(AccessOutcome::Declined)
        );
        let lines = || {
            std::fs::read_to_string(root.join(access_record::FILE))
                .unwrap()
                .lines()
                .count()
        };
        for n in 0..100 {
            let outcome = [AccessOutcome::Triggered, AccessOutcome::Granted][n % 2];
            access_record::append(root, 9, Access::LocalNetwork, outcome).unwrap();
            assert!(lines() <= 64, "{n}");
        }
        assert_eq!(lines(), 64, "the file is kept to its last lines");
    }

    /// A step judged again in each new process (a Not now that still holds) repeats its row's
    /// latest outcome: the record keeps it once, so it never pushes the revision's earlier steps,
    /// Local Network's first, out of the kept lines. A changed outcome is written, a repeat of an
    /// earlier one included, and so is the same outcome in another revision.
    #[test]
    fn the_access_record_does_not_repeat_a_rows_latest_outcome() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path();
        let subject = Some([0x54; 32]);
        let lines = || {
            std::fs::read_to_string(root.join(access_record::FILE))
                .unwrap()
                .lines()
                .count()
        };
        access_record::append_for_subject(
            root,
            9,
            Access::LocalNetwork,
            AccessOutcome::Triggered,
            subject,
            || {},
        )
        .unwrap();
        for _ in 0..100 {
            access_record::append(root, 9, Access::FullDiskAccess, AccessOutcome::Declined)
                .unwrap();
        }
        assert_eq!(lines(), 2);
        assert_eq!(
            access_record::latest_for_subject(root, 9, Access::LocalNetwork, subject),
            Some(AccessOutcome::Triggered)
        );
        access_record::append(root, 9, Access::FullDiskAccess, AccessOutcome::Handed).unwrap();
        access_record::append(root, 9, Access::FullDiskAccess, AccessOutcome::Declined).unwrap();
        access_record::append(root, 10, Access::FullDiskAccess, AccessOutcome::Declined).unwrap();
        assert_eq!(lines(), 5);
        assert_eq!(
            access_record::read_for_subject(root, 9, subject),
            vec![
                (Access::LocalNetwork, AccessOutcome::Triggered),
                (Access::FullDiskAccess, AccessOutcome::Declined),
                (Access::FullDiskAccess, AccessOutcome::Handed),
                (Access::FullDiskAccess, AccessOutcome::Declined),
            ]
        );
    }

    /// The watcher reads the line back from disk; a failed write tells it nothing.
    #[test]
    fn a_written_access_step_tells_its_watcher_after_the_line_is_on_disk() {
        use std::sync::Mutex;
        /// A line the watcher was told of, and what the record held for it then.
        type Seen = (u64, Access, AccessOutcome, Option<AccessOutcome>);
        static SEEN: Mutex<Vec<Seen>> = Mutex::new(Vec::new());
        static DIR: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);
        fn appended(revision: u64, access: Access, outcome: AccessOutcome) {
            // Only this test's lines: other tests append to their own directories.
            if !(4_201..=4_202).contains(&revision) {
                return;
            }
            let dir = DIR.lock().unwrap().clone().unwrap();
            let probe = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(dir.join(".system-state-access.lock"))
                .unwrap();
            #[expect(
                clippy::disallowed_methods,
                reason = "the watcher must run after the publication has released its file lock"
            )]
            probe.try_lock().unwrap();
            probe.unlock().unwrap();
            let on_disk = access_record::latest(&dir, revision, access);
            SEEN.lock()
                .unwrap()
                .push((revision, access, outcome, on_disk));
            access_record::append(&dir, 4_203, Access::Debugger, AccessOutcome::Granted).unwrap();
        }
        access_record::on_append(appended);
        let dir = aterm_tempfile::tempdir().unwrap();
        *DIR.lock().unwrap() = Some(dir.path().to_path_buf());
        access_record::append(
            dir.path(),
            4_201,
            Access::FullDiskAccess,
            AccessOutcome::Granted,
        )
        .unwrap();
        let unwritable = dir.path().join(access_record::FILE).join("no");
        assert!(
            access_record::append(&unwritable, 4_202, Access::Debugger, AccessOutcome::Granted)
                .is_err()
        );
        assert_eq!(
            *SEEN.lock().unwrap(),
            [(
                4_201,
                Access::FullDiskAccess,
                AccessOutcome::Granted,
                Some(AccessOutcome::Granted)
            )]
        );
        assert_eq!(
            access_record::latest(dir.path(), 4_203, Access::Debugger),
            Some(AccessOutcome::Granted),
            "the watcher can append to the record it just observed"
        );
    }

    #[test]
    fn a_macos_dialog_counts_as_up_only_while_its_guard_lives() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static READY: AtomicUsize = AtomicUsize::new(0);
        fn ready() {
            READY.fetch_add(1, Ordering::SeqCst);
        }
        on_macos_dialog_waiter_ready(ready);
        assert!(!macos_dialog_up());
        let first = MacosDialog::up();
        let second = MacosDialog::up();
        drop(first);
        assert!(macos_dialog_up());
        assert!(MacosDialog::when_none_up(std::time::Duration::ZERO).is_none());
        drop(second);
        assert!(!macos_dialog_up());
        let waited = MacosDialog::when_none_up(std::time::Duration::ZERO);
        assert!(waited.is_some() && macos_dialog_up());
        // Taken from none up in one step: a second caller sees it up.
        assert!(MacosDialog::when_none_up(std::time::Duration::ZERO).is_none());
        drop(waited);
        assert!(!macos_dialog_up());
        let max_waited = MacosDialog::when_none_up(std::time::Duration::MAX);
        assert!(max_waited.is_some() && macos_dialog_up());
        drop(max_waited);
        assert!(!macos_dialog_up());
        assert_eq!(READY.load(Ordering::SeqCst), 0);

        let first = MacosDialog::up();
        let last = MacosDialog::up();
        assert!(MacosDialog::when_none_up(std::time::Duration::from_nanos(1)).is_none());
        drop(first);
        assert_eq!(READY.load(Ordering::SeqCst), 0);
        drop(last);
        assert_eq!(READY.load(Ordering::SeqCst), 1);
        drop(MacosDialog::up());
        assert_eq!(READY.load(Ordering::SeqCst), 1, "the waiter was consumed");

        // The last dialog can end after the timed-out caller saw contention
        // but before it arms the wake. Its post-arm check still notifies once.
        arm_macos_dialog_waiter();
        assert_eq!(READY.load(Ordering::SeqCst), 2);
        notify_macos_dialog_waiter();
        assert_eq!(READY.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn system_state_coming_to_the_front_calls_its_watcher_once_per_arrival() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SHOWN: AtomicUsize = AtomicUsize::new(0);
        fn shown() {
            SHOWN.fetch_add(1, Ordering::SeqCst);
        }
        on_system_state_front(shown);
        assert!(!system_state_front());
        set_system_state_front(true);
        set_system_state_front(true);
        assert!(system_state_front());
        assert_eq!(SHOWN.load(Ordering::SeqCst), 1);
        set_system_state_front(false);
        assert!(!system_state_front());
        set_system_state_front(true);
        assert_eq!(SHOWN.load(Ordering::SeqCst), 2);
        set_system_state_front(false);
    }

    #[test]
    fn an_opt_out_is_kept_per_grant_until_the_window_clears_it() {
        use opt_out::OptOut;
        let off = OptOut::WhileOff("You turned it off.");
        assert_eq!(opt_out::get(Access::Debugger), None);
        opt_out::set(Access::Debugger, Some(off));
        assert_eq!(opt_out::get(Access::Debugger), Some(off));
        for other in Access::ALL {
            if other != Access::Debugger {
                assert_eq!(opt_out::get(other), None, "{other:?}");
            }
        }
        opt_out::set(Access::Debugger, None);
        assert_eq!(opt_out::get(Access::Debugger), None);
    }

    #[test]
    fn a_turned_off_opt_out_holds_only_of_an_off_reading() {
        use opt_out::OptOut;
        assert_eq!(OptOut::WhileOff("off").holds(true), Some("off"));
        assert_eq!(OptOut::WhileOff("off").holds(false), None);
        for reads_off in [true, false] {
            assert_eq!(OptOut::Always("switch").holds(reads_off), Some("switch"));
        }
    }
}
