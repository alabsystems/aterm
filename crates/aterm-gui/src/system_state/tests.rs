// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::mpsc;
use std::time::Duration;

use super::*;
use crate::native_app::{
    ActionInvocation, AppViewState, NativeApp, NativeRuntime, ViewId, ViewMotionCx,
};
use crate::native_ui::{CompiledUi, LogicalRect};

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

    // In place: one value and the check; a login-time change reads in place.
    let caps = Subject::Setting(Setting::CapsLock);
    let status = with_readings(
        Selection::all(),
        vec![
            reading(natural, Standing::InPlace, Some("Off")),
            reading(caps, Standing::AtLogin, Some("Caps Lock")),
        ],
    );
    let row = row_view(natural, &status, Selection::all());
    assert_eq!(
        (row.value, row.chip),
        (RowValue::One("Off".into()), Some(Chip::InPlace))
    );
    assert_eq!(row.label, "Natural scrolling: Off, in place");
    let row = row_view(caps, &status, Selection::all());
    assert_eq!(row.value, RowValue::One("Control · at login".into()));
    assert_eq!(row.chip, Some(Chip::InPlace));

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

/// A probe puts a row's reason in its description (`Observation::detail`):
/// every row that needs a person, is not needed or was opted out of shows it
/// under the row, where a screen reader reads it too, as a failure's reason
/// always was. A waiting row shows what it waits on from its probe's note.
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
    // NEGATIVE CONTROLS: a row read as aterm wants it, a group left as is,
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

/// NO ENGINE WORDS UNDER A ROW THAT WILL CHANGE (§3.3: the value names the
/// part that differs). A planned engine row's description is its own
/// `from → to` line, and a blocked one's names the item it waits for or the
/// rule its probe applied (a token file's mode): a row that differs or waits
/// shows neither, busy or not, on a fresh Mac's most common view or
/// anywhere. A row that differs shows no note; one that waits shows its own
/// note, or the one every waiting row can say ([`WAITING_NOTE`]).
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
    // one note is the repositories', in the words of what they wait for.
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status_of("fresh"));
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 900.0);
    let repositories = Subject::Setting(Setting::Repositories);
    for subject in Subject::all() {
        let key = format!("system-state/row/{}/note", row_slug(subject));
        let node = compiled.semantic(&UiKey::new(key.clone()));
        if subject == repositories {
            assert_eq!(
                node.map(|node| node.label.as_str()),
                Some("Waits for your GitHub token from the Secrets vault"),
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

/// A WAITING ROW ALWAYS SAYS WHY (§3.3), in a person's words: the reason its
/// probe gave in the note, or, where it gave none (or only blanks), the one
/// thing true of every waiting row. Never the engine's description, busy or
/// not, in any row of the page, and on the page as the row's note.
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

/// AN ATERM RUN FROM A DISK IMAGE CANNOT KEEP THE VAULT IN SYNC, and only a
/// person can move it: the Secrets row needs them and says the move in its
/// note, the headline names the row and shows it, and the menu line carries
/// the note (§3.6). The run's own row for the step (`blocked`, the stage's
/// path-naming line) is no note on the page. (Ported from the checker's
/// round-2 scratch test, which found the wait said nowhere.)
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
    let head = headline(&status, &rows(&status, Selection::all()));
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
        headline(status, &rows(status, selection))
    };
    assert_eq!(line(&status_of("checking")).text, "Checking this Mac…");
    assert_eq!(
        line(&status_of("running")).text,
        "Setting up this Mac… 10 of 16"
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
    assert_eq!(line(&status_of("set-up")).text, "This Mac is set up");
    assert_eq!(line(&status_of("non-alab")).text, "Permissions are set up");
    let mixed = line(&status_of("mixed"));
    assert_eq!(mixed.text, "3 changes to make");
    assert!(mixed.start, "a run would change the rows that differ");
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

/// ROWS THAT ONLY WAIT ARE NO CHANGE TO MAKE (§3.2): Set up now changes the
/// rows that differ, so the count and the button come only with one; rows
/// that only wait are named, page order, with a Show to the first, and each
/// says what it waits on in its note. A row that differs counts the rows
/// that will change with it, waiting ones too (each shows `Will change`).
#[test]
fn rows_that_only_wait_are_named_and_not_counted_as_changes() {
    let line = |observed: Vec<Observation>| {
        let status = with_readings(Selection::all(), observed);
        headline(&status, &rows(&status, Selection::all()))
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
    let fresh = headline(&fresh, &rows(&fresh, Selection::all()));
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

/// A STEP THE LAST RUN COULD NOT FINISH lights the tab as a failed row does
/// (§3.6), though no reading says so (a join the vault refused reads Not
/// joined); one for a group left as is does not.
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
    assert_eq!(
        status
            .failed_steps(status.plan.as_ref())
            .collect::<Vec<_>>(),
        [Some(repository)]
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

#[test]
fn check_result_without_a_subject_still_signals_its_failure() {
    let mut status = failed_repository_check(false);
    status.check.as_mut().unwrap().rows[0].name = "Check".into();
    assert_eq!(
        status
            .failed_steps(status.plan.as_ref())
            .collect::<Vec<_>>(),
        [None]
    );
    assert!(crate::system_state_access::menu_line(&status, false).is_some());
    let mut app = SystemStateApp::opened();
    app.publish(status);
    assert!(app.needs_attention());
}

/// A FAILURE THE RUN STILL RETRIES BY ITSELF lights nothing (§3.5, §3.6): a
/// stage's error and a row that failed leave the tab's diamond dark while the
/// worker retries them, and light it once it stops retrying; the page says
/// the failure throughout. A row that needs a person lights it at once.
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
            let shown = headline(&status, &rows(&status, Selection::all()));
            assert!(
                shown.text.contains("failed")
                    || status
                        .notes(status.plan.as_ref().unwrap())
                        .any(|note| note.words().contains("failed")),
                "the page says it: {}",
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
        ("mixed", false),
        ("fresh", false),
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
fn set_up_now_runs_the_plan_again_when_rows_differ() {
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status_of("mixed"));
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 900.0);
    assert_eq!(
        label(&compiled, "system-state/headline/start"),
        "Set up now: 3 changes to make"
    );
    app.gesture(true);
    act(&mut app, "system-state/headline/start");
    let (plan, admission, _) = submitted(app.take_action());
    assert_eq!(plan.selection, status_of("mixed").plan.unwrap().selection);
    assert_eq!(admission, Admission::Person);
    act(&mut app, "system-state/headline/start");
    assert!(app.take_action().is_none(), "not while it runs");
}

#[test]
fn unread_vault_rows_cannot_establish_that_the_mac_is_set_up() {
    let complete = status_of("set-up");
    let selection = complete.plan.as_ref().unwrap().selection;
    let finished = headline(&complete, &rows(&complete, selection));
    let first_read = Headline::before_plan(&SetupStatus::default());
    assert_ne!(finished.text, first_read.text);
    for source in [None, Some("http://other-vault.local:7484/v1/team".into())] {
        let mut partial = complete.clone();
        partial.observed_vault_url = source;
        let waiting = headline(&partial, &rows(&partial, selection));
        assert_eq!(waiting.text, first_read.text);
        assert!(!waiting.start);
        let no_managed = headline(&partial, &rows(&partial, Selection::none()));
        assert_eq!(
            no_managed.text,
            headline(&complete, &rows(&complete, Selection::none())).text
        );
        partial
            .observed
            .iter_mut()
            .find(|row| row.subject == Subject::Setting(Setting::CapsLock))
            .unwrap()
            .standing = Standing::Differs;
        assert!(
            headline(&partial, &rows(&partial, selection)).start,
            "a known change still offers setup while the other source is unread"
        );
    }
}

#[test]
fn an_unknown_managed_row_keeps_the_headline_unread() {
    let complete = status_of("set-up");
    let selection = complete.plan.as_ref().unwrap().selection;
    let finished = headline(&complete, &rows(&complete, selection));
    let first_read = Headline::before_plan(&SetupStatus::default());
    assert_ne!(finished.text, first_read.text);

    let mut partial = complete.clone();
    partial
        .observed
        .iter_mut()
        .find(|row| row.subject == Subject::Setting(Setting::Repositories))
        .unwrap()
        .standing = Standing::Unknown;
    let waiting = headline(&partial, &rows(&partial, selection));
    assert_eq!(waiting.text, first_read.text);
    assert!(!waiting.start);
    assert_eq!(
        headline(&partial, &rows(&partial, Selection::none())).text,
        headline(&complete, &rows(&complete, Selection::none())).text
    );

    partial
        .observed
        .iter_mut()
        .find(|row| row.subject == Subject::Setting(Setting::CapsLock))
        .unwrap()
        .standing = Standing::Differs;
    assert!(
        headline(&partial, &rows(&partial, selection)).start,
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
    app.publish(status_of("mixed"));
    act(&mut app, "system-state/headline/start");
    assert!(!chosen(app.take_action()));
    // So does the first-run offer.
    let (send, _receive) = mpsc::channel();
    let mut app = SystemStateApp::new(Plan::default());
    assert!(app.offer(Request::new(Plan::default(), send)));
    app.tick(true, true, false);
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
fn the_first_run_offer_applies_on_the_first_active_tick_with_no_timer() {
    let (send, receive) = mpsc::channel();
    let request = Request::new(Plan::default(), send);
    let mut app = SystemStateApp::new(Plan::default());
    assert!(app.offer(request.clone()));
    app.tick(false, true, false);
    assert!(
        app.take_action().is_none(),
        "not until it is in an active window"
    );
    app.tick(true, true, false);
    let Some(SystemStateAction::Apply(answer, who)) = app.take_action() else {
        panic!("the offer applies");
    };
    assert_eq!(who, Admission::Automatic);
    assert!(request.finish_answer(answer));
    assert!(matches!(
        receive
            .recv_timeout(Duration::from_secs(60))
            .unwrap()
            .decision,
        WorkstationSetupDecision::Begin(_)
    ));
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
        act(&mut app, "system-state/headline/open");
        assert!(matches!(
            app.take_action(),
            Some(SystemStateAction::OpenAccess {
                access: Access::FullDiskAccess,
                ..
            })
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
    app.publish(status_of("mixed"));
    act(&mut app, "system-state/headline/start");
    assert!(!typed(app.take_action()), "Set up now");
    app.status.busy = false;
    act(&mut app, "system-state/vault/join");
    assert!(typed(app.take_action()), "Join");
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
            assert_eq!(label(&compiled, "system-state/brand"), "aterm · ALab");
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

#[test]
fn a_vault_that_needs_you_is_shown_from_the_headline() {
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status_of("vault"));
    let mut view = SystemStateViewState::default();
    let join = UiKey::new("system-state/vault/join");
    let compiled = compile(&app, &view, 640.0, 480.0);
    assert_eq!(label(&compiled, HEADLINE), "Needs you: Secrets vault");
    assert!(
        compiled
            .semantic(&UiKey::new("system-state/headline/open"))
            .is_none(),
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

/// A SYNC THE PERSON TURNED OFF IS THEIR CHOICE (§3.3, §4): the Secrets vault
/// row reads Sync off in place, at the value it then wants, with its note; it
/// is no change to make (no count, no Set up now), and it carries a quiet Turn
/// On Sync whose press says whose it was. NEGATIVE CONTROL: a sync that is not
/// set up differs, counted with Set up now, and has no Turn On Sync to press.
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
    let line = headline(&status, &all);
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
    let line = headline(&status, &all);
    assert_eq!((line.text.as_str(), line.start), ("1 change to make", true));
    app.publish(status);
    act(&mut app, "system-state/row/secrets/sync-on");
    assert!(app.take_action().is_none(), "nothing to turn on");
}

/// THE PAGE KEEPS ITS WORDS WHOLE in a narrow pane and at a large text size: in every
/// scenario, from 1x text at 128 points, 1.5x at 160 and 2x at 200, up to a wide window, no
/// text, button face or chip overflows its box or is cut by the card, with the vault open
/// too. Below those widths a status chip's pill is wider than the card.
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

/// A SHORT WINDOW KEEPS THE PAGE'S CONTROLS IN VIEW: the brand and then the title give way
/// before the headline's buttons or the footer leave the window, so either shows only where
/// those are whole, the brand only beside the title, and a taller window never shows less.
/// Whether they show depends on the window and the text size, not on which state the page is in,
/// while the headline names at most one row. With such a headline, a window as tall as `least`
/// (200 points at 1x, 320 at 1.5x, 440 at 2x, 320 wide) keeps the controls in view; the page
/// before this rule needed 280, 448 and 592. A headline naming every setting as failed counts its
/// own height, and is held to the order alone.
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
            let one_name = scenario != "failed-many";
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
    // stood: only an edit pins the vault open.
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
        assert_eq!(page_in(&runtime, view).0.vault_open, None, "{event:?}");
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
    let spec = village_spec(&status, &rows, selection, 480);
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
    let running = "Setting up this Mac… 10 of 16";
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
        for (key, words) in [
            (HEADLINE, running),
            ("system-state/brand", paint::BRAND_WORDS),
        ] {
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
    assert_eq!(label(&first, "system-state/brand"), "aterm · ALab");
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
                Some("Your GitHub token is for another account"),
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
