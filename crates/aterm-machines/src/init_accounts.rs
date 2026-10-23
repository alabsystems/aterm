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
const GZIP: &str = "/usr/bin/gzip";
const PLUTIL: &str = "/usr/bin/plutil";
const OSASCRIPT: &str = "/usr/bin/osascript";

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
}

struct SignOut {
    service: Service,
    user: User,
    blocked: Mutex<Option<String>>,
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
        match registered(sys, &self.user, self.service) {
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
        *self.blocked.lock().unwrap_or_else(PoisonError::into_inner) = None;
        let deadline = Instant::now() + Duration::from_secs(60);
        if !accessibility_ready_for(sys, Some(&self.user), Some(deadline))? {
            *self.blocked.lock().unwrap_or_else(PoisonError::into_inner) = Some("macOS Accessibility permission for aterm is unavailable; account setup will retry automatically".into());
            return Ok(());
        }
        let labels = localized_labels(sys, &self.user, self.service, Some(deadline))?;
        let command = limited(
            signout_cmd(&self.user, self.service, &labels),
            Some(deadline),
        )?;
        let ran = sys
            .run_complete_stdout(&command)
            .map_err(|_| format!("{} sign-out could not run", self.service.name()))?;
        if ran.success() && ran.out().trim() == "permission-blocked" {
            *self.blocked.lock().unwrap_or_else(PoisonError::into_inner) = Some("macOS Automation permission to control System Events is unavailable; account setup will retry automatically".into());
            return Ok(());
        }
        if !ran.success() || ran.out().trim() != "sign-out-requested" {
            return Err(format!(
                "{} account Settings controls were unavailable; sign-out remains unverified",
                self.service.name()
            ));
        }
        for attempt in 0..60 {
            if !registered_until(sys, &self.user, self.service, Some(deadline))? {
                return Ok(());
            }
            if attempt != 59 {
                let wait = limited(
                    Cmd::new("/bin/sleep").arg("1").as_user(&self.user),
                    Some(deadline),
                )?;
                let ran = sys
                    .run(&wait)
                    .map_err(|_| "could not await account deregistration".to_owned())?;
                if !ran.success() {
                    return Err("could not await account deregistration".into());
                }
            }
        }
        Err("the account remains registered after Sign Out; verify its account settings".into())
    }

    fn stood_aside(&self) -> Option<aterm_converge::StoodAside> {
        self.blocked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .map(aterm_converge::StoodAside::Permission)
    }
}

fn store_path(user: &User) -> PathBuf {
    Path::new(&user.home).join(STORE)
}

fn registered(sys: &dyn System, user: &User, service: Service) -> Result<bool, String> {
    registered_until(sys, user, service, None)
}

fn registered_until(
    sys: &dyn System,
    user: &User,
    service: Service,
    deadline: Option<Instant>,
) -> Result<bool, String> {
    let path = store_path(user);
    let persisted = match sys.stat(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(_) => return Err("cannot inspect the persisted account store".into()),
        Ok(s) if s.dir || s.symlink => {
            return Err("persisted account store is not a regular file".into());
        }
        Ok(_) => {
            let gzip = limited(
                Cmd::new(GZIP).args(["-dc", "--"]).arg(path).as_user(user),
                deadline,
            )?;
            let unpacked = sys
                .run_complete_stdout(&gzip)
                .map_err(|_| "cannot read the complete persisted account store".to_owned())?;
            if !unpacked.success() {
                return Err("cannot decompress the persisted account store".into());
            }
            let convert = limited(
                Cmd::new(PLUTIL)
                    .args(["-convert", "xml1", "-o", "-", "--", "-"])
                    .as_user(user),
                deadline,
            )?;
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
    let cmd = limited(
        Cmd::new("/usr/bin/defaults")
            .args(["export", service.domain(), "-"])
            .as_user(user),
        deadline,
    )?;
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

const AX_PREFLIGHT: &str = "ObjC.import('ApplicationServices'); $.AXIsProcessTrusted()";
const AX_REQUEST: &str = "ObjC.import('Foundation'); ObjC.import('ApplicationServices'); $.AXIsProcessTrustedWithOptions($.NSDictionary.dictionaryWithObjectForKey($.NSNumber.numberWithBool(true), $.kAXTrustedCheckOptionPrompt))";

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

/// Ask macOS to show its native Accessibility consent once for automatic setup.
///
/// # Errors
/// The bounded trust request could not run or returned an unknown result.
pub fn request_accessibility(sys: &dyn System) -> Result<bool, String> {
    let request = Cmd::new(OSASCRIPT)
        .args(["-l", "JavaScript", "-e", AX_REQUEST])
        .budget(Duration::from_secs(10));
    let answer = sys.run_complete_stdout(&request).map_err(|_| {
        "could not request Accessibility permission for account sign-out".to_owned()
    })?;
    if !answer.success() {
        return Err("could not request Accessibility permission for account sign-out".into());
    }
    trust_answer(answer.out().trim())
}

/// Inspect Accessibility trust without requesting permission or opening an app.
///
/// # Errors
/// The OS trust query could not run or returned an unknown result.
pub fn accessibility_ready(sys: &dyn System) -> Result<bool, String> {
    accessibility_ready_for(sys, None, None)
}

fn accessibility_ready_for(
    sys: &dyn System,
    user: Option<&User>,
    deadline: Option<Instant>,
) -> Result<bool, String> {
    let mut permission = Cmd::new(OSASCRIPT).args(["-l", "JavaScript", "-e", AX_PREFLIGHT]);
    if let Some(user) = user {
        permission = permission.as_user(user).in_session();
    }
    let answer = sys
        .run_complete_stdout(&limited(permission, deadline)?)
        .map_err(|_| "could not check Accessibility permission for account sign-out".to_owned())?;
    if !answer.success() {
        return Err("could not check Accessibility permission for account sign-out".into());
    }
    trust_answer(answer.out().trim())
}

fn trust_answer(answer: &str) -> Result<bool, String> {
    match answer {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err("Accessibility trust query returned an unknown result".into()),
    }
}

fn limited(cmd: Cmd, deadline: Option<Instant>) -> Result<Cmd, String> {
    let Some(deadline) = deadline else {
        return Ok(cmd);
    };
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err("account sign-out exceeded its one-minute deadline".into());
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
        set settingsReady to false
        repeat 120 times
            my checkDeadline(deadline)
            tell application "System Events"
                if exists process appName then
                    tell process appName
                        if exists menu bar 1 then set settingsReady to true
                    end tell
                end if
            end tell
            if settingsReady then exit repeat
            delay 0.1
        end repeat
        if settingsReady is false then error "account Settings menu unavailable"
        set priorSettings to my accountSettings(appName, paneNames)
        set closeSettings to priorSettings is missing value
        if priorSettings is missing value then
            tell application "System Events"
                tell process appName
                    set openedByMenu to false
                    try
                        repeat with mi in menu items of menu 1 of menu bar item 2 of menu bar 1
                            try
                                if (value of attribute "AXMenuItemCmdChar" of mi) is "," then
                                    click mi
                                    set openedByMenu to true
                                    exit repeat
                                end if
                            end try
                        end repeat
                    end try
                    if openedByMenu is false then keystroke "," using command down
                end tell
            end tell
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
        tell application "System Events" to click paneButton
        set signOutButton to missing value
        repeat 120 times
            my checkDeadline(deadline)
            set signOutButton to my namedButton(settingsWindow, signOutNames)
            if signOutButton is not missing value then exit repeat
            delay 0.1
        end repeat
        if signOutButton is missing value then error "account Sign Out control unavailable"
        tell application "System Events" to click signOutButton
        set acknowledged to false
        repeat 120 times
            my checkDeadline(deadline)
            tell application "System Events"
                if exists sheet 1 of settingsWindow then
                    set confirmButton to my namedButton(sheet 1 of settingsWindow, signOutNames)
                    if confirmButton is not missing value then
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
        my closeOwnedSettings(settingsWindow, closeSettings)
    on error errorText number errorNumber
        my closeOwnedSettings(settingsWindow, closeSettings)
        my restoreApplications(appName, wasRunning, priorApp)
        if errorNumber is -1743 or errorNumber is -25211 then return "permission-blocked"
        error "account sign-out controls unavailable"
    end try
    my restoreApplications(appName, wasRunning, priorApp)
    return "sign-out-requested"
    end timeout
end run

on checkDeadline(deadline)
    if (current date) is greater than deadline then error "account Settings deadline expired"
end checkDeadline

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

on closeOwnedSettings(settingsWindow, closeSettings)
    if closeSettings and settingsWindow is not missing value then
        try
            with timeout of 2 seconds
            tell application "System Events"
                if exists settingsWindow then
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
        };
        (Fake::root(), ctx, item)
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
    }

    #[test]
    fn absent_registrations_do_not_open_apps_or_request_permissions() {
        let (sys, ctx, item) = fixture();
        store(&sys, &item, "<dict/>");
        prefs(&sys, &item, EMPTY_PREFS);
        assert!(matches!(item.probe(&sys, &ctx), Probe::Ok { .. }));
        assert!(sys.calls().iter().all(|c| !c.contains("osascript")));
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

    #[test]
    fn permission_refusal_does_not_launch_an_account_app() {
        let (sys, ctx, item) = fixture();
        let cmd = Cmd::new(OSASCRIPT)
            .args(["-l", "JavaScript", "-e", AX_PREFLIGHT])
            .as_user(&item.user)
            .in_session();
        sys.on(&cmd.display(), Ran::ok("false"));
        item.apply(&sys, &ctx)
            .expect("permission block is a structured outcome");
        assert!(matches!(
            item.stood_aside(),
            Some(aterm_converge::StoodAside::Permission(_))
        ));
        assert_eq!(sys.calls().len(), 1);
    }

    #[test]
    fn a_consent_request_is_bounded_and_waits_for_the_grant() {
        for (answer, expected) in [
            ("false", Some(false)),
            ("true", Some(true)),
            ("unknown", None),
        ] {
            let sys = Fake::root();
            let request = Cmd::new(OSASCRIPT)
                .args(["-l", "JavaScript", "-e", AX_REQUEST])
                .budget(Duration::from_secs(10));
            sys.on(&request.display(), Ran::ok(answer));
            assert_eq!(request_accessibility(&sys).ok(), expected);
            assert_eq!(sys.calls(), vec![request.display()]);
        }
    }

    #[test]
    fn unsupported_localized_labels_refuse_before_launching_the_app() {
        let (sys, ctx, item) = fixture();
        let preflight = Cmd::new(OSASCRIPT)
            .args(["-l", "JavaScript", "-e", AX_PREFLIGHT])
            .as_user(&item.user)
            .in_session();
        sys.on(&preflight.display(), Ran::ok("true"));
        sys.on(
            &labels_cmd(&item.user, item.service).display(),
            Ran::ok(r#"{"panes":[],"sign_out":[]}"#),
        );
        assert!(item.apply(&sys, &ctx).is_err());
        assert_eq!(sys.calls().len(), 2);
        assert!(sys.calls().iter().all(|c| !c.contains("keystroke")));
    }

    #[test]
    fn an_expired_shared_deadline_refuses_before_running_another_child() {
        let (sys, _ctx, item) = fixture();
        let expired = Instant::now() - Duration::from_secs(60);
        assert!(registered_until(&sys, &item.user, item.service, Some(expired)).is_err());
        assert!(sys.calls().is_empty());
    }

    #[test]
    fn ui_exit_success_requires_removal_and_readback() {
        for removed in [false, true] {
            let (sys, ctx, item) = fixture();
            store(&sys, &item, DISABLED);
            if removed {
                store(&sys, &item, DISABLED);
                store(&sys, &item, "<dict/>");
            }
            prefs(&sys, &item, EMPTY_PREFS);
            let preflight = Cmd::new(OSASCRIPT)
                .args(["-l", "JavaScript", "-e", AX_PREFLIGHT])
                .as_user(&item.user)
                .in_session();
            sys.on(&preflight.display(), Ran::ok("true"));
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
                Ran::ok("sign-out-requested"),
            );
            sys.on(
                &Cmd::new("/bin/sleep")
                    .arg("1")
                    .as_user(&item.user)
                    .display(),
                Ran::ok(""),
            );
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
}
