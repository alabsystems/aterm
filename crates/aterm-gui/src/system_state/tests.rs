// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::mpsc;
use std::time::Duration;

use super::*;
use crate::native_app::{
    ActionInvocation, AppViewState, NativeApp, NativeRuntime, ViewId, ViewMotionCx,
};
use crate::native_ui::{CompiledUi, LogicalRect};
use aterm_types::macos_setup::{PutBack, RestoreOutcome};

fn runtime_with_app(
    app: SystemStateApp,
) -> (NativeRuntime, crate::native_app::AppInstanceId, ViewId) {
    let mut runtime = NativeRuntime::new();
    let instance = runtime
        .insert_instance(NativeApp::SystemState(Box::new(app)))
        .unwrap();
    let view = ViewId::from_stored(1);
    runtime
        .attach_view(view, instance, AppViewState::SystemState(Box::default()))
        .unwrap();
    (runtime, instance, view)
}

fn cx(viewport: LogicalRect, serious: bool) -> ViewCx<'static> {
    ViewCx {
        viewport,
        animation_phase_ms: 0,
        motion: ViewMotionCx {
            serious,
            ..ViewMotionCx::default()
        },
        terminal_font_px: 13.0,
        terminal_scale: 2.0,
        semantic_font: None,
        document: None,
    }
}

fn compile(
    app: &SystemStateApp,
    view: &SystemStateViewState,
    width: f32,
    height: f32,
) -> CompiledUi {
    let viewport = LogicalRect::new(0.0, 0.0, width, height);
    app.view(view, &cx(viewport, false))
        .compile(viewport)
        .expect("the page compiles")
}

fn act(app: &mut SystemStateApp, key: &str) {
    let mut view = SystemStateViewState::default();
    app.act(&mut view, key, None);
}

fn status_of(scenario: &str) -> SetupStatus {
    fixture_status(scenario).expect("a named scenario")
}

/// The `mixed` scenario once its Debugger access is on: one managed row (Rust in Spotlight)
/// still differs, which a run changes.
fn to_change() -> SetupStatus {
    let mut status = status_of("mixed");
    let debugger = Subject::Access(Access::Debugger);
    if let Some(reading) = status.observed.iter_mut().find(|o| o.subject == debugger) {
        reading.standing = Standing::InPlace;
        reading.now = Some("On".to_owned());
        reading.detail = None;
    }
    status
}

/// The headline's words: the page heading's subtitle line.
const HEADLINE: &str = "settings/page-subtitle/system-state";

fn label(compiled: &CompiledUi, key: &str) -> String {
    compiled
        .semantic(&UiKey::new(key))
        .unwrap_or_else(|| panic!("{key} is in the tree"))
        .label
        .clone()
}

fn reading(subject: Subject, standing: Standing, now: Option<&str>) -> Observation {
    Observation {
        subject,
        standing,
        now: now.map(str::to_owned),
        after_login: None,
        wanted: subject.wanted().to_owned(),
        note: None,
        detail: None,
    }
}

fn with_readings(selection: Selection, observed: Vec<Observation>) -> SetupStatus {
    SetupStatus {
        observed_vault_url: Some(Plan::default().vault_url),
        plan: Some(Plan::from(selection)),
        observed,
        ..SetupStatus::default()
    }
}

#[test]
fn rows_show_now_and_wanted_and_at_most_one_chip() {
    let natural = Subject::Setting(Setting::NaturalScrolling);
    let status = with_readings(
        Selection::all(),
        vec![reading(natural, Standing::Differs, Some("On"))],
    );
    let row = row_view(natural, &status, Selection::all());
    assert_eq!(
        row.value,
        RowValue::Change {
            now: "On".into(),
            wanted: "Off".into()
        }
    );
    assert_eq!(row.chip, Some(Chip::WillChange { busy: false }));
    assert_eq!(row.label, "Natural scrolling: On, will change to Off");

    // Left as is: the reading in muted ink, and the chip column says so.
    let mut left = Selection::all();
    for setting in Group::Input.settings() {
        left.set(*setting, false);
    }
    let row = row_view(natural, &status, left);
    assert_eq!(row.value, RowValue::Muted("On".into()));
    assert_eq!(row.chip, Some(Chip::LeftAsIs));
    assert_eq!(row.label, "Natural scrolling: On, left as is");
    assert!(!row.managed);

    // In place: one value and the check; a login-time change reads in place, and its note
    // says when it is all in effect.
    let trackpad = Subject::Setting(Setting::TrackpadScrolling);
    let status = with_readings(
        Selection::all(),
        vec![
            reading(natural, Standing::InPlace, Some("Off")),
            reading(trackpad, Standing::AtLogin, Some("All on")),
        ],
    );
    let row = row_view(natural, &status, Selection::all());
    assert_eq!(
        (row.value, row.chip),
        (RowValue::One("Off".into()), Some(Chip::InPlace))
    );
    assert_eq!(row.label, "Natural scrolling: Off, in place");
    let row = row_view(trackpad, &status, Selection::all());
    assert_eq!(
        (row.value, row.chip, row.note.as_deref(), row.label.as_str()),
        (
            RowValue::One("All on".into()),
            Some(Chip::InPlace),
            Some(AT_LOGIN_NOTE),
            "Trackpad scrolling: All on, in place"
        )
    );

    // A row read as wanted with no value read shows `—` beside the check,
    // never the wanted value as if it had been read.
    let status = with_readings(
        Selection::all(),
        vec![reading(
            Subject::Access(Access::LocalNetwork),
            Standing::InPlace,
            None,
        )],
    );
    let row = row_view(
        Subject::Access(Access::LocalNetwork),
        &status,
        Selection::all(),
    );
    assert_eq!(
        (row.value, row.chip, row.label.as_str()),
        (
            RowValue::One("—".into()),
            Some(Chip::InPlace),
            "Local Network: in place"
        )
    );

    // Never a guess: an unread row is `—` with no chip; a grant macOS does
    // not report reads `—` on its way to On.
    let row = row_view(Subject::Setting(Setting::Dock), &status, Selection::all());
    assert_eq!((row.value, row.chip), (RowValue::One("—".into()), None));
    let network = Subject::Access(Access::LocalNetwork);
    let status = with_readings(
        Selection::all(),
        vec![reading(network, Standing::Differs, None)],
    );
    assert_eq!(
        row_view(network, &status, Selection::all()).value,
        RowValue::Change {
            now: "—".into(),
            wanted: "On".into()
        }
    );
}

#[test]
fn a_row_waiting_on_a_person_carries_its_open_and_a_failure_its_reason() {
    let fda = Subject::Access(Access::FullDiskAccess);
    let repositories = Subject::Setting(Setting::Repositories);
    let mut failed = reading(repositories, Standing::Failed, Some("3 of 12"));
    failed.detail = Some("REASON17".into());
    let status = with_readings(
        Selection::all(),
        vec![reading(fda, Standing::NeedsYou, Some("Off")), failed],
    );
    let row = row_view(fda, &status, Selection::all());
    assert_eq!(row.chip, Some(Chip::NeedsYou));
    assert_eq!(row.open, Some(Access::FullDiskAccess));
    assert_eq!(row.label, "Full Disk Access: Off, needs you");
    let row = row_view(repositories, &status, Selection::all());
    assert_eq!(row.chip, Some(Chip::Failed));
    assert_eq!(row.note.as_deref(), Some("REASON17"));
    assert_eq!(row.open, None);
    assert_eq!(open_words(Access::Debugger).1, "Turn On…");
    assert_eq!(
        open_words(Access::FullDiskAccess).0,
        "Open Full Disk Access"
    );
}

/// A probe's reason (`Observation::detail`) shows under every row that needs a person, is not
/// needed or was opted out of, where a screen reader reads it too; a waiting row shows what it
/// waits on from its probe's note.
#[test]
fn a_rows_description_is_its_note_wherever_it_explains_the_row() {
    let note = |subject: Subject, observed: Observation, selection: Selection| {
        row_view(
            subject,
            &with_readings(selection, vec![observed]),
            selection,
        )
        .note
    };
    let described =
        |subject: Subject, standing: Standing, now: Option<&str>, why: &str| Observation {
            detail: Some(why.to_owned()),
            ..reading(subject, standing, now)
        };
    let debugger = Subject::Access(Access::Debugger);
    let sampling = "macOS asks for an administrator password in a dialog named SamplingTools.";
    let widgets = Subject::Setting(Setting::Widgets);
    let dock = Subject::Setting(Setting::Dock);
    let accessibility = Subject::Access(Access::Accessibility);
    let caps = Subject::Setting(Setting::CapsLock);
    let all = Selection::all();
    for (subject, observed, why) in [
        (
            debugger,
            described(debugger, Standing::NeedsYou, Some("Off"), sampling),
            sampling,
        ),
        (
            widgets,
            Observation {
                note: Some("NOTIFICATIONS".to_owned()),
                ..described(widgets, Standing::Waiting, Some("Shown"), "ENGINE")
            },
            "NOTIFICATIONS",
        ),
        (
            accessibility,
            described(accessibility, Standing::NotNeeded, None, "SIGN-OUTS"),
            "SIGN-OUTS",
        ),
    ] {
        assert_eq!(
            note(subject, observed, all).as_deref(),
            Some(why),
            "{subject:?}"
        );
    }
    // A grant the person opted out of reads in place at what they chose,
    // and says so.
    let opted = Observation {
        wanted: "Off".to_owned(),
        ..described(debugger, Standing::InPlace, Some("Off"), "OPTED")
    };
    assert_eq!(note(debugger, opted, all).as_deref(), Some("OPTED"));
    // A short note a probe gives (an approval code) comes first.
    let code = Observation {
        note: Some("CODE".to_owned()),
        ..described(
            Subject::Setting(Setting::Secrets),
            Standing::NeedsYou,
            Some("Awaiting code"),
            "DETAIL",
        )
    };
    assert_eq!(
        note(Subject::Setting(Setting::Secrets), code, all).as_deref(),
        Some("CODE")
    );
    // Negative controls: a row read as aterm wants it, a group left as is,
    // and a reading from while the row was still needed explain nothing.
    assert_eq!(
        note(
            caps,
            described(caps, Standing::InPlace, Some("Control"), "ENGINE"),
            all
        ),
        None
    );
    let mut left = all;
    for setting in Group::Desktop.settings() {
        left.set(*setting, false);
    }
    assert_eq!(
        note(
            dock,
            described(dock, Standing::Differs, Some("9 apps"), "PINNED"),
            left
        ),
        None
    );
    let mut no_sign_out = all;
    no_sign_out.set(Setting::Messages, false);
    no_sign_out.set(Setting::FaceTime, false);
    assert_eq!(
        note(
            accessibility,
            described(accessibility, Standing::NeedsYou, Some("Off"), "STALE"),
            no_sign_out
        ),
        None
    );

    // On the page: the Debugger row names the dialog macOS shows, and a
    // screen reader hears it.
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status_of("debugger"));
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 900.0);
    assert!(
        label(&compiled, "system-state/row/debugger/note").contains("SamplingTools"),
        "the Debugger row explains the SamplingTools dialog"
    );
}

/// The engine's descriptions (a planned row's `from → to` line, a blocked row's item or probe
/// rule) never show under a row that differs or waits (§3.3: the value names the part that
/// differs), busy or not: a row that differs shows no note, and one that waits shows its own note
/// or [`WAITING_NOTE`].
#[test]
fn a_row_that_will_change_shows_no_engine_line_under_it() {
    let engine_lines = [
        (
            Setting::TrackpadScrolling,
            Standing::Differs,
            Some("Some off"),
            "absent → true; false → true",
        ),
        (
            Setting::Dock,
            Standing::Differs,
            Some("7 apps"),
            "com.apple.dock persistent-apps, com.apple.dock show-recents → Dock pins: aterm",
        ),
        (
            Setting::CapsLock,
            Standing::Differs,
            Some("Caps Lock"),
            "Caps Lock is not Control on every attached service → Caps Lock acts as Control",
        ),
        (
            Setting::Messages,
            Standing::Differs,
            Some("Signed in"),
            "account remains registered (including disabled accounts) → signed out",
        ),
        (
            Setting::Repositories,
            Standing::Waiting,
            None,
            "waits for init.github.auth@person",
        ),
        (
            Setting::Repositories,
            Standing::Waiting,
            None,
            "GitHub token source must be an owned private regular file (0600), without links",
        ),
    ];
    for (setting, standing, now, line) in engine_lines {
        let subject = Subject::Setting(setting);
        let observed = Observation {
            detail: Some(line.to_owned()),
            ..reading(subject, standing, now)
        };
        let mut status = with_readings(Selection::all(), vec![observed]);
        for busy in [false, true] {
            status.running = if busy { vec![setting] } else { Vec::new() };
            let row = row_view(subject, &status, Selection::all());
            assert_eq!(row.chip, Some(Chip::WillChange { busy }), "{setting:?}");
            let waits = (standing == Standing::Waiting).then_some(WAITING_NOTE);
            assert_eq!(row.note.as_deref(), waits, "{setting:?} busy={busy}");
            assert!(!row.label.contains(line), "{setting:?}: {}", row.label);
        }
    }

    // On the page: the fresh Mac's scenario carries an engine line on every
    // row that will change, and no node a person sees or hears says any. The
    // notes are the repositories', in the words of what they wait for, and
    // Local Network's calm one: macOS tells apps nothing of it.
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status_of("fresh"));
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 900.0);
    let repositories = Subject::Setting(Setting::Repositories);
    let lan = Subject::Access(Access::LocalNetwork);
    for subject in Subject::all() {
        let key = format!("system-state/row/{}/note", row_slug(subject));
        let node = compiled.semantic(&UiKey::new(key.clone()));
        if subject == repositories {
            assert_eq!(
                node.map(|node| node.label.as_str()),
                Some("Waits for the vault to finish syncing this Mac"),
                "{key}"
            );
        } else if subject == lan {
            assert_eq!(
                node.map(|node| node.label.as_str()),
                Some(aterm_types::macos_setup::LOCAL_NETWORK_UNTOLD),
                "{key}"
            );
        } else {
            assert!(node.is_none(), "{key}");
        }
    }
    for node in &compiled.semantics {
        let words = format!(
            "{} {:?} {:?} {:?}",
            node.label, node.value, node.description, node.value_text
        );
        for engine in [
            "→ true",
            "com.apple",
            "waits for",
            "init.github",
            "→ signed out",
            "rust-docs",
            "0600",
            "regular file",
            "64 KiB",
        ] {
            assert!(!words.contains(engine), "{:?} says {engine:?}", node.key);
        }
    }
}

/// A waiting row always says why in a person's words (§3.3): its probe's note, or
/// [`WAITING_NOTE`] where it gave none or only blanks; never the engine's description, busy or
/// not, in any row or on the page.
#[test]
fn a_waiting_row_always_says_why_and_never_in_the_engines_words() {
    const ENGINE: &str = "ENGINE LINE: waits for init.example@person";
    let all = Selection::all();
    for subject in Subject::all() {
        for (note, says) in [
            (None, WAITING_NOTE),
            (Some("   "), WAITING_NOTE),
            (Some("REASON"), "REASON"),
        ] {
            let observed = Observation {
                note: note.map(str::to_owned),
                detail: Some(ENGINE.to_owned()),
                ..reading(subject, Standing::Waiting, None)
            };
            let mut status = with_readings(all, vec![observed]);
            for busy in [false, true] {
                status.running = if busy {
                    vec![subject.setting()]
                } else {
                    Vec::new()
                };
                let row = row_view(subject, &status, all);
                assert_eq!(row.note.as_deref(), Some(says), "{subject:?} {note:?}");
                assert!(!row.label.contains("ENGINE"), "{subject:?}: {}", row.label);
            }
            let mut app = SystemStateApp::new(Plan::from(all));
            app.publish(status);
            let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 4000.0);
            let key = format!("system-state/row/{}/note", row_slug(subject));
            assert_eq!(label(&compiled, &key), says, "{key}");
            for node in &compiled.semantics {
                let words = format!(
                    "{} {:?} {:?} {:?}",
                    node.label, node.value, node.description, node.value_text
                );
                assert!(
                    !words.contains("ENGINE"),
                    "{:?} says the engine's line",
                    node.key
                );
            }
        }
    }
}

/// An aterm run from a disk image cannot keep the vault in sync: the Secrets row needs the person
/// and says the move in its note, the headline names and shows the row, and the menu line carries
/// the note (§3.6). The run's own `blocked` row for the step is no note on the page.
#[test]
fn a_disk_image_vault_wait_is_said_on_the_row_the_headline_and_the_menu_line() {
    let secrets = Subject::Setting(Setting::Secrets);
    // As `macos_doctor`'s vault observation reads a member whose aterm runs
    // from a disk image.
    let move_it = "Move aterm to Applications and open it from there";
    let mut status = with_readings(
        Selection::all(),
        vec![Observation {
            note: Some(move_it.to_owned()),
            ..reading(secrets, Standing::NeedsYou, Some("Out of sync"))
        }],
    );
    status.rows = vec![SetupStatusRow {
        name: "Secrets vault".into(),
        status: "blocked".into(),
        detail: "this aterm runs from /Volumes/aterm/aterm.app/Contents/MacOS/aterm, which goes \
                 away when it quits, so it cannot keep ~/.secrets in sync; move aterm to \
                 Applications and open it from there, and setup finishes this step"
            .into(),
    }];
    let row = row_view(secrets, &status, Selection::all());
    assert_eq!(row.note.as_deref(), Some(move_it));
    assert_eq!(row.chip, Some(Chip::NeedsYou));
    let mut app = SystemStateApp::new(Plan::from(Selection::all()));
    app.publish(status.clone());
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 4000.0);
    assert_eq!(label(&compiled, "system-state/row/secrets/note"), move_it);
    assert_eq!(label(&compiled, HEADLINE), "Needs you: Secrets vault");
    let head = headline(
        &status,
        &rows(&status, Selection::all()),
        status.plan.as_ref(),
    );
    assert_eq!(head.show, Some(secrets));
    let tree = format!("{:?}", compiled.semantics);
    assert!(
        !tree.contains("/Volumes"),
        "the stage's own line is the run's, not the page's"
    );
    assert!(app.needs_attention());
    let line = crate::system_state_access::menu_line(&status, false).expect("a menu line");
    assert_eq!(line.label, "System State: Secrets vault needs you");
    assert_eq!(line.note.as_deref(), Some(move_it));
}

#[test]
fn a_restore_failure_is_visible_even_when_its_group_is_left_as_is() {
    let status = status_of("restore-blocked");
    let detail = status.rows[0].detail.clone();
    let mut app = SystemStateApp::new(status.plan.clone().unwrap());
    app.publish(status.clone());
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 4000.0);
    let text = format!("{:?}", compiled.semantics);
    assert!(
        text.contains(&detail),
        "the operation's unresolved result stays visible"
    );
    assert!(app.needs_attention());
    let selection = status.plan.as_ref().unwrap().selection;
    assert!(!selection.contains(Setting::Widgets));
    assert_eq!(
        status
            .observed
            .iter()
            .find(|o| o.subject == Subject::Setting(Setting::Widgets))
            .unwrap()
            .standing,
        Standing::InPlace
    );
    // The failure names Restore and its row, never setup in general (§3.4): the headline
    // shows the row, the note says why, and the menu line says the same.
    let plan = status.plan.clone().unwrap();
    let head = headline(&status, &rows(&status, plan.selection), Some(&plan));
    assert_eq!(
        (head.text.as_str(), head.show),
        (
            "Restore previous settings failed: Widgets",
            Some(Subject::Setting(Setting::Widgets))
        )
    );
    assert_eq!(
        status.notes(&plan),
        [
            "Natural scrolling and Dock are as they were. Caps Lock key and Trackpad scrolling \
             come back at your next login. Desktop stayed as it is: macOS hid its earlier value.",
            "Widgets could not be put back as they were.",
        ]
    );
    assert_eq!(
        crate::system_state_access::menu_line(&status, false).map(|line| line.label),
        Some("System State: Restore failed for Widgets".to_owned())
    );
}

/// Right after a Restore of the whole profile (§3.4): the page says what it did in one calm line,
/// in its own row names, with the settings that load at the next login folded into it; the groups
/// it put back are left as is, so nothing is a change to make; the Caps Lock key and the scroll
/// direction read their live values until that login, and Trackpad scrolling its saved one; and
/// nothing failed: no menu line, no attention mark, no preference key anywhere.
#[test]
fn a_restore_is_one_calm_line_in_row_names_and_no_failure() {
    let status = status_of("restore-at-login");
    let plan = status.plan.clone().unwrap();
    const LINE: &str = "Restore previous settings finished. Desktop, Widgets and Dock are as \
                        they were. Caps Lock key, Natural scrolling and Trackpad scrolling come \
                        back at your next login.";
    assert_eq!(status.notes(&plan), [LINE]);
    let mut app = SystemStateApp::new(plan.clone());
    app.publish(status.clone());
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 4000.0);
    assert_eq!(label(&compiled, "system-state/note/0"), LINE);
    assert!(
        compiled
            .semantics
            .iter()
            .all(|node| !node.label.contains("failed") && !node.label.contains("com.apple")),
        "no failure framing and no preference key"
    );
    assert!(!app.needs_attention());
    assert_eq!(status.failed_steps(Some(&plan)).count(), 0);
    assert!(crate::system_state_access::menu_line(&status, false).is_none());
    // The groups Restore put back are left as is, and so are no change to make.
    for group in [Group::Input, Group::Desktop] {
        for setting in group.settings() {
            assert!(!plan.selection.contains(*setting), "{setting:?}");
        }
    }
    let all = rows(&status, plan.selection);
    let line = headline(&status, &all, Some(&plan)).text;
    assert!(!line.contains("change"), "{line}");
    // A value live only until the next login reads as itself, marked; a screen reader hears it.
    let caps = row_view(Subject::Setting(Setting::CapsLock), &status, plan.selection);
    assert_eq!(
        (caps.value, caps.chip, caps.label.as_str()),
        (
            RowValue::Muted("Control · until login".into()),
            Some(Chip::LeftAsIs),
            "Caps Lock key: Control until login, left as is"
        )
    );
    let natural = row_view(
        Subject::Setting(Setting::NaturalScrolling),
        &status,
        plan.selection,
    );
    assert_eq!(natural.value, RowValue::Muted("Off · until login".into()));
    let trackpad = row_view(
        Subject::Setting(Setting::TrackpadScrolling),
        &status,
        plan.selection,
    );
    assert_eq!(trackpad.value, RowValue::Muted("Some off".into()));
    // One setting back at the next login alone is named alone.
    let one = SetupStatus {
        rows: restore_rows(&RestoreOutcome::of([(
            Some(Setting::CapsLock),
            PutBack::AtLogin,
        )])),
        ..status
    };
    assert_eq!(
        one.notes(&plan),
        ["Restore previous settings finished. Caps Lock key comes back at your next login."]
    );
}

/// A Restore's rows name System State's rows, never a preference key: one calm line for what came
/// back, what comes back at the next login, what stayed as it is because macOS hid its earlier
/// value, and what is put back once aterm has Full Disk Access (nobody can act on any of those);
/// and a failure, named as Restore's and its row's, for each setting it could not put back, after
/// which the line no longer says Restore finished.
#[test]
fn a_restore_names_its_rows_and_each_setting_it_could_not_put_back() {
    let of = |rows: &[(Option<Setting>, PutBack)]| restore_rows(&RestoreOutcome::of(rows.to_vec()));
    let words = |rows: Vec<SetupStatusRow>| -> Vec<(String, String, String)> {
        rows.into_iter()
            .map(|row| (row.name, row.status, row.detail))
            .collect()
    };
    let row = |name: &str, status: &str, detail: &str| {
        (name.to_owned(), status.to_owned(), detail.to_owned())
    };
    let calm = |detail: &str| row("Restore previous settings", "restored", detail);
    assert_eq!(
        words(of(&[(Some(Setting::Dock), PutBack::Back)])),
        [calm(
            "Restore previous settings finished. Dock is as it was."
        )]
    );
    assert_eq!(
        words(of(&[(Some(Setting::Widgets), PutBack::Back)])),
        [calm(
            "Restore previous settings finished. Widgets are as they were."
        )]
    );
    assert_eq!(
        words(of(&[])),
        [calm(
            "Restore previous settings finished. There was nothing to put back."
        )]
    );
    assert_eq!(
        words(of(&[
            (Some(Setting::Widgets), PutBack::Unknown),
            (Some(Setting::Dock), PutBack::Waits),
        ])),
        [calm(
            "Restore previous settings finished. Widgets stayed as they are: macOS hid their \
             earlier value. Dock is put back once aterm has Full Disk Access."
        )]
    );
    assert_eq!(
        words(of(&[
            (Some(Setting::Desktop), PutBack::Unknown),
            (Some(Setting::Widgets), PutBack::Unknown),
        ])),
        [calm(
            "Restore previous settings finished. Desktop and Widgets stayed as they are: macOS \
             hid their earlier values."
        )]
    );
    // Each key of a row counts once, and its worst outcome speaks for it.
    assert_eq!(
        words(of(&[
            (Some(Setting::Desktop), PutBack::Back),
            (Some(Setting::Desktop), PutBack::Failed),
            (Some(Setting::Dock), PutBack::Back),
            (None, PutBack::Failed),
        ])),
        [
            calm("Dock is as it was."),
            row(
                "Restore: Desktop",
                "failed",
                "Desktop could not be put back as it was."
            ),
            row(
                "Restore: another Mac setting",
                "failed",
                "Another Mac setting could not be put back as it was."
            ),
        ]
    );
    assert_eq!(
        words(of(&[(Some(Setting::Widgets), PutBack::Failed)])),
        [row(
            "Restore: Widgets",
            "failed",
            "Widgets could not be put back as they were."
        )]
    );
    let mut refused = RestoreOutcome::of([(Some(Setting::Dock), PutBack::Back)]);
    refused.journal_refused = true;
    assert_eq!(
        words(restore_rows(&refused))[1],
        row("Change log", "noted", JOURNAL_NOTE)
    );
}

/// What a Restore could not change is no failure (§3.4): a value macOS hid stays as it is, and
/// one in another app's container is put back once aterm has Full Disk Access. Each is part of
/// the calm line, and nothing says setup needs attention: no headline, no menu line, no mark.
#[test]
fn a_value_restore_could_not_reach_is_part_of_the_calm_line_and_no_failure() {
    let mut status = status_of("restore-at-login");
    let plan = status.plan.clone().unwrap();
    status.rows = restore_rows(&RestoreOutcome::of([
        (Some(Setting::Dock), PutBack::Back),
        (Some(Setting::Widgets), PutBack::Unknown),
        (Some(Setting::Desktop), PutBack::Waits),
    ]));
    assert_eq!(
        status.notes(&plan),
        [
            "Restore previous settings finished. Dock is as it was. Widgets stayed as they are: \
             macOS hid their earlier value. Desktop is put back once aterm has Full Disk Access."
        ]
    );
    let line = headline(&status, &rows(&status, plan.selection), Some(&plan)).text;
    assert!(
        !line.contains("fail") && line != "Setup needs attention",
        "{line}"
    );
    assert_eq!(status.failed_steps(Some(&plan)).count(), 0);
    assert!(crate::system_state_access::menu_line(&status, false).is_none());
    let mut app = SystemStateApp::new(plan);
    app.publish(status);
    assert!(!app.needs_attention());
}

/// A setup pass says what its readings cannot in row names (§3.6): a change that did not take
/// is its row's failure, which the headline names and shows; one it could not confirm is said
/// calmly; and what its rollback put back is one calm line in a Restore's words, never a
/// preference key.
#[test]
fn a_pass_says_each_unfinished_row_and_its_rollback_in_row_names() {
    let mut rolled = RestoreOutcome::of([
        (Some(Setting::Dock), PutBack::Back),
        (Some(Setting::Widgets), PutBack::Failed),
        (Some(Setting::NaturalScrolling), PutBack::AtLogin),
    ]);
    rolled.journal_refused = true;
    let outcome = PassOutcome {
        rolled_back: Some(rolled),
        unfinished: vec![
            (Setting::Widgets, Unfinished::Failed),
            (Setting::Dock, Unfinished::Failed),
            (Setting::Spotlight, Unfinished::Unconfirmed),
        ],
    };
    let mut status = status_of("set-up");
    status.rows = pass_rows(&outcome);
    let plan = status.plan.clone().unwrap();
    assert_eq!(
        status.notes(&plan),
        [
            "Widgets failed. The change did not take, and aterm could not put back its value \
             from before.",
            "Dock failed. The change did not take.",
            "Rust in Spotlight could not be confirmed.",
            "aterm put back what it could not finish. Dock is as it was. Natural scrolling comes \
             back at your next login.",
            JOURNAL_NOTE,
        ]
    );
    let head = headline(&status, &rows(&status, plan.selection), Some(&plan));
    assert_eq!(
        (head.text.as_str(), head.show),
        (
            "2 steps failed: Widgets, Dock",
            Some(Subject::Setting(Setting::Widgets))
        )
    );
    assert_eq!(
        crate::system_state_access::menu_line(&status, false).map(|line| line.label),
        Some("System State: 2 failed".to_owned()),
        "the change log's refusal is no failure"
    );
}

/// A row the run set for the next login reads in place, with a note saying when it is all in
/// effect; a value live only until that login is marked so only on a row that is not changing.
/// A row that will change shows what the Mac reads after the login as its from-side: one row
/// never reads "until login" and then "at login", nor `Control → Control`.
#[test]
fn a_row_marks_until_login_only_while_it_does_not_change_and_at_login_in_its_note() {
    let caps = Subject::Setting(Setting::CapsLock);
    let live = Observation {
        now: Some(aterm_types::macos_setup::until_login("Control")),
        after_login: Some("Caps Lock".into()),
        ..reading(caps, Standing::Differs, None)
    };
    let status = with_readings(Selection::all(), vec![live.clone()]);
    let row = row_view(caps, &status, Selection::all());
    assert_eq!(
        (row.value, row.label.as_str()),
        (
            RowValue::Change {
                now: "Caps Lock".into(),
                wanted: "Control".into()
            },
            "Caps Lock key: Caps Lock, will change to Control"
        )
    );
    // Left as is, the row is not changing: its live value, marked.
    let left = row_view(caps, &status, Selection::none());
    assert_eq!(left.value, RowValue::Muted("Control · until login".into()));
    // Nothing read of the next login: no from-side, never the marked value.
    let status = with_readings(
        Selection::all(),
        vec![Observation {
            after_login: None,
            ..live
        }],
    );
    let row = row_view(caps, &status, Selection::all());
    assert_eq!(
        row.value,
        RowValue::Change {
            now: "—".into(),
            wanted: "Control".into()
        }
    );
    assert_eq!(row.label, "Caps Lock key: not read, will change to Control");
    // Once set for the next login: the value with the check, and the note.
    let status = with_readings(
        Selection::all(),
        vec![reading(caps, Standing::AtLogin, Some("Control"))],
    );
    let row = row_view(caps, &status, Selection::all());
    assert_eq!(
        (row.value, row.chip, row.note.as_deref()),
        (
            RowValue::One("Control".into()),
            Some(Chip::InPlace),
            Some("Fully in effect after your next login")
        )
    );
    assert_eq!(row.label, "Caps Lock key: Control, in place");
}

/// A reading that ended without an answer (its reader unwound) is no check under way: the
/// headline names the row it could not read and shows it, and the row says to Check again.
#[test]
fn an_unwound_reading_neither_claims_a_check_nor_hides_its_words() {
    let mut status = status_of("set-up");
    let plan = status.plan.clone().unwrap();
    let dock = Subject::Setting(Setting::Dock);
    let unwound = Observation {
        note: Some("This reading did not finish. Check again.".into()),
        ..Observation::unknown(dock)
    };
    match status.observed.iter_mut().find(|row| row.subject == dock) {
        Some(row) => *row = unwound,
        None => status.observed.push(unwound),
    }
    assert!(!status.busy);
    let head = headline(&status, &rows(&status, plan.selection), Some(&plan));
    assert_eq!(
        (head.text.as_str(), head.show),
        ("1 row could not be read: Dock", Some(dock))
    );
    let row = row_view(dock, &status, plan.selection);
    assert_eq!(
        (row.value, row.note.as_deref(), row.label.as_str()),
        (
            RowValue::One("—".into()),
            Some("This reading did not finish. Check again."),
            "Dock: not read"
        )
    );
}

#[test]
fn accessibility_is_managed_only_while_a_sign_out_is() {
    let accessibility = Subject::Access(Access::Accessibility);
    let status = with_readings(
        Selection::all(),
        vec![reading(accessibility, Standing::Differs, Some("Off"))],
    );
    assert!(row_view(accessibility, &status, Selection::all()).managed);
    let mut selection = Selection::all();
    selection.set(Setting::Messages, false);
    selection.set(Setting::FaceTime, false);
    let row = row_view(accessibility, &status, selection);
    assert!(!row.managed);
    assert!(!row.attention());
    assert_eq!(
        (row.value, row.chip),
        (RowValue::Muted("Not needed".into()), None)
    );
}

#[test]
fn accounts_accessibility_remains_actionable_with_permissions_left_as_is() {
    let accessibility = Subject::Access(Access::Accessibility);
    let mut selection = Selection::none();
    selection.set(Setting::Messages, true);
    let mut grant = reading(accessibility, Standing::NeedsYou, Some("Off"));
    grant.detail = Some("The Messages and FaceTime sign-outs need it to press Sign Out.".into());
    let status = with_readings(
        selection,
        vec![
            grant,
            reading(
                Subject::Setting(Setting::Messages),
                Standing::Waiting,
                Some("Signed in"),
            ),
        ],
    );
    let key = "system-state/row/accessibility/open";
    // The ordinary Permissions-on form proves the published action exists
    // for this actual producer-shaped reading before testing the exception.
    let mut all_permissions = status.clone();
    all_permissions
        .plan
        .as_mut()
        .unwrap()
        .selection
        .set(Setting::Permissions, true);
    let mut baseline = SystemStateApp::new(all_permissions.plan.clone().unwrap());
    baseline.publish(all_permissions.clone());
    assert!(
        compile(&baseline, &SystemStateViewState::default(), 999.0, 900.0)
            .semantic(&UiKey::new(key))
            .unwrap()
            .state
            .unwrap()
            .enabled
    );
    let baseline_menu = crate::system_state_access::menu_line(&all_permissions, false)
        .unwrap()
        .label;

    let mut app = SystemStateApp::new(status.plan.clone().unwrap());
    app.publish(status.clone());
    let ui = compile(&app, &SystemStateViewState::default(), 999.0, 900.0);
    assert!(
        ui.semantic(&UiKey::new(key))
            .is_some_and(|node| node.state.is_some_and(|state| state.enabled)),
        "the selected sign-out's required grant must keep its remedy"
    );
    assert!(app.can_open_access(Access::Accessibility));
    assert!(app.needs_attention());
    assert_eq!(
        headline(&status, &app.reviewed_rows(), Some(&app.reviewed_plan())).open,
        Some(Access::Accessibility)
    );
    assert_eq!(
        crate::system_state_access::menu_line(&status, false)
            .unwrap()
            .label,
        baseline_menu
    );
    app.gesture(true);
    act(&mut app, key);
    assert!(matches!(
        app.take_action(),
        Some(SystemStateAction::OpenAccess {
            access: Access::Accessibility,
            by_person: true
        })
    ));
    assert!(app.take_action().is_none());
}

#[test]
fn accounts_accessibility_does_not_reopen_an_unneeded_or_settled_grant() {
    for (permissions, accounts, standing, opted_out) in [
        (false, false, Standing::NeedsYou, false),
        (true, false, Standing::NeedsYou, false),
        (false, true, Standing::InPlace, false),
        (false, true, Standing::InPlace, true),
    ] {
        let mut selection = Selection::none();
        selection.set(Setting::Permissions, permissions);
        selection.set(Setting::Messages, accounts);
        let mut grant = reading(
            Subject::Access(Access::Accessibility),
            standing,
            Some(if opted_out { "Off" } else { "On" }),
        );
        if opted_out {
            grant.wanted = "Off".into();
            grant.note = Some("You chose Not now.".into());
        }
        let status = with_readings(selection, vec![grant]);
        let mut app = SystemStateApp::new(status.plan.clone().unwrap());
        app.publish(status);
        let ui = compile(&app, &SystemStateViewState::default(), 999.0, 900.0);
        let key = "system-state/row/accessibility/open";
        assert!(ui.semantic(&UiKey::new(key)).is_none());
        assert!(!app.can_open_access(Access::Accessibility));
        assert!(!app.needs_attention());
        act(&mut app, key);
        assert!(app.take_action().is_none());
    }
}

#[test]
fn group_headers_count_only_what_is_not_in_place() {
    let status = with_readings(
        Selection::all(),
        vec![
            reading(
                Subject::Setting(Setting::CapsLock),
                Standing::InPlace,
                Some("Control"),
            ),
            reading(
                Subject::Setting(Setting::NaturalScrolling),
                Standing::Differs,
                Some("On"),
            ),
            reading(
                Subject::Setting(Setting::TrackpadScrolling),
                Standing::InPlace,
                Some("All on"),
            ),
        ],
    );
    let all = rows(&status, Selection::all());
    assert_eq!(
        group_words(Group::Input, Selection::all(), &all),
        (
            Some("1 of 3 to change".into()),
            "Keyboard & Trackpad, Set up, 1 of 3 to change".into()
        )
    );
    assert_eq!(
        group_words(Group::Desktop, Selection::all(), &all),
        (None, "Desktop & Dock, Set up".into())
    );
    let mut mixed = Selection::all();
    mixed.set(Setting::NaturalScrolling, false);
    assert_eq!(group_choice(Group::Input, mixed), GroupChoice::Partly);
    assert_eq!(
        group_words(Group::Input, mixed, &rows(&status, mixed))
            .0
            .as_deref(),
        Some("Partly set up")
    );
    assert_eq!(
        group_choice(Group::Input, Selection::none()),
        GroupChoice::LeaveAsIs
    );
    assert_eq!(
        group_words(
            Group::Input,
            Selection::none(),
            &rows(&status, Selection::none())
        ),
        (None, "Keyboard & Trackpad, Left as is".into())
    );
}

#[test]
fn headlines_say_the_state_in_the_designs_words() {
    let line = |status: &SetupStatus| {
        let selection = status
            .plan
            .as_ref()
            .map_or_else(Selection::all, |plan| plan.selection);
        headline(status, &rows(status, selection), status.plan.as_ref())
    };
    assert_eq!(line(&status_of("checking")).text, "Checking this Mac…");
    assert_eq!(
        line(&status_of("running")).text,
        "Setting up this Mac… 11 of 16"
    );
    assert_eq!(
        line(&status_of("debugger")).text,
        "Enter your password for Debugger access"
    );
    let needs = line(&status_of("needs-you"));
    assert_eq!(needs.text, "Needs you: Full Disk Access and 1 more");
    assert_eq!(needs.open, Some(Access::FullDiskAccess));
    let failed = line(&status_of("failed"));
    assert_eq!(failed.text, "1 step failed: GitHub repositories");
    assert_eq!(failed.show, Some(Subject::Setting(Setting::Repositories)));
    assert!(
        !failed.start,
        "its note names its own remedy, which Set up now cannot give"
    );
    let step = line(&status_of("named-result-failed"));
    assert_eq!(step.text, "1 step failed: GitHub repositories");
    assert!(
        step.start,
        "a step the run could not finish: its note names Set up now"
    );
    assert_eq!(line(&status_of("set-up")).text, "This Mac is set up");
    assert_eq!(line(&status_of("non-alab")).text, "Permissions are set up");
    let mixed = line(&status_of("mixed"));
    assert_eq!(mixed.text, "Needs you: Debugger access");
    assert_eq!(mixed.open, Some(Access::Debugger));
    let differs = line(&to_change());
    assert_eq!(differs.text, "1 change to make");
    assert!(differs.start, "a run would change the rows that differ");
    assert!(!line(&status_of("set-up")).start);
    // Busy with a Check or a Restore is no run: no count, no meter, no Stop.
    for (operation, words) in [
        (SetupOperation::Check, "Checking this Mac…"),
        (SetupOperation::Restore, "Restoring previous settings…"),
    ] {
        let status = SetupStatus {
            busy: true,
            operation: Some(operation),
            ..status_of("set-up")
        };
        assert_eq!(line(&status).text, words);
        assert!(!status.running());
    }
    assert!(status_of("running").running());
    assert_eq!(
        Headline::before_plan(&SetupStatus::default()).text,
        "Checking this Mac…"
    );
    let nothing = SetupStatus {
        plan: Some(Plan::from(Selection::none())),
        ..status_of("set-up")
    };
    assert_eq!(line(&nothing).text, "Everything is left as is");
    assert!(fixture_status("toggles").is_none());
}

/// Rows that only wait are no change to make (§3.2): the count and Set up now come only with a
/// row that differs, and the count then takes in the waiting rows too (each shows `Will change`).
/// Rows that only wait are named in page order, with a Show to the first, each saying what it
/// waits on.
#[test]
fn rows_that_only_wait_are_named_and_not_counted_as_changes() {
    let line = |observed: Vec<Observation>| {
        let status = with_readings(Selection::all(), observed);
        headline(
            &status,
            &rows(&status, Selection::all()),
            status.plan.as_ref(),
        )
    };
    let repositories = Subject::Setting(Setting::Repositories);
    let secrets = Subject::Setting(Setting::Secrets);
    let dock = Subject::Setting(Setting::Dock);
    let one = line(vec![reading(repositories, Standing::Waiting, None)]);
    assert_eq!(one.text, "1 step waiting: GitHub repositories");
    assert_eq!((one.show, one.start), (Some(repositories), false));
    let two = line(vec![
        reading(repositories, Standing::Waiting, None),
        reading(secrets, Standing::Waiting, Some("Not joined")),
    ]);
    assert_eq!(
        two.text,
        "2 steps waiting: Secrets vault, GitHub repositories"
    );
    assert_eq!((two.show, two.start), (Some(secrets), false));
    let with_a_change = line(vec![
        reading(repositories, Standing::Waiting, None),
        reading(dock, Standing::Differs, Some("9 apps")),
    ]);
    assert_eq!(with_a_change.text, "2 changes to make");
    assert_eq!((with_a_change.show, with_a_change.start), (None, true));
    // A fresh Mac: ten rows differ and the repositories wait on the vault.
    let fresh = status_of("fresh");
    let fresh = headline(&fresh, &rows(&fresh, Selection::all()), fresh.plan.as_ref());
    assert_eq!(fresh.text, "11 changes to make");
    assert!(fresh.start);
    // Waiting is no attention: nothing here needs the person yet.
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(with_readings(
        Selection::all(),
        vec![reading(repositories, Standing::Waiting, None)],
    ));
    assert!(!app.needs_attention());
}

/// A step the last run could not finish lights the tab as a failed row does (§3.6), though no
/// reading says so (a join the vault refused reads Not joined); one for a group left as is does
/// not.
#[test]
fn a_step_the_run_could_not_finish_lights_the_tab() {
    let joined_wrong = |selection: Selection| SetupStatus {
        rows: vec![SetupStatusRow {
            name: "Secrets vault".into(),
            status: "error".into(),
            detail: "the vault refused the join".into(),
        }],
        ..with_readings(
            selection,
            vec![reading(
                Subject::Setting(Setting::Secrets),
                Standing::Differs,
                Some("Not joined"),
            )],
        )
    };
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(joined_wrong(Selection::all()));
    assert!(app.needs_attention());
    let mut left = Selection::all();
    left.set(Setting::Secrets, false);
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(joined_wrong(left));
    assert!(!app.needs_attention(), "the vault is left as is");
}

#[test]
fn named_result_failure_headline_survives_new_readings_until_recovery() {
    let repository = Subject::Setting(Setting::Repositories);
    let secrets = Subject::Setting(Setting::Secrets);
    let settled = with_readings(
        Selection::all(),
        Subject::all()
            .into_iter()
            .map(|subject| reading(subject, Standing::InPlace, Some("as wanted")))
            .collect(),
    );
    let line = |status: &SetupStatus| {
        headline(
            status,
            &rows(status, Selection::all()),
            status.plan.as_ref(),
        )
    };
    let mut observed_failure = settled.clone();
    observed_failure
        .observed
        .iter_mut()
        .find(|row| row.subject == repository)
        .unwrap()
        .standing = Standing::Failed;
    let expected = line(&observed_failure);
    // execute_initial converts a Repositories stage Err to this named error.
    let failed = SetupStatus {
        rows: vec![SetupStatusRow {
            name: repository.name().into(),
            status: "error".into(),
            detail: "injected repository stage failure".into(),
        }],
        ..settled.clone()
    };
    let mut app = SystemStateApp::opened();
    let view = SystemStateViewState::default();
    for retrying in [true, false] {
        let current = SetupStatus {
            retrying,
            ..failed.clone()
        };
        app.publish(current.clone());
        assert_eq!(
            line(&current).show,
            Some(repository),
            "fresh in-place readings cannot make a failed operation successful"
        );
        // While the run retries the step waits; once it stops, the step failed.
        let words = if retrying {
            "1 step waiting: GitHub repositories"
        } else {
            expected.text.as_str()
        };
        assert_eq!(line(&current).text, words);
        assert_eq!(app.needs_attention(), !retrying);
        let compiled = compile(&app, &view, 999.0, 740.0);
        assert_eq!(label(&compiled, HEADLINE), words);
        assert!(
            compiled
                .semantic(&UiKey::new("system-state/headline/show"))
                .is_some_and(|node| node.action.is_some() && !node.offscreen)
        );
    }
    // The same failed subject is counted once, however it was reported.
    observed_failure.rows = failed.rows.clone();
    assert_eq!(line(&observed_failure).text, expected.text);
    // Result order is worker order; the headline still uses page order.
    observed_failure.rows.push(SetupStatusRow {
        name: secrets.name().into(),
        status: "failed".into(),
        detail: "injected vault stage failure".into(),
    });
    let mut both_observed = observed_failure.clone();
    both_observed.rows.clear();
    both_observed
        .observed
        .iter_mut()
        .find(|row| row.subject == secrets)
        .unwrap()
        .standing = Standing::Failed;
    assert_eq!(line(&observed_failure).show, Some(secrets));
    assert_eq!(line(&observed_failure).text, line(&both_observed).text);

    // A subsequent successful Check replaces the previous operation result.
    let recovered = SetupStatus {
        check: Some(SetupCheck {
            plan: settled.plan.clone().unwrap(),
            rows: vec![SetupStatusRow {
                name: format!("Check: {}", repository.name()),
                status: "ok".into(),
                detail: "injected successful repository check".into(),
            }],
        }),
        ..settled.clone()
    };
    app.publish(recovered);
    assert_eq!(
        label(&compile(&app, &view, 999.0, 740.0), HEADLINE),
        line(&settled).text
    );
    assert!(!app.needs_attention());
    assert!(line(&app.status).show.is_none());
}

#[test]
fn named_result_failure_headline_respects_review_scope_and_higher_priorities() {
    let repository = Subject::Setting(Setting::Repositories);
    let mut checked = status_of("set-up");
    checked.check = Some(SetupCheck {
        plan: checked.plan.clone().unwrap(),
        rows: vec![SetupStatusRow {
            name: format!("Check: {}", repository.name()),
            status: "error".into(),
            detail: "injected named check failure".into(),
        }],
    });
    let plan = checked.plan.clone().unwrap();
    let mut different_selection = plan.clone();
    different_selection.selection.set(Setting::CapsLock, false);
    let mut different_url = plan.clone();
    different_url.vault_url = "http://other.local:7484/v1/team".into();
    let mut unmanaged = plan.clone();
    unmanaged.selection.set(Setting::Repositories, false);
    for reviewed in [different_selection, different_url, unmanaged.clone()] {
        let mut current = checked.clone();
        if reviewed == unmanaged {
            current.check.as_mut().unwrap().plan = reviewed.clone();
        }
        let current_rows = rows(&current, reviewed.selection);
        let mut no_check = current.clone();
        no_check.check = None;
        let actual = headline(&current, &current_rows, Some(&reviewed));
        let baseline = headline(&no_check, &current_rows, Some(&reviewed));
        assert!(actual.show.is_none());
        assert_eq!(actual.text, baseline.text);
        assert!(current.failed_steps(Some(&reviewed)).next().is_none());
    }
    for mut priority in [status_of("needs-you"), status_of("running")] {
        let baseline = headline(&priority, &rows(&priority, plan.selection), Some(&plan));
        priority.check = checked.check.clone();
        let actual = headline(&priority, &rows(&priority, plan.selection), Some(&plan));
        assert_eq!(actual.text, baseline.text);
        assert_eq!((actual.open, actual.show), (baseline.open, baseline.show));
    }
    let matched = headline(&checked, &rows(&checked, plan.selection), Some(&plan));
    assert_eq!(matched.show, Some(repository));
}

#[test]
fn a_restored_operation_advisory_stays_visible_without_failure_attention() {
    let detail = "injected restored settings need a later logout";
    let mut restored = status_of("set-up");
    let baseline = headline(
        &restored,
        &rows(&restored, Selection::all()),
        restored.plan.as_ref(),
    );
    restored.rows.push(SetupStatusRow {
        name: "Restore".into(),
        status: "restored".into(),
        detail: detail.into(),
    });
    assert!(
        restored
            .failed_steps(restored.plan.as_ref())
            .next()
            .is_none()
    );
    let mut app = SystemStateApp::opened();
    app.publish(restored);
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 740.0);
    assert!(
        compiled
            .semantics
            .iter()
            .any(|node| !node.offscreen && node.label.contains(detail))
    );
    assert_eq!(label(&compiled, HEADLINE), baseline.text);
    assert!(!app.needs_attention());
}

fn failed_repository_check(managed: bool) -> SetupStatus {
    let mut selection = Selection::all();
    selection.set(Setting::Repositories, managed);
    let mut status = with_readings(
        selection,
        vec![reading(
            Subject::Setting(Setting::Repositories),
            Standing::Failed,
            None,
        )],
    );
    status.check = Some(SetupCheck {
        plan: status.plan.clone().unwrap(),
        rows: vec![SetupStatusRow {
            name: format!("Check: {}", Subject::Setting(Setting::Repositories).name()),
            status: "error".into(),
            detail: "injected repository authentication failure".into(),
        }],
    });
    status
}

#[test]
fn check_result_subject_is_counted_once_with_its_observed_failure() {
    let status = failed_repository_check(true);
    let repository = Subject::Setting(Setting::Repositories);
    let line = crate::system_state_access::menu_line(&status, false).unwrap();
    let mut observed_only = status.clone();
    observed_only.check = None;
    let baseline = crate::system_state_access::menu_line(&observed_only, false).unwrap();
    assert_eq!(
        line.label, baseline.label,
        "one failure, however it was read"
    );
    // The reading carries the failure; the Check's own result is its note (§3.6).
    assert!(status.failed_steps(status.plan.as_ref()).next().is_none());
    let plan = status.plan.clone().unwrap();
    let line = headline(&status, &rows(&status, plan.selection), Some(&plan));
    assert_eq!(
        (line.text.as_str(), line.show),
        ("1 step failed: GitHub repositories", Some(repository))
    );
    let mut app = SystemStateApp::opened();
    app.publish(status);
    assert!(app.needs_attention());
}

#[test]
fn check_result_subject_left_unmanaged_is_a_note_without_attention() {
    let status = failed_repository_check(false);
    assert!(crate::system_state_access::menu_line(&status, false).is_none());
    assert!(status.failed_steps(status.plan.as_ref()).next().is_none());
    let mut app = SystemStateApp::opened();
    app.publish(status);
    assert!(!app.needs_attention());
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 900.0);
    assert!(compiled.semantics.iter().any(|node| {
        !node.offscreen
            && node
                .label
                .contains("injected repository authentication failure")
    }));
}

/// A Check again that failed as a whole is the page's note alone (§3.6): it only read, and
/// pressing it again is its remedy, so neither the menu line nor the tab's diamond says it, and
/// the headline, which names no failure, says the rows are as aterm last read them.
#[test]
fn a_failed_check_again_is_a_page_note_only() {
    let mut permissions_only = status_of("check-failed");
    let mut selection = Selection::none();
    selection.set(Setting::Permissions, true);
    permissions_only.plan = Some(Plan::from(selection));
    permissions_only.check.as_mut().unwrap().plan = Plan::from(selection);
    for (status, words) in [
        (
            status_of("check-failed"),
            "This Mac is set up, as last read",
        ),
        (permissions_only, "Permissions are set up, as last read"),
    ] {
        let plan = status.plan.clone().unwrap();
        assert_eq!(
            status.notes(&plan),
            [format!("Check again failed. {CHECK_FAILED_NOTE}")]
        );
        assert!(status.failed_steps(Some(&plan)).next().is_none());
        assert!(crate::system_state_access::menu_line(&status, false).is_none());
        let mut app = SystemStateApp::new(plan.clone());
        app.publish(status.clone());
        assert!(!app.needs_attention(), "{words}");
        let line = app.headline();
        assert_eq!(
            (line.text.as_str(), line.show, line.start),
            (words, None, false)
        );
        let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 900.0);
        assert_eq!(label(&compiled, HEADLINE), words);
    }
}

/// A stage's error or a failed row the worker still retries leaves the tab's diamond dark, and
/// lights it once retrying stops (§3.5, §3.6). While it retries the page says the step waits;
/// once retrying stops the headline says it failed, with the diamond. A row that needs a person
/// lights it at once.
#[test]
fn a_failure_the_run_still_retries_leaves_the_tab_dark() {
    let secrets = Subject::Setting(Setting::Secrets);
    let dock = Subject::Setting(Setting::Dock);
    let error = || SetupStatusRow {
        name: "Secrets vault".into(),
        status: "error".into(),
        detail: "the vault did not answer".into(),
    };
    for (readings, steps) in [
        (
            vec![reading(secrets, Standing::Differs, Some("Not joined"))],
            vec![error()],
        ),
        (
            vec![reading(dock, Standing::Failed, Some("9 apps"))],
            Vec::new(),
        ),
    ] {
        for retrying in [true, false] {
            let status = SetupStatus {
                rows: steps.clone(),
                retrying,
                ..with_readings(Selection::all(), readings.clone())
            };
            let shown = headline(
                &status,
                &rows(&status, Selection::all()),
                status.plan.as_ref(),
            );
            assert_eq!(
                (
                    shown.text.contains("failed"),
                    shown.text.contains("waiting")
                ),
                (!retrying, retrying),
                "retrying {retrying}: {}",
                shown.text
            );
            let mut app = SystemStateApp::new(Plan::default());
            app.publish(status);
            assert_eq!(app.needs_attention(), !retrying, "retrying {retrying}");
        }
    }
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(SetupStatus {
        rows: vec![error()],
        retrying: true,
        ..with_readings(
            Selection::all(),
            vec![reading(secrets, Standing::NeedsYou, Some("Awaiting code"))],
        )
    });
    assert!(app.needs_attention(), "a person's step is said at once");
}

#[test]
fn only_a_managed_row_that_needs_a_person_or_failed_lights_the_tab() {
    for (scenario, attention) in [
        ("checking", false),
        ("running", false),
        ("needs-you", true),
        ("failed", true),
        ("set-up", false),
        ("non-alab", false),
        // Its Debugger access is off, which Permissions manage: a person turns it on.
        ("mixed", true),
        ("fresh", false),
        ("widened", false),
        ("vault-unreached", false),
        // The run still tries the vault again by itself.
        ("vault-failed", false),
        ("restore-unlogged", false),
    ] {
        let mut app = SystemStateApp::new(Plan::default());
        app.publish(status_of(scenario));
        assert_eq!(
            app.presentation(&SystemStateViewState::default())
                .indicators
                .attention,
            attention,
            "{scenario}"
        );
        assert!(
            !app.presentation(&SystemStateViewState::default())
                .indicators
                .dirty
        );
    }
    // A failure in a group left as is is not aterm's to fix.
    let mut selection = Selection::all();
    selection.set(Setting::Repositories, false);
    let mut status = status_of("failed");
    status.plan = Some(Plan::from(selection));
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status);
    assert!(!app.needs_attention());
}

#[test]
fn a_group_choice_takes_effect_at_once_and_says_whose_it_is() {
    for (by_person, admission) in [(true, Admission::Person), (false, Admission::Client)] {
        let mut app = SystemStateApp::new(Plan::default());
        app.gesture(by_person);
        act(&mut app, "system-state/group/2/leave");
        let Some(SystemStateAction::Apply(answer, who)) = app.take_action() else {
            panic!("a choice submits its plan");
        };
        assert_eq!(who, admission);
        let WorkstationSetupDecision::Begin(plan) = &answer.decision else {
            panic!("the plan begins");
        };
        for setting in Group::Desktop.settings() {
            assert!(!plan.selection.contains(*setting));
        }
        assert!(plan.selection.contains(Setting::CapsLock));
        assert!(app.status.busy, "the page shows the run starting");
        assert!(app.take_action().is_none());
    }
    // Choosing what the plan already has submits nothing.
    let mut app = SystemStateApp::new(Plan::default());
    act(&mut app, "system-state/group/0/set-up");
    assert!(app.take_action().is_none());
}

/// The plan an action submits, whose it is, and the password it carries.
fn submitted(action: Option<SystemStateAction>) -> (Plan, Admission, Option<SecretBytes>) {
    let Some(SystemStateAction::Apply(answer, admission)) = action else {
        panic!("a plan was submitted");
    };
    let WorkstationSetupDecision::Begin(plan) = answer.decision else {
        panic!("the plan begins");
    };
    (plan, admission, answer.password)
}

#[test]
fn a_choice_during_a_run_goes_at_once_and_nothing_waits_on_the_page() {
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(SetupStatus {
        busy: true,
        plan: Some(Plan::default()),
        ..SetupStatus::default()
    });
    app.gesture(true);
    act(&mut app, "system-state/group/3/leave");
    // The worker stops the run and runs this next; the page holds nothing a
    // closing view could lose.
    let (plan, admission, _) = submitted(app.take_action());
    assert_eq!(admission, Admission::Person);
    assert!(!plan.selection.contains(Setting::Messages));
    assert!(app.take_action().is_none(), "no Stop of the page's own");
    // A later choice overtakes it, and says whose it is.
    app.gesture(false);
    act(&mut app, "system-state/group/1/leave");
    let (plan, admission, _) = submitted(app.take_action());
    assert_eq!(admission, Admission::Client);
    assert!(!plan.selection.contains(Setting::Messages));
    assert!(!plan.selection.contains(Setting::CapsLock));
    app.tick(true, true, false);
    assert!(app.take_action().is_none(), "nothing is queued for later");
}

#[test]
fn stop_is_pressed_once_and_only_while_a_run_goes() {
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(SetupStatus {
        busy: true,
        ..SetupStatus::default()
    });
    act(&mut app, "system-state/stop");
    assert!(matches!(app.take_action(), Some(SystemStateAction::Stop)));
    act(&mut app, "system-state/stop");
    assert!(app.take_action().is_none(), "Stop was sent already");
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 900.0);
    assert!(
        compiled
            .semantic(&UiKey::new("system-state/stop"))
            .and_then(|node| node.state)
            .is_some_and(|state| !state.enabled)
    );
    // The run ended: the next one can be stopped again.
    app.publish(SetupStatus::default());
    app.publish(SetupStatus {
        busy: true,
        ..SetupStatus::default()
    });
    act(&mut app, "system-state/stop");
    assert!(matches!(app.take_action(), Some(SystemStateAction::Stop)));

    // Check again is no run: nothing to stop, no meter.
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status_of("set-up"));
    act(&mut app, "system-state/check");
    assert!(matches!(
        app.take_action(),
        Some(SystemStateAction::Check(_))
    ));
    assert_eq!(app.status.operation, Some(SetupOperation::Check));
    act(&mut app, "system-state/stop");
    assert!(app.take_action().is_none());
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 900.0);
    for key in ["system-state/stop", "system-state/meter"] {
        assert!(compiled.semantic(&UiKey::new(key)).is_none(), "{key}");
    }
    assert_eq!(label(&compiled, HEADLINE), "Checking this Mac…");
}

#[test]
fn a_refused_choice_puts_the_groups_back_and_keeps_the_vault_draft() {
    let mut app = SystemStateApp::new(Plan::default());
    app.url.select_all();
    app.url.insert("http://draft.local:7484/v1/team");
    app.password
        .input(TextInputEvent::Commit("inert-password".into()));
    act(&mut app, "system-state/group/2/leave");
    // A group choice sends the address the worker holds, and no password.
    let (plan, _, password) = submitted(app.take_action());
    assert_eq!(plan.vault_url, Plan::default().vault_url);
    assert!(password.is_none());
    assert!(!app.plan.selection.contains(Setting::Dock));
    assert!(app.apply_refused());
    assert!(
        app.plan.selection.contains(Setting::Dock),
        "the groups show what is taking effect"
    );
    assert_eq!(app.url.value(), "http://draft.local:7484/v1/team");
    assert_eq!(app.password.text(), "inert-password");
    assert!(!app.apply_refused(), "nothing else was waiting");
}

#[test]
fn a_page_opened_by_hand_waits_for_the_plan_in_force() {
    let mut app = SystemStateApp::opened();
    assert!(!app.plan_known());
    act(&mut app, "system-state/group/3/leave");
    act(&mut app, "system-state/headline/start");
    assert!(app.take_action().is_none(), "no guess is submitted");
    assert!(!app.needs_attention());
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 900.0);
    let segment = compiled
        .semantic(&UiKey::new("system-state/group/0/set-up"))
        .and_then(|node| node.state)
        .unwrap();
    assert!(!segment.enabled && !segment.selected);
    assert_eq!(label(&compiled, HEADLINE), "Checking this Mac…");
    assert!(
        compiled
            .semantic(&UiKey::new("system-state/meter"))
            .is_none()
    );
    // The worker's saved plan, Repositories left as is, becomes the page's,
    // even while a run is going.
    let mut saved = Plan::default();
    saved.selection.set(Setting::Repositories, false);
    app.publish(SetupStatus {
        busy: true,
        plan: Some(saved.clone()),
        ..status_of("set-up")
    });
    assert!(app.plan_known());
    assert_eq!(app.plan, saved);
    act(&mut app, "system-state/group/3/leave");
    let (plan, _, _) = submitted(app.take_action());
    assert!(!plan.selection.contains(Setting::Repositories));
    assert!(!plan.selection.contains(Setting::Messages));
}

#[test]
fn unknown_plan_check_stays_disabled_after_a_status_error() {
    for status in [SetupStatus::default(), status_of("plan-unread")] {
        let mut app = SystemStateApp::opened();
        app.publish(status);
        let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 740.0);
        let enabled = |key: &str| {
            compiled
                .semantic(&UiKey::new(key))
                .and_then(|node| node.state)
                .unwrap()
                .enabled
        };
        assert!(!enabled("system-state/check"));
        assert!(enabled("system-state/restore"));
        act(&mut app, "system-state/check");
        assert!(app.take_action().is_none(), "Check submits no guessed plan");
        assert!(!app.status.busy && !app.plan_known());
        act(&mut app, "system-state/restore");
        assert!(matches!(
            app.take_action(),
            Some(SystemStateAction::Restore)
        ));
    }
}

#[test]
fn unknown_plan_join_keeps_its_draft_until_the_saved_plan_is_known() {
    for status in [SetupStatus::default(), status_of("plan-unread")] {
        let (mut runtime, instance, view) = runtime_with_app(SystemStateApp::opened());
        runtime.publish_system_state(status);
        runtime.system_state_gesture(true);
        let press = |runtime: &mut NativeRuntime, key: &str, value| {
            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: ActionId::new(key),
                        value,
                    }),
                )
                .unwrap();
        };
        let compiled = |runtime: &NativeRuntime| {
            let (app, state) = page_in(runtime, view);
            compile(app, state, 999.0, 740.0)
        };
        let enabled = |tree: &CompiledUi, key: &str| {
            tree.semantic(&UiKey::new(key))
                .and_then(|node| node.state)
                .unwrap()
                .enabled
        };
        press(&mut runtime, "system-state/row/secrets/disclose", None);
        for (key, text) in [
            ("system-state/url", "http://typed.local:7484/v1/team"),
            ("system-state/password", "inert-password"),
        ] {
            press(&mut runtime, key, Some(SemanticInput::Text(text.into())));
        }
        let tree = compiled(&runtime);
        assert!(enabled(&tree, "system-state/url"));
        assert!(enabled(&tree, "system-state/password"));
        assert!(enabled(&tree, "system-state/restore"));
        assert!(!enabled(&tree, "system-state/vault/join"));
        press(&mut runtime, "system-state/vault/join", None);
        assert!(runtime.take_system_state_action().is_none());

        let mut selection = Selection::none();
        selection.set(Setting::Permissions, true);
        selection.set(Setting::Dock, true);
        runtime.publish_system_state(SetupStatus {
            plan: Some(Plan {
                selection,
                vault_url: "http://saved.local:7484/v1/team".into(),
            }),
            ..SetupStatus::default()
        });
        let (app, _) = page_in(&runtime, view);
        assert_eq!(app.url.value(), "http://typed.local:7484/v1/team");
        assert_eq!(app.password.text(), "inert-password");
        assert_eq!(app.plan.selection, selection);
        assert!(enabled(&compiled(&runtime), "system-state/vault/join"));
        press(&mut runtime, "system-state/vault/join", None);
        let (plan, admission, password) = submitted(runtime.take_system_state_action());
        selection.set(Setting::Secrets, true);
        assert_eq!(plan.selection, selection);
        assert_eq!(plan.vault_url, "http://typed.local:7484/v1/team");
        assert_eq!(admission, Admission::Person);
        assert_eq!(
            password.as_ref().and_then(SecretBytes::as_str),
            Some("inert-password")
        );
        assert!(runtime.take_system_state_action().is_none());
        assert!(!enabled(&compiled(&runtime), "system-state/vault/join"));
    }
}

/// A saved setup aterm cannot read just now (a transient read error, or a record it has not set
/// aside yet) is no failure (§3.6): aterm reads it again by itself, so the page says so in one
/// calm line, with no failure headline, tab attention or menu line, across a reopen and while
/// the readings it cached stay, until a read succeeds.
#[test]
fn a_saved_setup_unread_just_now_is_said_calmly_until_a_read_succeeds() {
    const WHY: &str = STATE_UNREAD_NOTE;
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    app.set_system_state_handlers_for_test(
        crate::system_state_host::InitialSetupHandlers::inert_for_test(),
    );
    assert!(app.open_system_state_tab());
    let compiled = |app: &crate::App| {
        let (_, view) = app.active_native_view(wid).unwrap();
        let (page, state) = page_in(&app.native_runtime, view);
        compile(page, state, 999.0, 740.0)
    };
    let says_why = |app: &crate::App| {
        compiled(app)
            .semantics
            .iter()
            .any(|node| node.label.contains(WHY))
    };
    let calm = |app: &crate::App| {
        let (_, view) = app.active_native_view(wid).unwrap();
        let page = page_in(&app.native_runtime, view).0;
        !page.needs_attention()
            && crate::system_state_access::menu_line(&page.status, false).is_none()
    };
    let loading = label(&compiled(&app), HEADLINE);
    let unread = SetupStatus {
        rows: vec![calm_setup_row(WHY)],
        ..SetupStatus::default()
    };
    for retrying in [true, false] {
        app.publish_system_state(SetupStatus {
            retrying,
            ..unread.clone()
        });
        assert_eq!(label(&compiled(&app), HEADLINE), loading);
        assert!(says_why(&app));
        assert!(calm(&app), "retrying {retrying}");
    }
    for operation in [
        None,
        Some(SetupOperation::Check),
        Some(SetupOperation::Restore),
    ] {
        let mut busy_page = SystemStateApp::opened();
        busy_page.publish(SetupStatus {
            busy: true,
            operation,
            ..unread.clone()
        });
        assert_eq!(
            label(
                &compile(&busy_page, &SystemStateViewState::default(), 999.0, 740.0),
                HEADLINE
            ),
            Headline::before_plan(&SetupStatus {
                busy: true,
                operation,
                ..SetupStatus::default()
            })
            .text
        );
    }
    let (instance, _) = app.active_native_view(wid).unwrap();
    app.close_active_native_tab(wid).unwrap();
    assert!(app.native_runtime.app(instance).is_none());
    assert!(app.open_system_state_tab());
    app.publish_system_state(unread.clone());
    assert!(says_why(&app));
    assert!(calm(&app));

    let mut selection = Selection::none();
    selection.set(Setting::Dock, true);
    let plan = Plan {
        selection,
        vault_url: "http://saved.local:7484/v1/team".into(),
    };
    let success = SetupStatus {
        plan: Some(plan.clone()),
        observed_vault_url: Some(plan.vault_url.clone()),
        ..status_of("set-up")
    };
    app.publish_system_state(success.clone());
    let settled = label(&compiled(&app), HEADLINE);
    assert!(!says_why(&app));
    let (_, view) = app.active_native_view(wid).unwrap();
    assert_eq!(page_in(&app.native_runtime, view).0.reviewed_plan(), plan);
    // A later read failed while the readings stay cached: the page keeps the plan it knew and
    // claims nothing of it until a read succeeds.
    app.publish_system_state(SetupStatus {
        observed: success.observed.clone(),
        observed_vault_url: success.observed_vault_url.clone(),
        ..unread.clone()
    });
    assert_eq!(label(&compiled(&app), HEADLINE), CHECKING);
    assert_ne!(CHECKING, settled);
    assert!(page_in(&app.native_runtime, view).0.plan_known());
    assert!(says_why(&app));
    assert!(calm(&app));
    app.publish_system_state(success);
    assert_eq!(label(&compiled(&app), HEADLINE), settled);
    assert!(calm(&app));
    assert!(app.structural_invariants_ok());
}

#[test]
fn a_generic_check_failure_headline_follows_only_the_exact_reviewed_plan() {
    let mut selection = Selection::none();
    selection.set(Setting::Dock, true);
    let plan = Plan {
        selection,
        vault_url: "http://saved.local:7484/v1/team".into(),
    };
    let success = SetupStatus {
        plan: Some(plan.clone()),
        observed_vault_url: Some(plan.vault_url.clone()),
        ..status_of("set-up")
    };
    let mut app = SystemStateApp::new(plan.clone());
    let mut view = SystemStateViewState::default();
    app.publish(success.clone());
    let settled = label(&compile(&app, &view, 999.0, 740.0), HEADLINE);
    let checked = SetupStatus {
        retrying: true,
        check: Some(SetupCheck {
            plan,
            rows: vec![SetupStatusRow {
                name: "Check".into(),
                status: "failed".into(),
                detail: "injected selected check failure".into(),
            }],
        }),
        ..success
    };
    app.publish(checked.clone());
    assert_ne!(
        label(&compile(&app, &view, 999.0, 740.0), HEADLINE),
        settled
    );
    assert!(
        !app.needs_attention(),
        "retry suppression is only a signal rule"
    );
    app.act(
        &mut view,
        "system-state/url",
        Some(SemanticInput::Text(
            "http://draft.local:7484/v1/team".into(),
        )),
    );
    for _ in 0..2 {
        app.publish(checked.clone());
        assert_eq!(
            label(&compile(&app, &view, 999.0, 740.0), HEADLINE),
            settled
        );
        assert!(!app.needs_attention());
    }
}

#[test]
fn set_up_now_runs_the_plan_again_when_rows_differ() {
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(to_change());
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 900.0);
    assert_eq!(label(&compiled, HEADLINE), "1 change to make");
    // The button says its action alone: VoiceOver reads the headline beside it.
    assert_eq!(
        label(&compiled, "system-state/headline/start"),
        "Set up now"
    );
    app.gesture(true);
    act(&mut app, "system-state/headline/start");
    let (plan, admission, _) = submitted(app.take_action());
    assert_eq!(plan.selection, to_change().plan.unwrap().selection);
    assert_eq!(admission, Admission::Person);
    act(&mut app, "system-state/headline/start");
    assert!(app.take_action().is_none(), "not while it runs");
}

#[test]
fn unread_vault_rows_cannot_establish_that_the_mac_is_set_up() {
    let complete = status_of("set-up");
    let selection = complete.plan.as_ref().unwrap().selection;
    let finished = headline(
        &complete,
        &rows(&complete, selection),
        complete.plan.as_ref(),
    );
    let first_read = Headline::before_plan(&SetupStatus::default());
    assert_ne!(finished.text, first_read.text);
    for source in [None, Some("http://other-vault.local:7484/v1/team".into())] {
        let mut partial = complete.clone();
        partial.observed_vault_url = source;
        let waiting = headline(&partial, &rows(&partial, selection), partial.plan.as_ref());
        assert_eq!(waiting.text, first_read.text);
        assert!(!waiting.start);
        let no_managed = headline(
            &partial,
            &rows(&partial, Selection::none()),
            partial.plan.as_ref(),
        );
        assert_eq!(
            no_managed.text,
            headline(
                &complete,
                &rows(&complete, Selection::none()),
                complete.plan.as_ref()
            )
            .text
        );
        partial
            .observed
            .iter_mut()
            .find(|row| row.subject == Subject::Setting(Setting::CapsLock))
            .unwrap()
            .standing = Standing::Differs;
        assert!(
            headline(&partial, &rows(&partial, selection), partial.plan.as_ref()).start,
            "a known change still offers setup while the other source is unread"
        );
    }
}

#[test]
fn an_unknown_managed_row_keeps_the_headline_unread() {
    let complete = status_of("set-up");
    let selection = complete.plan.as_ref().unwrap().selection;
    let finished = headline(
        &complete,
        &rows(&complete, selection),
        complete.plan.as_ref(),
    );
    let first_read = Headline::before_plan(&SetupStatus::default());
    assert_ne!(finished.text, first_read.text);

    let mut partial = complete.clone();
    partial
        .observed
        .iter_mut()
        .find(|row| row.subject == Subject::Setting(Setting::Repositories))
        .unwrap()
        .standing = Standing::Unknown;
    let waiting = headline(&partial, &rows(&partial, selection), partial.plan.as_ref());
    assert_eq!(waiting.text, first_read.text);
    assert!(!waiting.start);
    assert_eq!(
        headline(
            &partial,
            &rows(&partial, Selection::none()),
            partial.plan.as_ref()
        )
        .text,
        headline(
            &complete,
            &rows(&complete, Selection::none()),
            complete.plan.as_ref()
        )
        .text
    );

    partial
        .observed
        .iter_mut()
        .find(|row| row.subject == Subject::Setting(Setting::CapsLock))
        .unwrap()
        .standing = Standing::Differs;
    assert!(
        headline(&partial, &rows(&partial, selection), partial.plan.as_ref()).start,
        "a known change still offers setup while another reading is unknown"
    );
}

#[test]
fn only_a_group_choice_or_join_makes_the_plan_the_choosers() {
    let chosen = |action: Option<SystemStateAction>| {
        let Some(SystemStateAction::Apply(answer, _)) = action else {
            panic!("a plan was submitted");
        };
        answer.chosen
    };
    // A choice and a choice back are both the person's (§3.5): aterm's
    // provisional plan, reached again by hand, is never widened later.
    let mut app = SystemStateApp::new(Plan::default());
    app.gesture(true);
    act(&mut app, "system-state/group/1/leave");
    assert!(chosen(app.take_action()));
    act(&mut app, "system-state/group/1/set-up");
    assert!(chosen(app.take_action()));
    let mut app = SystemStateApp::new(Plan::default());
    act(&mut app, "system-state/vault/join");
    assert!(chosen(app.take_action()), "Join chooses the vault");
    // Set up now runs the plan as it stands.
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(to_change());
    act(&mut app, "system-state/headline/start");
    assert!(!chosen(app.take_action()));
    // So does the first-run offer.
    let (send, _receive) = mpsc::channel();
    let mut app = SystemStateApp::new(Plan::default());
    assert!(app.offer(Request::new(Plan::default(), send)));
    let now = Instant::now();
    app.tick_at(now, true, true, false);
    app.tick_at(now + Duration::from_secs(30), true, true, false);
    assert!(!chosen(app.take_action()));
}

#[test]
fn an_accepted_plan_does_not_undo_a_choice_made_while_it_waited() {
    let mut app = SystemStateApp::new(Plan::default());
    act(&mut app, "system-state/group/3/leave");
    let (first, _, _) = submitted(app.take_action());
    act(&mut app, "system-state/group/2/leave");
    let (second, _, _) = submitted(app.take_action());
    app.apply_accepted(first.clone());
    assert!(
        !app.plan.selection.contains(Setting::Dock),
        "the later choice stays"
    );
    assert_eq!(app.submitted, first);
    // A read taken meanwhile still names the first plan: it cannot undo the
    // second, which the worker has not answered yet.
    app.publish(SetupStatus {
        plan: Some(first),
        ..SetupStatus::default()
    });
    assert!(!app.plan.selection.contains(Setting::Dock));
    app.apply_accepted(second.clone());
    assert_eq!(app.plan, second);
    assert!(app.awaiting.is_none());
}

#[test]
fn startup_review_waits_before_automatically_submitting() {
    let (send, receive) = mpsc::channel();
    let request = Request::new(Plan::default(), send);
    let mut app = SystemStateApp::new(Plan::default());
    assert!(app.offer(request.clone()));
    app.tick(true, true, false);
    assert!(
        app.take_action().is_none(),
        "the first visible tick starts the review, not its work"
    );
    assert!(request.pending());
    assert!(matches!(receive.try_recv(), Err(mpsc::TryRecvError::Empty)));
}

#[test]
fn startup_review_group_changes_remain_unsubmitted_until_apply() {
    let (send, receive) = mpsc::channel();
    let request = Request::new(Plan::default(), send);
    let mut app = SystemStateApp::new(Plan::default());
    assert!(app.offer(request.clone()));
    let (mut runtime, instance, view) = runtime_with_app(app);
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("system-state/group/2/leave"),
                value: None,
            }),
        )
        .unwrap();
    assert!(
        runtime.take_system_state_action().is_none(),
        "a first-launch group change edits the review without starting setup"
    );
    let NativeApp::SystemState(page) = runtime.app(instance).unwrap() else {
        unreachable!()
    };
    assert!(!page.reviewed_plan().selection.contains(Setting::Dock));
    assert!(request.pending());
    assert!(matches!(receive.try_recv(), Err(mpsc::TryRecvError::Empty)));
}

#[test]
fn startup_review_join_keeps_the_address_and_password_for_apply() {
    let (send, receive) = mpsc::channel();
    let request = Request::new(Plan::default(), send);
    let mut app = SystemStateApp::new(Plan::default());
    assert!(app.offer(request.clone()));
    let (mut runtime, instance, view) = runtime_with_app(app);
    for (key, value) in [
        (
            "system-state/url",
            Some("http://draft.local:7484/v1/review"),
        ),
        ("system-state/password", Some("synthetic-review-secret")),
        ("system-state/vault/join", None),
    ] {
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new(key),
                    value: value.map(|value| SemanticInput::Text(value.into())),
                }),
            )
            .unwrap();
    }
    assert!(
        runtime.take_system_state_action().is_none(),
        "Join in the startup review must not release credentials before Apply"
    );
    let NativeApp::SystemState(page) = runtime.app(instance).unwrap() else {
        unreachable!()
    };
    assert_eq!(
        page.reviewed_plan().vault_url,
        "http://draft.local:7484/v1/review"
    );
    assert_eq!(page.password.text(), "synthetic-review-secret");
    assert!(request.pending());
    assert!(matches!(receive.try_recv(), Err(mpsc::TryRecvError::Empty)));
}

#[test]
fn startup_review_counts_only_active_visible_time_and_submits_once() {
    let (send, receive) = mpsc::channel();
    let request = Request::new(Plan::default(), send);
    let mut app = SystemStateApp::new(Plan::default());
    assert!(app.offer(request.clone()));
    let start = Instant::now();
    app.tick_at(start, false, true, false);
    assert!(
        app.take_action().is_none(),
        "not until it is in an active window"
    );
    app.tick_at(start + Duration::from_secs(100), true, false, false);
    assert!(app.take_action().is_none(), "a hidden page counts no time");
    app.tick_at(start + Duration::from_secs(101), true, true, false);
    app.tick_at(start + Duration::from_secs(111), true, true, false);
    let view = SystemStateViewState::default();
    assert_eq!(
        compile(&app, &view, 900.0, 900.0)
            .semantic(&UiKey::new("system-state/apply"))
            .unwrap()
            .label,
        "Apply (20)"
    );
    app.publish(SetupStatus {
        plan: Some(Plan::default()),
        ..SetupStatus::default()
    });
    app.tick_at(start + Duration::from_secs(111), false, true, false);
    app.tick_at(start + Duration::from_secs(1000), true, true, false);
    app.tick_at(start + Duration::from_secs(1019), true, true, false);
    assert!(
        app.take_action().is_none(),
        "focus and status never restart or consume paused time"
    );
    app.tick_at(start + Duration::from_secs(1020), true, true, false);
    let Some(SystemStateAction::Apply(answer, who)) = app.take_action() else {
        panic!("the offer applies");
    };
    assert_eq!(who, Admission::Automatic);
    assert!(!answer.chosen && !answer.vault_typed && answer.password.is_none());
    assert_eq!(
        answer.changed, None,
        "the full reviewed selection is applied"
    );
    assert!(request.finish_answer(answer));
    assert!(matches!(
        receive
            .recv_timeout(Duration::from_secs(60))
            .unwrap()
            .decision,
        WorkstationSetupDecision::Begin(_)
    ));
    app.tick_at(start + Duration::from_secs(2000), true, true, false);
    assert!(app.take_action().is_none());
    assert!(receive.try_recv().is_err());
}

#[test]
fn startup_review_pauses_when_busy_starts_and_ends_between_ticks() {
    let (send, _receive) = mpsc::channel();
    let mut app = SystemStateApp::new(Plan::default());
    assert!(app.offer(Request::new(Plan::default(), send)));
    let start = Instant::now();
    app.tick_at(start, true, true, false);
    app.tick_at(start + Duration::from_secs(10), true, true, false);
    app.publish(SetupStatus {
        busy: true,
        ..SetupStatus::default()
    });
    app.publish(SetupStatus::default());
    app.tick_at(start + Duration::from_secs(1000), true, true, false);
    assert!(
        app.take_action().is_none(),
        "busy publications pause the review even without a busy tick"
    );
    assert_eq!(app.review_seconds(), Some(20));
    app.tick_at(start + Duration::from_secs(1019), true, true, false);
    assert!(app.take_action().is_none());
    app.tick_at(start + Duration::from_secs(1020), true, true, false);
    assert!(matches!(
        app.take_action(),
        Some(SystemStateAction::Apply(_, Admission::Automatic))
    ));
}

#[test]
fn startup_review_interaction_stops_automatic_apply_for_the_whole_offer() {
    for event in [
        AppEvent::ScrollLines(1),
        AppEvent::Action(ActionInvocation {
            id: ActionId::new("system-state/row/secrets/disclose"),
            value: None,
        }),
        AppEvent::TextInput(TextInputEvent::Commit("x".into())),
        AppEvent::TextInput(TextInputEvent::Left { extend: false }),
    ] {
        let (send, _receive) = mpsc::channel();
        let mut app = SystemStateApp::new(Plan::default());
        assert!(app.offer(Request::new(Plan::default(), send)));
        let (mut runtime, instance, view) = runtime_with_app(app);
        let start = Instant::now();
        runtime.tick_system_state_at(start, true, true, &[view]);
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::FocusChanged(Some(UiKey::new("system-state/url"))),
            )
            .unwrap();
        runtime.dispatch(instance, view, event).unwrap();
        runtime.tick_system_state_at(start + Duration::from_secs(60), true, true, &[view]);
        runtime.publish_system_state(SetupStatus {
            plan: Some(Plan::default()),
            ..SetupStatus::default()
        });
        runtime.tick_system_state_at(start + Duration::from_secs(120), false, false, &[]);
        runtime.tick_system_state_at(start + Duration::from_secs(180), true, true, &[view]);
        assert!(runtime.take_system_state_action().is_none());
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("system-state/apply"),
                    value: None,
                }),
            )
            .unwrap();
        let (_, admission, _) = submitted(runtime.take_system_state_action());
        assert_eq!(admission, Admission::Client);
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("system-state/apply"),
                    value: None,
                }),
            )
            .unwrap();
        assert!(
            runtime.take_system_state_action().is_none(),
            "Apply is single-flight"
        );
    }
}

#[test]
fn startup_review_focus_and_empty_preedit_do_not_stop_the_countdown() {
    let (send, _receive) = mpsc::channel();
    let mut app = SystemStateApp::new(Plan::default());
    assert!(app.offer(Request::new(Plan::default(), send)));
    let (mut runtime, instance, view) = runtime_with_app(app);
    let start = Instant::now();
    runtime.tick_system_state_at(start, true, true, &[view]);
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::FocusChanged(Some(UiKey::new("system-state/url"))),
        )
        .unwrap();
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::TextInput(TextInputEvent::Preedit {
                text: String::new(),
                selection: None,
            }),
        )
        .unwrap();
    runtime.tick_system_state_at(start + Duration::from_secs(30), true, true, &[view]);
    let (_, admission, _) = submitted(runtime.take_system_state_action());
    assert_eq!(admission, Admission::Automatic);
}

#[test]
fn closing_never_cancels_and_never_waits() {
    for scope in [
        CloseScope::View,
        CloseScope::Tab,
        CloseScope::Window,
        CloseScope::AppQuit,
        CloseScope::Relaunch,
    ] {
        let (send, receive) = mpsc::channel();
        let request = Request::new(Plan::default(), send);
        let mut app = SystemStateApp::new(Plan::default());
        app.offer(request.clone());
        act(&mut app, "system-state/group/1/leave");
        app.password
            .input(TextInputEvent::Commit("inert-password".into()));
        let (mut runtime, instance, view) = runtime_with_app(app);
        assert_eq!(
            runtime
                .prepare_close(instance, view, CloseRequest { scope })
                .unwrap()
                .0,
            CloseReadiness::Ready
        );
        assert!(request.pending(), "a preflight resolves nothing");
        runtime.finish_system_state_close(instance, scope);
        assert_eq!(
            receive.try_recv().unwrap().decision,
            WorkstationSetupDecision::Unavailable
        );
    }
}

#[test]
fn a_published_plan_is_adopted_only_when_nothing_is_pending_here() {
    let mut saved = Plan::default();
    saved.selection.set(Setting::Messages, false);
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(SetupStatus {
        plan: Some(saved.clone()),
        ..SetupStatus::default()
    });
    assert_eq!(app.plan, saved);
    // A vault draft is the person's: a poll never replaces it.
    app.url.select_all();
    app.url.insert("http://draft.local:7484/v1/team");
    app.publish(SetupStatus {
        plan: Some(Plan::default()),
        ..SetupStatus::default()
    });
    assert_eq!(app.url.value(), "http://draft.local:7484/v1/team");
    assert!(!app.can_offer());
}

#[test]
fn open_buttons_carry_whose_press_it_was() {
    for by_person in [true, false] {
        let mut app = SystemStateApp::new(Plan::default());
        app.publish(status_of("needs-you"));
        app.gesture(by_person);
        act(&mut app, "system-state/row/full-disk-access/open");
        assert!(matches!(
            app.take_action(),
            Some(SystemStateAction::OpenAccess { access: Access::FullDiskAccess, by_person: by }) if by == by_person
        ));
        act(&mut app, "system-state/headline/open/full-disk-access");
        assert!(matches!(
            app.take_action(),
            Some(SystemStateAction::OpenAccess {
                access: Access::FullDiskAccess,
                by_person: by,
            }) if by == by_person
        ));
        // A row that does not need a person has no Open to press.
        act(&mut app, "system-state/row/debugger/open");
        assert!(app.take_action().is_none());
    }
}

/// Only Join names a vault the person typed, which moves the Mac; a group choice and Set
/// up now ask for the vault System State shows when the worker admits them, even with an
/// edited field (a draft stays a draft). Negative control: a flag set for every
/// submission reads a group choice's held vault as typed.
#[test]
fn only_a_join_names_a_typed_vault() {
    let typed = |action: Option<SystemStateAction>| {
        let Some(SystemStateAction::Apply(answer, _)) = action else {
            panic!("a plan was submitted");
        };
        answer.vault_typed
    };
    let mut app = SystemStateApp::new(Plan::default());
    app.gesture(true);
    app.url.select_all();
    app.url.insert("http://typed.local:7484/v1/team");
    act(&mut app, "system-state/group/1/leave");
    assert!(!typed(app.take_action()), "a group choice");
    app.status.busy = false;
    app.publish(to_change());
    act(&mut app, "system-state/headline/start");
    assert!(!typed(app.take_action()), "Set up now");
    app.status.busy = false;
    act(&mut app, "system-state/vault/join");
    assert!(typed(app.take_action()), "Join");
}

/// A Join aterm cannot make because it cannot read this Mac's vault settings is said in one
/// calm line, never a failure: no Set up now, menu line or tab attention. Refused as a press on
/// the page, it leaves the typed vault and its password in their fields; the launch's answer,
/// admitted without its Join, keeps the typed vault in its field. Either way the person Joins
/// once the settings read.
#[test]
fn a_join_aterm_cannot_make_for_unread_vault_settings_is_said_calmly() {
    const TYPED: &str = "http://typed.local:7484/v1/team";
    for refused in [true, false] {
        let mut app = SystemStateApp::new(Plan::default());
        let unread = status_of("vault-unreadable");
        app.publish(unread.clone());
        app.gesture(true);
        app.password
            .input(TextInputEvent::Commit("test-only-password".into()));
        app.url.select_all();
        app.url.insert(TYPED);
        act(&mut app, "system-state/vault/join");
        let (plan, _, _) = submitted(app.take_action());
        let (note, in_force) = if refused {
            assert!(app.apply_refused());
            (VAULT_UNREAD_NOTE, unread.plan.clone())
        } else {
            app.apply_accepted(plan.clone());
            (VAULT_NOT_JOINED_NOTE, Some(plan))
        };
        app.publish(SetupStatus {
            plan: in_force,
            rows: vec![calm_setup_row(note)],
            ..unread.clone()
        });
        let reviewed = app.reviewed_plan();
        assert!(
            app.status.notes(&reviewed).iter().any(|said| said == note),
            "{:?}",
            app.status.notes(&reviewed)
        );
        assert!(!app.headline().start, "refused {refused}");
        assert!(!app.needs_attention(), "refused {refused}");
        assert!(crate::system_state_access::menu_line(&app.status, false).is_none());
        assert_eq!(app.url.value(), TYPED, "refused {refused}");
        assert_eq!(app.password.value.is_some(), refused);
    }
}

#[test]
fn join_submits_the_vault_with_its_password_once_and_never_shows_it() {
    let mut selection = Selection::all();
    selection.set(Setting::Secrets, false);
    let mut app = SystemStateApp::new(Plan::from(selection));
    app.password
        .input(TextInputEvent::Commit("test-only-password".into()));
    let view = SystemStateViewState::default();
    app.vault_open = Some(true);
    let compiled = compile(&app, &view, 999.0, 900.0);
    assert!(
        compiled
            .semantics
            .iter()
            .all(|node| !node.label.contains("test-only")
                && !format!("{:?}", node.value).contains("test-only"))
    );
    app.url.select_all();
    app.url.insert("http://joined.local:7484/v1/team");
    act(&mut app, "system-state/vault/join");
    let (plan, _, password) = submitted(app.take_action());
    assert!(plan.selection.contains(Setting::Secrets));
    assert_eq!(plan.vault_url, "http://joined.local:7484/v1/team");
    assert_eq!(password.unwrap().as_slice(), b"test-only-password");
    app.apply_accepted(plan.clone());
    assert!(app.password.take().is_none(), "the password went once");
    assert_eq!(app.url.value(), "http://joined.local:7484/v1/team");
}

#[test]
fn the_vault_opens_by_itself_only_when_it_waits_for_input_here() {
    let mut app = SystemStateApp::new(Plan::default());
    assert!(!app.vault_shown());
    let mut status = status_of("set-up");
    status
        .observed
        .retain(|o| o.subject != Subject::Setting(Setting::Secrets));
    status.observed.push(reading(
        Subject::Setting(Setting::Secrets),
        Standing::NeedsYou,
        Some("Not joined"),
    ));
    app.publish(status);
    assert!(app.vault_shown());
    // An approval code waiting on another Mac is the row's note, not input.
    app.publish(status_of("needs-you"));
    assert!(!app.vault_shown());
    act(&mut app, "system-state/row/secrets/disclose");
    assert!(app.vault_shown());
}

#[test]
fn every_scenario_lays_out_at_every_width_with_the_designs_labels() {
    for scenario in [
        "checking",
        "running",
        "debugger",
        "needs-you",
        "vault",
        "set-up",
        "non-alab",
        "failed",
        "mixed",
        "fresh",
    ] {
        for (width, height) in [(999.0, 900.0), (640.0, 480.0), (420.0, 700.0)] {
            let mut app = SystemStateApp::new(Plan::default());
            app.publish(status_of(scenario));
            let view = SystemStateViewState::default();
            let compiled = compile(&app, &view, width, height);
            assert_eq!(label(&compiled, "system-state/brand"), "aterm ALab");
            assert!(
                compiled
                    .semantic(&UiKey::new("system-state/check"))
                    .is_some()
            );
            let running = app.status.running();
            assert_eq!(
                compiled
                    .semantic(&UiKey::new("system-state/stop"))
                    .is_some(),
                running,
                "{scenario}: Stop only while a run is going"
            );
            // Every row is listed, on screen or off it.
            for subject in Subject::all() {
                let key = format!("system-state/row/{}/info", row_slug(subject));
                assert!(
                    compiled.semantic(&UiKey::new(key.clone())).is_some(),
                    "{scenario} {key}"
                );
            }
        }
    }
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status_of("running"));
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 900.0);
    assert_eq!(
        label(&compiled, "system-state/row/dock/info"),
        "Dock: 9 apps, changing now to 3 apps"
    );
    assert_eq!(
        label(&compiled, "system-state/group/1"),
        "Keyboard & Trackpad, Set up"
    );
}

#[test]
fn the_list_scrolls_by_points_and_reveals_rows_and_the_vault_fields() {
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status_of("needs-you"));
    let mut view = SystemStateViewState::default();
    let compiled = compile(&app, &view, 640.0, 480.0);
    let last = UiKey::new("system-state/row/repositories");
    assert!(compiled.semantic(&last).is_some_and(|node| node.offscreen));
    assert!(view.reveal_listed_control(&last));
    let compiled = compile(&app, &view, 640.0, 480.0);
    assert!(compiled.semantic(&last).is_some_and(|node| !node.offscreen));
    // The password field, in the list with the other vault fields, scrolls
    // in whole like any control: it shows bullets, never what was typed.
    app.vault_open = Some(true);
    app.password
        .input(TextInputEvent::Commit("test-only-password".into()));
    let password = UiKey::new("system-state/password");
    view.scroll.set(0.0);
    let compiled = compile(&app, &view, 640.0, 480.0);
    assert!(
        compiled
            .semantic(&password)
            .is_some_and(|node| node.offscreen)
    );
    assert!(view.reveal_listed_control(&password));
    let compiled = compile(&app, &view, 640.0, 480.0);
    let field = compiled.semantic(&password).expect("the field is listed");
    let whole = compiled
        .paint
        .iter()
        .find(|node| node.key == password)
        .expect("the field is drawn")
        .rect;
    assert!(!field.offscreen);
    assert!(
        (field.rect.height - whole.height).abs() < 0.5,
        "the room shows the field whole: {:?} of {whole:?}",
        field.rect
    );
    assert!(!format!("{:?}", field.value).contains("test-only"));
    // From the top of a room taller than a line, where no span the room
    // cuts would be carried past whole, one line down moves a full line.
    view.scroll.set(0.0);
    let room = view.room.get();
    assert!(room > LINE_PX && view.limit.get() > LINE_PX, "{room}");
    assert!(
        !view
            .spans
            .borrow()
            .iter()
            .any(|span| span.top > 0.01 && span.top < LINE_PX - 0.01 && span.bottom > room + 0.01),
        "a span stops the line"
    );
    view.scroll_steps(1, LINE_PX);
    assert!(
        (view.scroll.get() - LINE_PX).abs() < 0.01,
        "a notch moved {}",
        view.scroll.get()
    );
    assert!(!view.reveal_listed_control(&UiKey::new("system-state/row/nowhere")));
}

#[test]
fn the_failure_headlines_show_button_scrolls_to_the_failed_row() {
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status_of("failed"));
    let mut view = SystemStateViewState::default();
    let row = UiKey::new("system-state/row/repositories/info");
    let compiled = compile(&app, &view, 640.0, 480.0);
    assert!(
        compiled
            .semantic(&UiKey::new("system-state/headline/show"))
            .is_some()
    );
    assert!(compiled.semantic(&row).is_some_and(|node| node.offscreen));
    app.act(&mut view, "system-state/headline/show", None);
    assert!(app.take_action().is_none(), "Show only scrolls");
    let compiled = compile(&app, &view, 640.0, 480.0);
    assert!(compiled.semantic(&row).is_some_and(|node| !node.offscreen));
}

/// A vault that waits for an address is shown from the headline: with its fields open, Show
/// brings Join and the fields into view.
#[test]
fn a_waiting_vault_is_shown_from_the_headline() {
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status_of("vault"));
    let mut view = SystemStateViewState::default();
    app.act(&mut view, "system-state/row/secrets/disclose", None);
    let join = UiKey::new("system-state/vault/join");
    let compiled = compile(&app, &view, 640.0, 480.0);
    assert_eq!(
        label(&compiled, HEADLINE),
        "2 steps waiting: Secrets vault, GitHub repositories"
    );
    assert!(
        compiled
            .semantics
            .iter()
            .all(|node| !node.key.as_str().starts_with("system-state/headline/open")),
        "macOS has no pane for the vault"
    );
    assert!(compiled.semantic(&join).is_some_and(|node| node.offscreen));
    app.act(&mut view, "system-state/headline/show", None);
    assert!(app.take_action().is_none(), "Show only scrolls");
    let compiled = compile(&app, &view, 640.0, 480.0);
    for key in [&join, &UiKey::new("system-state/url")] {
        assert!(
            compiled.semantic(key).is_some_and(|node| !node.offscreen),
            "{key:?}"
        );
    }
}

#[test]
fn serious_mode_and_high_contrast_show_the_words_alone() {
    let app = SystemStateApp::new(Plan::default());
    let view = SystemStateViewState::default();
    let viewport = LogicalRect::new(0.0, 0.0, 999.0, 900.0);
    for serious in [false, true] {
        let tree = app.view(&view, &cx(viewport, serious));
        let art = &tree.root.children[0].children[0];
        let UiContent::Custom(spec) = &art.content else {
            panic!("the brand is drawn by its audit");
        };
        assert_eq!(spec.audit_id, crate::native_ui::SYSTEM_STATE_BRAND_AUDIT);
        assert_eq!(BrandSpec::parse(spec.value.as_deref()).kitty, !serious);
        // What the painter draws from that value: the words alone are the
        // prompt, the words and the caret, with no kitty and no ribbon
        // behind them; the caret is the one panel left.
        let mut prims = Vec::new();
        paint_brand(
            &mut prims,
            LogicalRect::new(0.0, 0.0, 400.0, 56.0),
            spec.value.as_deref(),
            &crate::settings::Roles::from_theme(aterm_render::Theme::default()),
        );
        let panels = prims
            .iter()
            .filter(|prim| matches!(prim, crate::widget::DrawPrim::Panel { .. }))
            .count();
        let kitty = prims
            .iter()
            .any(|prim| matches!(prim, crate::widget::DrawPrim::Image { .. }));
        assert_eq!(kitty, !serious, "serious: {serious}");
        if serious {
            assert_eq!(panels, 1, "the caret alone, no ribbon stripe");
        } else {
            assert!(panels > 6, "six ribbon stripes and the caret");
        }
    }
}

#[test]
fn busy_setup_ignores_escape_and_accessibility_text_edits() {
    let mut app = SystemStateApp::new(Plan::default());
    app.status.busy = true;
    let (mut runtime, instance, view) = runtime_with_app(app);
    for event in [
        AppEvent::FocusChanged(Some(UiKey::new("system-state/url"))),
        AppEvent::TextInput(TextInputEvent::Cancel),
        AppEvent::InsertText("unwanted-edit".to_owned()),
    ] {
        runtime.dispatch(instance, view, event).unwrap();
    }
    let Some(NativeApp::SystemState(app)) = runtime.app(instance) else {
        panic!("System State must remain registered");
    };
    assert_eq!(app.url.value(), Plan::default().vault_url);
    assert!(app.actions.is_empty());
}

#[test]
fn a_dispatched_choice_reaches_the_queue_through_the_runtime() {
    let (mut runtime, instance, view) = runtime_with_app(SystemStateApp::new(Plan::default()));
    runtime.system_state_gesture(true);
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("system-state/group/4/leave"),
                value: None,
            }),
        )
        .unwrap();
    assert!(matches!(
        runtime.take_system_state_action(),
        Some(SystemStateAction::Apply(_, Admission::Person))
    ));
}

#[test]
fn password_edit_incremental_clusters_keep_a_valid_caret_and_delete_whole() {
    for (initial, insertion, middle) in [
        ("e", "\u{301}", false),
        ("👩👩", "\u{200d}", true),
        ("🇺", "🇸", false),
    ] {
        let mut password = Password::default();
        password.input(TextInputEvent::Commit(initial.into()));
        if middle {
            password.input(TextInputEvent::Left { extend: false });
        }
        password.input(TextInputEvent::Commit(insertion.into()));
        let projection = password.projection();
        assert_eq!(projection.text, "•");
        assert_eq!(projection.selection, TextSelection::caret("•".len()));
        password.input(TextInputEvent::Backspace);
        assert!(password.projection().text.is_empty());
        assert!(password.take().is_none());
    }
}

#[test]
fn password_edit_arrows_collapse_either_selection_before_typing() {
    for (right, reverse) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut password = Password::default();
        password.input(TextInputEvent::Commit("aβz".into()));
        if reverse {
            password.input(TextInputEvent::Home { extend: true });
        } else {
            password.input(TextInputEvent::SelectAll);
        }
        password.input(if right {
            TextInputEvent::Right { extend: false }
        } else {
            TextInputEvent::Left { extend: false }
        });
        assert_eq!(
            password.projection().selection,
            TextSelection::caret(if right { "•••".len() } else { 0 })
        );
        password.input(TextInputEvent::Commit("x".into()));
        assert_eq!(
            password.take().unwrap().as_str(),
            Some(if right { "aβzx" } else { "xaβz" })
        );
    }
}

#[test]
fn password_projection_never_contains_the_secret_and_grapheme_edits_work() {
    let mut password = Password::default();
    password.input(TextInputEvent::Commit("e\u{301}👩‍💻z".to_owned()));
    assert_eq!(password.projection().text, "•••");
    password.input(TextInputEvent::Left { extend: false });
    password.input(TextInputEvent::Backspace);
    assert_eq!(password.take().unwrap().as_str(), Some("e\u{301}z"));
    assert!(password.projection().text.is_empty());
}

/// A sync the person turned off is their choice (§3.3, §4): the Secrets row reads Sync off in
/// place with its note, is no change to make, and carries a quiet Turn On Sync whose press says
/// whose it was. Negative control: a sync not set up differs, is counted with Set up now, and has
/// no Turn On Sync.
#[test]
fn a_sync_turned_off_reads_as_the_persons_choice_with_a_quiet_way_back_on() {
    let secrets = Subject::Setting(Setting::Secrets);
    let note = "You turned sync off on this Mac";
    let off = Observation {
        wanted: "Sync off".into(),
        note: Some(note.into()),
        ..reading(secrets, Standing::InPlace, Some("Sync off"))
    };
    let mut status = status_of("set-up");
    *status
        .observed
        .iter_mut()
        .find(|row| row.subject == secrets)
        .unwrap() = off;
    let all = rows(&status, Selection::all());
    let row = all.iter().find(|row| row.subject == secrets).unwrap();
    assert!(row.turn_on_sync);
    assert_eq!(
        (row.chip, row.note.as_deref()),
        (Some(Chip::InPlace), Some(note))
    );
    assert!(!row.to_change());
    let line = headline(&status, &all, status.plan.as_ref());
    assert_eq!(
        (line.text.as_str(), line.start),
        ("This Mac is set up", false)
    );
    let mut app = SystemStateApp::new(Plan::from(Selection::all()));
    app.publish(status);
    let compiled = compile(&app, &SystemStateViewState::default(), 900.0, 1400.0);
    assert_eq!(
        label(&compiled, "system-state/row/secrets/sync-on"),
        "Turn On Sync"
    );
    for by_person in [true, false] {
        app.gesture(by_person);
        act(&mut app, "system-state/row/secrets/sync-on");
        assert!(matches!(
            app.take_action(),
            Some(SystemStateAction::TurnSyncOn { by_person: by }) if by == by_person
        ));
    }

    let not_set_up = reading(secrets, Standing::Differs, Some("Not set up"));
    let status = with_readings(Selection::all(), vec![not_set_up]);
    let all = rows(&status, Selection::all());
    let row = all.iter().find(|row| row.subject == secrets).unwrap();
    assert!(!row.turn_on_sync);
    let line = headline(&status, &all, status.plan.as_ref());
    assert_eq!((line.text.as_str(), line.start), ("1 change to make", true));
    app.publish(status);
    act(&mut app, "system-state/row/secrets/sync-on");
    assert!(app.take_action().is_none(), "nothing to turn on");
}

/// In every scenario, from 1x text at 128 points, 1.5x at 160 and 2x at 200 up to a wide window,
/// no text, button face or chip overflows its box or is cut by the card, the vault open too.
/// Narrower, a status chip's pill is wider than the card.
#[test]
fn every_word_and_button_face_is_whole_in_a_narrow_pane_at_every_text_size() {
    use crate::native_appearance::{AppearancePreferences, install_preferences};
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut cut = Vec::new();
    for (scale, least) in [(1.0_f32, 128.0_f32), (1.5, 160.0), (2.0, 200.0)] {
        let _ = install_preferences(AppearancePreferences {
            text_scale: scale,
            ..AppearancePreferences::default()
        });
        for scenario in [
            "checking",
            "running",
            "debugger",
            "needs-you",
            "vault",
            "set-up",
            "non-alab",
            "failed",
            "mixed",
            "fresh",
        ] {
            for width in [
                128.0, 160.0, 200.0, 240.0, 320.0, 420.0, 520.0, 720.0, 999.0,
            ] {
                if width < least {
                    continue;
                }
                let mut app = SystemStateApp::new(Plan::default());
                app.publish(status_of(scenario));
                app.vault_open = Some(true);
                let compiled = compile(&app, &SystemStateViewState::default(), width, 4000.0);
                for node in &compiled.paint {
                    let Some(audit) = compiled.text_fit_audit_line(&node.key) else {
                        continue;
                    };
                    // A text field scrolls its text by design.
                    if !audit.contains("kind=text-field")
                        && (audit.contains("overflow=true")
                            || audit.contains("clip-truncated=true"))
                    {
                        cut.push(format!("{scale}x {width} {scenario}: {audit}"));
                    }
                }
            }
        }
    }
    let _ = install_preferences(AppearancePreferences::default());
    assert!(cut.is_empty(), "{cut:#?}");
}

/// A short window keeps the page's controls in view: the brand and then the title give way
/// before the headline's buttons or the footer leave the window, the brand shows only beside the
/// title, and a taller window never shows less. What shows depends on the window and the text
/// size, not the page's state, while the headline names at most one row; then a window `least`
/// tall (200 points at 1x, 320 at 1.5x, 440 at 2x, 320 wide) keeps the controls in view. A
/// headline naming several rows (every setting as failed, or the vault and the repositories that
/// wait on it) counts its own height and is held to the order alone.
#[test]
fn a_short_window_keeps_the_headline_and_the_footer_in_view() {
    use crate::native_appearance::{AppearancePreferences, install_preferences};
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let controls = [
        "system-state/headline/open",
        "system-state/headline/show",
        "system-state/headline/start",
        "system-state/stop",
        "system-state/check",
        "system-state/restore",
    ];
    for (scale, narrowest, least) in [(1.0_f32, 128, 200), (1.5, 160, 320), (2.0, 200, 440)] {
        let _ = install_preferences(AppearancePreferences {
            text_scale: scale,
            ..AppearancePreferences::default()
        });
        let mut steady = std::collections::HashMap::new();
        let mut failed_many = status_of("failed");
        for observation in &mut failed_many.observed {
            if matches!(observation.subject, Subject::Setting(_)) {
                observation.standing = Standing::Failed;
            }
        }
        for (scenario, status) in [
            "checking",
            "running",
            "debugger",
            "needs-you",
            "vault",
            "set-up",
            "non-alab",
            "failed",
            "check-failed",
            "mixed",
            "fresh",
        ]
        .map(|scenario| (scenario, status_of(scenario)))
        .into_iter()
        .chain([("failed-many", failed_many)])
        {
            // The vault that waits for an address names the repositories that wait on it too.
            let one_name = !matches!(scenario, "failed-many" | "vault");
            let mut app = SystemStateApp::new(Plan::default());
            app.publish(status);
            for width in [narrowest, 240, 320, 420, 520, 720] {
                let mut before: Option<(bool, bool, bool)> = None;
                for height in (160..=1000).step_by(8) {
                    let (w, h) = (width as f32, height as f32);
                    let compiled = compile(&app, &SystemStateViewState::default(), w, h);
                    let shown = |key: &str| {
                        let part = format!("{key}/");
                        compiled.paint.iter().any(|node| {
                            node.key.as_str() == key || node.key.as_str().starts_with(&part)
                        })
                    };
                    let whole = |key: &str| {
                        let part = format!("{key}/");
                        compiled
                            .paint
                            .iter()
                            .filter(|node| {
                                node.key.as_str() == key || node.key.as_str().starts_with(&part)
                            })
                            .all(|node| {
                                !node.clipped() && node.rect.y >= 0.0 && node.rect.bottom() <= h
                            })
                    };
                    let in_view = shown("system-state/check")
                        && shown("system-state/restore")
                        && controls.iter().all(|key| whole(key));
                    let title = shown("settings/page-heading/system-state");
                    let brand = shown("system-state/brand");
                    let at = format!("{scenario} in a {width}×{height} window at {scale}×");
                    assert!(!brand || title, "the brand without the title, {at}");
                    assert!(!title || in_view, "the title over cut controls, {at}");
                    if one_name && width == 320 && height >= least {
                        assert!(in_view, "the controls, {at}");
                    }
                    if one_name && height == 1000 {
                        assert!(brand && title, "the brand and the title, {at}");
                        // The list takes the room left: the footer ends at the page's padding.
                        let foot = compiled
                            .paint
                            .iter()
                            .filter(|node| {
                                ["system-state/check", "system-state/restore"]
                                    .iter()
                                    .any(|key| {
                                        node.key.as_str() == *key
                                            || node.key.as_str().starts_with(&format!("{key}/"))
                                    })
                            })
                            .map(|node| node.rect.bottom())
                            .fold(0.0_f32, f32::max);
                        let pad_y = Metrics::new(w, h).pad_y;
                        assert!(
                            (h - pad_y - foot).abs() < 0.5,
                            "the footer ends {} above the window's foot, {at}",
                            h - foot
                        );
                    }
                    if one_name {
                        assert_eq!(
                            *steady.entry((width, height)).or_insert((title, brand)),
                            (title, brand),
                            "the title and the brand as in every other state, {at}"
                        );
                    }
                    if let Some(before) = before {
                        assert!(
                            in_view >= before.0 && title >= before.1 && brand >= before.2,
                            "less than one point shorter, {at}"
                        );
                    }
                    before = Some((in_view, title, brand));
                }
            }
        }
    }
    let _ = install_preferences(AppearancePreferences::default());
}

/// The page and the view state the runtime holds for `view`.
fn page_in(runtime: &NativeRuntime, view: ViewId) -> (&SystemStateApp, &SystemStateViewState) {
    let instance = runtime
        .instance_by_kind(crate::native_app::AppKind::SystemState)
        .expect("System State is registered");
    let Some(NativeApp::SystemState(app)) = runtime.app(instance) else {
        panic!("System State is registered");
    };
    let Some(AppViewState::SystemState(state)) = runtime.view_state(view) else {
        panic!("its view is attached");
    };
    (app, state)
}

/// Where the list sits in a compiled page.
fn list_rect(compiled: &CompiledUi) -> LogicalRect {
    compiled
        .semantic(&UiKey::new("system-state/groups"))
        .expect("the list is in the tree")
        .rect
}

/// `key`'s paint node, where the page paints it.
fn painted<'a>(compiled: &'a CompiledUi, key: &str) -> Option<&'a crate::native_ui::PaintNode> {
    compiled.paint.iter().find(|node| node.key.as_str() == key)
}

/// Whether `key` is painted whole between `top` and `bottom`.
fn whole_within(compiled: &CompiledUi, key: &str, top: f32, bottom: f32) -> bool {
    painted(compiled, key)
        .is_some_and(|node| node.rect.y >= top - 0.01 && node.rect.bottom() <= bottom + 0.01)
}

#[test]
fn a_short_list_steps_no_further_than_its_room_and_shows_every_note_row_and_control_whole() {
    use crate::native_appearance::{AppearancePreferences, install_preferences};
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    for scale in [1.0_f32, 1.5, 2.0] {
        let _ = install_preferences(AppearancePreferences {
            text_scale: scale,
            ..AppearancePreferences::default()
        });
        // The window a one-line headline, the footer and a list one row
        // tall fill at this text size: 124pt at 1×.
        let floor = 34.0 + 90.0 * scale;
        for (scenario, above) in [
            ("check-failed", 0.0_f32),
            ("check-failed", 22.0),
            ("check-failed", 76.0),
            ("vault", 0.0),
            ("vault", 126.0),
            ("needs-you", 36.0),
        ] {
            let height = floor + above * scale;
            let at = format!("{scenario} in a 720×{height} window at {scale}×");
            let mut app = SystemStateApp::new(Plan::default());
            app.publish(status_of(scenario));
            let (mut runtime, instance, view) = runtime_with_app(app);
            let compile_now = |runtime: &NativeRuntime| {
                let (app, state) = page_in(runtime, view);
                compile(app, state, 720.0, height)
            };
            let mut compiled = compile_now(&runtime);
            let list = list_rect(&compiled);
            let room = page_in(&runtime, view).1.room.get();
            if scenario == "check-failed" && above == 0.0 {
                // The short-window rule's floor: a list one row tall, whose
                // room over the strip is shorter than one wheel line at 1×.
                assert!(
                    (list.height - Metrics::new(720.0, height).row_h).abs() < 0.01,
                    "{at}"
                );
                assert!(scale > 1.0 || room < LINE_PX, "{at}");
            }
            // Every note, group, row and control the list places, with where
            // it must show whole: in the room where it fits there, else in the
            // list where it fits that.
            let spans: Vec<(String, f32)> = page_in(&runtime, view)
                .1
                .spans
                .borrow()
                .iter()
                .filter(|span| span.bottom - span.top <= list.height + 0.01)
                .map(|span| {
                    let tall = span.bottom - span.top > room + 0.01;
                    let bottom = if tall { list.bottom() } else { list.y + room };
                    (span.key.to_string(), bottom)
                })
                .collect();
            // At 1× the list holds a row and a note whole; at a larger text
            // size a row sets its value under its name and may stand taller.
            if scale == 1.0 {
                assert!(
                    spans
                        .iter()
                        .any(|(key, _)| key.starts_with("system-state/row/")),
                    "{at}: {spans:?}"
                );
            }
            if scale == 1.0 && scenario == "check-failed" {
                assert!(
                    spans
                        .iter()
                        .any(|(key, _)| key.starts_with("system-state/note/")),
                    "{at}: {spans:?}"
                );
            }
            for direction in [1, -1] {
                let mut seen = std::collections::BTreeSet::new();
                let mut longest = 0.0_f32;
                let mut path = Vec::new();
                loop {
                    let (before, limit) = {
                        let state = page_in(&runtime, view).1;
                        (state.scroll.get(), state.limit.get())
                    };
                    // Where a step may stop short: going down, a span's top at
                    // the list's top; going up, a span's bottom at the room's
                    // foot, or a taller one's top at the list's top.
                    let edges: Vec<f32> = page_in(&runtime, view)
                        .1
                        .spans
                        .borrow()
                        .iter()
                        .flat_map(|span| {
                            let foot = (direction < 0).then_some(span.bottom - room);
                            std::iter::once(span.top).chain(foot)
                        })
                        .collect();
                    path.push(before);
                    for (key, bottom) in &spans {
                        if whole_within(&compiled, key, list.y, *bottom) {
                            seen.insert(key.clone());
                        }
                    }
                    runtime
                        .dispatch(instance, view, AppEvent::ScrollLines(direction))
                        .unwrap();
                    let after = page_in(&runtime, view).1.scroll.get();
                    let step = (after - before).abs();
                    longest = longest.max(step);
                    if step > 0.01
                        && step < room.min(LINE_PX) - 0.01
                        && after > 0.01
                        && after < limit - 0.01
                    {
                        assert!(
                            edges.iter().any(|edge| (edge - after).abs() < 0.01),
                            "a step of {step} from {before} stops at no span's edge, \
                             scrolling {direction:+}, {at}"
                        );
                    }
                    if (after - before).abs() < 0.01 {
                        break;
                    }
                    compiled = compile_now(&runtime);
                }
                let missed: Vec<&String> = spans
                    .iter()
                    .map(|(key, _)| key)
                    .filter(|key| !seen.contains(*key))
                    .collect();
                assert!(
                    missed.is_empty(),
                    "{missed:?} never show whole, scrolling {direction:+} through {path:?}, \
                     {at}"
                );
                assert!(
                    longest <= room.min(LINE_PX) + 0.01,
                    "a step of {longest} past the room's {room}, {at}"
                );
            }
        }
    }
    let _ = install_preferences(AppearancePreferences::default());
}

#[test]
fn a_focused_vault_field_stays_in_view_as_the_list_shrinks_and_shows_when_typed_into() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status_of("vault"));
    // The person opened the vault's fields under its row.
    act(&mut app, "system-state/row/secrets/disclose");
    let (mut runtime, instance, view) = runtime_with_app(app);
    let compile_at = |runtime: &NativeRuntime, height: f32| {
        let (app, state) = page_in(runtime, view);
        compile(app, state, 720.0, height)
    };
    let shown = |compiled: &CompiledUi, key: &str| {
        let part = format!("{key}/");
        compiled
            .paint
            .iter()
            .any(|node| node.key.as_str() == key || node.key.as_str().starts_with(&part))
    };
    // Whether `key` is painted whole in the list's room, or in the list
    // from its top where it stands taller than the room.
    let in_view = |runtime: &NativeRuntime, compiled: &CompiledUi, key: &str| {
        let list = list_rect(compiled);
        let room = page_in(runtime, view).1.room.get();
        let tall = painted(compiled, key).is_some_and(|node| node.rect.height > room + 0.01);
        whole_within(
            compiled,
            key,
            list.y,
            if tall { list.bottom() } else { list.y + room },
        )
    };
    let url = UiKey::new("system-state/url");
    let compiled = compile_at(&runtime, 249.0);
    assert!(
        shown(&compiled, "settings/page-heading/system-state")
            && !shown(&compiled, "system-state/brand"),
        "at 720×249 the title shows and the brand does not"
    );
    runtime
        .dispatch(instance, view, AppEvent::FocusChanged(Some(url.clone())))
        .unwrap();
    let compiled = compile_at(&runtime, 249.0);
    assert!(
        in_view(&runtime, &compiled, url.as_str()),
        "focus reveals the field"
    );
    // One point taller, the brand comes back and the list loses its height.
    let room = page_in(&runtime, view).1.room.get();
    let compiled = compile_at(&runtime, 250.0);
    assert!(
        shown(&compiled, "system-state/brand"),
        "the brand at 720×250"
    );
    assert!(page_in(&runtime, view).1.room.get() + 30.0 < room);
    assert!(
        in_view(&runtime, &compiled, url.as_str()),
        "the focused field stays whole in a room the brand shrank"
    );
    // A window that shrinks a point at a time keeps it in view too.
    runtime
        .dispatch(instance, view, AppEvent::ScrollLines(-10_000))
        .unwrap();
    compile_at(&runtime, 400.0);
    runtime
        .dispatch(instance, view, AppEvent::FocusChanged(Some(url.clone())))
        .unwrap();
    for height in (124..=400).rev() {
        let compiled = compile_at(&runtime, height as f32);
        assert!(
            in_view(&runtime, &compiled, url.as_str()),
            "the focused field in a 720×{height} window"
        );
    }
    // A caret move, a selection or a submit leaves the disclosure as it
    // stood.
    let stood = page_in(&runtime, view).0.vault_open;
    for event in [
        TextInputEvent::Left { extend: false },
        TextInputEvent::Home { extend: true },
        TextInputEvent::SelectAll,
        TextInputEvent::Submit,
        TextInputEvent::Preedit {
            text: String::new(),
            selection: None,
        },
    ] {
        runtime
            .dispatch(instance, view, AppEvent::TextInput(event.clone()))
            .unwrap();
        assert_eq!(page_in(&runtime, view).0.vault_open, stood, "{event:?}");
    }
    // A focused field scrolled to a sliver the list leaves unpainted is one
    // the person scrolled away from: a room that shrinks leaves it there.
    compile_at(&runtime, 249.0);
    let bottom = page_in(&runtime, view)
        .1
        .spans
        .borrow()
        .iter()
        .find(|span| *span.key == *url.as_str())
        .expect("the address field is listed")
        .bottom;
    let sliver = bottom - 3.0;
    page_in(&runtime, view).1.scroll.set(sliver);
    compile_at(&runtime, 249.0);
    assert!((page_in(&runtime, view).1.scroll.get() - sliver).abs() < 0.01);
    compile_at(&runtime, 250.0);
    assert!(
        (page_in(&runtime, view).1.scroll.get() - sliver).abs() < 0.01,
        "a sliver of the focused field leaves the list where it stood"
    );
    // Scrolled out of sight, a field shows again once it is typed into.
    for (key, event) in [
        (
            "system-state/url",
            AppEvent::TextInput(TextInputEvent::Commit("x".into())),
        ),
        ("system-state/url", AppEvent::InsertText("y".into())),
        (
            "system-state/password",
            AppEvent::TextInput(TextInputEvent::Commit("p".into())),
        ),
    ] {
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::FocusChanged(Some(UiKey::new(key))),
            )
            .unwrap();
        runtime
            .dispatch(instance, view, AppEvent::ScrollLines(-10_000))
            .unwrap();
        let compiled = compile_at(&runtime, 250.0);
        assert!(painted(&compiled, key).is_none(), "{key} scrolled away");
        runtime.dispatch(instance, view, event).unwrap();
        // A typed address the worker has not read yet leaves the row
        // nothing to wait on; the vault stays open for the draft.
        assert!(
            page_in(&runtime, view).0.vault_shown(),
            "{key} typed into keeps the vault open"
        );
        let compiled = compile_at(&runtime, 250.0);
        assert!(in_view(&runtime, &compiled, key), "{key} typed into shows");
    }
    let (app, _) = page_in(&runtime, view);
    assert!(app.url.value().contains('x') && app.url.value().contains('y'));
}

/// The village paint node's spec in a compiled page, where the page shows one.
fn village(compiled: &CompiledUi) -> Option<String> {
    compiled
        .paint
        .iter()
        .find(|node| node.key.as_str() == "system-state/village")
        .map(|node| match &node.content {
            UiContent::Custom(spec) => {
                assert_eq!(spec.audit_id, crate::native_ui::RAINBOW_BANNER_AUDIT);
                spec.value.clone().expect("the village carries its spec")
            }
            other => panic!("the village is drawn by its audit: {other:?}"),
        })
}

/// What the village painter draws from `spec` in a 640 × 48 strip.
fn village_prims(spec: &str) -> Vec<crate::widget::DrawPrim> {
    let mut prims = Vec::new();
    crate::settings::paint_rainbow_kitty_village(
        &mut prims,
        0.0,
        0.0,
        640.0,
        48.0,
        [0x18, 0x16, 0x22],
        [0x38, 0x34, 0x4C],
        spec,
    );
    prims
}

#[test]
fn the_village_has_one_station_per_setting_in_the_rows_words() {
    let dock = Subject::Setting(Setting::Dock);
    let natural = Subject::Setting(Setting::NaturalScrolling);
    let mut selection = Selection::all();
    selection.set(Setting::Messages, false);
    let mut observed: Vec<Observation> = Subject::all()
        .into_iter()
        .map(|subject| reading(subject, Standing::InPlace, Some("as wanted")))
        .collect();
    let mut set = |subject: Subject, standing: Standing| {
        for reading in observed.iter_mut().filter(|o| o.subject == subject) {
            reading.standing = standing;
        }
    };
    set(natural, Standing::Differs);
    set(dock, Standing::Differs);
    set(Subject::Setting(Setting::Desktop), Standing::Waiting);
    set(Subject::Setting(Setting::Secrets), Standing::Failed);
    set(Subject::Access(Access::Debugger), Standing::NeedsYou);
    set(Subject::Setting(Setting::Repositories), Standing::Unknown);
    let mut status = with_readings(selection, observed);
    status.busy = true;
    status.running = vec![Setting::Dock];
    let rows = rows(&status, selection);
    let spec = village_spec(&status, &rows, &Plan::from(selection), 480);
    // Caps Lock, Natural scrolling, Trackpad, Desktop, Widgets, Dock,
    // Messages, FaceTime, Spotlight, Permissions, Secrets, Repositories.
    let managed = rows.iter().filter(|row| row.managed).count();
    let done = rows.iter().filter(|row| row.settled()).count();
    assert_eq!(
        spec,
        format!("phase=480;busy=1;done={done};total={managed};states=GDGDGW_GGEED")
    );
    // The painter reads back what the page says.
    let (phase, busy, parsed_done, total, states) =
        crate::settings::parse_rainbow_kitty_village_spec(&spec);
    assert_eq!(
        (phase, busy, parsed_done, total),
        (480, true, done, managed)
    );
    assert_eq!(states.len(), Setting::ALL.len());
}

/// The headline and the brand reach a screen reader in words (§3.1): each lowers to
/// a Label, whose words AppKit and AT-SPI read from its value, in one line or wrapped.
#[cfg(a11y_tree)]
#[test]
fn the_headline_and_the_brand_publish_their_words_as_a_labels_value() {
    use aterm_a11y::model::Role;
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status_of("running"));
    let running = "Setting up this Mac… 11 of 16";
    for width in [999.0, 220.0] {
        let compiled = compile(&app, &SystemStateViewState::default(), width, 900.0);
        let projection =
            crate::native_accessibility::project_native_accessibility(&compiled, None).unwrap();
        let wrapped = compiled
            .paint
            .iter()
            .any(|node| node.key.as_str().starts_with(&format!("{HEADLINE}/line-")));
        assert_eq!(
            wrapped,
            width < 999.0,
            "{width}: the headline wraps only when narrow"
        );
        for (key, words) in [(HEADLINE, running), ("system-state/brand", "aterm ALab")] {
            let id = projection
                .id_for_key(&UiKey::new(key))
                .unwrap_or_else(|| panic!("{width}: {key} is published"));
            let (_, node) = projection
                .update()
                .nodes
                .iter()
                .find(|(candidate, _)| *candidate == id)
                .unwrap();
            assert_eq!(node.role(), Role::Label, "{width}: {key}");
            assert_eq!(node.value(), Some(words), "{width}: {key}");
        }
    }
}

#[test]
fn the_village_walks_beside_the_lockup_while_a_run_goes() {
    let viewport = LogicalRect::new(0.0, 0.0, 999.0, 900.0);
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status_of("running"));
    let view = SystemStateViewState::default();
    let frame = |app: &SystemStateApp, motion: ViewMotionCx| {
        let cx = ViewCx {
            motion,
            ..cx(viewport, false)
        };
        app.view(&view, &cx).compile(viewport).unwrap()
    };
    let first = frame(&app, ViewMotionCx::default());
    let spec0 = village(&first).expect("a wide page shows the village");
    // Its count is the meter's.
    let meter = first
        .paint
        .iter()
        .find(|node| node.key.as_str() == "system-state/meter")
        .and_then(|node| match &node.content {
            UiContent::Custom(spec) => spec.value.clone(),
            _ => None,
        })
        .expect("the meter shows while a run goes");
    let (done, total) = meter.split_once(',').unwrap();
    assert!(
        spec0.starts_with(&format!("phase=0;busy=1;done={done};total={total};states=")),
        "{spec0}"
    );
    assert!(
        spec0.contains("states=GGGGGW"),
        "the running Dock works: {spec0}"
    );
    // Only the lockup is the mark: the village names nothing aloud.
    assert_eq!(label(&first, "system-state/brand"), "aterm ALab");
    assert!(
        first
            .semantic(&UiKey::new("system-state/village"))
            .is_none()
    );

    assert!(
        app.tick(true, true, view.village_walks()),
        "a busy tick repaints a page that shows the village"
    );
    let second = frame(&app, ViewMotionCx::default());
    let spec1 = village(&second).unwrap();
    assert_eq!(spec1, spec0.replacen("phase=0", "phase=120", 1));
    assert_ne!(
        format!("{:?}", village_prims(&spec0)),
        format!("{:?}", village_prims(&spec1)),
        "the kitties walk across busy ticks"
    );

    // Reduced motion: the kitties stand, and a busy tick owes no repaint.
    let still = ViewMotionCx {
        system_reduced: true,
        ..ViewMotionCx::default()
    };
    let held = village(&frame(&app, still)).unwrap();
    assert!(held.starts_with("phase=0;"), "{held}");
    assert!(!app.tick(true, true, view.village_walks()));
    assert_eq!(village(&frame(&app, still)).unwrap(), held);

    // A window too short for the brand leaves the village out with it, and a
    // busy tick then owes it no repaint.
    let _ = frame(&app, ViewMotionCx::default());
    assert!(app.tick(true, true, view.village_walks()));
    let short = LogicalRect::new(0.0, 0.0, 999.0, 200.0);
    let page = app.view(&view, &cx(short, false)).compile(short).unwrap();
    assert!(village(&page).is_none());
    assert!(
        !app.tick(true, true, view.village_walks()),
        "a busy tick repaints a village the page leaves out"
    );

    // A run that ends leaves the scene where it stands.
    let mut done_status = status_of("set-up");
    done_status.busy = false;
    app.publish(done_status);
    let _ = frame(&app, ViewMotionCx::default());
    assert!(
        !app.tick(true, true, view.village_walks()),
        "an idle page does not repaint for the village"
    );
}

#[test]
fn the_village_shows_only_where_the_brand_row_has_room_and_the_kitty_may_show() {
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status_of("running"));
    let view = SystemStateViewState::default();
    let at = |app: &SystemStateApp, width: f32, serious: bool| {
        let viewport = LogicalRect::new(0.0, 0.0, width, 900.0);
        village(
            &app.view(&view, &cx(viewport, serious))
                .compile(viewport)
                .unwrap(),
        )
    };
    assert!(at(&app, 999.0, false).is_some());
    assert!(
        at(&app, 320.0, false).is_none(),
        "a narrow page shows the lockup alone"
    );
    assert!(
        at(&app, 999.0, true).is_none(),
        "serious mode shows the words alone"
    );
    // A page that does not know the plan yet draws no scene of it.
    let opened = SystemStateApp::opened();
    assert!(at(&opened, 999.0, false).is_none());
}

#[test]
fn rainbow_kitty_village_builds_and_synergies_stay_stable_when_state_is_stable_while_kitties_roam()
{
    let sky = [0x18, 0x16, 0x22];
    let rim = [0x38, 0x34, 0x4C];
    let arch_for = |states: &str, phase_ms: u64| -> Vec<crate::widget::DrawPrim> {
        let spec = format!(
            "phase={phase_ms};busy=0;done={};total={};states={states}",
            states.chars().filter(|&c| c == 'G').count(),
            states.len()
        );
        let (p, b, done, total, parsed) = crate::settings::parse_rainbow_kitty_village_spec(&spec);
        let mut prims = Vec::new();
        crate::settings::paint_rainbow_kitty_village_architecture(
            &mut prims, 0.0, 0.0, 640.0, 48.0, sky, rim, p, b, done, total, &parsed,
        );
        prims
    };
    let kitties_for = |phase_ms: u64| -> Vec<crate::widget::DrawPrim> {
        let mut prims = Vec::new();
        crate::settings::paint_rainbow_kitty_village_kitties(
            &mut prims, 0.0, 640.0, 40.0, phase_ms, false,
        );
        prims
    };

    // The village fills in as stations converge D → G, with neighbour synergies.
    let pending = arch_for("DDDDDDDDDDDD", 0);
    let one_done = arch_for("GDDDDDDDDDDD", 0);
    let pair_synergy = arch_for("GGDDDDDDDDDD", 0);
    let district_synergy = arch_for("GGGDDDDDDDDD", 0);
    let all_verified = arch_for("GGGGGGGGGGGG", 0);
    assert!(
        pending.len() < one_done.len()
            && one_done.len() < pair_synergy.len()
            && pair_synergy.len() < district_synergy.len()
            && district_synergy.len() < all_verified.len(),
        "{} < {} < {} < {} < {}",
        pending.len(),
        one_done.len(),
        pair_synergy.len(),
        district_synergy.len(),
        all_verified.len(),
    );

    // With no station working, the buildings never move with the phase.
    for stable_states in [
        "GGGGGGGGGGGG",
        "GDG_GGGGGGGG",
        "DDDDDDDDDDDD",
        "GGGGGGGGGGGGGGGGGG",
    ] {
        let base = format!("{:?}", arch_for(stable_states, 0));
        for phase in [120, 480, 1440, 5200] {
            assert_eq!(
                base,
                format!("{:?}", arch_for(stable_states, phase)),
                "{stable_states} at phase={phase}"
            );
        }
    }

    // The kitties roam all the same.
    assert_ne!(
        format!("{:?}", kitties_for(0)),
        format!("{:?}", kitties_for(360))
    );
    assert_ne!(
        format!("{:?}", kitties_for(360)),
        format!("{:?}", kitties_for(1200))
    );
}

/// The village walks only on a page someone can see: a page whose tab is in
/// the background takes no step and asks for no repaint on a busy tick, and
/// walks on from where it stood once shown again.
#[test]
fn a_hidden_page_owes_no_repaint_for_the_village() {
    let viewport = LogicalRect::new(0.0, 0.0, 999.0, 900.0);
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status_of("running"));
    let view = SystemStateViewState::default();
    let phase = |app: &SystemStateApp| {
        let compiled = app
            .view(&view, &cx(viewport, false))
            .compile(viewport)
            .unwrap();
        village(&compiled).expect("village shows")
    };
    assert!(phase(&app).starts_with("phase=0;busy=1;"));
    // The tab goes to the background: nothing renders the page any more.
    let asked = (0..40)
        .filter(|_| app.tick(true, false, view.village_walks()))
        .count();
    assert_eq!(
        asked, 0,
        "a page nobody sees asked for {asked} repaints in 40 ticks"
    );
    assert!(phase(&app).starts_with("phase=0;"), "it stood still");
    assert!(
        app.tick(true, true, view.village_walks()),
        "shown again, it walks"
    );
    assert!(phase(&app).starts_with("phase=120;"));
}

/// Each view of the one page renders at its own window's size, and a busy
/// tick walks the village only where a shown view drew it walking: a second
/// window too short or too narrow for the village neither freezes the first
/// window's kitties nor, shown alone, repaints a page with nothing moving,
/// whichever of the two rendered last.
#[test]
fn a_shown_views_own_render_decides_whether_a_busy_tick_walks() {
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status_of("running"));
    let (mut runtime, instance, tall) = runtime_with_app(app);
    let other = ViewId::from_stored(2);
    runtime
        .attach_view(other, instance, AppViewState::SystemState(Box::default()))
        .unwrap();
    let render = |runtime: &NativeRuntime, view: ViewId, width: f32, height: f32| {
        let viewport = LogicalRect::new(0.0, 0.0, width, height);
        let tree = runtime
            .render(instance, view, &cx(viewport, false))
            .unwrap();
        village(&tree.compile(viewport).unwrap())
    };
    let walks = |runtime: &NativeRuntime, view: ViewId| match runtime.view_state(view) {
        Some(AppViewState::SystemState(state)) => state.village_walks(),
        _ => panic!("{view:?} is a System State view"),
    };
    for (width, height) in [(999.0, 200.0), (320.0, 900.0)] {
        for tall_last in [false, true] {
            let mut order = [(tall, 999.0, 900.0), (other, width, height)];
            if tall_last {
                order.reverse();
            }
            for (view, w, h) in order {
                let shows = render(&runtime, view, w, h).is_some();
                assert_eq!(shows, view == tall, "{view:?} at {w}x{h}");
            }
            let case = format!("{width}x{height}, tall rendered last: {tall_last}");
            assert!(walks(&runtime, tall), "{case}");
            assert!(!walks(&runtime, other), "{case}");
            assert!(
                runtime.tick_system_state(true, true, &[tall, other]),
                "both shown, the tall window's kitties walk ({case})"
            );
            assert!(
                runtime.tick_system_state(true, true, &[tall]),
                "the tall window alone walks ({case})"
            );
            assert!(
                !runtime.tick_system_state(true, true, &[other]),
                "a page shown only where its village is left out owes no repaint ({case})"
            );
        }
    }
}

/// Check again and Restore keep the worker busy, and neither is a run: the
/// village stands and says no run is going, and their ticks owe no repaint.
#[test]
fn the_village_stands_through_check_again_and_restore() {
    let viewport = LogicalRect::new(0.0, 0.0, 999.0, 900.0);
    for operation in [SetupOperation::Check, SetupOperation::Restore] {
        let mut app = SystemStateApp::new(Plan::default());
        let mut status = status_of("set-up");
        status.busy = true;
        status.operation = Some(operation);
        app.publish(status);
        let view = SystemStateViewState::default();
        let compiled = app
            .view(&view, &cx(viewport, false))
            .compile(viewport)
            .unwrap();
        let spec = village(&compiled).expect("village shows");
        assert!(
            spec.contains(";busy=0;"),
            "{operation:?} reads as a run: {spec}"
        );
        assert!(
            !app.tick(true, true, view.village_walks()),
            "{operation:?} repaints every tick for the village"
        );
    }
}

/// The page's own ticks are the walk's only clock: between runs the kitties
/// stand where they are, whatever frame clock repaints the page.
#[test]
fn idle_kitties_stand_still_whatever_repaints_the_page() {
    let viewport = LogicalRect::new(0.0, 0.0, 999.0, 900.0);
    let app = SystemStateApp::new(Plan::default());
    let view = SystemStateViewState::default();
    let spec = |phase: u64| {
        let cx = ViewCx {
            animation_phase_ms: phase,
            ..cx(viewport, false)
        };
        village(&app.view(&view, &cx).compile(viewport).unwrap()).expect("village shows")
    };
    assert_eq!(spec(0), spec(4_321));
    assert!(
        spec(4_321).starts_with("phase=0;busy=0;"),
        "{}",
        spec(4_321)
    );
}

/// A reading of `subject` with its note.
fn observed_with_note(
    subject: Subject,
    standing: Standing,
    now: Option<&str>,
    note: Option<&str>,
) -> Observation {
    Observation {
        note: note.map(str::to_owned),
        ..reading(subject, standing, now)
    }
}

/// Every state the page draws, by name, each with whether Stop was pressed.
fn page_states() -> Vec<(String, SetupStatus, bool)> {
    let mut out: Vec<(String, SetupStatus, bool)> = Vec::new();
    for name in [
        "checking",
        "running",
        "debugger",
        "needs-you",
        "vault",
        "set-up",
        "non-alab",
        "failed",
        "mixed",
        "restore-blocked",
        "fresh",
    ] {
        out.push((name.to_owned(), status_of(name), false));
    }
    let secrets = Subject::Setting(Setting::Secrets);
    let repos = Subject::Setting(Setting::Repositories);
    let replace = |mut status: SetupStatus, o: Observation| {
        if let Some(slot) = status.observed.iter_mut().find(|x| x.subject == o.subject) {
            *slot = o;
        } else {
            status.observed.push(o);
        }
        status
    };
    // Sync off: In place, with its note and the Turn On Sync button.
    let mut off = observed_with_note(
        secrets,
        Standing::InPlace,
        Some("Sync off"),
        Some("You turned sync off on this Mac"),
    );
    off.wanted = "Sync off".into();
    out.push((
        "sync-off".into(),
        replace(status_of("set-up"), off.clone()),
        false,
    ));
    // A join waiting for an admin on a Mac whose sync is off: its longest note.
    off.note = Some(
        "You turned sync off; once it's on, approving 7QK2-M9WD-4HTX-PL3B on an admin Mac finishes the join"
            .into(),
    );
    out.push((
        "pending-sync-off".into(),
        replace(status_of("set-up"), off),
        false,
    ));
    // The person's own project build folders Spotlight indexes: a quiet note.
    out.push((
        "spotlit-projects".into(),
        replace(
            status_of("set-up"),
            observed_with_note(
                Subject::Setting(Setting::Spotlight),
                Standing::InPlace,
                Some("as wanted"),
                Some(
                    "Spotlight indexes 12 of your project build folders; renaming each to its \
                     .noindex name and linking the old name to it keeps it out",
                ),
            ),
        ),
        false,
    ));
    // Not set up: differs, with the long note; Set up now on the headline.
    out.push((
        "not-set-up".into(),
        replace(
            status_of("set-up"),
            observed_with_note(
                secrets,
                Standing::Differs,
                Some("Not set up"),
                Some("Not set up to sync this Mac; Set up now installs it"),
            ),
        ),
        false,
    ));
    // Pending with password: the longest vault note.
    out.push((
        "pending-retype".into(),
        replace(
            status_of("set-up"),
            observed_with_note(
                secrets,
                Standing::NeedsYou,
                Some("Awaiting code"),
                Some("Approve on an admin Mac: 7QK2-M9WD-4HTX-PL3B, or retype the join password"),
            ),
        ),
        false,
    ));
    // Fleeting.
    out.push((
        "fleeting".into(),
        replace(
            status_of("set-up"),
            observed_with_note(
                secrets,
                Standing::NeedsYou,
                Some("Out of sync"),
                Some("Move aterm to Applications and open it from there"),
            ),
        ),
        false,
    ));
    // Waiting only: "1 step waiting: GitHub repositories" with Show.
    out.push((
        "waiting-only".into(),
        replace(
            status_of("set-up"),
            observed_with_note(
                repos,
                Standing::Waiting,
                None,
                Some("GitHub can't be reached"),
            ),
        ),
        false,
    ));
    // Failure notes under the headline, long.
    let mut failed = status_of("failed");
    failed.rows = vec![
        SetupStatusRow {
            name: "GitHub repositories".into(),
            status: "error".into(),
            detail: "cloning someone/aterm-very-long-repository-name-for-probing failed: the remote end hung up unexpectedly while 1234 objects were left".into(),
        },
        SetupStatusRow {
            name: "Check again".into(),
            status: "not started".into(),
            detail: "Try again.".into(),
        },
    ];
    out.push(("failure-notes".into(), failed, false));
    // Restore going.
    let mut restoring = status_of("set-up");
    restoring.busy = true;
    restoring.operation = Some(SetupOperation::Restore);
    out.push(("restoring".into(), restoring, false));
    // Running, Stop pressed.
    out.push(("stopping".into(), status_of("running"), true));
    out
}

/// Every state of the page keeps its words whole and apart, at every 8 points
/// from 320 to 800 and 1x, 1.5x and 2x text, the painter's own overlap audit
/// included: Sync off and its Turn On Sync, Not set up, a pending join's
/// longest note, a disk-image copy, waiting only with Show, failure notes under
/// the headline, Restore going and Stop pressed among them.
#[test]
fn every_state_keeps_its_words_whole_and_apart_from_320_to_800() {
    use crate::native_appearance::{AppearancePreferences, install_preferences};
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut cut = std::collections::BTreeMap::<String, Vec<String>>::new();
    let scenarios = page_states();
    for scale in [1.0_f32, 1.5, 2.0] {
        let _ = install_preferences(AppearancePreferences {
            text_scale: scale,
            ..AppearancePreferences::default()
        });
        for (name, status, stop) in &scenarios {
            for vault_open in [true, false] {
                let mut width = 320.0_f32;
                while width <= 800.0 {
                    let mut app = SystemStateApp::new(Plan::default());
                    app.publish(status.clone());
                    app.vault_open = Some(vault_open);
                    if *stop {
                        app.stopping = true;
                    }
                    let compiled = compile(&app, &SystemStateViewState::default(), width, 4000.0);
                    for failure in compiled.fit_failures() {
                        if failure.contains("kind=text-field") {
                            continue;
                        }
                        let key = format!("{scale}x {name} open={vault_open}");
                        let shape: String = failure
                            .split_whitespace()
                            .filter(|w| {
                                w.starts_with("key=")
                                    || w.starts_with("a=")
                                    || w.starts_with("b=")
                                    || w.starts_with("paint-")
                            })
                            .collect::<Vec<_>>()
                            .join(" ");
                        let entry = cut.entry(format!("{key} :: {shape}")).or_default();
                        if entry.len() < 3 {
                            entry.push(format!("w={width}: {failure}"));
                        }
                    }
                    for node in &compiled.paint {
                        let Some(audit) = compiled.text_fit_audit_line(&node.key) else {
                            continue;
                        };
                        if !audit.contains("kind=text-field")
                            && audit.contains("clip-truncated=true")
                        {
                            let entry = cut
                                .entry(format!(
                                    "{scale}x {name} open={vault_open} :: clip-truncated {}",
                                    node.key.as_str()
                                ))
                                .or_default();
                            if entry.len() < 3 {
                                entry.push(format!("w={width}: {audit}"));
                            }
                        }
                    }
                    width += 8.0;
                }
            }
        }
    }
    let _ = install_preferences(AppearancePreferences::default());
    assert!(
        cut.is_empty(),
        "{} kinds of cut or overlap:\n{cut:#?}",
        cut.len()
    );
}

/// The same at display scales 1x and 1.5x (the device pixels the brand's kitty
/// is sized in), 1x text: the brand's words never run under its kitty. 2x is
/// the display the sweep above renders at (`cx`), with 1x text among its rungs.
#[test]
fn every_state_keeps_its_words_whole_at_every_display_scale() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut cut = std::collections::BTreeMap::<String, Vec<String>>::new();
    for display in [1.0_f32, 1.5] {
        for (name, status, stop) in page_states() {
            let mut width = 320.0_f32;
            while width <= 800.0 {
                let mut app = SystemStateApp::new(Plan::default());
                app.publish(status.clone());
                app.vault_open = Some(true);
                app.stopping = stop;
                let viewport = LogicalRect::new(0.0, 0.0, width, 4000.0);
                let cx = ViewCx {
                    terminal_scale: display,
                    ..cx(viewport, false)
                };
                let compiled = app
                    .view(&SystemStateViewState::default(), &cx)
                    .compile(viewport)
                    .unwrap();
                for failure in compiled.fit_failures() {
                    if failure.contains("kind=text-field") {
                        continue;
                    }
                    let shape: String = failure
                        .split_whitespace()
                        .filter(|w| {
                            w.starts_with("key=") || w.starts_with("a=") || w.starts_with("b=")
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    let entry = cut
                        .entry(format!("{display}d {name} :: {shape}"))
                        .or_default();
                    if entry.len() < 2 {
                        entry.push(format!("w={width}: {failure}"));
                    }
                }
                width += 8.0;
            }
        }
    }
    assert!(cut.is_empty(), "{} kinds:\n{cut:#?}", cut.len());
}

/// The kitty stands behind the caret as `assets/aterm-rainbow-kitty.png` draws it,
/// measured by the pixels it draws: its drawn left edge is under the caret, so the caret
/// covers its haunch, its tail rises past the caret's top, and none of it inks the words.
#[test]
fn the_caret_covers_the_kittys_haunch_as_the_brand_image_draws_it() {
    use crate::widget::{ArtSpan, DrawPrim};
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    for (ground, theme) in [
        ("dark", aterm_render::Theme::default()),
        (
            "light",
            aterm_render::Theme {
                fg: 0x0010_1010,
                bg: 0x00FA_FAFA,
                ..aterm_render::Theme::default()
            },
        ),
    ] {
        let roles = crate::settings::Roles::from_theme(theme);
        for (px, scale) in [(22.0_f32, 1.0_f32), (22.0, 2.0), (16.0, 2.0), (30.0, 3.0)] {
            let spec = BrandSpec {
                kitty: true,
                px,
                scale,
            };
            let mut prims = Vec::new();
            let rect = LogicalRect::new(8.0, 4.0, 400.0, spec.needed_height());
            paint_brand(&mut prims, rect, Some(&spec.value()), &roles);
            let at = format!("{ground} {px}px @{scale}x");
            let Some((x, y, w, h, art)) = prims.iter().find_map(|prim| match prim {
                DrawPrim::Image { x, y, w, h, art } => Some((*x, *y, *w, *h, *art)),
                _ => None,
            }) else {
                panic!("{at}: no kitty");
            };
            let Some(DrawPrim::Panel {
                x: caret_x,
                y: caret_top,
                w: caret_w,
                ..
            }) = prims.last()
            else {
                panic!("{at}: the caret is not drawn last");
            };
            let Some((words_x, baseline, words)) = prims.iter().find_map(|prim| match prim {
                DrawPrim::Text { x, baseline, s, .. } => Some((*x, *baseline, s.clone())),
                _ => None,
            }) else {
                panic!("{at}: no words");
            };
            let drawn = art.mask().drawn(ArtSpan::BOX).expect("the kitty draws");
            let (left, top, right) = (x + drawn.left * w, y + drawn.top * h, x + drawn.right * w);
            assert!(
                (left - (caret_x + caret_w * 0.5)).abs() < 0.01,
                "{at}: the kitty's drawn left edge {left} is not at the caret's middle"
            );
            assert!(
                caret_x + caret_w < right,
                "{at}: the kitty goes on past the caret"
            );
            assert!(
                top < *caret_top,
                "{at}: the tail rises past the caret's top"
            );
            // The words' ink, as the overlap audit reads it: the kitty draws none of it.
            let words_w =
                crate::tray_raster::measure_text(&words, px, crate::widget::TextWeight::Regular);
            let body = 0.7 * px;
            let over_words = art.mask().drawn(ArtSpan {
                left: (words_x - x) / w,
                top: (baseline - body - y) / h,
                right: (words_x + words_w - x) / w,
                bottom: (baseline - y) / h,
            });
            assert_eq!(over_words, None, "{at}: the kitty inks the words");
        }
    }
}

/// While Restore previous settings runs, no group takes a press (design §3.4): the page shows
/// the plan the Restore is changing, so a choice made from it could set a group the Restore
/// puts back again. Once the Restore is done, the groups take a press again.
#[test]
fn a_group_takes_no_press_while_restore_runs() {
    let in_force = || SetupStatus {
        plan: Some(Plan::default()),
        ..SetupStatus::default()
    };
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(in_force());
    app.gesture(true);
    act(&mut app, "system-state/restore");
    assert!(matches!(
        app.take_action(),
        Some(SystemStateAction::Restore)
    ));
    // The worker's read while the Restore runs (the window fills `operation`).
    app.publish(SetupStatus {
        busy: true,
        operation: Some(SetupOperation::Restore),
        ..in_force()
    });
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 900.0);
    let segment = compiled
        .semantic(&UiKey::new("system-state/group/3/leave"))
        .and_then(|node| node.state)
        .expect("the group's choice shows");
    assert!(
        !segment.enabled,
        "no group takes a press while Restore runs"
    );
    act(&mut app, "system-state/group/3/leave");
    assert!(app.take_action().is_none());
    assert_eq!(app.plan, Plan::default(), "the page shows no choice taken");
    app.publish(in_force());
    act(&mut app, "system-state/group/3/leave");
    assert!(matches!(
        app.take_action(),
        Some(SystemStateAction::Apply(..))
    ));
}

/// A choice names the groups the person changed since the page last took the plan in force
/// (design §3.3), and the worker takes only those onto the plan in force: a page shown before
/// a Restore or another window's change never sets again a group someone else left as is.
/// Presses made while an earlier choice waits add up, a group pressed back included, so the
/// worker keeps the person's own press whichever earlier choice it admitted; once the page
/// takes the plan in force they start again. Set up now names none.
#[test]
fn a_choice_names_the_groups_changed_since_the_plan_in_force() {
    fn sent(action: Option<SystemStateAction>) -> (Plan, Option<Selection>) {
        let Some(SystemStateAction::Apply(answer, _)) = action else {
            panic!("a plan was submitted");
        };
        let WorkstationSetupDecision::Begin(plan) = answer.decision else {
            panic!("the plan begins");
        };
        (plan, answer.changed)
    }
    fn groups(groups: &[Group]) -> Option<Selection> {
        let mut changed = Selection::none();
        for setting in groups.iter().flat_map(|group| group.settings()) {
            changed.set(*setting, true);
        }
        Some(changed)
    }
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(SetupStatus {
        plan: Some(Plan::default()),
        ..SetupStatus::default()
    });
    act(&mut app, "system-state/group/3/leave");
    let (first, changed) = sent(app.take_action());
    assert_eq!(changed, groups(&[Group::Accounts]));
    act(&mut app, "system-state/group/2/leave");
    let (second, changed) = sent(app.take_action());
    assert_eq!(changed, groups(&[Group::Accounts, Group::Desktop]));
    // Pressed back while the earlier choices wait: still the person's own.
    act(&mut app, "system-state/group/3/set-up");
    let (third, changed) = sent(app.take_action());
    assert_eq!(changed, groups(&[Group::Accounts, Group::Desktop]));
    for plan in [first, second, third] {
        app.apply_accepted(plan);
    }
    assert!(app.awaiting.is_none());
    act(&mut app, "system-state/group/1/leave");
    let (input_left, changed) = sent(app.take_action());
    assert_eq!(changed, groups(&[Group::Input]));
    app.apply_accepted(input_left);
    // Its run done, with rows that differ from the plan, Set up now runs it as it stands.
    app.publish(to_change());
    act(&mut app, "system-state/headline/start");
    let (_, changed) = sent(app.take_action());
    assert_eq!(
        changed,
        Some(Selection::none()),
        "Set up now changes no group"
    );
}

/// macOS never tells an app whether it may use the local network. A row nothing in this launch
/// can read (`Untold`) shows `—` with its one calm note and lets the page settle; a row still
/// to be read holds the headline at a check.
#[test]
fn local_network_that_macos_never_tells_lets_the_page_settle() {
    let mut permissions = Selection::none();
    permissions.set(Setting::Permissions, true);
    let lan = Subject::Access(Access::LocalNetwork);
    let with_lan = |reading_of_lan: Observation| {
        let observed = Subject::all()
            .into_iter()
            .map(|subject| {
                if subject == lan {
                    reading_of_lan.clone()
                } else {
                    reading(subject, Standing::InPlace, Some(subject.wanted()))
                }
            })
            .collect();
        with_readings(permissions, observed)
    };
    let untold = with_lan(Observation {
        note: Some(aterm_types::macos_setup::LOCAL_NETWORK_UNTOLD.to_owned()),
        ..reading(lan, Standing::Untold, None)
    });
    let shown = rows(&untold, permissions);
    assert_eq!(
        headline(&untold, &shown, untold.plan.as_ref()).text,
        "Permissions are set up"
    );
    let row = shown.iter().find(|row| row.subject == lan).unwrap();
    assert_eq!(
        (&row.value, row.chip, row.note.as_deref()),
        (
            &RowValue::One("—".to_owned()),
            None,
            Some(aterm_types::macos_setup::LOCAL_NETWORK_UNTOLD)
        )
    );
    let unread = with_lan(reading(lan, Standing::Unknown, None));
    assert_eq!(
        headline(&unread, &rows(&unread, permissions), unread.plan.as_ref()).text,
        "Checking this Mac…",
        "a row still to be read is a check under way"
    );
}

/// A group pressed on aterm's provisional plan is sent as the person's choice even where the
/// plan already has it: it makes the plan theirs, so a later widen never sets a group they
/// left as is (§3.5). On a person's plan the same press sends nothing.
#[test]
fn leave_as_is_on_aterms_provisional_plan_is_kept_as_the_persons() {
    let mut permissions = Selection::none();
    permissions.set(Setting::Permissions, true);
    let narrow = Plan::from(permissions);
    for provisional in [true, false] {
        let mut app = SystemStateApp::new(narrow.clone());
        app.publish(SetupStatus {
            plan: Some(narrow.clone()),
            provisional,
            ..SetupStatus::default()
        });
        app.gesture(true);
        act(&mut app, "system-state/group/3/leave");
        let action = app.take_action();
        if !provisional {
            assert!(action.is_none(), "a person's plan already leaves it");
            continue;
        }
        let Some(SystemStateAction::Apply(answer, admission)) = action else {
            panic!("the press on aterm's plan is sent");
        };
        assert!(answer.chosen, "the plan becomes the person's");
        assert_eq!(admission, Admission::Person);
        let WorkstationSetupDecision::Begin(plan) = &answer.decision else {
            panic!("the plan begins");
        };
        assert_eq!(
            plan.selection, permissions,
            "the groups as the person left them"
        );
        // aterm's widen, published while the choice is admitted, does not undo the press.
        app.publish(SetupStatus {
            plan: Some(Plan::default()),
            busy: true,
            ..SetupStatus::default()
        });
        assert_eq!(
            group_choice(Group::Accounts, app.plan.selection),
            GroupChoice::LeaveAsIs
        );
    }
}

/// A stage that failed as a whole reaches the page as the worker's page says it, under the
/// rows it covers with none of the stage's words (design §3.6): the headline names those rows
/// and offers Set up now, and the note says what comes next, in one sentence for the rows that
/// failed alike.
#[test]
fn a_stage_that_failed_as_a_whole_is_said_under_its_rows() {
    let step = |setting: Setting| SetupStatusRow {
        name: setting.name().into(),
        status: "error".into(),
        detail: String::new(),
    };
    let mut status = status_of("set-up");
    status.rows = vec![step(Setting::Spotlight)];
    let plan = status.plan.clone().unwrap();
    let line = headline(&status, &rows(&status, plan.selection), Some(&plan));
    assert_eq!(
        (line.text.as_str(), line.show, line.start),
        (
            "1 step failed: Rust in Spotlight",
            Some(Subject::Setting(Setting::Spotlight)),
            true
        )
    );
    assert_eq!(
        status.notes(&plan),
        ["Rust in Spotlight failed. Set up now tries again."]
    );
    status.retrying = true;
    assert_eq!(
        status.notes(&plan),
        [format!(
            "Rust in Spotlight did not finish this time. {RETRIES_NOTE}"
        )]
    );
    status.retrying = false;
    status.rows = vec![step(Setting::CapsLock), step(Setting::Dock)];
    assert_eq!(
        status.notes(&plan),
        [format!(
            "{} and {} failed. Set up now tries again.",
            Setting::CapsLock.name(),
            Setting::Dock.name()
        )]
    );
}

/// While a run aterm widened by itself goes, one calm note says why its groups changed
/// (design §3.5): no alert, no failure, and no note once it ends or for a run a person asked for.
#[test]
fn a_widened_run_says_why_its_groups_changed() {
    let status = status_of("widened");
    let plan = status.plan.clone().unwrap();
    assert_eq!(status.notes(&plan), [WIDENED_NOTE]);
    let mut app = SystemStateApp::new(plan.clone());
    app.publish(status.clone());
    assert!(!app.needs_attention());
    assert!(
        headline(&status, &rows(&status, plan.selection), Some(&plan))
            .text
            .starts_with("Setting up this Mac…")
    );
    let done = SetupStatus {
        busy: false,
        running: Vec::new(),
        ..status.clone()
    };
    assert!(done.notes(&plan).is_empty(), "the run ended");
    let asked = SetupStatus {
        widened: false,
        ..status
    };
    assert!(asked.notes(&plan).is_empty(), "a person's run");
}

/// Every scenario's notes are sentences in the page's own words (design §3.6): each opens with
/// a capital or with aterm, and none names a path, the vault's subscriber or System State itself,
/// which the page is.
#[test]
fn every_scenarios_notes_are_sentences_in_the_pages_words() {
    for scenario in SCENARIOS {
        let status = status_of(scenario);
        let Some(plan) = status.plan.clone() else {
            continue;
        };
        for note in status.notes(&plan) {
            for sentence in note.split(". ") {
                assert!(
                    sentence.starts_with(char::is_uppercase) || sentence.starts_with("aterm"),
                    "{scenario}: {note:?}"
                );
            }
            for word in [
                "/",
                "subscriber",
                "in System State",
                "journal",
                "Init",
                "stage",
                "record",
            ] {
                assert!(!note.contains(word), "{scenario}: {word:?} in {note:?}");
            }
        }
    }
    // The vault's try that did not take, as the worker's page says it while aterm tries again.
    let status = status_of("vault-failed");
    assert_eq!(
        status.notes(&status.plan.clone().unwrap()),
        [format!(
            "Secrets vault did not finish this time. {RETRIES_NOTE}"
        )]
    );
}

/// Every scenario the page's fixture seam draws.
const SCENARIOS: [&str; 35] = [
    "checking",
    "running",
    "widened",
    "debugger",
    "needs-you",
    "vault",
    "vault-unreached",
    "vault-not-syncing",
    "vault-not-set-up",
    "sync-off",
    "vault-failed",
    "set-up",
    "non-alab",
    "widen-stopped",
    "failed",
    "check-failed",
    "check-unconfirmed",
    "mixed",
    "named-result-failed",
    "restore-at-login",
    "restore-blocked",
    "restore-unlogged",
    "vault-source-mismatch",
    "plan-unread",
    "fresh",
    "queued-choice",
    "queued-restore",
    "set-aside",
    "set-aside-non-alab",
    "startup-retry",
    "restore-failed",
    "setup-retried",
    "failed-retrying",
    "github-sign-in",
    "vault-unreadable",
];

/// Changes aterm made and could not log are a fact for later, never a signal (design §3.6):
/// one calm note on the page, no menu line, no attention, no failure headline; the exceptions
/// digest's entry carries it.
/// The setup's facts for later are entries the exceptions digest can show: a launch's setup
/// that has not started for ten minutes, and records aterm set aside; each keeps its detail
/// for the log alone, and the one about the startup clears by its key.
#[test]
fn the_setups_facts_for_later_are_digest_entries() {
    for entry in [
        setup_not_started_entry("initial-setup.json: Permission denied"),
        setup_set_aside_entry("initial-setup.json.unreadable"),
    ] {
        assert_eq!(entry.family(), aterm_messages::exceptions::Family::Setup);
        assert!(entry.message().is_ok(), "{:?}", entry.message().err());
    }
    assert_eq!(
        setup_not_started_entry("").key(),
        setup_not_started_entry("another").key()
    );
    assert_ne!(
        setup_not_started_entry("").key(),
        setup_set_aside_entry("").key()
    );
}

#[test]
fn a_journal_refusal_is_no_signal() {
    let mut restored = RestoreOutcome::of([(Some(Setting::Dock), PutBack::Back)]);
    restored.journal_refused = true;
    let mut rolled = RestoreOutcome::of([(Some(Setting::Dock), PutBack::Back)]);
    rolled.journal_refused = true;
    let pass = PassOutcome {
        rolled_back: Some(rolled),
        unfinished: Vec::new(),
    };
    for said in [restore_rows(&restored), pass_rows(&pass)] {
        let mut status = status_of("set-up");
        status.rows = said;
        let plan = status.plan.clone().unwrap();
        let mut app = SystemStateApp::new(plan.clone());
        app.publish(status.clone());
        let line = headline(&status, &rows(&status, plan.selection), Some(&plan));
        assert_eq!(
            crate::system_state_access::menu_line(&status, false).map(|line| line.label),
            None
        );
        assert!(!app.needs_attention());
        assert_eq!(line.text, "This Mac is set up");
        assert!(status.failed_steps(Some(&plan)).next().is_none());
        let notes = status.notes(&plan);
        assert_eq!(
            notes
                .iter()
                .filter(|note| note.as_str() == JOURNAL_NOTE)
                .count(),
            1,
            "{notes:?}"
        );
    }
    let entry = journal_refused_entry("doctor.restore.dock: disk full");
    assert_eq!(entry.family(), aterm_messages::exceptions::Family::Setup);
    assert!(entry.message().is_ok(), "{:?}", entry.message().err());
    assert_eq!(
        entry.log_line_text(),
        Some("System State: doctor.restore.dock: disk full")
    );
}

/// The failed headline offers Set up now only where running the plan again can change the
/// outcome (design §3.2): a step the run could not finish, or a setup run that could not go,
/// whose note says Set up now tries it again; a failure aterm tries again by itself says so in
/// its note, a reading that failed by itself names its own remedy there, and the headline
/// offers Show alone.
#[test]
fn the_failed_headline_offers_the_action_its_note_names() {
    let line = |status: &SetupStatus| {
        let plan = status.plan.clone().unwrap();
        (
            headline(status, &rows(status, plan.selection), Some(&plan)),
            status.notes(&plan),
        )
    };
    let (failed, notes) = line(&status_of("failed"));
    assert!(!failed.start && failed.show.is_some(), "{notes:?}");
    let (step, notes) = line(&status_of("named-result-failed"));
    assert!(step.start);
    assert_eq!(
        notes,
        ["GitHub repositories failed. Set up now tries again."]
    );
    let mut setup = status_of("set-up");
    setup.rows = vec![SetupStatusRow {
        name: SETUP_ROW.into(),
        status: "failed".into(),
        detail: String::new(),
    }];
    let (could_not_go, notes) = line(&setup);
    assert_eq!(
        (could_not_go.text.as_str(), could_not_go.start),
        ("Setup needs attention", true)
    );
    assert_eq!(notes, ["Setup failed. Set up now tries again."]);
    // A Check that failed is pressed again where it is.
    let (check, notes) = line(&status_of("check-failed"));
    assert!(!check.start);
    assert_eq!(notes, [format!("Check again failed. {CHECK_FAILED_NOTE}")]);
    let (_, notes) = line(&status_of("check-unconfirmed"));
    assert_eq!(
        notes,
        ["Check again could not confirm GitHub repositories."]
    );
    // A step aterm tries again by itself is no failure: its note says it did not finish this
    // time and aterm tries again, and the headline offers Show alone meanwhile: the vault's try
    // as the worker's page says it, a step whose run retries, and a setup run that could not go
    // while the run retries.
    let (vault, notes) = line(&status_of("vault-failed"));
    assert!(!vault.start && vault.show.is_some());
    assert_eq!(
        notes,
        [format!(
            "Secrets vault did not finish this time. {RETRIES_NOTE}"
        )]
    );
    let mut retried = status_of("named-result-failed");
    retried.retrying = true;
    let (step, notes) = line(&retried);
    assert!(!step.start && step.show.is_some());
    assert_eq!(
        notes,
        [format!(
            "GitHub repositories did not finish this time. {RETRIES_NOTE}"
        )]
    );
    setup.retrying = true;
    let (could_not_go, notes) = line(&setup);
    assert!(!could_not_go.start);
    assert_eq!(
        notes,
        [format!("Setup did not finish this time. {RETRIES_NOTE}")]
    );
    // A vault try held after two names Set up now, which the headline offers.
    let mut held = status_of("vault-failed");
    held.retrying = false;
    held.rows[0].detail = "aterm tried twice, so it tries again once this Mac's network or \
                           vault changes, or on Set up now."
        .into();
    let (vault, _) = line(&held);
    assert!(vault.start);
    // A saved setup aterm cannot read is said in its own words: no setup ran to fail.
    assert_eq!(
        status_of("plan-unread").notes(&Plan::default()),
        [STATE_UNREAD_NOTE]
    );
}

/// A vault address typed and not joined is read by nothing until Join or Check again (design
/// §3.2): the headline never claims a check while none runs. On a set-up Mac it keeps what it
/// said of the vault in force; elsewhere it says what the rows the draft leaves read say.
#[test]
fn a_vault_draft_does_not_claim_a_check() {
    for scenario in [
        "set-up",
        "needs-you",
        "failed",
        "vault",
        "check-failed",
        "mixed",
    ] {
        let mut app = SystemStateApp::new(Plan::default());
        app.publish(status_of(scenario));
        let before = app.headline().text;
        let mut view = SystemStateViewState::default();
        app.act(
            &mut view,
            "system-state/url",
            Some(SemanticInput::Text(
                "http://team-vault.local:7484/v1/team".into(),
            )),
        );
        assert!(app.take_action().is_none(), "typing starts nothing");
        let after = app.headline().text;
        assert_ne!(after, CHECKING, "{scenario}: {before:?} became {after:?}");
        if scenario == "set-up" {
            assert_eq!(after, before, "{scenario}");
        }
    }
}

/// Every row of this Mac as wanted, in the plan with every group set up.
fn all_in_place() -> SetupStatus {
    let in_place: Vec<Observation> = Subject::all()
        .into_iter()
        .map(|subject| reading(subject, Standing::InPlace, Some(subject.wanted())))
        .collect();
    with_readings(Selection::all(), in_place)
}

/// A saved setup aterm cannot read is no failure (§3.6): aterm reads it again by itself, and
/// sets aside a record that stays unreadable, so nothing a person could press would change the
/// outcome. The tab's attention and the menu line stay dark however few buttons are live.
#[test]
fn an_unread_saved_setup_signals_nothing() {
    let mut app = SystemStateApp::opened();
    app.publish(status_of("plan-unread"));
    assert!(!app.needs_attention());
    assert!(crate::system_state_access::menu_line(&app.status, false).is_none());
    assert_eq!(app.headline().text, CHECKING);
    assert_eq!(app.status.notes(&Plan::default()), [STATE_UNREAD_NOTE]);
    // A page that knew the plan claims nothing of it while aterm cannot read it.
    let mut known = SystemStateApp::new(Plan::default());
    known.publish(all_in_place());
    known.publish(SetupStatus {
        plan: None,
        rows: vec![calm_setup_row(STATE_UNREAD_NOTE)],
        ..all_in_place()
    });
    assert_eq!(known.headline().text, CHECKING);
    assert!(!known.needs_attention());
}

/// The menu line never counts a saved setup aterm cannot read as a failed step: no setup ran
/// to fail.
#[test]
fn an_unread_saved_setup_is_no_failed_step_on_the_menu_line() {
    let status = status_of("plan-unread");
    assert!(status.failed_steps(None).next().is_none());
    assert!(status.failed_steps(Some(&Plan::default())).next().is_none());
    assert!(crate::system_state_access::menu_line(&status, false).is_none());
}

/// A choice made while another setup holds this Mac's setup records (another aterm, `aterm
/// doctor` in a terminal) waits for it (§3.3): the page keeps the choice showing, headlined as
/// waiting, says why once in a calm note, and shows no Stop, meter, failure, tab attention or
/// menu line. A later press goes at once as the person's latest intent, the earlier group with
/// it; a waiting Restore says so in its own words.
#[test]
fn a_choice_waiting_for_another_setup_says_why_once_and_calmly() {
    let in_force = all_in_place();
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(in_force.clone());
    app.gesture(true);
    act(&mut app, "system-state/group/2/leave");
    let (plan, _, _) = submitted(app.take_action());
    assert!(!plan.selection.contains(Setting::Dock));
    // The worker's status while the choice waits: the plan in force, unchanged, and the press
    // queued; the host shows the page busy while the choice is pending.
    let waiting = SetupStatus {
        busy: true,
        queued: true,
        ..in_force
    };
    app.publish(waiting.clone());
    assert!(
        !app.reviewed_plan().selection.contains(Setting::Dock),
        "the choice stays showing"
    );
    let headline = app.headline();
    assert_eq!(headline.text, "Waiting to apply your choice…");
    assert!(!headline.start);
    assert_eq!(waiting.notes(&app.reviewed_plan()), [QUEUED_CHOICE_NOTE]);
    assert!(!app.needs_attention());
    assert!(crate::system_state_access::menu_line(&waiting, false).is_none());
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 900.0);
    for key in ["system-state/stop", "system-state/meter"] {
        assert!(compiled.semantic(&UiKey::new(key)).is_none(), "{key}");
    }
    assert!(
        compiled
            .semantics
            .iter()
            .filter(|node| node.label.contains(QUEUED_CHOICE_NOTE))
            .count()
            == 1,
        "said once"
    );

    act(&mut app, "system-state/group/3/leave");
    let (plan, _, _) = submitted(app.take_action());
    assert!(!plan.selection.contains(Setting::Dock));
    assert!(!plan.selection.contains(Setting::Messages));

    let restore = SetupStatus {
        operation: Some(SetupOperation::Restore),
        ..waiting
    };
    assert_eq!(restore.notes(&Plan::default()), [QUEUED_RESTORE_NOTE]);
    assert_eq!(
        Headline::before_plan(&restore).text,
        "Waiting to restore previous settings…"
    );
    assert!(!restore.running());
}

/// After aterm set aside a saved setup it could not read (§3.5), the page says so in one calm
/// note under the plan it went on with: no failure, tab attention or menu line, and every
/// group's buttons stay live.
#[test]
fn a_set_aside_setup_is_one_calm_note_with_every_group_live() {
    let status = status_of("set-aside");
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status.clone());
    let notes = status.notes(&app.reviewed_plan());
    assert!(
        notes.len() == 1 && notes[0].starts_with(RECOVERED_NOTE),
        "{notes:?}"
    );
    assert!(!app.needs_attention());
    assert!(crate::system_state_access::menu_line(&status, false).is_none());
    app.gesture(true);
    act(&mut app, "system-state/group/2/set-up");
    let (plan, _, _) = submitted(app.take_action());
    assert!(plan.selection.contains(Setting::Dock));
}

/// A Restore that failed as a whole is headlined as the Restore it is (§3.4), never as a setup
/// that needs attention, with its reason as the page's note; the button that tries it again is
/// beside it, so the tab's attention and the menu line stay dark.
#[test]
fn a_whole_restore_failure_is_headlined_as_restore() {
    for reason in [RESTORE_FAILED_NOTE, RESTORE_BACKUP_UNREAD_NOTE] {
        let status = SetupStatus {
            rows: vec![SetupStatusRow {
                name: RESTORE_FINISHED.into(),
                status: "failed".into(),
                detail: reason.into(),
            }],
            ..all_in_place()
        };
        let mut app = SystemStateApp::new(Plan::default());
        app.publish(status.clone());
        assert_eq!(app.headline().text, "Restore previous settings failed");
        assert_eq!(status.notes(&Plan::default()), [reason]);
        assert!(!app.needs_attention());
        assert!(crate::system_state_access::menu_line(&status, false).is_none());
        let mut unknown = SystemStateApp::opened();
        unknown.publish(SetupStatus {
            plan: None,
            ..status
        });
        assert_eq!(unknown.headline().text, "Restore previous settings failed");
    }
}

/// A step aterm tries again by itself is no failure (design §3.2, §3.5): the headline counts it
/// as waiting, as it does a row that waits on something, so `vault-failed` and
/// `vault-unreached` (the same wait, another cause) read alike; its note says, in the page's
/// words, that it did not finish this time and aterm tries again; and nothing signals it, even
/// once the run itself parks, since the vault's own pause tries it again. The headline says
/// `failed` only once the tries stop.
#[test]
fn a_step_aterm_retries_by_itself_waits_and_is_no_failure() {
    let secrets = Subject::Setting(Setting::Secrets);
    let line = |status: &SetupStatus| {
        let plan = status.plan.clone().unwrap();
        headline(status, &rows(status, plan.selection), Some(&plan))
    };
    let unreached = line(&status_of("vault-unreached"));
    for retrying in [true, false] {
        let status = SetupStatus {
            retrying,
            ..status_of("vault-failed")
        };
        let waits = line(&status);
        assert_eq!(
            (waits.text.as_str(), waits.show, waits.start),
            (
                "2 steps waiting: Secrets vault, GitHub repositories",
                Some(secrets),
                false
            ),
            "retrying {retrying}"
        );
        assert_eq!(waits.text, unreached.text);
        let plan = status.plan.clone().unwrap();
        let notes = status.notes(&plan);
        assert_eq!(
            notes,
            [format!(
                "Secrets vault did not finish this time. {RETRIES_NOTE}"
            )]
        );
        assert!(notes.iter().all(|note| !note.contains("fail")), "{notes:?}");
        assert!(status.failed_steps(Some(&plan)).next().is_none());
        assert!(crate::system_state_access::menu_line(&status, false).is_none());
        let mut app = SystemStateApp::new(plan);
        app.publish(status);
        assert!(!app.needs_attention(), "retrying {retrying}");
    }
    // A step the run tries again, with no words of its own, waits too while the run retries.
    let mut step = status_of("named-result-failed");
    step.retrying = true;
    assert_eq!(line(&step).text, "1 step waiting: GitHub repositories");
    // Once the tries stop (a vault try held after two, a run that no longer retries), the step
    // failed, and the headline, the menu line and the diamond say so.
    let mut held = status_of("vault-failed");
    held.retrying = false;
    held.rows[0].detail = "aterm tried twice, so it tries again once this Mac's network or \
                           vault changes, or on Set up now."
        .into();
    assert_eq!(line(&held).text, "1 step failed: Secrets vault");
    assert_eq!(
        crate::system_state_access::menu_line(&held, false).map(|line| line.label),
        Some("System State: Secrets vault failed".to_owned())
    );
    // A vault try held after two has spent its tries though the run still retries other
    // steps: it failed, and says so with them.
    held.retrying = true;
    assert_eq!(line(&held).text, "1 step failed: Secrets vault");
    assert_eq!(
        crate::system_state_access::menu_line(&held, false).map(|line| line.label),
        Some("System State: Secrets vault failed".to_owned())
    );
    assert_eq!(
        row_view(secrets, &held, Selection::all()).chip,
        Some(Chip::Failed)
    );
    step.retrying = false;
    assert_eq!(line(&step).text, "1 step failed: GitHub repositories");
}

/// The village draws each station as the headline says its rows (design §3.1): a step the
/// headline names failed takes the error look, though its row reads in place, and a step aterm
/// tries again by itself the waiting look, never the settled one.
#[test]
fn the_village_draws_a_failed_step_as_failed_and_a_retried_one_as_waiting() {
    let station = |status: &SetupStatus, setting: Setting| {
        let mut app = SystemStateApp::new(Plan::default());
        app.publish(status.clone());
        let spec = village_spec(&app.status, &app.reviewed_rows(), &app.reviewed_plan(), 0);
        let at = Setting::ALL.iter().position(|s| *s == setting).unwrap();
        spec.rsplit("states=")
            .next()
            .and_then(|states| states.chars().nth(at))
            .unwrap()
    };
    let failed = status_of("named-result-failed");
    assert_eq!(station(&failed, Setting::Repositories), 'E');
    let mut retried = failed.clone();
    retried.retrying = true;
    assert_eq!(station(&retried, Setting::Repositories), 'D');
    assert_eq!(station(&status_of("vault-failed"), Setting::Secrets), 'D');
    assert_eq!(station(&status_of("set-up"), Setting::Repositories), 'G');
}

/// While macOS's administrator dialog for debugger access is up, Turn On… can start nothing
/// (design §3.4): the row says what it waits for in its place, with no button, and still needs
/// the person, whose dialog may be behind other windows, so the menu line and the diamond
/// stay. Once the step ends, Turn On… is back.
#[test]
fn the_debugger_row_waits_for_the_password_without_a_button() {
    let open = "system-state/row/debugger/open";
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status_of("debugger"));
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 900.0);
    assert!(
        compiled.semantic(&UiKey::new(open)).is_none(),
        "no Turn On… while the dialog is up"
    );
    assert!(
        compiled
            .semantics
            .iter()
            .any(|node| !node.offscreen && node.label == PASSWORD_NOTE)
    );
    app.gesture(true);
    act(&mut app, open);
    assert!(app.take_action().is_none(), "a press starts nothing");
    assert!(app.needs_attention());
    assert_eq!(
        crate::system_state_access::menu_line(&app.status, false).map(|line| line.label),
        Some("System State: Debugger access needs you".to_owned())
    );
    let mut ended = status_of("debugger");
    ended.access_step = None;
    app.publish(ended);
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 900.0);
    assert!(
        compiled
            .semantic(&UiKey::new(open))
            .is_some_and(|node| node.state.is_some_and(|state| state.enabled))
    );
    assert!(
        compiled
            .semantics
            .iter()
            .all(|node| node.label != PASSWORD_NOTE)
    );
}

/// Where no look at the local network answered aterm, Local Network reads as macOS leaves it
/// (design §3.2): `—` beside the calm note that macOS does not tell apps whether it is on, which
/// a screen reader hears right after `Local Network: unknown`, in every scenario that reads so.
#[test]
fn local_network_reads_unknown_with_its_note_where_no_look_answered() {
    let lan = Subject::Access(Access::LocalNetwork);
    for scenario in ["running", "vault-unreached", "fresh", "set-up"] {
        let mut app = SystemStateApp::new(Plan::default());
        app.publish(status_of(scenario));
        let row = app.reviewed_row(lan);
        assert_eq!(
            (row.value.clone(), row.label.as_str(), row.note.as_deref()),
            (
                RowValue::One("—".into()),
                "Local Network: unknown",
                Some(aterm_types::macos_setup::LOCAL_NETWORK_UNTOLD)
            ),
            "{scenario}"
        );
        let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 4000.0);
        let heard: Vec<&str> = compiled
            .semantics
            .iter()
            .map(|node| node.label.as_str())
            .filter(|label| !label.is_empty())
            .collect();
        let at = heard
            .iter()
            .position(|label| *label == "Local Network: unknown")
            .unwrap_or_else(|| panic!("{scenario}: {heard:?}"));
        assert_eq!(
            heard.get(at + 1).copied(),
            Some(aterm_types::macos_setup::LOCAL_NETWORK_UNTOLD),
            "{scenario}"
        );
    }
}

/// The brand's spoken label drops its middle dot, as a row's value does: the dot marks the
/// lockup for the eye, and a screen reader hears the words alone (design §3.1).
#[test]
fn the_brand_label_reads_without_its_middle_dot() {
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status_of("set-up"));
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 900.0);
    assert_eq!(label(&compiled, "system-state/brand"), "aterm ALab");
    assert!(paint::BRAND_WORDS.contains('·'), "the eye still sees it");
}

/// The headline claims no more than was confirmed (design §3.2): a row a Check, or a run,
/// could not confirm is counted and shown, as a row that could not be read is, beside the
/// note that says so, never `This Mac is set up`.
#[test]
fn a_headline_claims_no_more_than_was_confirmed() {
    let repositories = Subject::Setting(Setting::Repositories);
    let line = |status: &SetupStatus| {
        let plan = status.plan.clone().unwrap();
        headline(status, &rows(status, plan.selection), Some(&plan))
    };
    let checked = line(&status_of("check-unconfirmed"));
    assert_eq!(
        (checked.text.as_str(), checked.show, checked.start),
        (
            "1 row not confirmed: GitHub repositories",
            Some(repositories),
            false
        )
    );
    let mut ran = status_of("set-up");
    ran.rows = pass_rows(&PassOutcome {
        rolled_back: None,
        unfinished: vec![
            (Setting::Dock, Unfinished::Unconfirmed),
            (Setting::Spotlight, Unfinished::Unconfirmed),
        ],
    });
    assert_eq!(
        line(&ran).text,
        "2 rows not confirmed: Dock, Rust in Spotlight"
    );
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status_of("check-unconfirmed"));
    assert!(!app.needs_attention());
    assert!(crate::system_state_access::menu_line(&app.status, false).is_none());
}

/// The menu line's label for `status`, if one (design §3.6).
fn menu(status: &SetupStatus) -> Option<String> {
    crate::system_state_access::menu_line(status, false).map(|line| line.label)
}

/// A setup run that did not finish as a whole, which the run tries again by itself, is no
/// failure (design §3.5): the headline waits for that try, before and after the page knows the
/// plan, beside the one calm note saying what aterm does next, and nothing signals it. Once the
/// tries stop, `Setup needs attention` with Set up now, the menu line and the diamond say it
/// together.
#[test]
fn a_setup_run_aterm_retries_waits_and_is_no_failure() {
    let setup = status_of("setup-retried");
    let plan = setup.plan.clone().unwrap();
    let line = headline(&setup, &rows(&setup, plan.selection), Some(&plan));
    assert_eq!((line.text.as_str(), line.start), (SETUP_RETRIED, false));
    assert_eq!(
        setup.notes(&plan),
        [format!("Setup did not finish this time. {RETRIES_NOTE}")]
    );
    assert!(setup.failed_steps(Some(&plan)).next().is_none());
    assert_eq!(menu(&setup), None);
    let mut app = SystemStateApp::new(plan.clone());
    app.publish(setup.clone());
    assert!(!app.needs_attention());
    let unknown = SetupStatus {
        plan: None,
        ..setup.clone()
    };
    assert_eq!(Headline::before_plan(&unknown).text, SETUP_RETRIED);
    let stopped = SetupStatus {
        retrying: false,
        ..setup
    };
    let line = headline(&stopped, &rows(&stopped, plan.selection), Some(&plan));
    assert_eq!(
        (line.text.as_str(), line.start),
        ("Setup needs attention", true)
    );
    assert_eq!(
        menu(&stopped).as_deref(),
        Some("System State: 1 step failed")
    );
    app.publish(stopped);
    assert!(app.needs_attention());
}

/// A launch's setup that could not start and tries again by itself (design §3.5) waits for that
/// try: nothing a person presses changes what comes next, so the headline offers no Set up now
/// for the rows that run changes, beside the one calm line, and nothing signals.
#[test]
fn a_startup_aterm_retries_waits_with_no_set_up_now() {
    let startup = status_of("startup-retry");
    let plan = startup.plan.clone().unwrap();
    let line = headline(&startup, &rows(&startup, plan.selection), Some(&plan));
    assert_eq!((line.text.as_str(), line.start), (SETUP_RETRIED, false));
    assert_eq!(startup.notes(&plan), [STARTUP_RETRY_NOTE]);
    assert_eq!(menu(&startup), None);
    let mut app = SystemStateApp::new(plan);
    app.publish(startup);
    assert!(!app.needs_attention());
}

/// The headline, the menu line and the tab's diamond signal a failure together or not at all
/// (design §3.6). A step that failed with its own reason while the run still tries again by
/// itself waits for that try (`1 step waiting`), its row or note keeping the reason under the
/// waiting chip, never `Failed`, and the village draws it waiting; once the tries stop, all of
/// them say it failed at once.
#[test]
fn a_failure_signals_in_the_headline_the_menu_line_and_the_diamond_together() {
    let agree = |status: &SetupStatus| {
        let plan = status.plan.clone().unwrap();
        let line = headline(status, &rows(status, plan.selection), Some(&plan));
        let mut app = SystemStateApp::new(plan.clone());
        app.publish(status.clone());
        let fails = line.text.contains("failed");
        assert_eq!(
            (menu(status).is_some(), app.needs_attention()),
            (fails, fails),
            "headline {:?}, menu line {:?}, notes {:?}",
            line.text,
            menu(status),
            status.notes(&plan)
        );
        line.text
    };
    let station = |status: &SetupStatus, setting: Setting| {
        let plan = status.plan.clone().unwrap();
        let spec = village_spec(status, &rows(status, plan.selection), &plan, 0);
        let at = Setting::ALL.iter().position(|s| *s == setting).unwrap();
        spec.rsplit("states=")
            .next()
            .and_then(|states| states.chars().nth(at))
            .unwrap()
    };
    let repositories = Subject::Setting(Setting::Repositories);
    // A reading that failed, with its reason on its row.
    let retrying = status_of("failed-retrying");
    assert_eq!(agree(&retrying), "1 step waiting: GitHub repositories");
    assert_eq!(station(&retrying, Setting::Repositories), 'D');
    let plan = retrying.plan.clone().unwrap();
    assert_eq!(
        retrying.notes(&plan),
        [format!(
            "GitHub repositories did not finish this time. {RETRIES_NOTE}"
        )],
        "one line says what aterm does next"
    );
    let row = row_view(repositories, &retrying, Selection::all());
    assert_eq!(
        (row.chip, row.note.as_deref()),
        (Some(Chip::WillChange { busy: false }), Some(GITHUB_STOPPED)),
        "the row waits with its reason, never red"
    );
    let stopped = SetupStatus {
        retrying: false,
        ..retrying
    };
    assert_eq!(agree(&stopped), "1 step failed: GitHub repositories");
    assert_eq!(station(&stopped, Setting::Repositories), 'E');
    assert_eq!(
        row_view(repositories, &stopped, Selection::all()).chip,
        Some(Chip::Failed)
    );
    assert!(stopped.notes(&plan).is_empty(), "the row says why");
    // A step the run could not finish, with its own reason in the page's note.
    let mut step = status_of("set-up");
    step.rows = pass_rows(&PassOutcome {
        rolled_back: None,
        unfinished: vec![(Setting::Dock, Unfinished::Failed)],
    });
    step.retrying = true;
    let plan = step.plan.clone().unwrap();
    assert_eq!(agree(&step), "1 step waiting: Dock");
    assert_eq!(station(&step, Setting::Dock), 'D');
    assert_eq!(
        step.notes(&plan),
        [format!(
            "Dock did not finish this time. The change did not take. {RETRIES_NOTE}"
        )]
    );
    step.retrying = false;
    assert_eq!(agree(&step), "1 step failed: Dock");
    assert_eq!(station(&step, Setting::Dock), 'E');
    assert_eq!(step.notes(&plan), ["Dock failed. The change did not take."]);
}

/// What every signal says of one row (design §3.6): the headline, the row's chip, the village's
/// station, the menu line and the tab's diamond.
fn signals(
    status: &SetupStatus,
    subject: Subject,
) -> (String, Option<Chip>, char, Option<String>, bool) {
    let plan = status.plan.clone().unwrap();
    let all = rows(status, plan.selection);
    let spec = village_spec(status, &all, &plan, 0);
    let at = Setting::ALL
        .iter()
        .position(|setting| *setting == subject.setting())
        .unwrap();
    let station = spec
        .rsplit("states=")
        .next()
        .and_then(|states| states.chars().nth(at))
        .unwrap();
    let mut app = SystemStateApp::new(plan.clone());
    app.publish(status.clone());
    (
        headline(status, &all, Some(&plan)).text,
        row_view(subject, status, plan.selection).chip,
        station,
        menu(status),
        app.needs_attention(),
    )
}

/// A step that failed for good, as the worker reports GitHub repositories on a Mac with two
/// user accounts: its row reads failed with the same words, and `detail` is the step's.
fn failed_for_good(detail: &str, retrying: bool) -> SetupStatus {
    let mut status = status_of("set-up");
    status.retrying = retrying;
    if let Some(row) = status
        .observed
        .iter_mut()
        .find(|row| row.subject == Subject::Setting(Setting::Repositories))
    {
        row.standing = Standing::Failed;
        row.now = None;
        row.detail = Some(detail.to_owned());
    }
    status.rows = vec![SetupStatusRow {
        name: Setting::Repositories.name().into(),
        status: FAILED_FOR_GOOD.into(),
        detail: detail.to_owned(),
    }];
    status
}

/// Every failure has one kind, and each signal says it alike (design §3.6): the headline, the
/// row's chip, the village's station, the menu line and the tab's diamond agree for a transient
/// failure while aterm tries it again (waiting, calm) and once the tries are spent (failed), for
/// one only a person ends (needs you, at once, whether or not the run retries), and for one that
/// failed for good (failed, at once, though the run retries other steps).
#[test]
fn every_failure_kind_signals_alike_in_the_headline_the_row_the_village_the_menu_and_the_diamond() {
    let repositories = Subject::Setting(Setting::Repositories);
    let waiting = |name: &str| {
        (
            format!("1 step waiting: {name}"),
            Some(Chip::WillChange { busy: false }),
            'D',
            None,
            false,
        )
    };
    let failed = |name: &str| {
        (
            format!("1 step failed: {name}"),
            Some(Chip::Failed),
            'E',
            Some(format!("System State: {name} failed")),
            true,
        )
    };
    let needs_you = |name: &str| {
        (
            format!("Needs you: {name}"),
            Some(Chip::NeedsYou),
            'E',
            Some(format!("System State: {name} needs you")),
            true,
        )
    };
    let github = "GitHub repositories";

    // Transient: a reading that failed, and a step the run could not finish.
    assert_eq!(
        signals(&status_of("failed-retrying"), repositories),
        waiting(github)
    );
    assert_eq!(signals(&status_of("failed"), repositories), failed(github));
    let mut step = status_of("named-result-failed");
    step.retrying = true;
    assert_eq!(signals(&step, repositories), waiting(github));
    step.retrying = false;
    assert_eq!(
        signals(&step, repositories),
        failed(github),
        "though the row reads in place"
    );

    // Needs a person: never waiting, never retried, said at once.
    for retrying in [false, true] {
        let mut sign_in = status_of("github-sign-in");
        sign_in.retrying = retrying;
        assert_eq!(
            signals(&sign_in, repositories),
            needs_you(github),
            "retrying {retrying}"
        );
        let line = crate::system_state_access::menu_line(&sign_in, false).unwrap();
        assert_eq!(line.note.as_deref(), Some(GITHUB_TOKEN_REFUSED));
        let row = row_view(repositories, &sign_in, Selection::all());
        assert_eq!(
            (row.note.as_deref(), &row.value),
            (
                Some(GITHUB_TOKEN_REFUSED),
                &RowValue::Change {
                    now: "Needs sign-in".into(),
                    wanted: Setting::Repositories.wanted().into(),
                }
            )
        );
        let plan = sign_in.plan.clone().unwrap();
        assert!(sign_in.notes(&plan).is_empty(), "the row says the remedy");
        // A step of the run that names the row is the person's too: never a failure, a wait
        // or a try aterm makes again.
        sign_in.rows = vec![SetupStatusRow {
            name: Setting::Repositories.name().into(),
            status: "error".into(),
            detail: RETRIES_NOTE.into(),
        }];
        assert_eq!(
            signals(&sign_in, repositories),
            needs_you(github),
            "retrying {retrying}"
        );
        let failures: Vec<Failure> = sign_in.result_failures(Some(&plan)).collect();
        assert_eq!(
            failures
                .iter()
                .map(|failure| (failure.kind, failure.retried, failure.failed()))
                .collect::<Vec<_>>(),
            [(FailureKind::NeedsPerson, false, false)]
        );
        assert!(
            sign_in.notes(&plan).is_empty(),
            "{:?}",
            sign_in.notes(&plan)
        );
    }

    // Failed for good: said at once, though the run retries other steps, with its reason, and
    // no Set up now, which cannot change it.
    let reason = "aterm sets up GitHub repositories only on a Mac with one user account.";
    for retrying in [false, true] {
        let for_good = failed_for_good(reason, retrying);
        assert_eq!(
            signals(&for_good, repositories),
            failed(github),
            "retrying {retrying}"
        );
        let plan = for_good.plan.clone().unwrap();
        assert!(!headline(&for_good, &rows(&for_good, plan.selection), Some(&plan)).start);
        assert_eq!(
            for_good.notes(&plan),
            [format!("{github} failed. {reason}")]
        );
    }
    let unexplained = failed_for_good("", true);
    let plan = unexplained.plan.clone().unwrap();
    assert_eq!(
        unexplained.notes(&plan),
        [format!("{github} failed. {FOR_GOOD_NOTE}")]
    );

    // A transient vault try held after two has spent its tries, though the run retries.
    let mut held = status_of("vault-failed");
    held.rows[0].detail = TRIED_TWICE.into();
    assert_eq!(
        signals(&held, Subject::Setting(Setting::Secrets)),
        failed("Secrets vault")
    );
}

/// No fixture's signals contradict one another (design §3.6): wherever a row's chip says
/// `Failed` the headline says a step failed, the diamond and the menu line show, and the
/// village draws the error look, and wherever the headline counts a step waiting no row reads
/// `Failed` and nothing signals.
#[test]
fn no_fixture_says_waiting_and_failed_at_once() {
    for scenario in SCENARIOS {
        let status = status_of(scenario);
        let Some(plan) = status.plan.clone() else {
            continue;
        };
        let all = rows(&status, plan.selection);
        let line = headline(&status, &all, Some(&plan)).text;
        let red: Vec<Subject> = all
            .iter()
            .filter(|row| row.chip == Some(Chip::Failed))
            .map(|row| row.subject)
            .collect();
        let mut app = SystemStateApp::new(plan.clone());
        app.publish(status.clone());
        if !red.is_empty() {
            assert!(
                line.contains("failed") || line.starts_with("Needs you"),
                "{scenario}: {line:?} beside failed rows {red:?}"
            );
            assert!(app.needs_attention(), "{scenario}");
            assert!(menu(&status).is_some(), "{scenario}");
        }
        if line.contains("waiting") {
            assert!(
                red.is_empty(),
                "{scenario}: {line:?} beside failed rows {red:?}"
            );
            if !all.iter().any(|row| row.chip == Some(Chip::NeedsYou)) {
                assert!(!app.needs_attention(), "{scenario}: {line:?}");
                assert!(menu(&status).is_none(), "{scenario}: {line:?}");
            }
        }
    }
}

/// A Restore waiting for another setup leaves the person their say (design §3.4): the groups
/// and Check again stay live, and the Restore button says it waits. Check again reads the rows
/// beside it, the Restore keeping its words, and a group pressed meanwhile replaces it, as a
/// later press replaces a choice waiting: the latest intent wins.
#[test]
fn a_waiting_restore_leaves_the_groups_and_check_again_live() {
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(all_in_place());
    app.publish(status_of("queued-restore"));
    assert_eq!(app.headline().text, "Waiting to restore previous settings…");
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 2000.0);
    let enabled = |key: &str| {
        compiled
            .semantic(&UiKey::new(key))
            .unwrap_or_else(|| panic!("{key} is in the tree"))
            .state
            .is_some_and(|state| state.enabled)
    };
    for index in 0..Group::ALL.len() {
        for choice in ["set-up", "leave"] {
            let key = format!("system-state/group/{index}/{choice}");
            assert!(enabled(&key), "{key}");
        }
    }
    assert!(enabled("system-state/check"));
    assert!(!enabled("system-state/restore"));
    assert_eq!(label(&compiled, "system-state/restore"), RESTORE_WAITS);
    for key in ["system-state/stop", "system-state/meter"] {
        assert!(compiled.semantic(&UiKey::new(key)).is_none(), "{key}");
    }
    act(&mut app, "system-state/check");
    assert!(matches!(
        app.take_action(),
        Some(SystemStateAction::Check(_))
    ));
    assert_eq!(app.headline().text, "Waiting to restore previous settings…");
    app.gesture(true);
    act(&mut app, "system-state/group/2/leave");
    let (plan, _, _) = submitted(app.take_action());
    assert!(!plan.selection.contains(Setting::Dock));
    assert_eq!(app.headline().text, "Waiting to apply your choice…");
}

/// After aterm set aside a saved setup it could not read (design §3.5), its one calm note says
/// what aterm kept as the groups show it: every Mac preference group left as is, Permissions
/// set up, and on an ALab Mac the vault and repositories set up, which leaves Workspace partly
/// set up (Rust in Spotlight left as is), neither segment chosen. A Mac that is not an ALab Mac
/// hears nothing of a vault. Nothing signals either.
#[test]
fn the_set_aside_note_says_what_aterm_kept_as_the_groups_show_it() {
    for (scenario, note, workspace) in [
        (
            "set-aside",
            "aterm could not read its saved setup, so it started afresh: your Mac settings are \
             left as they are, and the vault and repositories stay set up.",
            GroupChoice::Partly,
        ),
        (
            "set-aside-non-alab",
            "aterm could not read its saved setup, so it started afresh: your Mac settings are \
             left as they are.",
            GroupChoice::LeaveAsIs,
        ),
    ] {
        let status = status_of(scenario);
        let plan = status.plan.clone().unwrap();
        assert_eq!(status.notes(&plan), [note], "{scenario}");
        for group in [Group::Input, Group::Desktop, Group::Accounts] {
            assert_eq!(
                group_choice(group, plan.selection),
                GroupChoice::LeaveAsIs,
                "{scenario}: {group:?}"
            );
        }
        assert_eq!(
            group_choice(Group::Access, plan.selection),
            GroupChoice::SetUp
        );
        assert_eq!(
            group_choice(Group::Developer, plan.selection),
            workspace,
            "{scenario}"
        );
        let (count, _) = group_words(
            Group::Developer,
            plan.selection,
            &rows(&status, plan.selection),
        );
        assert_eq!(
            count.as_deref(),
            (workspace == GroupChoice::Partly).then_some("Partly set up"),
            "{scenario}"
        );
        assert_eq!(menu(&status), None);
        let mut app = SystemStateApp::new(plan);
        app.publish(status);
        assert!(!app.needs_attention());
    }
}

/// The set-aside note speaks in the page's words: no record, file or path.
#[test]
fn the_set_aside_note_names_no_record() {
    for scenario in ["set-aside", "set-aside-non-alab"] {
        let status = status_of(scenario);
        for note in status.notes(&status.plan.clone().unwrap()) {
            for word in ["record", "file", ".json", "unreadable", "/"] {
                assert!(!note.contains(word), "{scenario}: {word:?} in {note:?}");
            }
        }
    }
}

/// A row read in place that a Check, or a run, could not confirm is not drawn as settled
/// (design §3.2): its village station waits (`D`), its row has no check mark and says `Not
/// confirmed` in its note, where the headline's Show lands; once confirmed it is settled again.
#[test]
fn an_unconfirmed_row_is_drawn_waiting_with_its_note() {
    let station = |app: &SystemStateApp, setting: Setting| {
        let spec = village_spec(&app.status, &app.reviewed_rows(), &app.reviewed_plan(), 0);
        let at = Setting::ALL.iter().position(|s| *s == setting).unwrap();
        spec.rsplit("states=")
            .next()
            .and_then(|states| states.chars().nth(at))
            .unwrap()
    };
    let repositories = Subject::Setting(Setting::Repositories);
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status_of("check-unconfirmed"));
    let line = app.headline();
    assert_eq!(line.show, Some(repositories));
    assert_eq!(station(&app, Setting::Repositories), 'D');
    let row = app.reviewed_row(repositories);
    assert_eq!(
        (row.chip, row.note.as_deref()),
        (None, Some(UNCONFIRMED_NOTE)),
        "{}",
        line.text
    );
    assert!(row.label.ends_with(", not confirmed"), "{}", row.label);
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 4000.0);
    let heard: Vec<&str> = compiled
        .semantics
        .iter()
        .map(|node| node.label.as_str())
        .filter(|label| !label.is_empty())
        .collect();
    let at = heard
        .iter()
        .position(|label| *label == row.label)
        .unwrap_or_else(|| panic!("{heard:?}"));
    assert_eq!(heard.get(at + 1).copied(), Some(UNCONFIRMED_NOTE));
    // A run that could not confirm a change does the same.
    let mut ran = status_of("set-up");
    ran.rows = pass_rows(&PassOutcome {
        rolled_back: None,
        unfinished: vec![(Setting::Dock, Unfinished::Unconfirmed)],
    });
    app.publish(ran);
    assert_eq!(station(&app, Setting::Dock), 'D');
    assert_eq!(
        app.reviewed_row(Subject::Setting(Setting::Dock))
            .note
            .as_deref(),
        Some(UNCONFIRMED_NOTE)
    );
    // Confirmed, the rows are settled again.
    app.publish(status_of("set-up"));
    assert_eq!(station(&app, Setting::Repositories), 'G');
    assert_eq!(station(&app, Setting::Dock), 'G');
    assert_eq!(app.reviewed_row(repositories).chip, Some(Chip::InPlace));
}
