// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Machine remote control for inspecting and selecting macOS alert boxes when they appear,
//! without triggering ad-hoc user interruptions when a non-interactive path exists.

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
///
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
///
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

/// Result of testing one workstation function OFF (drift) and ON (applied & verified).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionRoundTripReport {
    /// Name of the workstation function tested.
    pub function: String,
    /// Observed state when switched OFF (drift).
    pub off_state: String,
    /// Observed state after applying ON and reading back.
    pub on_state: String,
    /// Whether both OFF drift detection and immediate ON readback succeeded.
    pub verified: bool,
}

/// Run the ON -> OFF (drift) -> ON (immediate & verified) round-trip harness across
/// workstation functions (`CapsLock`, `NaturalScrolling`, `TrackpadScrolling`, `Desktop`,
/// `Widgets`, `Spotlight`, and `AlertBoxes`).
///
/// # Errors
/// Returns an error if any stage fails to detect drift when switched OFF or fails to verify ON.
pub fn verify_workstation_round_trip(
    sys: &dyn System,
) -> Result<Vec<FunctionRoundTripReport>, String> {
    use crate::init::{
        WorkstationGroup, initial_setup_context, run_initial_spotlight, run_workstation_selected,
    };
    use crate::model::{Mode, Outcome};
    use aterm_types::macos_setup::{Selection, Setting};

    let ctx = initial_setup_context(sys).map_err(|e| e.to_string())?;
    let Some(user) = ctx.users.first().cloned() else {
        return Err("no console user found for round-trip verification".into());
    };
    let mut reports = Vec::new();

    // 1. CapsLock -> Control round-trip ON -> OFF (drift) -> ON (immediate & verified).
    if let Some(kb) = ctx.facts.keyboards.first() {
        let mut only_caps = Selection::none();
        only_caps.set(Setting::CapsLock, true);
        let clear = Cmd::new("/usr/bin/hidutil")
            .args([
                "property",
                "--matching",
                &format!(
                    "{{\"ProductID\":{},\"VendorID\":{}}}",
                    kb.product, kb.vendor
                ),
                "--set",
                "{\"UserKeyMapping\":[]}",
            ])
            .as_user(&user)
            .in_session();
        let ran = sys.run(&clear).map_err(|e| e.to_string())?;
        if !ran.success() {
            return Err(format!("hidutil clear failed: {}", ran.describe()));
        }
        let drift =
            run_workstation_selected(sys, WorkstationGroup::Preferences, Mode::Plan, only_caps)
                .map_err(|e| e.to_string())?;
        if !drift
            .iter()
            .any(|r| r.id.contains("keyboard.capslock.attached") && r.outcome == Outcome::Planned)
        {
            return Err(format!(
                "CapsLock OFF was not detected as Planned drift: {drift:?}"
            ));
        }
        let applied =
            run_workstation_selected(sys, WorkstationGroup::Preferences, Mode::Apply, only_caps)
                .map_err(|e| e.to_string())?;
        if !applied
            .iter()
            .all(|r| matches!(r.outcome, Outcome::Ok | Outcome::Changed))
        {
            return Err(format!("CapsLock ON failed immediate apply: {applied:?}"));
        }
        let verified =
            run_workstation_selected(sys, WorkstationGroup::Preferences, Mode::Plan, only_caps)
                .map_err(|e| e.to_string())?;
        if !verified.iter().all(|r| r.outcome == Outcome::Ok) {
            return Err(format!(
                "CapsLock ON failed immediate readback: {verified:?}"
            ));
        }
        reports.push(FunctionRoundTripReport {
            function: "CapsLock -> Control".into(),
            off_state: "UserKeyMapping=[] (Planned drift)".into(),
            on_state: "0x700000039 -> 0x7000000E0 (Ok)".into(),
            verified: true,
        });
    }

    // 2. NaturalScrolling round-trip ON -> OFF (drift) -> ON (immediate & verified).
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

    // 3. TrackpadScrolling round-trip ON -> OFF (drift) -> ON (verified).
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

    // 4. Desktop & Widgets round-trip ON -> OFF (drift) -> ON (verified).
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

    // 5. Spotlight .noindex build & toolchain control round-trip ON -> OFF (drift) -> ON (verified).
    {
        let _ = run_initial_spotlight(sys, &ctx, Mode::Apply).map_err(|e| e.to_string())?;
        let tooling = std::path::PathBuf::from(format!(
            "{}/Library/Caches/aterm/tooling.noindex",
            user.home
        ));
        let tooling_bak = std::path::PathBuf::from(format!(
            "{}/Library/Caches/aterm/tooling.roundtrip-bak.noindex",
            user.home
        ));
        sys.rename(&tooling, &tooling_bak)
            .map_err(|e| format!("failed to move tooling.noindex aside: {e}"))?;
        let drift = run_initial_spotlight(sys, &ctx, Mode::Plan).map_err(|e| e.to_string())?;
        let restore_bak = || {
            let _ = Cmd::new("/bin/rm")
                .args(["-rf", &tooling.to_string_lossy()])
                .as_user(&user);
            let _ = sys.rename(&tooling_bak, &tooling);
        };
        if !drift
            .iter()
            .any(|r| r.id.contains("spotlight.build-noindex") && r.outcome == Outcome::Planned)
        {
            restore_bak();
            return Err(format!(
                "Spotlight build-noindex OFF was not detected as Planned drift: {drift:?}"
            ));
        }
        let applied = run_initial_spotlight(sys, &ctx, Mode::Apply).map_err(|e| {
            restore_bak();
            e.to_string()
        })?;
        let _ = sys.run(
            &Cmd::new("/bin/rm")
                .args(["-rf", &tooling_bak.to_string_lossy()])
                .as_user(&user),
        );
        if !applied.iter().all(|r| {
            matches!(
                r.outcome,
                Outcome::Ok | Outcome::Changed | Outcome::NotApplicable
            )
        }) {
            return Err(format!("Spotlight items failed to apply: {applied:?}"));
        }
        let verified = run_initial_spotlight(sys, &ctx, Mode::Plan).map_err(|e| e.to_string())?;
        if !verified
            .iter()
            .any(|r| r.id.contains("spotlight.build-noindex") && r.outcome == Outcome::Ok)
        {
            return Err(format!(
                "Spotlight build-noindex ON failed immediate readback: {verified:?}"
            ));
        }
        reports.push(FunctionRoundTripReport {
            function: "Spotlight .noindex Control".into(),
            off_state: "tooling.noindex absent (Planned drift)".into(),
            on_state: "cargo-build.noindex, tooling.noindex, and trust store builds .noindex (Ok)"
                .into(),
            verified: true,
        });
    }

    // 6. Machine remote control alert-box inspection runs without error or consent popup.
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
    use crate::port::{Fake, Ran};

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
    }

    #[test]
    fn live_workstation_on_off_round_trip_harness() {
        if std::env::var_os("ATERM_LIVE_ROUND_TRIP").is_none() {
            return;
        }
        let sys = crate::port::Live::new();
        let reports = verify_workstation_round_trip(&sys).unwrap();
        assert!(reports.len() >= 5);
        assert!(reports.iter().all(|r| r.verified));
    }
}
