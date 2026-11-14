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

/// The launchd label of the login job that resumes an unfinished setup run with no window
/// (`aterm_cli::macos_doctor::SETUP_RESUME_LABEL`), which an uninstall's sweep removes too.
pub const SETUP_RESUME_LABEL: &str = "systems.alab.aterm-setup";

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
            Self::Secrets => VAULT_LOCAL_READY,
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
    /// Accessibility, which the Messages and FaceTime sign-outs need.
    Accessibility,
}

impl Access {
    /// Every access row, in page order.
    pub const ALL: [Self; 4] = [
        Self::FullDiskAccess,
        Self::DeveloperTools,
        Self::Debugger,
        Self::Accessibility,
    ];

    /// The row's name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::FullDiskAccess => "Full Disk Access",
            Self::DeveloperTools => "Developer Tools",
            Self::Debugger => "Debugger access",
            Self::Accessibility => "Accessibility",
        }
    }

    /// What aterm wants the grant to read.
    #[must_use]
    pub const fn wanted(self) -> &'static str {
        match self {
            Self::FullDiskAccess | Self::DeveloperTools | Self::Debugger | Self::Accessibility => {
                "On"
            }
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
    /// One kind of credential synced across the person's Macs, riding [`Setting::Secrets`].
    /// Live readers supply evidenced rows; unimplemented categories are not enumerated by
    /// [`Subject::all`] and never acquire a guessed successful state.
    Sync(SyncKind),
}

impl Subject {
    /// Whether this row's read reads another app's data (the Mac settings, the sign-outs'
    /// account state, Spotlight's project inventory), which an ordinary refresh withholds while
    /// System State is not in front with a person ([`Observation::withheld`]): nothing in the
    /// background reads it again, so its reading stands for the row now only while it is
    /// [`Observation::current_at`].
    #[must_use]
    pub const fn reads_privately(self) -> bool {
        match self {
            Self::Setting(setting) => {
                setting.is_workstation() || matches!(setting, Setting::Spotlight)
            }
            Self::Access(_) | Self::Sync(_) => false,
        }
    }

    /// The row's word in the `setup=` status field and the coordinator's `setup status` reply
    /// (`fda`, `dock`, `vault`, …): what a reader away from the page names the row by.
    #[must_use]
    pub const fn setup_word(self) -> &'static str {
        match self {
            Self::Access(Access::FullDiskAccess) => "fda",
            Self::Access(Access::DeveloperTools) => "devtools",
            Self::Access(Access::Debugger) => "debugger",
            Self::Access(Access::Accessibility) => "ax",
            Self::Setting(Setting::CapsLock) => "capslock",
            Self::Setting(Setting::NaturalScrolling) => "scrolling",
            Self::Setting(Setting::TrackpadScrolling) => "trackpad",
            Self::Setting(Setting::Desktop) => "desktop",
            Self::Setting(Setting::Widgets) => "widgets",
            Self::Setting(Setting::Dock) => "dock",
            Self::Setting(Setting::Messages) => "messages",
            Self::Setting(Setting::FaceTime) => "facetime",
            Self::Setting(Setting::Spotlight) => "spotlight",
            Self::Setting(Setting::Permissions) => "permissions",
            Self::Setting(Setting::Secrets) => "vault",
            Self::Setting(Setting::Repositories) => "repos",
            Self::Sync(SyncKind::Peers) => "sync-peers",
            Self::Sync(SyncKind::Identity) => "sync-identity",
            Self::Sync(SyncKind::Fleet) => "sync-fleet",
            Self::Sync(SyncKind::Accounts) => "sync-accounts",
            Self::Sync(SyncKind::SharedData) => "sync-shared-data",
            Self::Sync(SyncKind::Name) => "sync-name",
        }
    }

    /// The row whose [`Self::setup_word`] is `word`, if one: how a reader of `setup=` names a row
    /// back.
    #[must_use]
    pub fn from_setup_word(word: &str) -> Option<Self> {
        Self::all()
            .into_iter()
            .chain(SyncKind::ALL.into_iter().map(Self::Sync))
            .find(|subject| subject.setup_word() == word)
    }

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
    /// Asked, and nothing answered: aterm asks again by itself, with nothing for anyone to do,
    /// and no answer may come (the ALab vault off its LAN). Neither done nor a step waited on: the
    /// rest of setup is said set up beside it, and it is never counted in place.
    Asking,
    /// Written; macOS takes it up at the next login.
    AtLogin,
    /// The last attempt failed, or the item cannot be changed here.
    Failed,
    /// Could not be read. Never shown as a value.
    Unknown,
    /// Does not apply on this Mac.
    NotNeeded,
}

impl Standing {
    /// Whether the row counts as in place for the page's "N of M" count.
    #[must_use]
    pub const fn settled(self) -> bool {
        matches!(self, Self::InPlace | Self::AtLogin | Self::NotNeeded)
    }

    /// Whether the row needs a person, or failed: the only states that may
    /// light the tab's attention indicator.
    #[must_use]
    pub const fn attention(self) -> bool {
        matches!(self, Self::NeedsYou | Self::Failed)
    }

    /// Every standing.
    pub const ALL: [Self; 9] = [
        Self::InPlace,
        Self::Differs,
        Self::NeedsYou,
        Self::Waiting,
        Self::Asking,
        Self::AtLogin,
        Self::Failed,
        Self::Unknown,
        Self::NotNeeded,
    ];

    /// The standing's one word in a record (`in-place`, `at-login`, …).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::InPlace => "in-place",
            Self::Differs => "differs",
            Self::NeedsYou => "needs-you",
            Self::Waiting => "waiting",
            Self::Asking => "asking",
            Self::AtLogin => "at-login",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
            Self::NotNeeded => "not-needed",
        }
    }

    /// The standing whose [`Self::word`] is `word`, if one.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|standing| standing.word() == word)
    }
}

/// What can end a failure (design §3.6), decided where the failure is found and carried to
/// System State's page, its signal line, the tab's diamond and the setup run's retry loop alike,
/// so none of them says a step waits that only a person can finish.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FailureKind {
    /// A retry can help: a network blip, a vault that did not answer, a busy lock, a process
    /// that exited oddly. While the run's worker goes on, trying it again by itself on a pause
    /// that grows and never ends (`aterm_types::retry_schedule`), the page is calm and counts it
    /// as waiting; it failed only once no worker goes on with the run (it stopped, or aterm runs
    /// none). A failure nobody classified is one of these, never a person's.
    Transient,
    /// Only a person's act ends it: a setting only they can fix, a vault password to retype, an
    /// approval only an admin Mac gives, a switch only the person flips. A missing or refused
    /// credential is never one: it waits on the need ledger. It needs
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
    /// while the run's worker goes on, then failed. A step only a person finishes is said by its row's
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

/// What a failed Secrets vault try's row says once two tries in a row failed: aterm tries it
/// again by itself, less often, up to once an hour, and at once when this Mac's network or vault
/// changes ([`FailureKind::Transient`]).
pub const TRIED_TWICE: &str = "Two tries did not take, so aterm tries again by itself, less often, and at once when this Mac's network or vault changes.";

/// The note of a workstation row that read back as aterm set it and reads otherwise since
/// (design §6.9). Nothing tells who changed it (the person, another app, macOS, a profile), so
/// the row says only that it changed, never whose change it is. No automatic pass sets it again,
/// so the row reads its value now, with no chip saying it will change, until Restore, turning its
/// group off and on, or Set up now; it is not set up, so it counts neither done nor among the
/// changes to make.
pub const CHANGED_SINCE: &str = "Changed since aterm set it";

/// The Secrets vault row's value on a member Mac whose sync the person turned off, which is
/// also what the row wants then (design §4: an explicit opt-out is the row's wanted state), so
/// its quiet Turn On Sync turns it back on.
pub const SYNC_OFF: &str = "Sync off";

/// The Secrets row's value for intact local vault materialization with a running subscriber.
pub const VAULT_LOCAL_READY: &str = "Local cache ready";

/// Scope of the local setup check; shared-state observations have their own evidence.
pub const VAULT_LOCAL_READY_NOTE: &str = "Vault sync is running on this Mac. Network State shows shared-state access and observed versions.";

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

/// The one sudoers rule setup's early elevation leaves behind (`aterm_machines::root_steps`): it
/// lets the account that typed the Mac password set this Mac's ComputerName, LocalHostName and
/// HostName to a swarm label (`m` and a number) through Apple's own `scutil`, and nothing else,
/// so the name an admission after the elevation gives the Mac needs no second password.
/// Root-owned, mode 0440; the window and `aterm init` read only whether it is there.
pub const ADMITTED_NAME_RULE: &str = "/private/etc/sudoers.d/aterm-admitted-name";

/// Where the elevation keeps a rule this Mac's `visudo` refused (a dotted name, which sudo never
/// reads): while it stands the rule is not owed, so a sudo that cannot read it never makes each
/// later launch ask for the password again.
pub const ADMITTED_NAME_RULE_REFUSED: &str = "/private/etc/sudoers.d/.aterm-admitted-name.refused";

/// The environment variable System State's Mac Setup tab runs `aterm init --only
/// base.clt` under, as the tab's own program (design §3.3). That run does nothing until the
/// window types [`COMMAND_LINE_TOOLS_TAB_GO`] (once the tab has landed), then speaks to a person
/// in plain words: one line before macOS asks for the Mac password, a plain password prompt, and
/// one line saying how it ended. It exits 0 once it said the tools are installed and 1 once it
/// said why the install did not finish (Low Data Mode, an account that may not install them, a
/// stop and Apple's window closed without installing them each with a code of its own); any
/// other end said nothing, and the window ends the tab with [`COMMAND_LINE_TOOLS_NOT_FINISHED`].
pub const COMMAND_LINE_TOOLS_TAB: &str = "ATERM_COMMAND_LINE_TOOLS_TAB";

/// The variable the window sets beside [`COMMAND_LINE_TOOLS_TAB`] where it reads developer mode
/// off as the person's own choice (they turned it off in aterm's Settings ▸ Security, or
/// something they ran turned it off after aterm saw it on): the tab's program owes nothing for
/// it, and its root half leaves it as it reads.
pub const COMMAND_LINE_TOOLS_TAB_LEAVE_DEVELOPER_MODE: &str =
    "ATERM_COMMAND_LINE_TOOLS_TAB_LEAVE_DEVELOPER_MODE";
/// The dedicated Mac Setup tab's program arguments, shared with its owned lab responder.
pub const COMMAND_LINE_TOOLS_ARGS: [&str; 3] = ["init", "--only", "base.clt"];

/// The dedicated setup run's `sudo -p` prompt, in its tab and on a person's terminal.
pub const MAC_SETUP_PASSWORD_PROMPT: &str = "Mac password: ";

/// The line the window types into the Mac Setup tab once it landed, which starts the
/// install ([`COMMAND_LINE_TOOLS_TAB`]): nothing runs before the window's landing check passes.
pub const COMMAND_LINE_TOOLS_TAB_GO: &str = "install";

/// How the Mac Setup tab says its run did not finish, before its plain reason:
/// `Mac Setup did not finish: <reason>.` It names the setup, not an install: on a Mac that has the tools the tab only takes
/// the elevation's other root steps.
pub const COMMAND_LINE_TOOLS_NOT_FINISHED: &str = "Mac Setup did not finish";

/// What the Mac Setup tab's program exits with when the window's go line
/// ([`COMMAND_LINE_TOOLS_TAB_GO`]) never came, or came behind other typing: it started nothing and
/// said nothing (neither 0 nor 1, which say it printed its last line), so the window ends the tab
/// with its own line and gives the attempt back in the account's record
/// ([`crate::clt_mark::End::Released`]): it asked nothing, and no person declined it.
pub const COMMAND_LINE_TOOLS_TAB_NOT_STARTED: u8 = 2;

/// What the Mac Setup tab's program exits with once it said Low Data Mode holds the
/// download back ([`COMMAND_LINE_TOOLS_TAB`]): the window spends no automatic opening on it, and
/// the GitHub repositories row waits, calmly, for a network without Low Data Mode.
pub const COMMAND_LINE_TOOLS_TAB_LOW_DATA: u8 = 3;

/// What the Mac Setup tab's program exits with once it said this account is not one of
/// the Mac's administrators, whom alone macOS lets install the tools: the GitHub repositories row
/// says so, and offers no Install… that would only say it again.
pub const COMMAND_LINE_TOOLS_TAB_NOT_ADMIN: u8 = 4;

/// What the Mac Setup tab's program exits with once it said a stop ended its install
/// before the tools arrived: a Ctrl-C in the tab, at the password prompt or during the install,
/// or System State's Stop. A person declined: the window records it in the account's record
/// ([`crate::clt_mark::End::Declined`]), as it does for a tab they closed while aterm ran on, and
/// no launch reopens it by itself before its long pause passes ([`crate::clt_mark::DECLINE_AGAIN`])
/// or their own yes (Install…, a typed `aterm init`, Set up now).
pub const COMMAND_LINE_TOOLS_TAB_STOPPED: u8 = 5;

/// What the Mac Setup tab's program exits with once it said Apple's installer window
/// closed without installing them: the window the program waited on (opened where Apple's
/// catalog lists none, or already open) went with no installer running. A person pressed Not
/// Now, Cancel, or Done on Apple's own failure there, each of which unlinks the placeholder, or
/// the window was quit before Install was pressed (a logout included), which leaves a
/// placeholder aterm's root half stamped beside it ([`APPLE_TOOLS_INSTALLER`]). Nothing tells
/// which, so the window records an unfinished try ([`crate::clt_mark::End::Unfinished`]), asked
/// again by itself on the record's schedule, never a decline.
pub const COMMAND_LINE_TOOLS_TAB_WINDOW_CLOSED: u8 = 6;

/// The Mac Setup tab lost its own console before entering `sudo`. Only this launch's exact
/// owned, non-recovered attempt may release its claim and keep its existing want for console
/// return. A root child or an entered attempt never emits this preprompt disposition.
pub const COMMAND_LINE_TOOLS_TAB_NOT_OWN_LOGIN: u8 = 7;

/// A current installer took custody before this Mac Setup tab entered `sudo`. The exact
/// owned, non-recovered attempt may keep its want behind the ordinary live installer gate.
/// Unread custody, a stopped attempt, and an entered root child do not emit this disposition.
pub const COMMAND_LINE_TOOLS_TAB_INSTALLER_HELD: u8 = 8;

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

/// A current live mapping diagnostic for HID services, not physical keyboard counts.
/// It supplies no saved-preference readiness, setup completion or effect authority.
/// It describes reported OS mapping composition, never an observed physical key press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveKeyboardMapping {
    /// Services whose effective Caps output is left or right Control.
    pub control: usize,
    /// Services with an explicit effective non-Control choice.
    pub explicit_other: usize,
    /// Held services whose mapping the activation receipt does not attest.
    pub held: usize,
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
    /// Whether, and when, the row was read: what its age is told from, and what tells a read
    /// that was withheld from one that ran and failed (design §3.5).
    pub read: Read,
}

/// Whether, and when, a row was read ([`Observation::read`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Read {
    /// No stamp: a placeholder, or a reading its reader returned before the observer that
    /// publishes it stamped it. It is evidence of no age, so it verifies nothing.
    #[default]
    Unstamped,
    /// Not read at all: an ordinary refresh reads no other app's data (the Mac settings, the
    /// sign-outs' account state, Spotlight's project inventory) while System State is not in
    /// front with a person ([`Observation::withheld`]). It is no value and no failure, and an
    /// earlier reading of the row stands against it.
    Withheld,
    /// Read when the read started, in Unix seconds.
    At(u64),
}

impl Read {
    /// Whether a reading read so stands at `now` (Unix seconds): it was read, it was read no
    /// later than `now` and no more than [`READING_STANDS_SECS`] before it. An unstamped or
    /// withheld reading never does, nor one dated after `now` (the clock moved back, so its age
    /// is unknown), nor one dated 0 (read with no clock to date it).
    #[must_use]
    pub const fn current_at(self, now: u64) -> bool {
        match self {
            Self::At(at) => at != 0 && at <= now && now - at <= READING_STANDS_SECS,
            Self::Unstamped | Self::Withheld => false,
        }
    }
}

/// How long a reading of a row read privately ([`Subject::reads_privately`]) stands for the row
/// now, in seconds: nothing in the background reads such a row again, so past this its reading
/// is only what the row was. One that read it back settled ([`Observation::read_back_settled`])
/// is the row set up as last read (or changed since aterm set it, as `;changed:` says), which
/// `setup=` says `read-back:` and a check away from the console says with its age, never as in
/// place now (design §3.5). System State in front with a person reads it again.
pub const READING_STANDS_SECS: u64 = 10 * 60;

/// The schema of the running aterm's prompt-free `setup status` answer (`OK schema=<n> setup=…
/// verified=<bits> verified_at=<unix secs|-> observed_generation=<n>
/// rows=<word:said,…|->`, design §3.5), which its writer (`aterm_gui`'s control front door) and
/// its reader (`aterm init --check` and `aterm status` away from the console) share. Schema 6,
/// one grammar above schemas 2, 4 and 5 (6 names the rows changed since aterm set them
/// `;changed:`, which 5 named `;yours:` as if whoever changed them were known, and adds the
/// `asking:` head and the `asking` standing, a row asked again by itself that no answer
/// settled): `setup=` is the whole Mac's state as `status` says it, with `;changed:` naming
/// those rows;
/// `rows=` says each managed row in its word ([`Subject::setup_word`]) with its standing
/// ([`Standing::word`]), `unread` for a row no usable reading stands behind (a withheld read, a
/// read that said nothing, [`Observation::read_nothing`], or a private reading with no date)
/// and `offered` for a grant on offer ([`Observation::on_offer`]); `verified` is every selected
/// private row read back settled ([`Observation::read_back_settled`]: aterm's value, a value
/// macOS takes up in full at the next login, or one changed since aterm set it, which no pass
/// sets again) whatever its age, while no change that could move one is under way, and
/// `verified_at` the oldest of those readings, whose age the reader judges at its own clock. A
/// reader decides only the rows it asks about, from `rows=` and `verified`, and says a verified
/// row `rows=` reads `differs` as changed since aterm set it, never as in place.
pub const SETUP_STATUS_SCHEMA: u32 = 6;

impl Observation {
    /// Whether this is a grant on offer: one only a person gives, which nothing waits on and no
    /// step of aterm's asks for now ([`ACCESS_OFFERED`]), or developer mode left to the person
    /// once setup's one Mac password does not turn it on by itself ([`DEBUGGER_NOT_ASKED`]). It
    /// is neither done nor a person's step: offered, never waited on.
    #[must_use]
    pub fn on_offer(&self) -> bool {
        matches!(self.subject, Subject::Access(_))
            && self.standing == Standing::NotNeeded
            && self
                .note
                .as_deref()
                .is_some_and(|note| note == ACCESS_OFFERED || note == DEBUGGER_NOT_ASKED)
    }

    /// Whether this row changed after aterm set it and read it back (design §6.6,
    /// [`CHANGED_SINCE`]): it differs from aterm's value, by whoever changed it, and no automatic
    /// pass sets it again. It is not in place.
    #[must_use]
    pub fn changed_since(&self) -> bool {
        self.standing == Standing::Differs && self.note.as_deref() == Some(CHANGED_SINCE)
    }

    /// Whether this reading reads the row settled, whatever its age: as wanted (in place, not
    /// needed, a grant on offer being neither, [`Self::on_offer`]; or set for macOS to take up in
    /// full at the next login), or changed since aterm set it ([`Self::changed_since`]), which no
    /// pass sets again and nothing reads as in place; and dated by a clock ([`Read::At`] past 0),
    /// so its age can be told. Only such a reading verifies a row (`setup status`' `verified`:
    /// nothing is left for setup to do on it); whether it stands for the row now is
    /// [`Self::current_at`].
    #[must_use]
    pub fn read_back_settled(&self) -> bool {
        (matches!(
            self.standing,
            Standing::InPlace | Standing::NotNeeded | Standing::AtLogin
        ) || self.changed_since())
            && !self.on_offer()
            && self.read_at().is_some_and(|at| at != 0)
    }

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
            read: Read::Unstamped,
        }
    }

    /// A row an ordinary refresh did not read ([`Read::Withheld`]): [`Standing::Unknown`], with
    /// no value, no note and no detail.
    #[must_use]
    pub fn withheld(subject: Subject) -> Self {
        Self {
            read: Read::Withheld,
            ..Self::unknown(subject)
        }
    }

    /// Whether an ordinary refresh withheld this row's read ([`Read::Withheld`]).
    #[must_use]
    pub const fn is_withheld(&self) -> bool {
        matches!(self.read, Read::Withheld)
    }

    /// Whether a read of this row ran and said nothing of it: [`Standing::Unknown`] from a read
    /// stamped as run ([`Read::At`]), such as a grant this process cannot read, or one whose note
    /// says why (a reader that unwound). No read of it is under way, so it is unread here, never
    /// a wait: a reader away from the page says it `unread`, as a withheld read is.
    #[must_use]
    pub const fn read_nothing(&self) -> bool {
        matches!(self.standing, Standing::Unknown)
            && !self.is_withheld()
            && (self.note.is_some() || matches!(self.read, Read::At(_)))
    }

    /// When the row was read, in Unix seconds; `None` for a row nothing read.
    #[must_use]
    pub const fn read_at(&self) -> Option<u64> {
        match self.read {
            Read::At(at) => Some(at),
            Read::Unstamped | Read::Withheld => None,
        }
    }

    /// Whether this reading stands for the row at `now` (Unix seconds) ([`Read::current_at`]).
    #[must_use]
    pub const fn current_at(&self, now: u64) -> bool {
        self.read.current_at(now)
    }

    /// Whether this reading was read, and is past standing for the row at `now`
    /// ([`Self::current_at`]): a read of it is owed again. An unstamped or withheld reading is
    /// neither current nor stale.
    #[must_use]
    pub const fn stale_at(&self, now: u64) -> bool {
        matches!(self.read, Read::At(_)) && !self.current_at(now)
    }

    /// This reading, stamped as read at `at` (Unix seconds), unless its read was withheld,
    /// which stays unread.
    #[must_use]
    pub fn stamped(self, at: u64) -> Self {
        match self.read {
            Read::Withheld => self,
            Read::Unstamped | Read::At(_) => Self {
                read: Read::At(at),
                ..self
            },
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

    /// The settings this selection selects whose rows are read privately
    /// ([`Subject::reads_privately`]: the profile's and Rust in Spotlight), which `setup status`'
    /// `verified` speaks for.
    #[must_use]
    pub const fn private(self) -> Self {
        Self(self.workstation().0 | (self.0 & (1 << Setting::Spotlight as u8)))
    }

    /// Every setting both this selection and `other` select.
    #[must_use]
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
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
/// vault lane (discovery and the Sync rows ride it), GitHub repositories and Rust in Spotlight, which changes this
/// login's own rustup (its profile set to minimal, each toolchain's `rust-docs` set aside and
/// removed) and keeps aterm's build caches out of Spotlight. The rest is the ALab profile (the
/// workstation preferences, Dock and the Messages and FaceTime sign-outs), which
/// aterm admits by itself only on an ALab Mac.
pub const BASE_PLAN: Selection = {
    let mut selection = Selection::none();
    selection.set(Setting::Permissions, true);
    selection.set(Setting::Secrets, true);
    selection.set(Setting::Spotlight, true);
    selection.set(Setting::Repositories, true);
    selection
};

/// An Access row's note while no person asked for macOS's grants: setup goes on and finishes
/// without it, and the page's one press asks for the whole batch (System State's offer).
pub const ACCESS_OFFERED: &str =
    "macOS needs you to answer this one, so aterm offers it. Nothing else waits for it.";

/// The Debugger row's note while developer mode reads off on an account that may turn it on:
/// setup's one early Mac password, which aterm asks for by itself (`aterm_machines::root_steps`),
/// turns it on, so the row is neither offered for a press nor waited on.
/// It names the two routes left once that tab was stopped: `aterm init --only
/// base.clt` asks for the same password now, and aterm's Settings ▸ Security turns developer
/// mode on.
pub const DEBUGGER_BY_ELEVATION: &str = "Setup's one Mac password turns it on. Stopped, `aterm \
     init --only base.clt` asks for it now, or turn it on in aterm's Settings \u{25b8} Security.";

/// The Debugger row's note while developer mode reads off and setup's one Mac password does not
/// turn it on by itself: a person declined its last ask, the account's record of it cannot be
/// read ([`crate::clt_mark::Asked::coming`]), or setup's elevation finished once, so developer
/// mode was turned off since and is left as it reads ([`crate::clt_mark::Record::finished`]).
/// The row is the person's, never a calm wait, and names their two routes.
pub const DEBUGGER_NOT_ASKED: &str = "aterm does not turn it on by itself: run `aterm init \
     --only base.clt`, or turn it on in aterm's Settings \u{25b8} Security.";

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
            Self::Granted => "granted",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        [Self::Handed, Self::Declined, Self::Granted]
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
            Self::Accessibility => "accessibility",
        }
    }

    /// The row a record's word names, if it is one of [`Self::ALL`].
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

/// A wake that runs once no macOS dialog of this process is up, for a waiter that found one up
/// and looks again only when told: armed by it ([`DialogEndWake::arm`]), run and spent by the
/// last [`MacosDialog`]'s drop.
struct DialogEndWake {
    armed: std::sync::atomic::AtomicBool,
    ready: std::sync::OnceLock<fn()>,
}

impl DialogEndWake {
    const fn new() -> Self {
        Self {
            armed: std::sync::atomic::AtomicBool::new(false),
            ready: std::sync::OnceLock::new(),
        }
    }

    /// Run the wake once no dialog is up: at the last drop, or now where none is up already
    /// (the last may have ended between the waiter's look and this arm).
    fn arm(&self) {
        // This arm/check and the last drop's count transition/consume share one
        // SeqCst order: neither side can miss the other across the two atomics.
        self.armed.store(true, std::sync::atomic::Ordering::SeqCst);
        self.notify();
    }

    /// Run the wake, spending it, if it is armed and no dialog is up.
    fn notify(&self) {
        if MACOS_DIALOGS.load(std::sync::atomic::Ordering::SeqCst) == 0
            && self.armed.swap(false, std::sync::atomic::Ordering::SeqCst)
            && let Some(ready) = self.ready.get()
        {
            ready();
        }
    }
}

/// The setup worker's wake: a caller that timed out waiting behind another dialog
/// (`MacosDialog::when_none_up` with a bound) tries again once the last one ends.
static WORKER_WAKE: DialogEndWake = DialogEndWake::new();

/// The window's wake: a gesture of the window's that probes the slot without waiting and found
/// another dialog up ([`wake_window_when_no_macos_dialog`]) looks again once none is.
static WINDOW_WAKE: DialogEndWake = DialogEndWake::new();

/// Register what wakes the setup worker once the last macOS dialog ends, after one of its steps
/// timed out waiting for another (`MacosDialog::when_none_up`) or found one up without waiting
/// (`MacosDialog::if_none_up`). The window's own immediate slot probes do not subscribe here: a
/// window's subscribes through [`wake_window_when_no_macos_dialog`]. The first registration
/// wins.
pub fn on_macos_dialog_waiter_ready(ready: fn()) {
    let _ = WORKER_WAKE.ready.set(ready);
}

/// Register what wakes the window once no macOS dialog of this process is up, after one of its
/// gestures found another up ([`wake_window_when_no_macos_dialog`]). The first registration wins.
pub fn on_no_macos_dialog_for_window(ready: fn()) {
    let _ = WINDOW_WAKE.ready.set(ready);
}

/// Wake the window ([`on_no_macos_dialog_for_window`]) once no macOS dialog of this process is
/// up, or now where none is: a gesture of the window's that probes the slot without waiting
/// (`MacosDialog::when_none_up(ZERO)`, [`macos_dialog_up`]) and found another up arms it, since
/// nothing else need look again then. The Mac Setup tab deferred behind the Commons runtime's
/// Local Network question waited out a whole first launch for want of it (E3, 2026-11-05): that
/// question's hold ended 30 s on with the setup run parked on the tab, so no status read came.
pub fn wake_window_when_no_macos_dialog() {
    WINDOW_WAKE.arm();
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
                    WORKER_WAKE.arm();
                }
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }

    /// Count this one up if no other macOS dialog this process started is up, without waiting:
    /// a step nobody waits for (the setup worker's unattended sign-outs) never holds its thread
    /// on a person's answer to another. Where one is up it returns `None` and wakes the setup
    /// worker once none is ([`on_macos_dialog_waiter_ready`]), so the step is looked at again
    /// the moment it can go.
    #[must_use = "the dialog counts as up only while the guard lives"]
    pub fn if_none_up() -> Option<Self> {
        let free = MACOS_DIALOGS
            .compare_exchange(
                0,
                1,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok();
        if free {
            return Some(Self(()));
        }
        WORKER_WAKE.arm();
        None
    }
}

/// Whether System State is the visible view of a focused window with a person at this Mac
/// (input within the last minute, the screen unlocked): the only time a setup run's ordinary read
/// takes the rows it withholds from a page nobody is at
/// (`docs/DESIGN-system-state-2026-10-21.md` §6, `docs/DESIGN-init-on-by-default-2026-10-25.md`
/// §6.4). The window publishes it ([`set_system_state_front`]); a process with no window never
/// has it, so its reads leave those rows for a window. The Messages and FaceTime sign-outs never
/// read it: each reads its own login's session right before its clicks.
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

    static OPTED_OUT: std::sync::Mutex<[Option<OptOut>; 4]> = std::sync::Mutex::new([None; 4]);

    const fn slot(access: Access) -> usize {
        match access {
            Access::FullDiskAccess => 0,
            Access::DeveloperTools => 1,
            Access::Debugger => 2,
            Access::Accessibility => 3,
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

/// How the aterm window's desk door begins its refusal of a control verb that would take the
/// person's screen or keyboard focus for a client whose tab is not the one in front (owner
/// ruling 2026-11-09): what a front-door client (`aterm init`'s sign-out, `aterm doctor`) tells
/// from the verb's own answers, to ask nothing in its place ([`desk_refused`]).
pub const DESK_REFUSED: &str = "ERR desk refused";

/// Whether `reply` is the desk door's refusal ([`DESK_REFUSED`]).
#[must_use]
pub fn desk_refused(reply: &str) -> bool {
    reply.starts_with(DESK_REFUSED)
}

/// Whether `[swarm] enabled` leaves this process's automatic LAN uses on: the reader a process
/// installs ([`sharing::install`]) is asked right before such a use (Tab Control's sweep), so
/// a person who turned sharing off sees none go by itself
/// (`docs/DESIGN-alab-swarm-2026-10-27.md` §9).
pub mod sharing {
    use std::sync::OnceLock;

    /// A reader of the switch, from any thread.
    type Reader = Box<dyn Fn() -> bool + Send + Sync>;

    /// This process's reader ([`install`]).
    static SHARING: OnceLock<Reader> = OnceLock::new();
    /// Install this process's reader of whether `[swarm] enabled` leaves sharing on: while it
    /// reads off (or cannot be read), no automatic LAN use goes ([`is_on`]). A person's own action
    /// (a phrase typed, a command run) does not ask it. The first reader installed wins.
    pub fn install(reader: impl Fn() -> bool + Send + Sync + 'static) {
        let _ = SHARING.set(Box::new(reader));
    }

    /// Whether `[swarm] enabled` leaves sharing on, as this process's installed reader says; on
    /// where none is installed.
    #[must_use]
    pub fn is_on() -> bool {
        SHARING.get().is_none_or(|on| on())
    }
}

impl Drop for MacosDialog {
    fn drop(&mut self) {
        if MACOS_DIALOGS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst) == 1 {
            WORKER_WAKE.notify();
            WINDOW_WAKE.notify();
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
    /// already its row's latest in `revision` is not written again. Once it is on disk, the
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
        write(dir, revision, access, outcome, after_read)?;
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
        after_read: impl FnOnce(),
    ) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let _lock = WriterLock::take(dir)?;
        let path = dir.join(FILE);
        let previous = match read_record(&path) {
            Ok(previous) => previous,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error),
        };
        let mut lines: Vec<String> = previous.lines().map(str::to_owned).collect();
        after_read();
        // Every reader takes the latest applicable line per row, so a repeat (each new process that judges
        // the same step again) adds nothing it reads, and would only push older steps out of
        // the kept lines: a revision's first step would be asked again.
        let latest = previous.lines().rev().find_map(|line| {
            let step = parse_step(line)?;
            (step.revision == revision && step.access == access).then_some(step.outcome)
        });
        if latest == Some(outcome) {
            return Ok(());
        }
        lines.push(format!("{revision} {} {}", access.word(), outcome.word()));
        let start = lines.len().saturating_sub(KEEP);
        let staging = aterm_tempfile::Builder::new()
            .prefix(".system-state-access-")
            .suffix(".noindex")
            .tempdir_in(dir)?;
        let staged = staging.path().join(FILE);
        {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options
                    .mode(0o600)
                    .custom_flags(crate::fs_restricted::CREATE_NEW_OPEN_FLAG);
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

    /// The outcomes recorded for `revision`, oldest first.
    #[must_use]
    pub fn read(dir: &Path, revision: u64) -> Vec<(Access, AccessOutcome)> {
        outcomes(&read_record(&dir.join(FILE)).unwrap_or_default(), revision).collect()
    }

    /// The outcomes the record's `text` holds for `revision`, oldest first.
    fn outcomes(
        text: &str,
        revision: u64,
    ) -> impl DoubleEndedIterator<Item = (Access, AccessOutcome)> + '_ {
        text.lines().filter_map(move |line| {
            let step = parse_step(line)?;
            (step.revision == revision).then_some((step.access, step.outcome))
        })
    }

    struct Step {
        revision: u64,
        access: Access,
        outcome: AccessOutcome,
    }

    /// One `<revision> <row> <outcome>` line, or `None` for a line this build does not know: an
    /// unknown row or outcome (an older or newer aterm's), or more fields than these.
    fn parse_step(line: &str) -> Option<Step> {
        let mut words = line.split_whitespace();
        let at = words
            .next()
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))?
            .parse::<u64>()
            .ok()?;
        let access = Access::parse(words.next()?)?;
        let outcome = AccessOutcome::parse(words.next()?)?;
        words.next().is_none().then_some(Step {
            revision: at,
            access,
            outcome,
        })
    }

    /// The latest outcome recorded for `access` in `revision`.
    #[must_use]
    pub fn latest(dir: &Path, revision: u64, access: Access) -> Option<AccessOutcome> {
        read(dir, revision)
            .into_iter()
            .rev()
            .find_map(|(row, outcome)| (row == access).then_some(outcome))
    }

    /// The last outcome the record holds for `access` in any revision, revision 0 included: the
    /// file's last line for that row. Read like [`read`], so a line this build does not know (an
    /// unknown row, outcome or fields) is skipped.
    #[must_use]
    pub fn latest_of_any_revision(dir: &Path, access: Access) -> Option<AccessOutcome> {
        let text = read_record(&dir.join(FILE)).unwrap_or_default();
        text.lines().rev().find_map(|line| {
            let step = parse_step(line)?;
            (step.access == access).then_some(step.outcome)
        })
    }

    #[cfg(not(target_family = "wasm"))]
    const MAX_RECORD_BYTES: u64 = 64 * 1024;

    /// wasm has no files to keep the record in (and no `aterm-uds`), as `std::fs` answers there.
    #[cfg(target_family = "wasm")]
    fn read_record(_path: &Path) -> std::io::Result<String> {
        Err(std::io::ErrorKind::Unsupported.into())
    }

    #[cfg(not(target_family = "wasm"))]
    fn read_record(path: &Path) -> std::io::Result<String> {
        use std::io::Read as _;
        let (file, meta) = aterm_uds::regular_file::open(path)?;
        if meta.len() > MAX_RECORD_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the access record exceeds size limit",
            ));
        }
        let cap = usize::try_from(meta.len().min(MAX_RECORD_BYTES))
            .unwrap_or(0)
            .saturating_add(1);
        let mut text = String::with_capacity(cap);
        file.take(MAX_RECORD_BYTES + 1).read_to_string(&mut text)?;
        if text.len() as u64 > MAX_RECORD_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the access record exceeds size limit",
            ));
        }
        Ok(text)
    }

    #[cfg(test)]
    mod tests;
}

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
    /// How the words of a stage row parked after aterm stopped twice in it, or after it took over
    /// an hour, end (init-on-by-default design §6.5): first why (`Stopped twice while cloning
    /// GitHub repositories;`, `Took over an hour cloning GitHub repositories;`), then this. The
    /// park is a pause: the run's worker begins the stage again by itself at its end, an hour
    /// after the first park and longer after each, up to a day, so a stage that stops or hangs
    /// aterm costs a stop that often at most and is never given up on; Check again lifts it at
    /// once.
    pub const PARKED_TAIL: &str =
        "aterm tries it again by itself later, or at once on Check again.";

    /// How the words of a stage row end that has run past the hang limit in the aterm still
    /// running it (init-on-by-default design §6.5): first what it is still doing (`Still cloning
    /// GitHub repositories after more than an hour;`), then this. It offers no Check again: the
    /// stage still runs there, so a try now could change nothing.
    pub const STILL_RUNNING_TAIL: &str = "it ends by itself or when aterm quits.";

    /// The words of a stage row cut off before it ended, which a pass resumes with no press:
    /// aterm stopped in it (a quit, a crash, a logout, a restart), or a pass panicked in it and
    /// its worker starts again after a pause, so they claim neither.
    pub const RESUMING: &str = "Cut off before it finished; it runs again by itself.";

    /// What a Messages or FaceTime sign-out's row says while macOS has given no answer aterm can
    /// read to whether aterm may control System Events, which its clicks go through
    /// (aterm-machines' `init_accounts`): an unattended run never asks it. The window reads
    /// these words in the setup run's rows and asks macOS once Accessibility reads on, where no
    /// walk's Accessibility page asks it (aterm-gui's `App::look_for_automation_ask`).
    pub const AUTOMATION_UNANSWERED: &str = "aterm's window asks macOS once Accessibility is on";

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
#[path = "macos_setup_tests.rs"]
mod tests;
