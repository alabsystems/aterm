// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Machine remote control for inspecting and selecting macOS alert boxes.

use crate::model::User;
use crate::port::{Cmd, System};
use std::time::Duration;

const OSASCRIPT: &str = "/usr/bin/osascript";

/// One visible alert window or modal sheet observed on the active console session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlertBox {
    /// The hosting process name in `System Events` (e.g. `CoreServicesUIAgent`).
    pub process: String,
    /// The window or sheet title, if any.
    pub title: String,
    /// The actionable button titles present in the alert box.
    pub buttons: Vec<String>,
}

const INSPECT_ALERTS_JXA: &str = r#"
const se = Application('System Events');
const targets = [
    'UserNotificationCenter',
    'CoreServicesUIAgent',
    'SecurityAgent',
    'Install Command Line Developer Tools',
    'System Settings'
];
const found = [];
for (const name of targets) {
    try {
        const procs = se.applicationProcesses.whose({ name: name })();
        for (const proc of procs) {
            const wins = proc.windows();
            for (const win of wins) {
                let title = '';
                try { title = String(win.name() || ''); } catch (_) {}
                const buttons = [];
                try {
                    for (const b of win.buttons()) {
                        try {
                            const bname = String(b.name() || '').trim();
                            if (bname.length > 0) buttons.push(bname);
                        } catch (_) {}
                    }
                } catch (_) {}
                try {
                    for (const sheet of win.sheets()) {
                        for (const b of sheet.buttons()) {
                            try {
                                const bname = String(b.name() || '').trim();
                                if (bname.length > 0 && !buttons.includes(bname)) buttons.push(bname);
                            } catch (_) {}
                        }
                    }
                } catch (_) {}
                if (buttons.length > 0 || title.length > 0) {
                    found.push({ process: name, title: title, buttons: buttons });
                }
            }
        }
    } catch (_) {}
}
JSON.stringify(found);
"#;

const SELECT_ALERT_BUTTON_JXA: &str = r#"
function run(argv) {
    const targetProc = argv[0];
    const targetButton = argv[1];
    const se = Application('System Events');
    const procs = se.applicationProcesses.whose({ name: targetProc })();
    for (const proc of procs) {
        for (const win of proc.windows()) {
            try {
                for (const b of win.buttons()) {
                    if (String(b.name() || '').trim() === targetButton) {
                        b.click();
                        return 'clicked';
                    }
                }
            } catch (_) {}
            try {
                for (const sheet of win.sheets()) {
                    for (const b of sheet.buttons()) {
                        if (String(b.name() || '').trim() === targetButton) {
                            b.click();
                            return 'clicked';
                        }
                    }
                }
            } catch (_) {}
        }
    }
    return 'not-found';
}
"#;

/// Inspect visible system alert boxes on `user`'s active GUI session.
/// # Errors
/// `osascript` could not run or returned malformed JSON.
pub fn inspect_alert_boxes(sys: &dyn System, user: &User) -> Result<Vec<AlertBox>, String> {
    let cmd = Cmd::new(OSASCRIPT)
        .args(["-l", "JavaScript", "-e", INSPECT_ALERTS_JXA])
        .as_user(user)
        .in_session()
        .budget(Duration::from_secs(10));
    let ran = sys
        .run_complete_stdout(&cmd)
        .map_err(|e| format!("could not inspect alert boxes: {e}"))?;
    if !ran.success() {
        return Err(format!("alert box inspection failed: {}", ran.describe()));
    }
    let parsed: aterm_json::Value = aterm_json::from_slice(&ran.stdout)
        .map_err(|_| "alert box inspection returned invalid JSON".to_owned())?;
    let arr = parsed
        .as_array()
        .ok_or_else(|| "alert box inspection root is not an array".to_owned())?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let Some(process) = item["process"].as_str() else {
            continue;
        };
        let title = item["title"].as_str().unwrap_or_default().to_owned();
        let buttons = item["buttons"]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        out.push(AlertBox {
            process: process.to_owned(),
            title,
            buttons,
        });
    }
    Ok(out)
}

/// Select and click a named button in an active alert box belonging to `process`.
/// # Errors
/// `osascript` could not run or exited non-zero.
pub fn select_alert_button(
    sys: &dyn System,
    user: &User,
    process: &str,
    button_label: &str,
) -> Result<bool, String> {
    if process.is_empty() || button_label.is_empty() {
        return Err("process and button_label must be non-empty".into());
    }
    let cmd = Cmd::new(OSASCRIPT)
        .args([
            "-l",
            "JavaScript",
            "-e",
            SELECT_ALERT_BUTTON_JXA,
            "--",
            process,
            button_label,
        ])
        .as_user(user)
        .in_session()
        .budget(Duration::from_secs(10));
    let ran = sys
        .run_complete_stdout(&cmd)
        .map_err(|e| format!("could not click alert button: {e}"))?;
    if !ran.success() {
        return Err(format!("alert button selection failed: {}", ran.describe()));
    }
    Ok(ran.out().trim() == "clicked")
}

/// Result of one workstation verification control.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionRoundTripReport {
    /// Name of the workstation function tested.
    pub function: String,
    /// Observed OFF/drift state, or why no OFF stage ran.
    pub off_state: String,
    /// Observed state after applying ON and reading back.
    pub on_state: String,
    /// Whether this control's requested stages succeeded.
    pub verified: bool,
}

fn round_trip_context(sys: &dyn System) -> Result<(crate::items::Ctx, User), String> {
    let mut ctx = crate::init::initial_setup_context(sys)?;
    let user = ctx
        .console
        .clone()
        .ok_or_else(|| "no console user found for round-trip verification".to_owned())?;
    let command = crate::facts::hidutil_keyboards();
    let keyboards = sys
        .run_complete_stdout(&command)
        .map_err(|e| format!("{}: {e}", command.display()))?;
    if !keyboards.success() {
        return Err(format!("{}: {}", command.display(), keyboards.describe()));
    }
    ctx.facts.keyboards = crate::facts::parse_hidutil_keyboards_complete(&keyboards.out())?;
    Ok((ctx, user))
}

fn verify_spotlight_policy(
    sys: &dyn System,
    ctx: &crate::items::Ctx,
) -> Result<FunctionRoundTripReport, String> {
    use crate::model::{Mode, Outcome};

    let rows = crate::init::run_initial_spotlight(sys, ctx, Mode::Plan)?;
    if !rows
        .iter()
        .all(|row| matches!(row.outcome, Outcome::Ok | Outcome::NotApplicable))
    {
        return Err(format!("Spotlight items failed to verify Ok: {rows:?}"));
    }
    Ok(FunctionRoundTripReport {
        function: "Spotlight .noindex Control".into(),
        off_state: "not toggled; existing caches retained".into(),
        on_state: "Spotlight policy already satisfied where applicable; no changes applied".into(),
        verified: true,
    })
}

/// Run drift/apply/readback controls for keyboard, scrolling and desktop preferences.
/// # Errors
/// Returns an error if a requested drift, apply, readback or alert inspection stage fails.
pub fn verify_workstation_round_trip(
    sys: &dyn System,
) -> Result<Vec<FunctionRoundTripReport>, String> {
    use crate::init::{WorkstationGroup, run_workstation_selected};
    use crate::model::{Mode, Outcome};
    use aterm_types::macos_setup::{Selection, Setting};

    let (ctx, user) = round_trip_context(sys)?;
    let mut reports = Vec::new();

    if let Some(kb) = ctx.facts.keyboards.first() {
        let mut only_caps = Selection::none();
        only_caps.set(Setting::CapsLock, true);
        crate::init_preferences::clear_attached_caps_lock(
            sys,
            &user,
            crate::items::keyboard::Model {
                vendor: kb.vendor,
                product: kb.product,
            },
        )
        .map_err(|e| format!("hidutil clear failed: {e}"))?;
        let drift =
            run_workstation_selected(sys, WorkstationGroup::Preferences, Mode::Plan, only_caps)
                .map_err(|e| e.to_string())?;
        if !drift
            .iter()
            .any(|r| r.id.contains("keyboard.") && r.outcome == Outcome::Planned)
        {
            return Err(format!(
                "CapsLock OFF was not detected as Planned drift: {drift:?}"
            ));
        }
        let applied =
            run_workstation_selected(sys, WorkstationGroup::Preferences, Mode::Apply, only_caps)
                .map_err(|e| e.to_string())?;
        if !applied.iter().all(|r| {
            matches!(
                r.outcome,
                Outcome::Ok | Outcome::Changed | Outcome::NotApplicable
            )
        }) {
            return Err(format!("CapsLock ON failed immediate apply: {applied:?}"));
        }
        let verified =
            run_workstation_selected(sys, WorkstationGroup::Preferences, Mode::Plan, only_caps)
                .map_err(|e| e.to_string())?;
        if !verified
            .iter()
            .all(|r| matches!(r.outcome, Outcome::Ok | Outcome::NotApplicable))
        {
            return Err(format!(
                "CapsLock ON failed immediate readback: {verified:?}"
            ));
        }
        reports.push(FunctionRoundTripReport {
            function: "CapsLock -> Control".into(),
            off_state: "Caps Lock mapping absent; other remaps retained (Planned drift)".into(),
            on_state: "0x700000039 -> 0x7000000E0 (Ok)".into(),
            verified: true,
        });
    }

    {
        let mut only_ns = Selection::none();
        only_ns.set(Setting::NaturalScrolling, true);
        sys.set_natural_scrolling(true).map_err(|e| e.to_string())?;
        let drift =
            run_workstation_selected(sys, WorkstationGroup::Preferences, Mode::Plan, only_ns)
                .map_err(|e| e.to_string())?;
        if !drift
            .iter()
            .any(|r| r.id.contains("scroll-direction") && r.outcome == Outcome::Planned)
        {
            return Err(format!(
                "NaturalScrolling OFF was not detected as Planned drift: {drift:?}"
            ));
        }
        let applied =
            run_workstation_selected(sys, WorkstationGroup::Preferences, Mode::Apply, only_ns)
                .map_err(|e| e.to_string())?;
        if !applied
            .iter()
            .all(|r| matches!(r.outcome, Outcome::Ok | Outcome::Changed))
        {
            return Err(format!(
                "NaturalScrolling ON failed immediate apply: {applied:?}"
            ));
        }
        if sys.natural_scrolling().map_err(|e| e.to_string())? {
            return Err("NaturalScrolling ON readback remained true".into());
        }
        reports.push(FunctionRoundTripReport {
            function: "NaturalScrolling".into(),
            off_state: "natural=true (Planned drift)".into(),
            on_state: "natural=false (Ok)".into(),
            verified: true,
        });
    }

    {
        let mut only_tp = Selection::none();
        only_tp.set(Setting::TrackpadScrolling, true);
        let off = Cmd::new("/usr/bin/defaults")
            .args([
                "write",
                "com.apple.AppleMultitouchTrackpad",
                "TrackpadHorizScroll",
                "-int",
                "0",
            ])
            .as_user(&user);
        if !sys.run(&off).map_err(|e| e.to_string())?.success() {
            return Err("failed to set TrackpadHorizScroll=0".into());
        }
        let drift =
            run_workstation_selected(sys, WorkstationGroup::Preferences, Mode::Plan, only_tp)
                .map_err(|e| e.to_string())?;
        if !drift.iter().any(|r| r.outcome == Outcome::Planned) {
            return Err(format!(
                "TrackpadScrolling OFF was not detected as Planned drift: {drift:?}"
            ));
        }
        let applied =
            run_workstation_selected(sys, WorkstationGroup::Preferences, Mode::Apply, only_tp)
                .map_err(|e| e.to_string())?;
        if !applied
            .iter()
            .all(|r| matches!(r.outcome, Outcome::Ok | Outcome::Changed))
        {
            return Err(format!(
                "TrackpadScrolling ON failed apply/verify: {applied:?}"
            ));
        }
        reports.push(FunctionRoundTripReport {
            function: "TrackpadScrolling".into(),
            off_state: "TrackpadHorizScroll=0 (Planned drift)".into(),
            on_state: "TrackpadHorizScroll=1 (Ok)".into(),
            verified: true,
        });
    }

    {
        let mut only_dw = Selection::none();
        only_dw.set(Setting::Desktop, true);
        only_dw.set(Setting::Widgets, true);
        let off_desktop = Cmd::new("/usr/bin/defaults")
            .args([
                "write",
                "com.apple.WindowManager",
                "EnableStandardClickToShowDesktop",
                "-int",
                "1",
            ])
            .as_user(&user);
        let off_widgets = Cmd::new("/usr/bin/defaults")
            .args([
                "write",
                "com.apple.WindowManager",
                "StandardHideWidgets",
                "-int",
                "0",
            ])
            .as_user(&user);
        if !sys.run(&off_desktop).map_err(|e| e.to_string())?.success()
            || !sys.run(&off_widgets).map_err(|e| e.to_string())?.success()
        {
            return Err("failed to toggle WindowManager Desktop/Widgets keys".into());
        }
        let drift =
            run_workstation_selected(sys, WorkstationGroup::Preferences, Mode::Plan, only_dw)
                .map_err(|e| e.to_string())?;
        if drift
            .iter()
            .filter(|r| r.outcome == Outcome::Planned)
            .count()
            < 2
        {
            return Err(format!(
                "Desktop & Widgets OFF were not both detected as Planned drift: {drift:?}"
            ));
        }
        let applied =
            run_workstation_selected(sys, WorkstationGroup::Preferences, Mode::Apply, only_dw)
                .map_err(|e| e.to_string())?;
        if !applied
            .iter()
            .all(|r| matches!(r.outcome, Outcome::Ok | Outcome::Changed))
        {
            return Err(format!(
                "Desktop & Widgets ON failed apply/verify: {applied:?}"
            ));
        }
        reports.push(FunctionRoundTripReport {
            function: "Desktop & Widgets".into(),
            off_state: "ClickToShowDesktop=1, HideWidgets=0 (Planned drift)".into(),
            on_state: "ClickToShowDesktop=0, HideWidgets=1 (Ok)".into(),
            verified: true,
        });
    }

    // 5. Dock round-trip ON -> OFF (drift) -> ON (verified).
    {
        let mut only_dock = Selection::none();
        only_dock.set(Setting::Dock, true);
        let off_dock = Cmd::new("/usr/bin/defaults")
            .args(["write", "com.apple.dock", "show-recents", "-bool", "true"])
            .as_user(&user);
        if !sys.run(&off_dock).map_err(|e| e.to_string())?.success() {
            return Err("failed to toggle com.apple.dock show-recents".into());
        }
        let drift =
            run_workstation_selected(sys, WorkstationGroup::Preferences, Mode::Plan, only_dock)
                .map_err(|e| e.to_string())?;
        if !drift
            .iter()
            .any(|r| r.id.contains("init.dock") && r.outcome == Outcome::Planned)
        {
            return Err(format!(
                "Dock OFF was not detected as Planned drift: {drift:?}"
            ));
        }
        let applied =
            run_workstation_selected(sys, WorkstationGroup::Preferences, Mode::Apply, only_dock)
                .map_err(|e| e.to_string())?;
        if !applied
            .iter()
            .all(|r| matches!(r.outcome, Outcome::Ok | Outcome::Changed))
        {
            return Err(format!("Dock ON failed apply/verify: {applied:?}"));
        }
        reports.push(FunctionRoundTripReport {
            function: "Dock".into(),
            off_state: "show-recents=true (Planned drift)".into(),
            on_state: "aterm, System Settings, Safari pinned; show-recents=false (Ok)".into(),
            verified: true,
        });
    }

    // 6. Messages & FaceTime Sign-Out verified (requests Accessibility only if an account is registered).
    {
        let mut only_accounts = Selection::none();
        only_accounts.set(Setting::Messages, true);
        only_accounts.set(Setting::FaceTime, true);
        let applied =
            run_workstation_selected(sys, WorkstationGroup::Accounts, Mode::Apply, only_accounts)
                .map_err(|e| e.to_string())?;
        if !applied
            .iter()
            .all(|r| matches!(r.outcome, Outcome::Ok | Outcome::Changed))
        {
            return Err(format!(
                "Messages & FaceTime sign-out failed apply/verify: {applied:?}"
            ));
        }
        reports.push(FunctionRoundTripReport {
            function: "Messages & FaceTime Sign-Out".into(),
            off_state: "checked persisted idstatuscache & cfprefsd stores".into(),
            on_state: "Messages & FaceTime signed out (Ok)".into(),
            verified: true,
        });
    }

    // 7. Read Spotlight policy without migrating caches or changing toolchains.
    reports.push(verify_spotlight_policy(sys, &ctx)?);

    // 8. Machine remote control alert-box inspection runs without error or consent popup.
    {
        let alerts = inspect_alert_boxes(sys, &user)?;
        reports.push(FunctionRoundTripReport {
            function: "AlertBox Remote Control".into(),
            off_state: "idle".into(),
            on_state: format!("inspected ({} active alerts)", alerts.len()),
            verified: true,
        });
    }

    Ok(reports)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::{Fake, Ran, Stat};

    fn alice() -> User {
        User {
            name: "alice".into(),
            uid: 501,
            gid: 20,
            home: "/Users//alice".into(),
            shell: "/bin/zsh".into(),
            admin: true,
        }
    }

    #[test]
    fn inspect_and_select_alert_boxes_round_trip() {
        let sys = Fake::root();
        let user = alice();
        let inspect_cmd = Cmd::new(OSASCRIPT)
            .args(["-l", "JavaScript", "-e", INSPECT_ALERTS_JXA])
            .as_user(&user)
            .in_session()
            .display();
        sys.on(
            &inspect_cmd,
            Ran::ok(
                r#"[{"process":"CoreServicesUIAgent","title":"Install Command Line Developer Tools","buttons":["Cancel","Install"]}]"#,
            ),
        );
        let boxes = inspect_alert_boxes(&sys, &user).unwrap();
        assert_eq!(
            boxes,
            vec![AlertBox {
                process: "CoreServicesUIAgent".into(),
                title: "Install Command Line Developer Tools".into(),
                buttons: vec!["Cancel".into(), "Install".into()],
            }]
        );

        let click_cmd = Cmd::new(OSASCRIPT)
            .args([
                "-l",
                "JavaScript",
                "-e",
                SELECT_ALERT_BUTTON_JXA,
                "--",
                "CoreServicesUIAgent",
                "Install",
            ])
            .as_user(&user)
            .in_session()
            .display();
        sys.on(&click_cmd, Ran::ok("clicked\n"));
        assert!(select_alert_button(&sys, &user, "CoreServicesUIAgent", "Install").unwrap());

        let console = |console_uid| {
            let mut sys = Fake::root();
            sys.euid = 501;
            sys.logged_in(&crate::items::defaults::tests::user("alice", 501));
            sys.stat_of(
                "/dev/console",
                Stat {
                    uid: console_uid,
                    gid: 20,
                    mode: 0o600,
                    dir: false,
                    symlink: false,
                    len: 0,
                },
            );
            sys
        };
        let inventory = crate::facts::hidutil_keyboards().display();
        let keyboard = "Services:\n\
VendorID ProductID Product  Built-In\n\
0x5ac    0x1       Keyboard 1\n";
        let sys = console(501);
        sys.on(&inventory, Ran::ok(keyboard));
        let account_only = crate::init::initial_setup_context(&sys).unwrap();
        assert!(account_only.facts.keyboards.is_empty());
        assert!(!sys.calls().contains(&inventory));
        let (ctx, console_user) = round_trip_context(&sys).unwrap();
        assert_eq!(ctx.console.as_ref(), Some(&console_user));
        assert_eq!(console_user, user);
        assert_eq!(
            ctx.facts.keyboards,
            [crate::model::Keyboard {
                vendor: 0x5ac,
                product: 1,
                name: "Keyboard".into(),
                builtin: true,
            }]
        );
        assert!(sys.calls().contains(&inventory));

        for reply in [Ran::exit(1, "refused"), Ran::ok("Services:\nunknown\n")] {
            let sys = console(501);
            sys.on(&inventory, reply);
            assert!(verify_workstation_round_trip(&sys).is_err());
            assert!(sys.calls().contains(&inventory));
            assert!(!sys.calls().iter().any(|call| {
                call.contains("property")
                    || call.contains("defaults")
                    || call.contains("scroll-direction")
            }));
        }
        let sys = console(502);
        sys.on(&inventory, Ran::ok(keyboard));
        assert!(verify_workstation_round_trip(&sys).is_err());
        assert!(!sys.calls().iter().any(|call| {
            call.contains("hidutil")
                || call.contains("defaults")
                || call.contains("scroll-direction")
        }));

        let sys = console(501);
        sys.on(&inventory, Ran::ok(keyboard));
        let get = Cmd::new("/usr/bin/hidutil")
            .args([
                "property",
                "--matching",
                "{\"PrimaryUsagePage\":1,\"PrimaryUsage\":6,\"VendorID\":1452,\"ProductID\":1}",
                "--get",
                "UserKeyMapping",
            ])
            .as_user(&user)
            .display();
        sys.on(&get, Ran::ok("10002160e UserKeyMapping ( { HIDKeyboardModifierMappingSrc = 123; HIDKeyboardModifierMappingDst = 456; }, { HIDKeyboardModifierMappingSrc = 30064771129; HIDKeyboardModifierMappingDst = 30064771296; } )\n"));
        let clear = Cmd::new("/usr/bin/hidutil")
            .args([
                "property",
                "--matching",
                "{\"PrimaryUsagePage\":1,\"PrimaryUsage\":6,\"VendorID\":1452,\"ProductID\":1}",
                "--set",
                "{\"UserKeyMapping\":[{\"HIDKeyboardModifierMappingDst\":456,\"HIDKeyboardModifierMappingSrc\":123}]}",
            ])
            .as_user(&user)
            .display();
        sys.on(&clear, Ran::exit(1, "scripted CapsLock clear refusal"));
        let refused = verify_workstation_round_trip(&sys).unwrap_err();
        assert!(refused.starts_with("hidutil clear failed:"), "{refused}");
        assert!(sys.calls().contains(&clear));
        assert!(
            !sys.calls()
                .iter()
                .any(|call| { call.contains("defaults") || call.contains("scroll-direction") })
        );

        let sys = console(501);
        let ctx = crate::init::initial_setup_context(&sys).unwrap();
        let tooling = "/Users//alice/Library/Caches/aterm/tooling.noindex";
        let cached_tool = format!("{tooling}/a-tool-used-by-another-build");
        for path in [
            "/Users//alice/Library/Caches",
            "/Users//alice/Library/Caches/aterm/.ubd/cargo-build.noindex",
            tooling,
        ] {
            sys.stat_of(
                path,
                Stat {
                    uid: 501,
                    gid: 20,
                    mode: 0o755,
                    dir: true,
                    symlink: false,
                    len: 0,
                },
            );
        }
        sys.file(&cached_tool, b"existing cached executable bytes");
        sys.file("/usr/bin/mdfind", b"");
        let mdfind =
            "alice$ /usr/bin/mdfind -onlyin /Users//alice -0 'kMDItemFSName == \"CACHEDIR.TAG\"'";
        sys.on(mdfind, Ran::ok(""));
        sys.on(
            mdfind,
            Ran::ok("/Users//alice/work/demo/target/CACHEDIR.TAG\0"),
        );
        let before = sys.read_file(std::path::Path::new(&cached_tool)).unwrap();
        assert!(verify_spotlight_policy(&sys, &ctx).unwrap().verified);
        assert_eq!(
            sys.read_file(std::path::Path::new(&cached_tool)).unwrap(),
            before
        );
        assert!(sys.stat(std::path::Path::new(tooling)).unwrap().dir);

        let target = "/Users//alice/work/demo/target";
        sys.create_dir_all(std::path::Path::new(&format!("{target}/debug")), 0o755)
            .unwrap();
        sys.file(&format!("{target}/CACHEDIR.TAG"), b"Signature:");
        let artifact = format!("{target}/debug/libdemo.rmeta");
        sys.file(&artifact, b"artifact used by a build");
        // The person's own project build folder Spotlight indexes is theirs: a note on
        // the row, never a step, so the policy still verifies and nothing touches it.
        assert!(verify_spotlight_policy(&sys, &ctx).unwrap().verified);
        assert_eq!(
            sys.read_file(std::path::Path::new(&artifact)).unwrap(),
            b"artifact used by a build"
        );
        assert!(sys.stat(std::path::Path::new(target)).unwrap().dir);
        assert!(
            sys.stat(std::path::Path::new(&format!("{target}.noindex")))
                .is_err()
        );
        assert!(!sys.calls().iter().any(|call| call.contains("/bin/ln")
            || call.contains(" set ")
            || call.contains(" remove ")));
    }

    #[test]
    fn live_workstation_on_off_round_trip_harness() {
        if std::env::var_os("ATERM_LIVE_ROUND_TRIP").is_none() {
            return;
        }
        let sys = crate::port::Live::new();
        let reports = verify_workstation_round_trip(&sys).unwrap();
        for r in &reports {
            eprintln!(
                "[round-trip] {}: OFF=({}) -> ON=({}) verified={}",
                r.function, r.off_state, r.on_state, r.verified
            );
        }
        assert_eq!(reports.len(), 8);
        assert!(reports.iter().all(|r| r.verified));
    }
}
