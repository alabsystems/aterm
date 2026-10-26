// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Durable Messages and FaceTime sign-out for local initialization.

use crate::items::{Ctx, Item};
use crate::model::{PValue, Probe, User};
use crate::plist::{self, Entries};
use crate::port::{Cmd, System};
use aterm_types::macos_setup::{Selection, Setting};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

const STORE: &str = "Library/IdentityServices/Persistence/accounts.data";
const GZIP: &str = aterm_uds::os_path!("gzip");
const PLUTIL: &str = aterm_uds::os_path!("plutil");
const OSASCRIPT: &str = aterm_uds::os_path!("osascript");
const OPEN: &str = aterm_uds::os_path!("open");

#[derive(Clone, Copy)]
enum Service {
    Messages,
    FaceTime,
}

impl Service {
    const fn name(self) -> &'static str {
        match self {
            Self::Messages => "Messages",
            Self::FaceTime => "FaceTime",
        }
    }

    const fn id(self) -> &'static str {
        match self {
            Self::Messages => "messages",
            Self::FaceTime => "facetime",
        }
    }

    const fn identifier(self) -> &'static str {
        match self {
            Self::Messages => "com.apple.madrid",
            Self::FaceTime => "com.apple.ess",
        }
    }

    const fn domain(self) -> &'static str {
        match self {
            Self::Messages => "com.apple.imservice.ids.iMessage",
            Self::FaceTime => "com.apple.imservice.ids.FaceTime",
        }
    }

    /// The app's bundle id, which `open -b` brings forward.
    const fn bundle(self) -> &'static str {
        match self {
            Self::Messages => "com.apple.MobileSMS",
            Self::FaceTime => "com.apple.FaceTime",
        }
    }

    /// Where its Sign Out is, as a person finds it (English names).
    const fn path(self) -> &'static str {
        match self {
            Self::Messages => "Messages \u{25b8} Settings\u{2026} \u{25b8} iMessage",
            Self::FaceTime => "FaceTime \u{25b8} Settings\u{2026} \u{25b8} General",
        }
    }
}

/// How often a run looks again, in this process: for Accessibility once macOS asked for it,
/// and for the account to deregister once Sign Out was clicked.
const POLL: Duration = Duration::from_millis(250);

/// How long a run at a terminal waits for a person: to switch Accessibility on once macOS
/// asked, and to click Sign Out by hand where it stays off.
const PATIENCE: Duration = Duration::from_secs(2 * 60);

/// How long the clicks and the deregistration that follows them may take.
const SIGN_OUT_DEADLINE: Duration = Duration::from_secs(60);

/// Why the sign-out waits while another macOS dialog this process started is up.
const DIALOG_UP: &str = "another macOS dialog is still up: answer it, and the next pass signs \
                         out (`aterm init --sign-out`)";

/// Why it waits for macOS's Automation permission to control System Events.
const AUTOMATION_OFF: &str = "macOS Automation permission to control System Events is off: \
                              switch it on in System Settings \u{25b8} Privacy & Security \
                              \u{25b8} Automation, then run `aterm init --sign-out`";

/// The one Apple event that asks macOS's Automation question for System Events before any app
/// opens. It has to reach System Events: AppleScript answers an application's `name`, `id`,
/// `version` and `running` itself, sending nothing, so asking for one of those asks macOS
/// nothing.
const AUTOMATION_ASK: &str = "tell application id \"com.apple.systemevents\" to count processes";

/// Whether this run asked macOS for Accessibility already, shared by its sign-out items: it
/// asks once, and waits once.
#[derive(Default)]
struct Consent {
    asked: Mutex<bool>,
}

struct SignOut {
    service: Service,
    user: User,
    /// Why the last apply stood aside before signing out, if it did.
    blocked: Mutex<Option<aterm_converge::StoodAside>>,
    /// Whether the last probe found the account signed in, for System State.
    reading: crate::init_observe::LastReading,
    /// The run's one Accessibility request, shared with its other sign-out.
    consent: std::sync::Arc<Consent>,
}

/// The two account items for each selected local user.
///
/// # Errors
/// A selected user has no absolute home directory.
pub fn items(ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    selected_items(ctx, Selection::all())
}

/// Build only the selected account operations.
///
/// # Errors
/// A selected user has no absolute home directory.
pub fn selected_items(ctx: &Ctx, selected: Selection) -> Result<Vec<Box<dyn Item>>, String> {
    let mut items: Vec<Box<dyn Item>> = Vec::new();
    let consent = std::sync::Arc::new(Consent::default());
    for user in &ctx.users {
        if !Path::new(&user.home).is_absolute() {
            return Err("account sign-out needs an absolute user home".into());
        }
        for (service, setting) in [
            (Service::Messages, Setting::Messages),
            (Service::FaceTime, Setting::FaceTime),
        ] {
            if !selected.contains(setting) {
                continue;
            }
            items.push(Box::new(SignOut {
                service,
                user: user.clone(),
                blocked: Mutex::new(None),
                reading: crate::init_observe::LastReading::default(),
                consent: std::sync::Arc::clone(&consent),
            }));
        }
    }
    Ok(items)
}

impl Item for SignOut {
    fn id(&self) -> String {
        format!("init.{}.signed-out@{}", self.service.id(), self.user.name)
    }

    fn family(&self) -> &'static str {
        "init"
    }

    fn title(&self) -> String {
        format!("{} signed out ({})", self.service.name(), self.user.name)
    }

    fn probe(&self, sys: &dyn System, ctx: &Ctx) -> Probe {
        let registered = registered(sys, &self.user, self.service);
        // Signed in, whether or not this run may sign it out now.
        self.reading
            .set(matches!(registered, Ok(true)).then_some(crate::init_observe::Reading::SignedIn));
        match registered {
            Err(why) => Probe::Unusable { why },
            Ok(false) => Probe::Ok {
                now: "signed out (no persisted, active, or online account)".into(),
            },
            Ok(true) if !ctx.disrupt => Probe::Deferred {
                why: "signing out opens the app's account settings; use --now".into(),
            },
            Ok(true) if ctx.console.as_ref().map(|u| u.uid) != Some(self.user.uid) => {
                Probe::Blocked {
                    why: "sign-out needs this user's active console session".into(),
                }
            }
            Ok(true) => Probe::Change {
                from: "account remains registered (including disabled accounts)".into(),
                to: "signed out".into(),
            },
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        self.stand(None);
        // macOS's Accessibility and Automation dialogs, and an app left open for the person's
        // click, are each a dialog this process started: only one is up at a time (System
        // State design §6), so the sign-out holds the one slot from its first question to its
        // last click.
        let Some(_dialog) = aterm_types::macos_setup::MacosDialog::when_none_up(
            aterm_types::macos_setup::DIALOG_WAIT,
        ) else {
            self.stand(Some(aterm_converge::StoodAside::Waiting(DIALOG_UP.into())));
            return Ok(());
        };
        if !self.accessible(sys)? {
            return self.by_hand(sys);
        }
        if !self.automation(sys)? {
            self.stand(Some(aterm_converge::StoodAside::Permission(
                AUTOMATION_OFF.into(),
            )));
            return Ok(());
        }
        let deadline = Instant::now() + SIGN_OUT_DEADLINE;
        let labels = localized_labels(sys, &self.user, self.service, Some(deadline))?;
        let command = limited(
            signout_cmd(&self.user, self.service, &labels),
            Some(deadline),
        )?;
        let ran = sys
            .run_complete_stdout(&command)
            .map_err(|_| format!("{} sign-out could not run", self.service.name()))?;
        match ran.out().trim() {
            "permission-blocked" if ran.success() => {
                self.stand(Some(aterm_converge::StoodAside::Permission(
                    AUTOMATION_OFF.into(),
                )));
                return Ok(());
            }
            "not-frontmost" if ran.success() => {
                return Err(format!(
                    "{} left the front while it was being signed out, and nothing more was \
                     clicked: run `aterm init --sign-out` again",
                    self.service.name()
                ));
            }
            "sign-out-requested" if ran.success() => {}
            _ => {
                return Err(format!(
                    "{} account Settings controls were unavailable; sign-out remains unverified",
                    self.service.name()
                ));
            }
        }
        if self.deregistered(sys, SIGN_OUT_DEADLINE)? {
            return Ok(());
        }
        Err("the account remains registered after Sign Out; verify its account settings".into())
    }

    fn stood_aside(&self) -> Option<aterm_converge::StoodAside> {
        self.blocked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn detail(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        self.reading.detail()
    }
}

impl SignOut {
    fn stand(&self, aside: Option<aterm_converge::StoodAside>) {
        *self.blocked.lock().unwrap_or_else(PoisonError::into_inner) = aside;
    }

    /// Whether the app macOS holds responsible for this run holds Accessibility, which the
    /// clicks need. A run at a terminal that finds it off asks macOS once for the run (its own
    /// consent dialog), then reads it again every [`POLL`] in this process for at most
    /// [`PATIENCE`], going on the moment it is on; a later sign-out of the same run does not ask
    /// or wait again. Anywhere else (System State, which asks for it itself) nothing is asked.
    fn accessible(&self, sys: &dyn System) -> Result<bool, String> {
        if accessibility_ready(sys)? {
            return Ok(true);
        }
        if !sys.has_tty() {
            return Ok(false);
        }
        {
            let mut asked = self
                .consent
                .asked
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if *asked {
                return Ok(false);
            }
            *asked = true;
        }
        say(&format!(
            "signing out of {} clicks its Sign Out, which needs Accessibility for {}: switch it \
             on in macOS's dialog (System Settings \u{25b8} Privacy & Security \u{25b8} \
             Accessibility), and init goes on the moment it is on (Ctrl-C stops waiting)",
            self.service.name(),
            responsible(sys)
        ));
        if request_accessibility(sys)? {
            return Ok(true);
        }
        for _ in 0..ticks(PATIENCE) {
            if sys.stopped().is_some() {
                return Ok(false);
            }
            sys.sleep(POLL);
            if accessibility_ready(sys)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Where Accessibility stays off: at a terminal, the app comes forward and the person
    /// clicks Sign Out themselves (aterm cannot open its Settings without Accessibility), and
    /// the run reads the account every [`POLL`] for at most [`PATIENCE`], going on once it
    /// deregisters. Otherwise, or once that wait ends, it stands aside naming the app the grant
    /// goes to.
    fn by_hand(&self, sys: &dyn System) -> Result<(), String> {
        let refused = aterm_converge::StoodAside::Permission(format!(
            "{} has no Accessibility, which clicking {}'s Sign Out needs: switch it on in System \
             Settings \u{25b8} Privacy & Security \u{25b8} Accessibility, then run `aterm init \
             --sign-out`",
            responsible(sys),
            self.service.name()
        ));
        if !sys.has_tty() {
            self.stand(Some(refused));
            return Ok(());
        }
        let open = Cmd::new(OPEN)
            .args(["-b", self.service.bundle()])
            .as_user(&self.user)
            .in_session();
        if !sys.run(&open).is_ok_and(|ran| ran.success()) {
            self.stand(Some(refused));
            return Ok(());
        }
        say(&format!(
            "{} is open: choose {} and click Sign Out, and init goes on once it is signed out \
             (Ctrl-C stops waiting)",
            self.service.name(),
            self.service.path()
        ));
        if self.deregistered(sys, PATIENCE)? {
            return Ok(());
        }
        self.stand(Some(refused));
        Ok(())
    }

    /// Ask macOS's Automation question for System Events once, before any app opens: `false`
    /// when the person (or a standing answer) refused it.
    fn automation(&self, sys: &dyn System) -> Result<bool, String> {
        let ask = Cmd::new(OSASCRIPT)
            .args(["-e", AUTOMATION_ASK])
            .as_user(&self.user)
            .in_session()
            .budget(PATIENCE);
        let ran = sys.run(&ask).map_err(|_| {
            "could not ask for Automation permission to control System Events".to_owned()
        })?;
        if ran.success() {
            return Ok(true);
        }
        if ran.err().contains("-1743") {
            return Ok(false);
        }
        Err(format!(
            "the Automation question for System Events did not answer: {}",
            ran.describe()
        ))
    }

    /// Whether the account deregistered within `patience`, read every [`POLL`] in this process;
    /// a stop ends the wait.
    fn deregistered(&self, sys: &dyn System, patience: Duration) -> Result<bool, String> {
        let deadline = Instant::now() + patience;
        for tick in 0..=ticks(patience) {
            // Each reading keeps its own budget; the wait ends at its deadline or its last
            // poll, whichever comes first, never in the middle of a reading.
            if !registered(sys, &self.user, self.service)? {
                return Ok(true);
            }
            if tick == ticks(patience) || Instant::now() >= deadline || sys.stopped().is_some() {
                break;
            }
            sys.sleep(POLL);
        }
        Ok(false)
    }
}

/// The polls in `patience`.
fn ticks(patience: Duration) -> u128 {
    patience.as_millis() / POLL.as_millis()
}

/// The app a grant this run needs goes to ([`System::responsible_app`]), else aterm.
fn responsible(sys: &dyn System) -> String {
    sys.responsible_app().unwrap_or_else(|| "aterm".to_owned())
}

/// One line on this run's terminal while it waits for a person. A terminal gone with its tab
/// is no error.
fn say(line: &str) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "{line}");
}

fn store_path(user: &User) -> PathBuf {
    Path::new(&user.home).join(STORE)
}

fn registered(sys: &dyn System, user: &User, service: Service) -> Result<bool, String> {
    let path = store_path(user);
    let persisted = match sys.stat(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(_) => return Err("cannot inspect the persisted account store".into()),
        Ok(s) if s.dir || s.symlink => {
            return Err("persisted account store is not a regular file".into());
        }
        Ok(_) => {
            let gzip = Cmd::new(GZIP).args(["-dc", "--"]).arg(path).as_user(user);
            let unpacked = sys
                .run_complete_stdout(&gzip)
                .map_err(|_| "cannot read the complete persisted account store".to_owned())?;
            if !unpacked.success() {
                return Err("cannot decompress the persisted account store".into());
            }
            let convert = Cmd::new(PLUTIL)
                .args(["-convert", "xml1", "-o", "-", "--", "-"])
                .as_user(user);
            let xml = sys
                .run_complete_stdout_input(&convert, &unpacked.stdout)
                .map_err(|_| "cannot decode the complete persisted account store".to_owned())?;
            if !xml.success() {
                return Err("cannot decode the persisted account store".into());
            }
            let entries = plist::parse_domain(&xml.stdout).map_err(|_| {
                "persisted account store is not a supported property list".to_owned()
            })?;
            persisted_registration(&entries, service)?
        }
    };
    let cmd = Cmd::new("/usr/bin/defaults")
        .args(["export", service.domain(), "-"])
        .as_user(user);
    let ran = sys
        .run_complete_stdout(&cmd)
        .map_err(|_| "cannot read the complete account service preferences".to_owned())?;
    if !ran.success() {
        return Err("cannot read account service preferences".into());
    }
    let entries = plist::parse_domain(&ran.stdout)
        .map_err(|_| "account service preferences are not a supported property list".to_owned())?;
    let mut visible = false;
    for key in ["Accounts", "ActiveAccounts", "OnlineAccounts"] {
        if let Some(value) = value(&entries, key)? {
            visible |= match value {
                PValue::Array(a) => !a.is_empty(),
                PValue::Dict(d) if key == "Accounts" => !d.is_empty(),
                _ => {
                    return Err(
                        "account service preferences have an unknown account-list format".into(),
                    );
                }
            };
        }
    }
    Ok(persisted || visible)
}

fn value<'a>(entries: &'a Entries, key: &str) -> Result<Option<&'a PValue>, String> {
    let mut found = entries.iter().filter(|(k, _)| k == key);
    let Some((_, v)) = found.next() else {
        return Ok(None);
    };
    if found.next().is_some() {
        return Err("account metadata contains duplicate keys".into());
    }
    v.as_ref()
        .map(Some)
        .map_err(|_| "account metadata has an unsupported value".into())
}

fn persisted_registration(entries: &Entries, service: Service) -> Result<bool, String> {
    let key = format!("com.apple.ids.service.{}", service.identifier());
    let Some(service) = value(entries, &key)? else {
        return Ok(false);
    };
    let PValue::Dict(fields) = service else {
        return Err("persisted service account metadata has an unknown format".into());
    };
    let mut accounts = None;
    let mut enabled = None;
    for (key, value) in fields {
        match (key.as_str(), value) {
            ("Accounts", PValue::Dict(a)) if accounts.is_none() => accounts = Some(a.len()),
            ("EnabledAccounts", PValue::Array(a)) if enabled.is_none() => enabled = Some(a.len()),
            ("Accounts" | "EnabledAccounts", _) => {
                return Err("persisted service account lists have an unknown format".into());
            }
            _ => {}
        }
    }
    match (accounts, enabled) {
        (Some(a), Some(e)) if e <= a => Ok(a != 0),
        _ => Err("persisted service account lists are missing or inconsistent".into()),
    }
}

/// Whether the shared account plan found a registration that needs sign-out.
///
/// # Errors
/// The console account or the shared plan cannot be resolved.
pub fn accessibility_needed(sys: &dyn System) -> Result<bool, String> {
    accessibility_needed_selected(sys, Selection::all())
}

/// Whether a selected account needs sign-out, without reading unchecked accounts.
///
/// # Errors
/// The console account or the selected shared plan cannot be resolved.
pub fn accessibility_needed_selected(
    sys: &dyn System,
    selected: Selection,
) -> Result<bool, String> {
    let rows = crate::init::run_workstation_selected(
        sys,
        crate::init::WorkstationGroup::Accounts,
        crate::model::Mode::Plan,
        selected,
    )?;
    Ok(rows
        .iter()
        .any(|row| row.outcome == crate::model::Outcome::Planned))
}

/// Ask macOS to show its native Accessibility consent once for automatic setup, in this
/// process ([`System::request_accessibility`]), answering the trust as it stands.
///
/// # Errors
/// The trust cannot be asked for here.
pub fn request_accessibility(sys: &dyn System) -> Result<bool, String> {
    sys.request_accessibility()
        .ok_or_else(|| "could not request Accessibility permission for account sign-out".into())
}

/// Inspect Accessibility trust without requesting permission or opening an app, in this
/// process ([`System::accessibility_trusted`]).
///
/// # Errors
/// The trust cannot be read here.
pub fn accessibility_ready(sys: &dyn System) -> Result<bool, String> {
    sys.accessibility_trusted()
        .ok_or_else(|| "could not check Accessibility permission for account sign-out".into())
}

fn limited(cmd: Cmd, deadline: Option<Instant>) -> Result<Cmd, String> {
    let Some(deadline) = deadline else {
        return Ok(cmd);
    };
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err("account sign-out ran out of its time".into());
    }
    let budget = cmd.budget.min(remaining);
    Ok(cmd.budget(budget))
}

#[derive(Clone)]
struct Labels {
    panes: Vec<String>,
    sign_out: Vec<String>,
}

fn labels_cmd(user: &User, service: Service) -> Cmd {
    Cmd::new(OSASCRIPT)
        .args([
            "-l",
            "JavaScript",
            "-e",
            LOCALIZED_LABELS,
            "--",
            service.name(),
        ])
        .as_user(user)
}

fn localized_labels(
    sys: &dyn System,
    user: &User,
    service: Service,
    deadline: Option<Instant>,
) -> Result<Labels, String> {
    let answer = sys
        .run_complete_stdout(&limited(labels_cmd(user, service), deadline)?)
        .map_err(|_| {
            "could not read account Settings labels from the installed Apple app".to_owned()
        })?;
    if !answer.success() {
        return Err("the installed app's account Settings labels are unsupported".into());
    }
    let value: aterm_json::Value = aterm_json::from_slice(&answer.stdout)
        .map_err(|_| "unrecognized account Settings labels".to_owned())?;
    let read = |key: &str| -> Result<Vec<String>, String> {
        let labels = value[key]
            .as_array()
            .ok_or("account Settings label list is missing")?;
        if labels.is_empty() || labels.len() > 128 {
            return Err("account Settings label list is unsupported".into());
        }
        labels
            .iter()
            .map(|label| {
                label
                    .as_str()
                    .filter(|s| !s.is_empty() && s.len() <= 512 && !s.contains(['\r', '\n']))
                    .map(str::to_owned)
                    .ok_or_else(|| "account Settings label is unsupported".into())
            })
            .collect()
    };
    Ok(Labels {
        panes: read("panes")?,
        sign_out: read("sign_out")?,
    })
}

const LOCALIZED_LABELS: &str = r#"ObjC.import('Foundation');
function labels(path, key) {
    const table = $.NSDictionary.dictionaryWithContentsOfFile(path);
    if (!table || table.isNil()) throw Error('label table unavailable');
    const entries = ObjC.deepUnwrap(table);
    const values = Object.values(entries).map(x => x && typeof x === 'object' ? x[key] : null)
        .filter(x => typeof x === 'string' && x.length > 0);
    if (values.length === 0) throw Error('label key unavailable');
    return [...new Set(values)].sort();
}
function run(argv) {
    if (argv[0] === 'Messages') {
        const path = '/System/Applications/Messages.app/Contents/PlugIns/MessagesAppKitBridge.bundle/Contents/Resources/MessagesAppKitBridgeLocalizable.loctable';
        return JSON.stringify({panes: labels(path, 'SETTINGS_TAB_IMESSAGE'), sign_out: labels(path, 'IMESSAGE_SIGN_OUT_BUTTON_SIGN_OUT')});
    }
    if (argv[0] === 'FaceTime') {
        const path = '/System/iOSSupport/System/Library/PrivateFrameworks/FaceTimeSettingsUI.framework/Resources/Localizable.loctable';
        return JSON.stringify({panes: labels(path, 'GENERAL_TAB_TITLE'), sign_out: labels(path, 'SIGN_OUT_BUTTON_TEXT')});
    }
    throw Error('unsupported app');
}"#;

fn signout_cmd(user: &User, service: Service, labels: &Labels) -> Cmd {
    Cmd::new(OSASCRIPT)
        .args([
            "-e",
            SIGN_OUT,
            "--",
            service.name(),
            &labels.panes.join("\n"),
            &labels.sign_out.join("\n"),
        ])
        .as_user(user)
        .in_session()
}

const SIGN_OUT: &str = r#"property stopAt : missing value
on run argv
    set appName to item 1 of argv
    set paneNames to paragraphs of item 2 of argv
    set signOutNames to paragraphs of item 3 of argv
    set deadline to (current date) + 40
    set my stopAt to deadline
    with timeout of 3 seconds
    if appName is not "Messages" and appName is not "FaceTime" then error "unsupported app"
    set settingsWindow to missing value
    set closeSettings to false
    set priorApp to missing value
    set wasRunning to true
    try
        tell application "System Events"
            set priorApp to name of first application process whose frontmost is true
            set wasRunning to exists process appName
        end tell
        tell application appName to activate
        -- Ready once its menu bar is there and it is in front: activation lands a moment after
        -- `activate` returns, and the first click's frontmost check must not race it.
        set settingsReady to false
        repeat 120 times
            my checkDeadline(deadline)
            tell application "System Events"
                if exists process appName then
                    tell process appName
                        if (exists menu bar 1) and frontmost then set settingsReady to true
                    end tell
                end if
            end tell
            if settingsReady then exit repeat
            delay 0.1
        end repeat
        if settingsReady is false then
            my mustBeFront(appName)
            error "account Settings menu unavailable"
        end if
        set priorSettings to my accountSettings(appName, paneNames)
        set closeSettings to priorSettings is missing value
        if priorSettings is missing value then
            set settingsItem to missing value
            tell application "System Events"
                tell process appName
                    try
                        repeat with mi in menu items of menu 1 of menu bar item 2 of menu bar 1
                            try
                                if (value of attribute "AXMenuItemCmdChar" of mi) is "," then
                                    set settingsItem to mi
                                    exit repeat
                                end if
                            end try
                        end repeat
                    end try
                end tell
            end tell
            if settingsItem is missing value then error "account Settings menu item unavailable"
            my mustBeFront(appName)
            tell application "System Events" to click settingsItem
        end if
        repeat 120 times
            my checkDeadline(deadline)
            set settingsWindow to my accountSettings(appName, paneNames)
            if settingsWindow is not missing value then exit repeat
            delay 0.1
        end repeat
        if settingsWindow is missing value then error "account Settings window unavailable"
        set paneButton to my namedButton(settingsWindow, paneNames)
        if paneButton is missing value then error "account settings pane unavailable"
        my mustBeFront(appName)
        tell application "System Events" to click paneButton
        set signOutButton to missing value
        repeat 120 times
            my checkDeadline(deadline)
            set signOutButton to my namedButton(settingsWindow, signOutNames)
            if signOutButton is not missing value then exit repeat
            delay 0.1
        end repeat
        if signOutButton is missing value then error "account Sign Out control unavailable"
        my mustBeFront(appName)
        tell application "System Events" to click signOutButton
        set acknowledged to false
        repeat 120 times
            my checkDeadline(deadline)
            tell application "System Events"
                if exists sheet 1 of settingsWindow then
                    set confirmButton to my namedButton(sheet 1 of settingsWindow, signOutNames)
                    if confirmButton is not missing value then
                        my mustBeFront(appName)
                        click confirmButton
                        set acknowledged to true
                    end if
                else if not (exists settingsWindow) then
                    set acknowledged to true
                else if my existingButton(settingsWindow, signOutNames) is missing value then
                    set acknowledged to true
                end if
            end tell
            if acknowledged then exit repeat
            delay 0.1
        end repeat
        if acknowledged is false then error "account Sign Out did not finish"
        my closeOwnedSettings(appName, settingsWindow, closeSettings)
    on error errorText number errorNumber
        my closeOwnedSettings(appName, settingsWindow, closeSettings)
        my restoreApplications(appName, wasRunning, priorApp)
        if errorNumber is -1743 or errorNumber is -25211 then return "permission-blocked"
        if errorNumber is 1001 then return "not-frontmost"
        error "account sign-out controls unavailable"
    end try
    my restoreApplications(appName, wasRunning, priorApp)
    return "sign-out-requested"
    end timeout
end run

on checkDeadline(deadline)
    if (current date) is greater than deadline then error "account Settings deadline expired"
end checkDeadline

-- Before every click: the app aterm drives is still in front, so a click never lands in a
-- window the person brought forward meanwhile.
on mustBeFront(appName)
    tell application "System Events"
        if not (frontmost of process appName) then error "not frontmost" number 1001
    end tell
end mustBeFront

on restoreApplications(appName, wasRunning, priorApp)
    try
        with timeout of 2 seconds
            if wasRunning is false then tell application appName to quit
        end timeout
    end try
    try
        with timeout of 2 seconds
            if application priorApp is running then tell application priorApp to activate
        end timeout
    end try
end restoreApplications

on accountSettings(appName, paneNames)
    tell application "System Events"
        tell process appName
            repeat with candidateWindow in windows
                my checkDeadline(my stopAt)
                -- Inspect only toolbar controls until the account Settings window is
                -- identified; never traverse a conversation or call-history view.
                repeat with candidateToolbar in toolbars of candidateWindow
                    my checkDeadline(my stopAt)
                    set paneButton to my namedButton(candidateToolbar, paneNames)
                    if paneButton is not missing value then return candidateWindow
                end repeat
            end repeat
        end tell
    end tell
    return missing value
end accountSettings

on namedButton(containerElement, buttonNames)
    set buttonRef to my existingButton(containerElement, buttonNames)
    if buttonRef is missing value then return missing value
    tell application "System Events"
        if enabled of buttonRef then return buttonRef
    end tell
    return missing value
end namedButton

on existingButton(containerElement, buttonNames)
    tell application "System Events"
        set childrenList to {}
        try
            set childrenList to UI elements of containerElement
        end try
        repeat with elementRef in childrenList
            my checkDeadline(my stopAt)
            try
                if role of elementRef is in {"AXButton", "AXRadioButton"} then
                    try
                        if (name of elementRef as string) is in buttonNames then return elementRef
                    end try
                    try
                        if (description of elementRef as string) is in buttonNames then return elementRef
                    end try
                end if
            end try
            set nestedRef to my existingButton(elementRef, buttonNames)
            if nestedRef is not missing value then return nestedRef
        end repeat
    end tell
    return missing value
end existingButton

on closeOwnedSettings(appName, settingsWindow, closeSettings)
    if closeSettings and settingsWindow is not missing value then
        try
            with timeout of 2 seconds
            tell application "System Events"
                if (exists settingsWindow) and (frontmost of process appName) then
                    repeat with closeButton in buttons of settingsWindow
                        if subrole of closeButton is "AXCloseButton" then
                            click closeButton
                            exit repeat
                        end if
                    end repeat
                end if
            end tell
            end timeout
        end try
    end if
end closeOwnedSettings"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{self, NoJournal};
    use crate::items::defaults::tests::{ctx, user};
    use crate::model::{Mode, Outcome};
    use crate::port::{Fake, Ran};

    fn fixture() -> (Fake, Ctx, SignOut) {
        let user = user("alice", 501);
        let mut ctx = ctx(std::slice::from_ref(&user));
        ctx.console = Some(user.clone());
        ctx.disrupt = true;
        let item = SignOut {
            service: Service::Messages,
            user,
            blocked: Mutex::new(None),
            reading: crate::init_observe::LastReading::default(),
            consent: std::sync::Arc::default(),
        };
        let mut sys = Fake::root();
        sys.responsible = Some("aterm".into());
        (sys, ctx, item)
    }

    /// The runs a sign-out makes once Accessibility is on: the Automation question, the
    /// labels, and the clicks answering `clicked`.
    fn clicks(sys: &Fake, item: &SignOut, clicked: &str) {
        sys.on(&automation_ask(item).display(), Ran::ok("System Events"));
        let labels = Labels {
            panes: vec!["iMessage".into()],
            sign_out: vec!["Sign Out".into()],
        };
        sys.on(
            &labels_cmd(&item.user, item.service).display(),
            Ran::ok(r#"{"panes":["iMessage"],"sign_out":["Sign Out"]}"#),
        );
        sys.on(
            &signout_cmd(&item.user, item.service, &labels).display(),
            Ran::ok(clicked),
        );
    }

    fn automation_ask(item: &SignOut) -> Cmd {
        Cmd::new(OSASCRIPT)
            .args(["-e", AUTOMATION_ASK])
            .as_user(&item.user)
            .in_session()
            .budget(PATIENCE)
    }

    /// Signed in for the first `reads` readings of the account, signed out after.
    fn signed_in_for(sys: &Fake, item: &SignOut, reads: usize) {
        for _ in 0..reads {
            store(sys, item, DISABLED);
        }
        store(sys, item, "<dict/>");
        prefs(sys, item, EMPTY_PREFS);
    }

    fn count(sys: &Fake, call: &str) -> usize {
        sys.calls().iter().filter(|c| c.as_str() == call).count()
    }

    fn prefs(sys: &Fake, item: &SignOut, body: &str) {
        let cmd = Cmd::new("/usr/bin/defaults")
            .args(["export", item.service.domain(), "-"])
            .as_user(&item.user);
        sys.on(&cmd.display(), Ran::ok(body));
    }

    fn store(sys: &Fake, item: &SignOut, xml: &str) {
        let path = store_path(&item.user);
        sys.file(path.to_str().unwrap(), b"compressed account fixture");
        let gzip = Cmd::new(GZIP)
            .args(["-dc", "--"])
            .arg(path)
            .as_user(&item.user);
        sys.on(&gzip.display(), Ran::ok("binary account fixture"));
        let convert = Cmd::new(PLUTIL)
            .args(["-convert", "xml1", "-o", "-", "--", "-"])
            .as_user(&item.user);
        sys.on(&convert.display(), Ran::ok(xml));
    }

    const EMPTY_PREFS: &str =
        "<dict><key>ActiveAccounts</key><array/><key>OnlineAccounts</key><array/></dict>";
    const DISABLED: &str = "<dict><key>com.apple.ids.service.com.apple.madrid</key><dict><key>Accounts</key><dict><key>private-account-id</key><dict><key>AuthToken</key><string>private-token</string></dict></dict><key>EnabledAccounts</key><array/></dict></dict>";

    #[test]
    fn disabled_and_disconnected_still_requires_durable_signout() {
        let (sys, ctx, item) = fixture();
        store(&sys, &item, DISABLED);
        prefs(&sys, &item, EMPTY_PREFS);
        assert!(matches!(item.probe(&sys, &ctx), Probe::Change { .. }));
        let printed = format!("{:?}", item.probe(&sys, &ctx));
        assert!(!printed.contains("private-account-id"));
        assert!(!printed.contains("private-token"));
        // System State reads it as signed in, from the probe, not the printed line.
        assert_eq!(
            item.detail(&sys, &ctx),
            Some(crate::init_observe::Reading::SignedIn.token())
        );
    }

    #[test]
    fn absent_registrations_do_not_open_apps_or_request_permissions() {
        let (sys, ctx, item) = fixture();
        store(&sys, &item, "<dict/>");
        prefs(&sys, &item, EMPTY_PREFS);
        assert!(matches!(item.probe(&sys, &ctx), Probe::Ok { .. }));
        assert!(sys.calls().iter().all(|c| !c.contains("osascript")));
        assert_eq!(item.detail(&sys, &ctx), None);
    }

    #[test]
    fn unreadable_or_truncated_account_store_is_never_reported_signedout() {
        let (sys, ctx, item) = fixture();
        let path = store_path(&item.user);
        sys.stat_fails(path.to_str().unwrap(), io::ErrorKind::PermissionDenied);
        assert!(matches!(item.probe(&sys, &ctx), Probe::Unusable { .. }));
        let (sys, ctx, item) = fixture();
        store(&sys, &item, "<dict/>");
        prefs(&sys, &item, EMPTY_PREFS);
        let gzip = Cmd::new(GZIP)
            .args(["-dc", "--"])
            .arg(store_path(&item.user))
            .as_user(&item.user);
        sys.refuse_complete_stdout(&gzip.display(), "record exceeded limit");
        assert!(matches!(item.probe(&sys, &ctx), Probe::Unusable { .. }));
    }

    /// Away from a terminal (System State, which asks for Accessibility itself) a sign-out
    /// without it asks nothing, opens nothing and waits for nothing: it stands aside naming the
    /// app the grant goes to.
    #[test]
    fn permission_refusal_does_not_launch_an_account_app() {
        let (sys, ctx, item) = fixture();
        sys.trusts(&[false]);
        item.apply(&sys, &ctx)
            .expect("permission block is a structured outcome");
        let Some(aterm_converge::StoodAside::Permission(why)) = item.stood_aside() else {
            panic!("{:?}", item.stood_aside());
        };
        assert!(why.starts_with("aterm has no Accessibility"), "{why}");
        assert_eq!(sys.calls(), ["ax-trusted"]);
    }

    /// At a terminal, macOS is asked once and the trust read every 250 ms in this process: the
    /// moment it flips the sign-out goes on in the same run, with no exit 77, no `sleep` child
    /// and no keystroke; the deregistration that follows is read in this process too.
    #[test]
    fn trust_that_flips_while_init_waits_signs_out_in_the_same_run() {
        let (mut sys, ctx, item) = fixture();
        sys.tty = true;
        sys.trusts(&[false, false, false, false, true]);
        clicks(&sys, &item, "sign-out-requested");
        signed_in_for(&sys, &item, 2);
        let rows = engine::run(&[Box::new(item)], Mode::Apply, &sys, &ctx, &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{:?}", rows[0]);
        assert_eq!(rows[0].stood_aside, None);
        assert_eq!(count(&sys, "ax-request"), 1);
        assert_eq!(count(&sys, "sleep 250ms"), 3 + 1, "{:?}", sys.calls());
        let calls = sys.calls();
        assert!(
            calls
                .iter()
                .all(|c| !c.contains("/bin/sleep") && !c.contains("keystroke")),
            "{calls:?}"
        );
        let at = |needle: &str| calls.iter().position(|c| c.contains(needle)).unwrap();
        assert!(at("ax-request") < at(AUTOMATION_ASK) && at(AUTOMATION_ASK) < at("Sign Out"));
    }

    /// Trust that never flips: after two minutes of reads (480, every 250 ms), the app comes
    /// forward for the person's own Sign Out click, read for two minutes more; still signed in,
    /// the row stands aside naming the app. The run's other sign-out neither asks nor waits for
    /// Accessibility again.
    #[test]
    fn trust_that_never_flips_asks_for_the_click_then_names_the_app() {
        let (mut sys, ctx, item) = fixture();
        sys.tty = true;
        sys.responsible = Some("Terminal".into());
        sys.trusts(&[false]);
        let open = Cmd::new(OPEN)
            .args(["-b", "com.apple.MobileSMS"])
            .as_user(&item.user)
            .in_session();
        sys.on(&open.display(), Ran::ok(""));
        store(&sys, &item, DISABLED);
        prefs(&sys, &item, EMPTY_PREFS);
        item.apply(&sys, &ctx).unwrap();
        let Some(aterm_converge::StoodAside::Permission(why)) = item.stood_aside() else {
            panic!("{:?}", item.stood_aside());
        };
        assert!(why.starts_with("Terminal has no Accessibility"), "{why}");
        assert!(why.contains("`aterm init --sign-out`"), "{why}");
        assert_eq!(count(&sys, "ax-request"), 1);
        assert_eq!(count(&sys, &open.display()), 1);
        assert_eq!(count(&sys, "sleep 250ms"), 480 + 480);
        assert!(sys.calls().iter().all(|c| !c.contains(AUTOMATION_ASK)));

        let other = SignOut {
            service: Service::FaceTime,
            user: item.user.clone(),
            blocked: Mutex::new(None),
            reading: crate::init_observe::LastReading::default(),
            consent: std::sync::Arc::clone(&item.consent),
        };
        let facetime = Cmd::new(OPEN)
            .args(["-b", "com.apple.FaceTime"])
            .as_user(&item.user)
            .in_session();
        sys.on(&facetime.display(), Ran::ok(""));
        sys.on(
            &Cmd::new("/usr/bin/defaults")
                .args(["export", Service::FaceTime.domain(), "-"])
                .as_user(&item.user)
                .display(),
            Ran::ok("<dict><key>ActiveAccounts</key><array><string>a</string></array></dict>"),
        );
        let before = count(&sys, "sleep 250ms");
        other.apply(&sys, &ctx).unwrap();
        assert_eq!(count(&sys, "ax-request"), 1, "asked once for the run");
        assert_eq!(
            count(&sys, "sleep 250ms") - before,
            480,
            "the click's wait alone"
        );

        // The person clicks Sign Out by hand: the run goes on, signed out.
        let (mut sys, ctx, item) = fixture();
        sys.tty = true;
        sys.trusts(&[false]);
        sys.on(&open.display(), Ran::ok(""));
        signed_in_for(&sys, &item, 3);
        item.apply(&sys, &ctx).unwrap();
        assert_eq!(item.stood_aside(), None);
    }

    /// The app left the front before a click: the script clicks nothing more and the row fails
    /// without reading for a deregistration it did not ask for.
    #[test]
    fn an_app_that_leaves_the_front_ends_the_clicks() {
        let (mut sys, ctx, item) = fixture();
        sys.tty = true;
        sys.trusts(&[true]);
        clicks(&sys, &item, "not-frontmost");
        let why = item.apply(&sys, &ctx).unwrap_err();
        assert!(why.starts_with("Messages left the front"), "{why}");
        assert!(
            sys.calls().iter().all(|c| !c.contains("defaults")),
            "{:?}",
            sys.calls()
        );
    }

    /// A refused Automation question stands aside before any app opens.
    #[test]
    fn a_refused_automation_question_opens_no_app() {
        let (sys, ctx, item) = fixture();
        sys.trusts(&[true]);
        sys.on(
            &automation_ask(&item).display(),
            Ran::exit(
                1,
                "execution error: Not authorized to send Apple events to System Events. (-1743)",
            ),
        );
        item.apply(&sys, &ctx).unwrap();
        assert_eq!(
            item.stood_aside(),
            Some(aterm_converge::StoodAside::Permission(
                AUTOMATION_OFF.into()
            ))
        );
        assert!(
            sys.calls()
                .iter()
                .all(|c| !c.contains("Sign Out") && !c.contains(OPEN))
        );
    }

    #[test]
    fn a_consent_request_is_answered_in_this_process() {
        for (readings, expected) in [
            (Some(false), Some(false)),
            (Some(true), Some(true)),
            (None, None),
        ] {
            let sys = Fake::root();
            if let Some(trusted) = readings {
                sys.trusts(&[trusted]);
            }
            assert_eq!(request_accessibility(&sys).ok(), expected);
            assert_eq!(sys.calls(), ["ax-request"]);
        }
    }

    #[test]
    fn unsupported_localized_labels_refuse_before_launching_the_app() {
        let (sys, ctx, item) = fixture();
        sys.trusts(&[true]);
        sys.on(&automation_ask(&item).display(), Ran::ok("System Events"));
        sys.on(
            &labels_cmd(&item.user, item.service).display(),
            Ran::ok(r#"{"panes":[],"sign_out":[]}"#),
        );
        assert!(item.apply(&sys, &ctx).is_err());
        assert_eq!(sys.calls().len(), 3, "{:?}", sys.calls());
        assert!(sys.calls().iter().all(|c| !c.contains("keystroke")));
    }

    #[test]
    fn an_expired_shared_deadline_refuses_before_running_another_child() {
        let (sys, _ctx, item) = fixture();
        let expired = Instant::now() - Duration::from_secs(60);
        assert!(localized_labels(&sys, &item.user, item.service, Some(expired)).is_err());
        assert!(sys.calls().is_empty());
    }

    #[test]
    fn ui_exit_success_requires_removal_and_readback() {
        for removed in [false, true] {
            let (sys, ctx, item) = fixture();
            sys.trusts(&[true]);
            clicks(&sys, &item, "sign-out-requested");
            if removed {
                signed_in_for(&sys, &item, 2);
            } else {
                store(&sys, &item, DISABLED);
                prefs(&sys, &item, EMPTY_PREFS);
            }
            let rows = engine::run(&[Box::new(item)], Mode::Apply, &sys, &ctx, &mut NoJournal);
            assert_eq!(
                rows[0].outcome,
                if removed {
                    Outcome::Changed
                } else {
                    Outcome::Failed
                }
            );
            assert!(sys.calls().iter().all(|c| !c.contains("private-token")));
            assert!(sys.calls().iter().all(|c| !c.contains("/bin/sleep")));
        }
    }

    #[test]
    fn sign_out_applescript_walks_ui_elements_and_matches_catalyst_descriptions() {
        assert!(
            !SIGN_OUT.contains("entire contents"),
            "macOS 27 Tahoe System Events returns 0 elements for `entire contents` on Messages/FaceTime windows"
        );
        assert!(SIGN_OUT.contains("UI elements of containerElement"));
        assert!(SIGN_OUT.contains("description of elementRef as string"));
        assert!(SIGN_OUT.contains("AXMenuItemCmdChar"));
    }

    /// The script never types a key, and every click it makes for the sign-out comes right after
    /// a look that the app is still in front.
    #[test]
    fn every_sign_out_click_follows_a_frontmost_check_and_nothing_is_typed() {
        assert!(!SIGN_OUT.contains("keystroke"));
        let lines: Vec<&str> = SIGN_OUT.lines().map(str::trim).collect();
        let clicks: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| {
                !line.starts_with("--") && line.contains("click ") && !line.contains("closeButton")
            })
            .map(|(at, _)| at)
            .collect();
        assert_eq!(clicks.len(), 4, "{clicks:?}");
        for at in clicks {
            assert_eq!(lines[at - 1], "my mustBeFront(appName)", "{}", lines[at]);
        }
    }
}
