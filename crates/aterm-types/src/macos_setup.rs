// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Selectable groups and durable plan types for the local Mac workstation profile.

/// The default ALab vault on the local network, advertised through mDNS.
pub const DEFAULT_VAULT_URL: &str = "http://alab-secrets.local:7484/v1/alab";

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
    /// Dock pins, extras, recent applications and minimization.
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
    /// Authenticate GitHub and clone the credential's accessible repositories.
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
                "Dock: only aterm, System Settings and Safari; no stacks or recents; minimize into app icons"
            }
            Self::Messages => "Sign out of Messages",
            Self::FaceTime => "Sign out of FaceTime",
            Self::Spotlight => "Keep Rust offline documentation and build folders out of Spotlight",
            Self::Permissions => "Set up aterm's macOS permissions",
            Self::Secrets => "Sync credentials from the secrets vault",
            Self::Repositories => "Set up GitHub and clone accessible repositories into home",
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

/// What a System State row is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Subject {
    /// One selectable setting.
    Setting(Setting),
    /// One grant in the Access group.
    Access(Access),
}

impl Subject {
    /// The page group the row sits in.
    #[must_use]
    pub const fn group(self) -> Group {
        match self {
            Self::Setting(setting) => setting.group(),
            Self::Access(_) => Group::Access,
        }
    }

    /// The setting bit whose selection manages this row.
    #[must_use]
    pub const fn setting(self) -> Setting {
        match self {
            Self::Setting(setting) => setting,
            Self::Access(_) => Setting::Permissions,
        }
    }

    /// The row's name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Setting(setting) => setting.name(),
            Self::Access(access) => access.name(),
        }
    }

    /// What aterm wants the row to read.
    #[must_use]
    pub const fn wanted(self) -> &'static str {
        match self {
            Self::Setting(setting) => setting.wanted(),
            Self::Access(access) => access.wanted(),
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

/// Where one row stands, read without changing anything. This is the converge
/// model's outcome reduced once for display (design §5): `Ok` is
/// [`Standing::InPlace`], a planned change [`Standing::Differs`], `Person`
/// [`Standing::NeedsYou`], `Blocked`, `Deferred` and `Waits`
/// [`Standing::Waiting`] with their reason, a change that did not land
/// [`Standing::Failed`], a change macOS takes up at login
/// [`Standing::AtLogin`], `Unknown` and `Unusable` [`Standing::Unknown`], and
/// `NotApplicable` [`Standing::NotNeeded`].
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
    /// The wanted value in the row's words.
    pub wanted: String,
    /// A short note in a person's words, shown under the row (an approval code,
    /// what a waiting row waits on).
    pub note: Option<String>,
    /// The row's description: why it stands as it does, in its probe's words. The
    /// page shows it under a row that needs a person, is not needed, was opted out
    /// of or failed; a row that differs or waits shows its note alone.
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
            wanted: subject.wanted().to_owned(),
            note: None,
            detail: None,
        }
    }
}

/// Bitmask of selected workstation profile groups.
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
/// not yet seen end: the setup worker's Accessibility, Automation and vault
/// prompts, and the window's access hand-overs. Every one of them checks
/// [`macos_dialog_up`] before starting, so only one macOS gesture is up at a
/// time (`docs/DESIGN-system-state-2026-10-21.md` §6).
static MACOS_DIALOGS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

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
        let deadline = std::time::Instant::now() + limit;
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
            if std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }
}

/// Whether System State is the visible view of a focused window: the only time a
/// setup run may put the Accounts stage's Accessibility and Automation prompts,
/// or its sign-outs, on screen (`docs/DESIGN-system-state-2026-10-21.md` §6).
/// The window publishes it ([`set_system_state_front`]); a process with no
/// window never has it, so its setup run leaves those steps for a window.
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
/// wanted state (`docs/DESIGN-system-state-2026-10-21.md` §4): `[privacy]
/// enabled = false`, `check = false` or `notice = false`, and a debugger access
/// that was turned off. The window owns the config and the debugger's records,
/// so it publishes them here; the setup worker's readings and its Permissions
/// stage read them. A panel's Not now is the access record's `declined`.
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

impl Drop for MacosDialog {
    fn drop(&mut self) {
        MACOS_DIALOGS.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// The per-revision record of access steps, a text file in aterm's state
/// directory: one `<revision> <row> <outcome>` line per step. Unknown lines are
/// skipped, so an older or newer aterm can share the file. Every line is
/// written through [`append`], which calls the registered watcher
/// ([`on_append`]) once the line is on disk: a step the window took, a grant it
/// verified or the debugger's outcome is what System State's Access rows and a
/// run parked on the Permissions stage wait for.
pub mod access_record {
    use std::io::Write as _;
    use std::path::Path;

    use super::{Access, AccessOutcome};

    /// The file's name inside aterm's state directory.
    pub const FILE: &str = "system-state-access";

    /// Lines kept when the file is rewritten: a few revisions of every row.
    const KEEP: usize = 64;

    /// Called after each line [`append`] wrote: the setup worker registers its
    /// re-reading of the Access rows and its wake.
    static APPENDED: std::sync::OnceLock<fn(u64, Access, AccessOutcome)> =
        std::sync::OnceLock::new();

    /// Register what runs after each line is written, with the line's revision,
    /// row and outcome. The first registration wins: the process has one setup
    /// worker.
    pub fn on_append(appended: fn(u64, Access, AccessOutcome)) {
        let _ = APPENDED.set(appended);
    }

    /// Append one step's outcome for `revision`, keeping the file short; once it
    /// is on disk, the registered watcher ([`on_append`]) is told.
    ///
    /// # Errors
    /// The state directory cannot be written.
    pub fn append(
        dir: &Path,
        revision: u64,
        access: Access,
        outcome: AccessOutcome,
    ) -> std::io::Result<()> {
        write(dir, revision, access, outcome)?;
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
    ) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(FILE);
        let mut lines: Vec<String> = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect();
        lines.push(format!("{revision} {} {}", access.word(), outcome.word()));
        let start = lines.len().saturating_sub(KEEP);
        let staged = dir.join(format!(".{FILE}.{}", std::process::id()));
        {
            let mut file = std::fs::File::create(&staged)?;
            for line in &lines[start..] {
                writeln!(file, "{line}")?;
            }
            // Ordered ahead of the rename: a power loss leaves the old file or
            // the new one, never a torn one, and a lost last step is read again
            // from macOS on the next look.
            aterm_durable::ordered(&file)?;
        }
        std::fs::rename(&staged, &path)
    }

    /// Every outcome recorded for `revision`, oldest first.
    #[must_use]
    pub fn read(dir: &Path, revision: u64) -> Vec<(Access, AccessOutcome)> {
        std::fs::read_to_string(dir.join(FILE))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let mut words = line.split_whitespace();
                let at = words.next()?.parse::<u64>().ok()?;
                let access = Access::parse(words.next()?)?;
                let outcome = AccessOutcome::parse(words.next()?)?;
                (at == revision && words.next().is_none()).then_some((access, outcome))
            })
            .collect()
    }

    /// The latest outcome recorded for `access` in `revision`.
    #[must_use]
    pub fn latest(dir: &Path, revision: u64, access: Access) -> Option<AccessOutcome> {
        read(dir, revision)
            .into_iter()
            .rev()
            .find_map(|(row, outcome)| (row == access).then_some(outcome))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        ] {
            assert_eq!(
                standing.settled(),
                matches!(
                    standing,
                    Standing::InPlace | Standing::AtLogin | Standing::NotNeeded
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
        for n in 0..100 {
            access_record::append(root, 9, Access::LocalNetwork, AccessOutcome::Triggered).unwrap();
            assert!(
                std::fs::read_to_string(root.join(access_record::FILE))
                    .unwrap()
                    .lines()
                    .count()
                    <= 64,
                "{n}"
            );
        }
    }

    /// A written step tells the registered watcher, once the line is on disk
    /// (it reads the record again); a line that could not be written tells it
    /// nothing.
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
            let on_disk = DIR
                .lock()
                .unwrap()
                .as_deref()
                .and_then(|dir| access_record::latest(dir, revision, access));
            SEEN.lock()
                .unwrap()
                .push((revision, access, outcome, on_disk));
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
    }

    #[test]
    fn a_macos_dialog_counts_as_up_only_while_its_guard_lives() {
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

    /// A sentence that says the person turned a grant off is said only of a
    /// grant that reads off; a `[privacy]` switch's of any reading.
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
