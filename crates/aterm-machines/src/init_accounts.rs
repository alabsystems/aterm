// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Durable Messages and FaceTime sign-out for local initialization.
//!
//! An offline or disabled account is still signed in. The primary read therefore checks
//! identityservicesd's persisted `Accounts`, including disabled accounts, and corroborates
//! it with the service's active/online preferences. It never changes those stores itself.
//! Apple's own Sign Out controls perform the change; the engine reads the stores again.
//! No account identifiers, tokens, or plist payloads belong in reports or diagnostics.

use crate::items::{Ctx, Item};
use crate::model::{PValue, Probe, User};
use crate::plist::{self, Entries};
use crate::port::{Cmd, System};
use std::io;
use std::path::{Path, PathBuf};

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

    // The shipped /System/Library/IdentityServices/ServiceDefinitions plists declare
    // these Identifier values (their filenames use the legacy iMessage/FaceTime names).
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

    const fn settings(self) -> &'static str {
        match self {
            Self::Messages => "Messages > Settings > iMessage > Sign Out",
            Self::FaceTime => "FaceTime > Settings > General > Sign Out",
        }
    }
}

struct SignOut {
    service: Service,
    user: User,
}

/// The two account items for each selected local user.
///
/// # Errors
/// A selected user has no absolute home directory.
pub fn items(ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    let mut items: Vec<Box<dyn Item>> = Vec::new();
    for user in &ctx.users {
        if !Path::new(&user.home).is_absolute() {
            return Err("account sign-out needs an absolute user home".into());
        }
        for service in [Service::Messages, Service::FaceTime] {
            items.push(Box::new(SignOut {
                service,
                user: user.clone(),
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
        // This does not prompt for or change Accessibility permission. In particular,
        // already-signed-out users never launch either app or reach this preflight.
        let permission = Cmd::new(OSASCRIPT)
            .args(["-l", "JavaScript", "-e", AX_PREFLIGHT])
            .as_user(&self.user)
            .in_session();
        let answer = sys.run_complete_stdout(&permission).map_err(|_| {
            "could not check Accessibility permission for account sign-out".to_owned()
        })?;
        if !answer.success() || answer.out().trim() != "true" {
            return Err(format!(
                "account sign-out needs Accessibility permission for aterm; or use {}",
                self.service.settings()
            ));
        }
        let command = signout_cmd(&self.user, self.service);
        let ran = sys.run_complete_stdout(&command).map_err(|_| {
            format!(
                "{} sign-out could not run; use {}",
                self.service.name(),
                self.service.settings()
            )
        })?;
        if !ran.success() || ran.out().trim() != "sign-out-requested" {
            // An AppleEvent refusal can contain personal UI text: never forward it.
            return Err(format!(
                "{} sign-out needs its Settings controls and Automation permission for System Events; use {}",
                self.service.name(),
                self.service.settings()
            ));
        }
        // Deregistration and cfprefsd updates can follow the UI acknowledgment. The
        // wait is at most 60 registration attempts, not a shared minute deadline:
        // each probe and sleep retains the port's own command budget. Only the
        // same durable registration probe ends this wait successfully.
        for attempt in 0..60 {
            if !registered(sys, &self.user, self.service)? {
                return Ok(());
            }
            if attempt != 59 {
                let wait = Cmd::new("/bin/sleep").arg("1").as_user(&self.user);
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
    // Check cfprefsd's live account lists as well as the durable account store. Neither
    // an empty exported domain nor an offline connection alone establishes sign-out.
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

const AX_PREFLIGHT: &str = "ObjC.import('ApplicationServices'); $.AXIsProcessTrusted()";

fn signout_cmd(user: &User, service: Service) -> Cmd {
    Cmd::new(OSASCRIPT)
        .args(["-e", SIGN_OUT, "--", service.name()])
        .as_user(user)
        .in_session()
}

// UI automation is confined to the account Settings window and its confirmation sheet.
// Never use Messages' `logout`: it disconnects accounts without removing registration.
// Apple documents these controls at support.apple.com/guide/messages/ichte16154fb/mac
// and support.apple.com/guide/facetime/fctme7243ba3/mac.
const SIGN_OUT: &str = r#"on run argv
    set appName to item 1 of argv
    if appName is not "Messages" and appName is not "FaceTime" then error "unsupported app"
    tell application "System Events"
        set priorApp to name of first application process whose frontmost is true
        set wasRunning to exists process appName
    end tell
    set settingsWindow to missing value
    set closeSettings to false
    if appName is "Messages" then
        set paneName to "iMessage"
    else
        set paneName to "General"
    end if
    try
        tell application appName to activate
        set settingsMenu to missing value
        repeat 120 times
            tell application "System Events"
                if exists process appName then
                    tell process appName
                        if exists menu bar 1 then
                            if exists menu item "Settings…" of menu appName of menu bar item appName of menu bar 1 then
                                set candidateMenu to menu item "Settings…" of menu appName of menu bar item appName of menu bar 1
                                if enabled of candidateMenu then set settingsMenu to candidateMenu
                            end if
                        end if
                    end tell
                end if
            end tell
            if settingsMenu is not missing value then exit repeat
            delay 0.1
        end repeat
        if settingsMenu is missing value then error "account Settings menu unavailable"
        set priorSettings to my accountSettings(appName, paneName)
        set closeSettings to priorSettings is missing value
        tell application "System Events"
            click settingsMenu
        end tell
        repeat 120 times
            set settingsWindow to my accountSettings(appName, paneName)
            if settingsWindow is not missing value then exit repeat
            delay 0.1
        end repeat
        if settingsWindow is missing value then error "account Settings window unavailable"
        set paneButton to my namedButton(settingsWindow, paneName)
        if paneButton is missing value then error "account settings pane unavailable"
        tell application "System Events" to click paneButton
        set signOutButton to missing value
        repeat 120 times
            set signOutButton to my namedButton(settingsWindow, "Sign Out")
            if signOutButton is not missing value then exit repeat
            delay 0.1
        end repeat
        if signOutButton is missing value then error "account Sign Out control unavailable"
        tell application "System Events" to click signOutButton
        set acknowledged to false
        repeat 120 times
            tell application "System Events"
                if exists sheet 1 of settingsWindow then
                    set confirmButton to my namedButton(sheet 1 of settingsWindow, "Sign Out")
                    if confirmButton is not missing value then
                        click confirmButton
                        set acknowledged to true
                    end if
                else if not (exists settingsWindow) then
                    set acknowledged to true
                else if my existingButton(settingsWindow, "Sign Out") is missing value then
                    set acknowledged to true
                end if
            end tell
            if acknowledged then exit repeat
            delay 0.1
        end repeat
        if acknowledged is false then error "account Sign Out did not finish"
        my closeOwnedSettings(settingsWindow, closeSettings)
    on error
        my closeOwnedSettings(settingsWindow, closeSettings)
        if wasRunning is false then tell application appName to quit
        tell application priorApp to activate
        error "account sign-out controls unavailable or permission refused"
    end try
    if wasRunning is false then tell application appName to quit
    tell application priorApp to activate
    return "sign-out-requested"
end run

on accountSettings(appName, paneName)
    tell application "System Events"
        tell process appName
            repeat with candidateWindow in windows
                -- Inspect only toolbar controls until the account Settings window is
                -- identified; never traverse a conversation or call-history view.
                repeat with candidateToolbar in toolbars of candidateWindow
                    set paneButton to my namedButton(candidateToolbar, paneName)
                    if paneButton is not missing value then return candidateWindow
                end repeat
            end repeat
        end tell
    end tell
    return missing value
end accountSettings

on namedButton(containerElement, buttonName)
    set buttonRef to my existingButton(containerElement, buttonName)
    if buttonRef is missing value then return missing value
    tell application "System Events"
        if enabled of buttonRef then return buttonRef
    end tell
    return missing value
end namedButton

on existingButton(containerElement, buttonName)
    tell application "System Events"
        repeat with elementRef in entire contents of containerElement
            if role of elementRef is in {"AXButton", "AXRadioButton"} then
                if name of elementRef is buttonName then return elementRef
            end if
        end repeat
    end tell
    return missing value
end existingButton

on closeOwnedSettings(settingsWindow, closeSettings)
    if closeSettings and settingsWindow is not missing value then
        try
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
        assert!(
            item.apply(&sys, &ctx)
                .unwrap_err()
                .contains("Accessibility")
        );
        assert_eq!(sys.calls().len(), 1);
    }

    #[test]
    fn ui_exit_success_requires_removal_and_readback() {
        for removed in [false, true] {
            let (sys, ctx, item) = fixture();
            store(&sys, &item, DISABLED);
            if removed {
                // The first post-click read still holds the account; a later durable
                // update, rather than the UI process's exit, completes the apply.
                store(&sys, &item, DISABLED);
                store(&sys, &item, "<dict/>");
            }
            prefs(&sys, &item, EMPTY_PREFS);
            let preflight = Cmd::new(OSASCRIPT)
                .args(["-l", "JavaScript", "-e", AX_PREFLIGHT])
                .as_user(&item.user)
                .in_session();
            sys.on(&preflight.display(), Ran::ok("true"));
            sys.on(
                &signout_cmd(&item.user, item.service).display(),
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
}
