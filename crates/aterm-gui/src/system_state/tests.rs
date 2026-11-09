// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::mpsc;
use std::time::Duration;

use super::lines::Mark;
use super::*;
use crate::native_app::{
    ActionInvocation, AppViewState, NativeApp, NativeRuntime, ViewId, ViewMotionCx,
};
use crate::native_ui::{ActionId, CompiledUi, LogicalRect, SemanticValue, UiContent};
use aterm_types::macos_setup::{PutBack, RestoreOutcome};

#[test]
fn completed_workstation_setup_keeps_unfinished_account_provisioning_visible() {
    let mut status = status_of("set-up");
    status.finished = true;
    let account = Subject::Sync(SyncKind::Accounts);
    status.observed.push(Observation {
        subject: account,
        standing: Standing::Waiting,
        now: Some("Not provisioned".to_owned()),
        after_login: None,
        wanted: "Provisioned".to_owned(),
        note: Some("No active Claude account is assigned to this machine".to_owned()),
        detail: None,
    });
    let app = page_reading(&status);
    let visible = app.reviewed_rows();
    let row = visible
        .iter()
        .find(|row| row.subject == account)
        .expect("account provisioning is visible");
    assert!(row.managed);
    assert!(
        row.note
            .as_deref()
            .unwrap()
            .contains("No active Claude account")
    );
    let hero = hero_of(&app);
    assert_ne!(hero.headline, lines::READY);
    assert!(hero.done < hero.total);
    let cards = lines::card_items(&app, &visible);
    assert!(cards.iter().any(|card| {
        card.subject == Some(account)
            && card.mark == Mark::Waiting
            && card
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("No active Claude account"))
    }));
    assert_eq!(line_in(&app, Setting::Secrets).mark, Mark::Waiting);
    let reading = status
        .observed
        .iter_mut()
        .find(|reading| reading.subject == account)
        .unwrap();
    reading.standing = Standing::InPlace;
    reading.now = Some("2 accounts provisioned".to_owned());
    reading.note = None;
    assert_eq!(hero_of(&page_reading(&status)).headline, lines::READY);
}

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

/// The page as `view` draws it with every line open, so each row the page can show is drawn.
fn compile(
    app: &SystemStateApp,
    view: &SystemStateViewState,
    width: f32,
    height: f32,
) -> CompiledUi {
    view.every_line_open.set(true);
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

/// The `fresh` scenario with Full Disk Access and Debugger access off, which a person grants
/// once Permissions run: what the launch's offer reads on a fresh signed Mac.
fn fresh_with_grants() -> SetupStatus {
    let mut status = status_of("fresh");
    for grant in [
        reading(
            Subject::Access(Access::FullDiskAccess),
            Standing::NeedsYou,
            Some("Not confirmed"),
        ),
        reading(
            Subject::Access(Access::Debugger),
            Standing::NeedsYou,
            Some("Off"),
        ),
    ] {
        if let Some(row) = status
            .observed
            .iter_mut()
            .find(|row| row.subject == grant.subject)
        {
            *row = grant;
        }
    }
    status
}

/// The `mixed` scenario once its Debugger access is on: one managed row (Rust in Spotlight)
/// still differs, which a run changes, and nothing waits on the elevation.
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
const HEADLINE: &str = "system-state/headline";

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

/// The lines the page draws for `app`, in page order, and the card's items.
fn lines_of(app: &SystemStateApp) -> (Vec<lines::LineView>, Vec<lines::CardItem>) {
    let rows = page::page_rows(app);
    let card = lines::card_items(app, &rows);
    let lines = lines::Line::ALL
        .into_iter()
        .map(|line| lines::line_view(app, line, &rows, &card))
        .collect();
    (lines, card)
}

/// The line that holds `setting` on `app`'s page.
fn line_in(app: &SystemStateApp, setting: Setting) -> lines::LineView {
    let at = lines::Line::of(Subject::Setting(setting));
    lines_of(app)
        .0
        .into_iter()
        .find(|line| line.line == at)
        .expect("every line is drawn")
}

/// A page that reads `status` under its own plan.
fn page_reading(status: &SetupStatus) -> SystemStateApp {
    let mut app = SystemStateApp::new(status.plan.clone().unwrap_or_default());
    app.publish(status.clone());
    app
}

/// The mark the line that holds `setting` shows on a page reading `status` (§4.4).
fn mark_on(status: &SetupStatus, setting: Setting) -> lines::Mark {
    line_in(&page_reading(status), setting).mark
}

#[test]
fn an_untouched_setup_offer_can_restart_without_being_answered() {
    let (send, receive) = mpsc::channel();
    let request = Request::new(Plan::default(), send);
    let mut page = SystemStateApp::new(Plan::default());
    assert!(page.offer(request.clone()));
    assert!(page.offer_hold(&request).is_none());
    page.status.busy = true;
    assert!(
        page.offer_hold(&request).is_none(),
        "the worker's busy status is not the offer's: an untouched offer still restarts"
    );
    assert!(
        receive.try_recv().is_err(),
        "checking never answers the offer"
    );
    assert!(request.pending());
    drop(page);
    assert_eq!(
        receive
            .recv_timeout(Duration::from_secs(60))
            .unwrap()
            .decision,
        WorkstationSetupDecision::Unavailable,
    );
}

#[test]
fn setup_offer_restart_preserves_drafts_and_pending_work() {
    let mutations: [fn(&mut SystemStateApp); 9] = [
        |page| page.plan.selection = Selection::none(),
        |page| page.url.insert("/draft"),
        |page| page.url.insert(" "),
        |page| {
            page.password
                .replace(SecretBytes::from("fixture draft".to_owned()))
        },
        |page| page.password.composing = true,
        |page| page.url.set_preedit("draft".into(), None),
        |page| {
            page.awaiting = Some(Awaiting {
                plan: page.plan.clone(),
                password: None,
                changed: None,
            })
        },
        |page| page.actions.push_back(SystemStateAction::Restore),
        |page| page.stopping = true,
    ];
    for (index, mutate) in mutations.into_iter().enumerate() {
        let (send, receive) = mpsc::channel();
        let request = Request::new(Plan::default(), send);
        let mut page = SystemStateApp::new(Plan::default());
        assert!(page.offer(request.clone()));
        mutate(&mut page);
        assert!(
            !page.offer_hold(&request).is_none(),
            "draft/work case {index}"
        );
        // The first six are a person's draft, theirs while it is there; the last three are the
        // page's own work, which it settles by itself.
        assert_eq!(
            page.offer_hold(&request),
            Some(if index < 6 {
                OfferHold::Draft
            } else {
                OfferHold::Answer
            }),
            "draft/work case {index}"
        );
        assert!(request.pending());
        assert!(receive.try_recv().is_err());
    }
}

#[test]
fn setup_offer_restart_rechecks_the_current_offer_and_late_edits() {
    let (send, _receive) = mpsc::channel();
    let request = Request::new(Plan::default(), send);
    let (other_send, _other_receive) = mpsc::channel();
    let other = Request::new(Plan::default(), other_send);
    let mut page = SystemStateApp::new(Plan::default());
    assert!(page.offer(request.clone()));
    assert!(!page.offer_hold(&other).is_none());
    assert!(page.offer_hold(&request).is_none());
    page.url.insert("/late-draft");
    assert!(!page.offer_hold(&request).is_none());
    page.url = TextInputState::new(page.submitted.vault_url.clone());
    assert!(page.offer_hold(&request).is_none());
    page.closed(CloseScope::Tab);
    assert!(!page.offer_hold(&request).is_none());
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
            Subject::Access(Access::Accessibility),
            Standing::InPlace,
            None,
        )],
    );
    let row = row_view(
        Subject::Access(Access::Accessibility),
        &status,
        Selection::all(),
    );
    assert_eq!(
        (row.value, row.chip, row.label.as_str()),
        (
            RowValue::One("—".into()),
            Some(Chip::InPlace),
            "Accessibility: in place"
        )
    );

    // Never a guess: an unread row is `—` with no chip; a grant macOS does
    // not report reads `—` on its way to On.
    let row = row_view(Subject::Setting(Setting::Dock), &status, Selection::all());
    assert_eq!((row.value, row.chip), (RowValue::One("—".into()), None));
    let network = Subject::Access(Access::Accessibility);
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

/// A row the person changed after aterm set it (init design §6.6) reads as they left it, under
/// "Changed by you", with no chip saying it will change, while an automatic pass runs its stage
/// too, since that pass keeps the person's value; a differing row without the note still reads
/// as one that will change.
#[test]
fn a_row_changed_by_you_reads_as_you_left_it() {
    let dock = Subject::Setting(Setting::Dock);
    let mut changed = reading(dock, Standing::Differs, Some("Recent apps shown"));
    changed.note = Some(aterm_types::macos_setup::CHANGED_BY_YOU.to_owned());
    let mut status = with_readings(Selection::all(), vec![changed]);
    let row = row_view(dock, &status, Selection::all());
    assert_eq!(
        (row.value, row.chip, row.note.as_deref(), row.label.as_str()),
        (
            RowValue::One("Recent apps shown".into()),
            None,
            Some("Changed by you"),
            "Dock: Recent apps shown, changed by you"
        )
    );

    // An automatic pass running the Dock's stage keeps the person's value (init design §6.6):
    // the row still reads as they left it, with no Will change chip and no "changing now".
    status.running = vec![Setting::Dock];
    let busy = row_view(dock, &status, Selection::all());
    assert_eq!(
        (
            busy.value,
            busy.chip,
            busy.note.as_deref(),
            busy.label.as_str()
        ),
        (
            RowValue::One("Recent apps shown".into()),
            None,
            Some("Changed by you"),
            "Dock: Recent apps shown, changed by you"
        ),
        "a pass leaves the person's value alone, so the row never says it changes now"
    );

    let status = with_readings(
        Selection::all(),
        vec![reading(dock, Standing::Differs, Some("Recent apps shown"))],
    );
    assert_eq!(
        row_view(dock, &status, Selection::all()).chip,
        Some(Chip::WillChange { busy: false })
    );
}

/// A row the person changed after aterm set it (design §6.6, "Changed by you") is their value,
/// kept by every automatic pass: it is no change to make, so the headline neither counts it nor
/// lights Set up now for it (which would undo it). Another row that differs still counts, and
/// lights Set up now, whose press sets the kept row again too. Before, the kept row read as one
/// change to make and lit Set up now.
#[test]
fn a_row_changed_by_you_is_no_change_to_make() {
    let dock = Subject::Setting(Setting::Dock);
    let mut kept = reading(dock, Standing::Differs, Some("Recent apps shown"));
    kept.note = Some(aterm_types::macos_setup::CHANGED_BY_YOU.to_owned());
    let mut only_dock = Selection::none();
    only_dock.set(Setting::Dock, true);
    let status = with_readings(only_dock, vec![kept.clone()]);
    let shown = rows(&status, only_dock);
    let line = headline(&status, &shown, status.plan.as_ref());
    assert!(
        !line.start && !line.text.contains("change"),
        "{:?} start={} set_up={}",
        line.text,
        line.start,
        line.set_up
    );

    let widgets = Subject::Setting(Setting::Widgets);
    let mut both = only_dock;
    both.set(Setting::Widgets, true);
    let status = with_readings(
        both,
        vec![kept, reading(widgets, Standing::Differs, Some("Shown"))],
    );
    let shown = rows(&status, both);
    let line = headline(&status, &shown, status.plan.as_ref());
    assert_eq!(
        (line.text.as_str(), line.start),
        ("1 change to make", true),
        "only the row aterm sets counts"
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
    let sampling = "Turning it on lets debuggers attach without a password.";
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
            // A waiting row is calm until its step runs.
            let chip = if standing == Standing::Waiting && !busy {
                Chip::Waiting
            } else {
                Chip::WillChange { busy }
            };
            assert_eq!(row.chip, Some(chip), "{setting:?}");
            let waits = (standing == Standing::Waiting).then_some(WAITING_NOTE);
            assert_eq!(row.note.as_deref(), waits, "{setting:?} busy={busy}");
            assert!(!row.label.contains(line), "{setting:?}: {}", row.label);
        }
    }

    // On the page: the fresh Mac's scenario carries an engine line on every
    // row that will change, and no node a person sees or hears says any. The
    // note is the repositories', in the words of what they wait for.
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
                Some("Waits for the vault to finish syncing this Mac"),
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
    // The card holds the row, with its reason; the line leaves it out (said once).
    assert_eq!(
        label(&compiled, "system-state/card/secrets/reason"),
        move_it
    );
    assert!(
        compiled
            .semantic(&UiKey::new("system-state/row/secrets/note"))
            .is_none()
    );
    assert_eq!(app.headline().text, "Needs you: Secrets vault");
    assert_eq!(label(&compiled, HEADLINE), "1 thing needs you");
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
    assert_eq!(line.label, "System State: Needs you: Secrets vault");
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
/// is its row's failure, which the headline names and shows, with Set up now, its reason on its
/// row and no note to say it again; one it could not confirm says so calmly on its row alone;
/// and what its rollback put back is one calm line in a Restore's words, never a preference key.
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
            "aterm put back what it could not finish. Dock is as it was. Natural scrolling comes \
             back at your next login.",
            JOURNAL_NOTE,
        ]
    );
    let spotlight = row_view(
        Subject::Setting(Setting::Spotlight),
        &status,
        plan.selection,
    );
    assert_eq!(spotlight.note.as_deref(), Some(UNCONFIRMED_NOTE));
    for (setting, reason) in [
        (
            Setting::Widgets,
            "The change did not take, and aterm could not put back its value from before.",
        ),
        (Setting::Dock, "The change did not take."),
    ] {
        let row = row_view(Subject::Setting(setting), &status, plan.selection);
        assert_eq!(
            (row.chip, row.note.as_deref()),
            (Some(Chip::Failed), Some(reason)),
            "{setting:?} keeps its reason"
        );
    }
    let head = headline(&status, &rows(&status, plan.selection), Some(&plan));
    assert_eq!(
        (head.text.as_str(), head.show, head.start),
        (
            "2 steps failed: Widgets, Dock",
            Some(Subject::Setting(Setting::Widgets)),
            true
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
        ("Could not read: Dock", Some(dock))
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

/// The owner's screenshot (2026-11-02): "1 thing needs you" over a Permissions list of four rows
/// all reading On, because the row that needed them had gone to the card. It stays in the list, with
/// its words and its button in the card alone.
#[test]
fn a_row_the_card_asks_about_stays_in_its_list() {
    let accessibility = Subject::Access(Access::Accessibility);
    let mut grant = reading(accessibility, Standing::NeedsYou, Some("Off"));
    grant.detail = Some("The Messages and FaceTime sign-outs need it to press Sign Out.".into());
    let status = with_readings(Selection::all(), vec![grant]);
    let mut app = SystemStateApp::new(status.plan.clone().unwrap());
    app.publish(status);
    let state = SystemStateViewState::default();
    state.open_every_line();
    let ui = compile(&app, &state, 999.0, 900.0);
    assert!(
        ui.semantic(&UiKey::new("system-state/card/accessibility"))
            .is_some(),
        "CONTROL: the card asks"
    );
    let row = label(&ui, "system-state/row/accessibility/info");
    assert!(row.contains("Off"), "the list says so too: {row}");
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

/// A line names what is not in place, now beside wanted, and says when only some of its settings
/// are on; a line with none on reads Off (§4.2, §4.3).
#[test]
fn a_line_names_what_is_not_in_place_and_says_when_it_is_partly_on() {
    let natural = Subject::Setting(Setting::NaturalScrolling);
    let status = with_readings(
        Selection::all(),
        vec![
            reading(
                Subject::Setting(Setting::CapsLock),
                Standing::InPlace,
                Some("Control"),
            ),
            reading(natural, Standing::Differs, Some("On")),
            reading(
                Subject::Setting(Setting::TrackpadScrolling),
                Standing::InPlace,
                Some("All on"),
            ),
        ],
    );
    let under = |selection: Selection| {
        let mut status = status.clone();
        status.plan = Some(Plan::from(selection));
        line_in(&page_reading(&status), Setting::CapsLock)
    };
    let mac = under(Selection::all());
    assert_eq!(
        (mac.mark, mac.phrase.as_str()),
        (
            Mark::Pending,
            format!(
                "{}: On → {}",
                natural.name(),
                Setting::NaturalScrolling.wanted()
            )
            .as_str()
        )
    );
    assert!(mac.label.contains(" to "), "{}", mac.label);
    let mut mixed = Selection::all();
    mixed.set(Setting::NaturalScrolling, false);
    let partly = under(mixed);
    assert!(
        partly.on && partly.phrase.starts_with("Partly on · "),
        "{partly:?}"
    );
    let off = under(Selection::none());
    assert_eq!(
        (off.on, off.mark, off.phrase.as_str()),
        (false, Mark::Off, lines::LEFT_AS_IS)
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
        "Setting up this Mac… 10 of 15"
    );
    let needs = line(&status_of("needs-you"));
    assert_eq!(needs.text, "Needs you: Full Disk Access");
    assert_eq!(needs.open, Some(Access::FullDiskAccess));
    let failed = line(&status_of("failed"));
    assert_eq!(failed.text, "1 step failed: GitHub repositories");
    assert_eq!(failed.show, Some(Subject::Setting(Setting::Repositories)));
    assert!(
        failed.start,
        "its note names Set up now, which tries it again"
    );
    // A reading that failed once the run no longer retries it keeps its own reason as its note.
    let mut reading_failed = status_of("failed-retrying");
    reading_failed.retrying = false;
    let reading_failed = line(&reading_failed);
    assert_eq!(reading_failed.text, "1 step failed: GitHub repositories");
    assert!(
        !reading_failed.start,
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
    // Debugger access waiting on setup's one early Mac password is no person's step and no
    // change a run makes: the line counts Rust in Spotlight alone, as once it is on.
    let mixed = line(&status_of("mixed"));
    assert_eq!(mixed.text, "1 change to make");
    assert_eq!(mixed.open, None);
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
    assert_eq!(line(&nothing).text, EVERYTHING_LEFT_AS_IS);
    assert!(fixture_status("toggles").is_none());
}

/// Where no ALab vault answered aterm's own join, the vault reads not joined in place, as
/// `macos_doctor` reads it on any plan. On the base plan alone (a non-ALab Mac, its profile left
/// as is, §3.2) the headline says Permissions are set up, the Secrets line reads done
/// settled, the row keeps its note and offers no Turn On Sync, and `setup=` reads done, though the
/// run still parks on the vault. On the whole profile, whose GitHub repositories `macos_doctor`
/// reads there as their stage runs them, on this Mac's own GitHub token, the headline says This
/// Mac is set up once they are cloned; with no token of the Mac's own they wait for the vault's,
/// as their stage does, and the headline, the Developer line and `setup=` say so. A join the person typed
/// a password for, which `macos_doctor` reads as waiting while the run holds that password for a
/// try, holds the headline, the Secrets line and `setup=` at the vault, on the base plan too.
#[test]
fn a_vault_nothing_answered_for_is_set_up_and_a_typed_join_waits_for_it() {
    use crate::system_state_access::setup_field;
    let secrets = Subject::Setting(Setting::Secrets);
    let line = |status: &SetupStatus| {
        let plan = status.plan.clone().unwrap();
        headline(status, &rows(status, plan.selection), Some(&plan))
    };
    let station = |status: &SetupStatus| mark_on(status, Setting::Secrets);
    fn reading_of(status: &mut SetupStatus, setting: Setting) -> &mut Observation {
        status
            .observed
            .iter_mut()
            .find(|observation| observation.subject == Subject::Setting(setting))
            .unwrap()
    }
    fn vault_of(status: &mut SetupStatus) -> &mut Observation {
        reading_of(status, Setting::Secrets)
    }
    let unanswered = "The ALab vault did not answer on this network";

    let base = status_of("non-alab");
    assert_eq!(
        base.plan.as_ref().map(|plan| plan.selection),
        Some(BASE_PLAN)
    );
    let done = line(&base);
    assert_eq!(
        (done.text.as_str(), done.show, done.start),
        ("Permissions are set up", None, false)
    );
    // A Mac in no vault syncs nothing: its line is calm, never done.
    assert_eq!(station(&base), Mark::Off);
    let row = row_view(secrets, &base, BASE_PLAN);
    assert_eq!(
        (&row.value, row.chip, row.note.as_deref(), row.turn_on_sync),
        (
            &RowValue::One("Not joined".into()),
            Some(Chip::InPlace),
            Some(unanswered),
            false
        )
    );
    // The run still parks on the vault and asks again by itself, so it has not finished: the
    // page says set up, and `setup=` says done and what the run still asks for by itself.
    assert!(!base.finished && !base.cancelled);
    assert!(
        setup_field(&base).starts_with("done"),
        "{}",
        setup_field(&base)
    );
    // While a pass goes, the page says so, and `setup=` is pending.
    let going = SetupStatus {
        busy: true,
        ..base.clone()
    };
    assert_ne!(line(&going).text, "Permissions are set up");
    assert_eq!(setup_field(&going), "pending");
    let ended = SetupStatus {
        finished: true,
        ..base.clone()
    };
    assert_eq!(setup_field(&ended), "done");
    let mut app = SystemStateApp::new(Plan::from(BASE_PLAN));
    app.publish(base.clone());
    assert!(!app.needs_attention());
    assert_eq!(app.headline().text, "Permissions are set up");
    assert_eq!(
        label(
            &compile(&app, &SystemStateViewState::default(), 999.0, 1400.0),
            HEADLINE
        ),
        "Selected setup complete"
    );

    // The whole profile, its vault read as on the base plan: the vault waits on nothing there
    // either. Its GitHub repositories read on this Mac's own token, as their stage clones them
    // there: in place once every one is cloned, with the count GitHub gave.
    let repositories = Subject::Setting(Setting::Repositories);
    let mut profile = status_of("set-up");
    *vault_of(&mut profile) = vault_of(&mut base.clone()).clone();
    *reading_of(&mut profile, Setting::Repositories) = Observation {
        standing: Standing::InPlace,
        now: Some("All 12".to_owned()),
        wanted: "All 12".to_owned(),
        ..Observation::unknown(repositories)
    };
    assert_eq!(profile.plan, Some(Plan::default()));
    let done = line(&profile);
    assert_eq!(
        (done.text.as_str(), done.show, done.start),
        ("This Mac is set up", None, false)
    );
    assert_eq!(station(&profile), Mark::Off);
    // Its run has not ended: `setup=` reads as the base plan's does.
    assert!(setup_field(&profile).starts_with("done"));
    // With no token of the Mac's own, they wait for the vault's, as their stage does.
    let mut no_token = profile.clone();
    *reading_of(&mut no_token, Setting::Repositories) = Observation {
        standing: Standing::Waiting,
        note: Some("Waits for the vault to finish syncing this Mac".to_owned()),
        ..Observation::unknown(repositories)
    };
    let waiting = line(&no_token);
    assert_eq!(
        (waiting.text.as_str(), waiting.show, waiting.start),
        (
            "1 step waiting: GitHub repositories",
            Some(repositories),
            false
        )
    );
    assert_eq!(station(&no_token), Mark::Off);
    assert_eq!(mark_on(&no_token, Setting::Repositories), Mark::Waiting);
    assert_eq!(setup_field(&no_token), "pending:repos");

    // A join the person typed waits for that vault, on the base plan too.
    let mut typed = base.clone();
    let waits = vault_of(&mut typed);
    waits.standing = Standing::Waiting;
    Setting::Secrets.wanted().clone_into(&mut waits.wanted);
    let waiting = line(&typed);
    assert_eq!(
        (waiting.text.as_str(), waiting.show, waiting.start),
        ("1 step waiting: Secrets vault", Some(secrets), false)
    );
    assert_eq!(station(&typed), Mark::Waiting);
    assert_eq!(setup_field(&typed), "pending:vault");
    // A run that ended holds no password for a try, so `macos_doctor` reads its vault as `base`'s,
    // which reads done once the run ends (`ended` above).
}

/// The base plan's headline names the permissions only where the plan sets them up: the same
/// groups with aterm's permissions left as is say This Mac is set up.
#[test]
fn the_base_plan_headline_names_permissions_only_where_it_sets_them_up() {
    let line = |status: &SetupStatus| {
        let plan = status.plan.clone().unwrap();
        headline(status, &rows(status, plan.selection), Some(&plan)).text
    };
    let base = status_of("non-alab");
    assert_eq!(line(&base), "Permissions are set up");
    let mut without = BASE_PLAN;
    without.set(Setting::Permissions, false);
    let status = SetupStatus {
        plan: Some(Plan::from(without)),
        ..base
    };
    assert_eq!(group_choice(Group::Access, without), GroupChoice::LeaveAsIs);
    assert_eq!(line(&status), "This Mac is set up");
}

/// Rows that only wait are no change to make (§3.2): the count and Set up now come only with a
/// row that differs, and the count takes in only those rows: a waiting row reads `Waiting`,
/// calm, since no press changes it before what it waits on comes.
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
    assert_eq!(with_a_change.text, "1 change to make");
    assert_eq!((with_a_change.show, with_a_change.start), (None, true));
    let status = with_readings(
        Selection::all(),
        vec![
            reading(repositories, Standing::Waiting, None),
            reading(dock, Standing::Differs, Some("9 apps")),
        ],
    );
    assert_eq!(
        row_view(repositories, &status, Selection::all()).chip,
        Some(Chip::Waiting)
    );
    // A fresh Mac: ten rows differ, and the repositories wait on the vault.
    let fresh = status_of("fresh");
    let fresh = headline(&fresh, &rows(&fresh, Selection::all()), fresh.plan.as_ref());
    assert_eq!(fresh.text, "10 changes to make");
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
        // The page says setup tries again while it does, naming the step; then it names the
        // failure, the step in the card saying what failed.
        let compiled = compile(&app, &view, 999.0, 740.0);
        let carded = lines_of(&app)
            .1
            .iter()
            .any(|item| item.mark == Mark::Failed && item.subject == Some(repository));
        if retrying {
            assert_eq!(label(&compiled, HEADLINE), "Setup will try again");
            assert!(label(&compiled, "system-state/sub").contains(repository.name()));
            assert!(!carded);
        } else {
            assert_eq!(label(&compiled, HEADLINE), "1 step failed");
            assert!(carded);
        }
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
    assert_eq!(line(&settled).text, "This Mac is set up");
    assert_eq!(
        label(&compile(&app, &view, 999.0, 740.0), HEADLINE),
        "Your Mac is ready"
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
    assert_eq!(app.headline().text, baseline.text);
    assert_eq!(label(&compiled, HEADLINE), "Your Mac is ready");
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
        // The page: ready, as last read.
        let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 900.0);
        assert_eq!(
            label(&compiled, HEADLINE),
            if plan.selection == Selection::all() {
                "Your Mac is ready"
            } else {
                "Selected setup complete"
            }
        );
        assert!(
            label(&compiled, "system-state/sub").ends_with(AS_LAST_READ),
            "{words}"
        );
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
        // Its Debugger access is off and waits on setup's one early Mac password, never a person.
        ("mixed", false),
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
    app.tick();
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
    assert!(
        compiled
            .semantic(&UiKey::new("system-state/line/permissions/switch"))
            .is_none(),
        "no guess is shown as wanted: no switch until the plan is known"
    );
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
        app.publish(SetupStatus {
            restorable: true,
            ..status
        });
        let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 740.0);
        let enabled = |key: &str| {
            compiled
                .semantic(&UiKey::new(key))
                .and_then(|node| node.state)
                .unwrap()
                .enabled
        };
        assert!(
            compiled
                .semantic(&UiKey::new("system-state/check"))
                .is_none(),
            "nothing read: no Check again"
        );
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
        // aterm saved preferences before: Restore stands while the plan is unknown.
        runtime.publish_system_state(SetupStatus {
            restorable: true,
            ..status
        });
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
        // Until the plan in force is known the Secrets line has no rows to open, so no Join
        // shows; the draft typed is kept.
        let tree = compiled(&runtime);
        assert!(enabled(&tree, "system-state/restore"));
        assert!(
            tree.semantic(&UiKey::new("system-state/vault/join"))
                .and_then(|node| node.state)
                .is_none_or(|state| !state.enabled)
        );
        press(&mut runtime, "system-state/vault/join", None);
        assert!(runtime.take_system_state_action().is_none());

        // Once it is known, the Secrets line opens to its rows and the draft's Join (an Off
        // line opens to nothing, so this plan keeps Secrets on).
        let mut selection = Selection::none();
        selection.set(Setting::Permissions, true);
        selection.set(Setting::Dock, true);
        selection.set(Setting::Secrets, true);
        runtime.publish_system_state(SetupStatus {
            plan: Some(Plan {
                selection,
                vault_url: "http://saved.local:7484/v1/team".into(),
            }),
            ..SetupStatus::default()
        });
        press(&mut runtime, "system-state/line/secrets/disclose", None);
        let (app, _) = page_in(&runtime, view);
        assert_eq!(app.url.value(), "http://typed.local:7484/v1/team");
        assert_eq!(app.password.text(), "inert-password");
        assert_eq!(app.plan.selection, selection);
        assert!(enabled(&compiled(&runtime), "system-state/vault/join"));
        press(&mut runtime, "system-state/vault/join", None);
        let (plan, admission, password) = submitted(runtime.take_system_state_action());
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
        // A run says §4.3's words; a Check or a Restore says what it does.
        let said = match operation {
            None => lines::SETTING_UP.to_owned(),
            Some(_) => {
                Headline::before_plan(&SetupStatus {
                    busy: true,
                    operation,
                    ..SetupStatus::default()
                })
                .text
            }
        };
        assert_eq!(
            label(
                &compile(&busy_page, &SystemStateViewState::default(), 999.0, 740.0),
                HEADLINE
            ),
            said
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
    let said = |app: &SystemStateApp, view: &SystemStateViewState| {
        let compiled = compile(app, view, 999.0, 740.0);
        format!(
            "{} · {}",
            label(&compiled, HEADLINE),
            label(&compiled, "system-state/sub")
        )
    };
    let settled = said(&app, &view);
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
    assert_ne!(said(&app, &view), settled);
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
        assert_eq!(said(&app, &view), settled);
        assert!(!app.needs_attention());
    }
}

/// Changes to make a Stop holds wait for Set up now, which runs the plan in force again as the
/// person's own admission.
#[test]
fn set_up_now_runs_the_plan_again_while_a_stop_holds_it() {
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(SetupStatus {
        run: Some(InitialSetupRunPolicy {
            revision: 1,
            allowed: false,
        }),
        ..to_change()
    });
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 900.0);
    assert_eq!(label(&compiled, HEADLINE), lines::STOPPED);
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
    app.tick();
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

/// The launch's offer is answered by the page itself, as the plan stands, at its first tick,
/// shown or not and whoever is at this Mac (init-on-by-default design §6.1: only the person stages
/// wait for a person). The answer is aterm's own, never a choice, and carries no typed vault.
#[test]
fn the_offer_is_answered_at_the_first_tick_with_no_wait_for_a_person() {
    let (send, receive) = mpsc::channel();
    let request = Request::new(Plan::default(), send);
    let mut app = SystemStateApp::new(Plan::default());
    assert!(app.offer(request.clone()));
    assert!(request.pending());
    assert_eq!(app.headline().text, STARTING_HEADLINE);
    app.tick();
    let Some(SystemStateAction::Apply(answer, who)) = app.take_action() else {
        panic!("the first tick answers the offer");
    };
    assert_eq!(who, Admission::Automatic);
    assert!(!answer.chosen && !answer.vault_typed && answer.password.is_none());
    assert_eq!(answer.changed, None, "the offered plan whole");
    assert!(request.finish_answer(answer));
    assert!(matches!(
        receive
            .recv_timeout(Duration::from_secs(60))
            .unwrap()
            .decision,
        WorkstationSetupDecision::Begin(_)
    ));
    app.tick();
    assert!(app.take_action().is_none(), "answered once");
    assert!(receive.try_recv().is_err());
}

/// A status that says the page is busy holds the page's own answer until it is not.
#[test]
fn the_pages_own_answer_waits_while_the_page_is_busy() {
    let (send, _receive) = mpsc::channel();
    let mut app = SystemStateApp::new(Plan::default());
    assert!(app.offer(Request::new(Plan::default(), send)));
    app.publish(SetupStatus {
        busy: true,
        ..SetupStatus::default()
    });
    app.tick();
    assert!(app.take_action().is_none());
    app.publish(SetupStatus::default());
    app.tick();
    assert!(matches!(
        app.take_action(),
        Some(SystemStateAction::Apply(_, Admission::Automatic))
    ));
}

/// A scroll, a disclosure, typing or a caret move never holds the answer (design revision 4):
/// there is no countdown for a touch to stop.
#[test]
fn interaction_never_holds_the_pages_own_answer() {
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
        runtime.system_state_gesture(true);
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::FocusChanged(Some(UiKey::new("system-state/url"))),
            )
            .unwrap();
        runtime.dispatch(instance, view, event).unwrap();
        assert!(runtime.take_system_state_action().is_none());
        runtime.tick_system_state();
        let (_, admission, password) = submitted(runtime.take_system_state_action());
        assert_eq!(admission, Admission::Automatic);
        assert!(password.is_none());
    }
}

/// A group press while the offer waits answers it at once, as the presser's choice.
#[test]
fn a_group_press_while_the_offer_waits_answers_it_as_the_pressers_choice() {
    let (send, _receive) = mpsc::channel();
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
    let Some(SystemStateAction::Apply(answer, admission)) = runtime.take_system_state_action()
    else {
        panic!("the press answers the offer");
    };
    assert_eq!(admission, Admission::Client);
    assert!(answer.chosen);
    let WorkstationSetupDecision::Begin(plan) = answer.decision else {
        panic!("the plan begins");
    };
    assert!(!plan.selection.contains(Setting::Dock));
    runtime.tick_system_state();
    assert!(
        runtime.take_system_state_action().is_none(),
        "the press was the answer"
    );
}

/// Join while the offer waits answers it with the vault as typed and its password; the page's
/// own answer never sends either, and leaves them for Join.
#[test]
fn only_join_sends_a_typed_vault_while_the_offer_waits() {
    let typed = [
        (
            "system-state/url",
            Some("http://draft.local:7484/v1/review"),
        ),
        ("system-state/password", Some("synthetic-review-secret")),
    ];
    for join in [false, true] {
        let (send, _receive) = mpsc::channel();
        let mut app = SystemStateApp::new(Plan::default());
        assert!(app.offer(Request::new(Plan::default(), send)));
        let (mut runtime, instance, view) = runtime_with_app(app);
        let keys = typed
            .iter()
            .copied()
            .chain(join.then_some(("system-state/vault/join", None)));
        for (key, value) in keys {
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
        runtime.tick_system_state();
        let Some(SystemStateAction::Apply(answer, _)) = runtime.take_system_state_action() else {
            panic!("the offer is answered");
        };
        let WorkstationSetupDecision::Begin(plan) = &answer.decision else {
            panic!("the plan begins");
        };
        assert_eq!(answer.vault_typed, join);
        assert_eq!(
            answer
                .password
                .is_some_and(|password| password.as_slice() == b"synthetic-review-secret"),
            join
        );
        assert_eq!(
            plan.vault_url == "http://draft.local:7484/v1/review",
            join,
            "join {join}"
        );
        assert_eq!(
            page_app(&runtime, instance).password.text(),
            "synthetic-review-secret",
            "the field keeps the draft until the worker accepts it"
        );
    }
}

/// A reveal from VoiceOver moves only the view it came from.
#[test]
fn an_ax_reveal_moves_only_its_own_view() {
    let (send, _receive) = mpsc::channel();
    let mut app = SystemStateApp::new(Plan::default());
    assert!(app.offer(Request::new(Plan::default(), send)));
    let (mut runtime, instance, first) = runtime_with_app(app);
    let second = ViewId::from_stored(2);
    runtime
        .attach_view(second, instance, AppViewState::SystemState(Box::default()))
        .unwrap();
    // Every line open in both views, so the list runs past the window.
    for view in [first, second] {
        let Some(AppViewState::SystemState(state)) = runtime.view_state(view) else {
            panic!("a System State view");
        };
        state.open_every_line();
    }
    let viewport = LogicalRect::new(0.0, 0.0, 640.0, 480.0);
    let render = |runtime: &NativeRuntime, view| {
        runtime
            .render(instance, view, &cx(viewport, false))
            .unwrap()
            .compile(viewport)
            .unwrap()
    };
    let row = UiKey::new("system-state/row/repositories");
    assert!(render(&runtime, first).semantic(&row).unwrap().offscreen);
    let outcome = runtime
        .dispatch(instance, first, AppEvent::RevealControl(row.clone()))
        .unwrap();
    assert!(!render(&runtime, first).semantic(&row).unwrap().offscreen);
    assert!(render(&runtime, second).semantic(&row).unwrap().offscreen);
    assert!(
        outcome
            .effects
            .iter()
            .any(|effect| { matches!(effect, crate::native_app::AppEffect::RepaintSelf(_)) })
    );
    assert!(
        !outcome
            .effects
            .iter()
            .any(|effect| { matches!(effect, crate::native_app::AppEffect::RepaintInstance(_)) })
    );
}

/// Closing the page before it answered the launch's offer answers nothing: the offer is let go
/// as one no window showed, so the worker makes it again where a page answers it at its first
/// tick (init-on-by-default design §6.1). Negative control: a page whose close answered the
/// offered plan fails the decision below.
#[test]
fn closing_lets_the_offer_go_and_never_waits() {
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
            WorkstationSetupDecision::Unavailable,
            "let go, to be made again where a page answers it: never an answer of the close's"
        );
    }
}

fn permissions_only() -> Plan {
    let mut selection = Selection::none();
    selection.set(Setting::Permissions, true);
    Plan::from(selection)
}

fn page_app(
    runtime: &NativeRuntime,
    instance: crate::native_app::AppInstanceId,
) -> &SystemStateApp {
    let Some(NativeApp::SystemState(app)) = runtime.app(instance) else {
        panic!("System State is open");
    };
    app
}

/// A Cancel an older aterm's review recorded (§3.3; no review is offered since design revision 4)
/// reads only as those groups turned off: each shows Leave as is, the page says nothing of a
/// review or a Cancel, the headline is the one for groups left as is with no Set up now, nothing
/// lights (no Needs you, no diamond, no menu line), and a group's switch turns it back on.
#[test]
fn an_older_cancel_reads_off_with_no_review_words() {
    let mut app = SystemStateApp::new(Plan::default());
    let cancelled = SetupStatus {
        plan: Some(Plan::from(Selection::none())),
        cancelled: true,
        finished: true,
        observed: vec![reading(
            Subject::Access(Access::FullDiskAccess),
            Standing::NeedsYou,
            Some("Off"),
        )],
        ..SetupStatus::default()
    };
    app.publish(cancelled.clone());
    assert!(app.plan.selection.is_empty(), "every group at Leave as is");
    for group in Group::ALL {
        assert_eq!(
            group_choice(group, app.plan.selection),
            GroupChoice::LeaveAsIs,
            "{group:?}"
        );
    }
    let notes = app.notes(&app.reviewed_plan());
    let headline = app.headline();
    for line in notes.iter().chain(std::iter::once(&headline.text)) {
        let lower = line.to_lowercase();
        assert!(
            !lower.contains("review") && !lower.contains("cancel"),
            "review-era words on the page: {line:?}"
        );
    }
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(headline.text, EVERYTHING_LEFT_AS_IS);
    assert!(
        !headline.start,
        "no Set up now: each group turns on by its switch"
    );
    assert!(!app.needs_attention(), "no diamond");
    assert_eq!(
        crate::system_state_access::menu_line(&cancelled, false),
        None,
        "no menu line"
    );
    // A group's switch turns it back on, as the person's own choice.
    app.gesture(true);
    act(&mut app, "system-state/group/0/set-up");
    let Some(SystemStateAction::Apply(answer, admission)) = app.take_action() else {
        panic!("the switch sends the group's choice");
    };
    assert_eq!(admission, Admission::Person);
    assert!(answer.chosen, "a choice, the person's");
    let WorkstationSetupDecision::Begin(plan) = &answer.decision else {
        panic!("a plan to begin");
    };
    assert!(
        Group::ALL[0]
            .settings()
            .iter()
            .all(|setting| plan.selection.contains(*setting)),
        "the group is on again: {plan:?}"
    );
}

/// A Cancel an older aterm recorded of a review that offered what a Restore brought forward
/// (§3.3) reads as any older Cancel: every group at Leave as is, nothing lit and no Set up now,
/// with the Restore's own line still saying what it put back.
#[test]
fn an_older_cancel_after_a_restore_keeps_the_restores_line_alone() {
    let mut app = SystemStateApp::new(Plan::default());
    let status = SetupStatus {
        plan: Some(Plan::from(Selection::none())),
        cancelled: true,
        finished: true,
        observed: vec![reading(
            Subject::Access(Access::FullDiskAccess),
            Standing::NeedsYou,
            Some("Off"),
        )],
        rows: restore_rows(&RestoreOutcome::of([(Some(Setting::Dock), PutBack::Back)])),
        ..SetupStatus::default()
    };
    app.publish(status.clone());
    assert!(app.plan.selection.is_empty(), "every group at Leave as is");
    assert_eq!(
        app.notes(&app.reviewed_plan()),
        ["Restore previous settings finished. Dock is as it was.".to_owned()]
    );
    let headline = app.headline();
    assert_eq!(headline.text, EVERYTHING_LEFT_AS_IS);
    assert!(!headline.start, "no Set up now");
    assert!(!app.needs_attention(), "no diamond");
    assert_eq!(
        crate::system_state_access::menu_line(&status, false),
        None,
        "no menu line"
    );
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
        // Said once: by the Secrets vault row where it reads so, else by the page's line.
        let reviewed = app.reviewed_plan();
        let by_row = app
            .reviewed_row(Subject::Setting(Setting::Secrets))
            .note
            .is_some_and(|said| {
                said.trim_end_matches('.') == VAULT_UNREAD_NOTE.trim_end_matches('.')
            });
        let by_line = app
            .status
            .notes(&reviewed)
            .iter()
            .filter(|said| *said == note)
            .count();
        assert_eq!(
            usize::from(by_row) + by_line,
            1,
            "refused {refused}: {:?}",
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
fn a_join_password_an_admin_refused_opens_the_fields_again() {
    let mut app = SystemStateApp::new(Plan::default());
    let with = |now: &str, note: Option<&str>| {
        let mut status = status_of("set-up");
        status
            .observed
            .retain(|o| o.subject != Subject::Setting(Setting::Secrets));
        status.observed.push(observed_with_note(
            Subject::Setting(Setting::Secrets),
            Standing::NeedsYou,
            Some(now),
            note,
        ));
        status
    };
    let waiting = with(
        "Awaiting code",
        Some("On an admin Mac: aterm secrets approve ABCD-EFGH, or retype the join password"),
    );
    app.publish(waiting.clone());
    act(&mut app, "system-state/row/secrets/disclose");
    act(&mut app, "system-state/row/secrets/disclose");
    assert!(!app.vault_shown(), "the person closed the fields");
    // CONTROL: the same reading again leaves them as the person set them.
    app.publish(waiting);
    assert!(!app.vault_shown());
    app.publish(with("Wrong password", None));
    assert!(app.vault_shown(), "a refused password opens them again");
}

#[test]
fn every_scenario_lays_out_at_every_width_with_the_designs_labels() {
    for scenario in [
        "checking",
        "running",
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
            assert_eq!(label(&compiled, "system-state/progress"), "Setup progress");
            // Check again is in the footer while nothing runs, once something was read.
            if !app.status.busy && !app.status.observed.is_empty() {
                assert!(
                    compiled
                        .semantic(&UiKey::new("system-state/check"))
                        .is_some(),
                    "{scenario}"
                );
            }
            let running = app.status.running();
            assert_eq!(
                compiled
                    .semantic(&UiKey::new("system-state/stop"))
                    .is_some(),
                running,
                "{scenario}: Stop only while a run is going"
            );
            // Every row of an open line is listed, on screen or off it, and every row the card
            // holds is in the card.
            let (lines, card) = lines_of(&app);
            for subject in lines
                .iter()
                .flat_map(|line| line.rows.iter().map(|row| row.subject))
            {
                let key = format!("system-state/row/{}/info", row_slug(subject));
                assert!(
                    compiled.semantic(&UiKey::new(key.clone())).is_some(),
                    "{scenario} {key}"
                );
            }
            for item in &card {
                let key = format!("system-state/card/{}", item.id);
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
        label(&compiled, "system-state/line/mac/disclose"),
        "Details"
    );
    let switch = compiled
        .semantic(&UiKey::new("system-state/line/mac/switch"))
        .expect("the Mac line's switch");
    assert!(
        switch.label.starts_with("Mac settings, working: "),
        "{}",
        switch.label
    );
    assert_eq!(switch.value, SemanticValue::Bool(true));
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
    // in whole like any control: it shows bullets, never what was typed. The person opened the
    // Secrets line to reach them.
    app.act(&mut view, "system-state/line/secrets/disclose", None);
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
    // From the top of the first line in a room taller than a line, where no span the room cuts
    // would be carried past whole, one line down moves a full line.
    let from = view
        .spans
        .borrow()
        .iter()
        .find(|span| &*span.key == "system-state/line/mac")
        .expect("the first line is listed")
        .top;
    view.scroll.set(from);
    let room = view.room.get();
    assert!(
        room > LINE_PX && view.limit.get() > from + LINE_PX,
        "{room}"
    );
    let stops: Vec<(String, f32, f32)> = view
        .spans
        .borrow()
        .iter()
        .filter(|span| {
            span.top > from + 0.01
                && span.top < from + LINE_PX - 0.01
                && span.bottom > from + room + 0.01
        })
        .map(|span| (span.key.to_string(), span.top, span.bottom))
        .collect();
    assert!(
        stops.is_empty(),
        "a span stops the line in a room of {room}: {stops:?}"
    );
    view.scroll_steps(1, LINE_PX);
    assert!(
        (view.scroll.get() - from - LINE_PX).abs() < 0.01,
        "a notch moved {} from {from}",
        view.scroll.get()
    );
    assert!(!view.reveal_listed_control(&UiKey::new("system-state/row/nowhere")));
}

/// A vault that waits for an address, after a Join the person typed a password for found none
/// there, is said under the headline as setup going on, naming what waits; with its fields open,
/// a reveal brings Join and the fields into view.
#[test]
fn a_waiting_vault_is_said_under_the_headline() {
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status_of("vault"));
    let mut view = SystemStateViewState::default();
    app.act(&mut view, "system-state/row/secrets/disclose", None);
    let join = UiKey::new("system-state/vault/join");
    let compiled = compile(&app, &view, 640.0, 480.0);
    assert_eq!(label(&compiled, HEADLINE), "Setting up your Mac");
    assert!(
        label(&compiled, "system-state/sub")
            .starts_with("2 steps waiting: Secrets vault, GitHub repositories"),
        "{}",
        label(&compiled, "system-state/sub")
    );
    assert!(
        compiled
            .semantics
            .iter()
            .all(|node| node.key.as_str() != "system-state/row/secrets/open"),
        "macOS has no pane for the vault"
    );
    assert!(compiled.semantic(&join).is_some_and(|node| node.offscreen));
    assert!(view.reveal_listed_control(&join));
    let compiled = compile(&app, &view, 640.0, 480.0);
    assert!(compiled.semantic(&join).is_some_and(|node| !node.offscreen));
}

/// Serious Mode shows the lockup's words alone (§4.5): the prompt, the words and the caret, with
/// no kitty and no ribbon behind them.
#[test]
fn serious_mode_shows_the_words_alone() {
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status_of("running"));
    let view = SystemStateViewState::default();
    let viewport = LogicalRect::new(0.0, 0.0, 999.0, 900.0);
    for serious in [false, true] {
        let compiled = app
            .view(&view, &cx(viewport, serious))
            .compile(viewport)
            .unwrap();
        let node = compiled
            .paint
            .iter()
            .find(|node| node.key.as_str() == "system-state/progress")
            .expect("the lockup is painted");
        let UiContent::Custom(spec) = &node.content else {
            panic!("the lockup is drawn by its audit");
        };
        assert_eq!(spec.audit_id, crate::native_ui::SYSTEM_STATE_BRAND_AUDIT);
        assert_eq!(LockupSpec::parse(spec.paint.as_deref()).kitty, !serious);
        let mut prims = Vec::new();
        paint_lockup(
            &mut prims,
            LogicalRect::new(0.0, 0.0, 640.0, 48.0),
            spec.paint.as_deref(),
            &crate::settings::Roles::from_theme(aterm_render::Theme::default()),
        );
        let kitty = prims
            .iter()
            .any(|prim| matches!(prim, crate::widget::DrawPrim::Image { .. }));
        assert_eq!(kitty, !serious, "serious: {serious}");
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
                // A text field scrolls its text by design.
                for (_, audit) in compiled.text_fit_cuts(|kind, overflow, clipped| {
                    kind != "text-field" && (overflow || clipped)
                }) {
                    cut.push(format!("{scale}x {width} {scenario}: {audit}"));
                }
            }
        }
    }
    let _ = install_preferences(AppearancePreferences::default());
    assert!(cut.is_empty(), "{cut:#?}");
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
        // A window whose list keeps a little room under the hero, the card and the footer.
        let floor = 300.0 * scale;
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
            // The line with rows open, so the list holds lines, rows and their controls.
            let line = if scenario == "needs-you" {
                "permissions"
            } else {
                "secrets"
            };
            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: ActionId::new(format!("system-state/line/{line}/disclose")),
                        value: None,
                    }),
                )
                .unwrap();
            let compile_now = |runtime: &NativeRuntime| {
                let (app, state) = page_in(runtime, view);
                compile(app, state, 720.0, height)
            };
            let mut compiled = compile_now(&runtime);
            let list = list_rect(&compiled);
            let room = page_in(&runtime, view).1.room.get();
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
    // The tallest window too short for the lockup: the headline shows and the lockup does not.
    let short = (124..=644)
        .rev()
        .find(|height| {
            !shown(
                &compile_at(&runtime, *height as f32),
                "system-state/progress",
            )
        })
        .expect("a short window leaves the lockup out");
    let (short, taller) = (short as f32, short as f32 + 1.0);
    let compiled = compile_at(&runtime, short);
    assert!(
        shown(&compiled, "system-state/headline") && !shown(&compiled, "system-state/progress"),
        "at 720×{short} the headline shows and the lockup does not"
    );
    runtime
        .dispatch(instance, view, AppEvent::FocusChanged(Some(url.clone())))
        .unwrap();
    let compiled = compile_at(&runtime, short);
    assert!(
        in_view(&runtime, &compiled, url.as_str()),
        "focus reveals the field"
    );
    // One point taller, the lockup comes back and the list loses its height.
    let room = page_in(&runtime, view).1.room.get();
    let compiled = compile_at(&runtime, taller);
    assert!(
        shown(&compiled, "system-state/progress"),
        "the lockup at 720×{taller}"
    );
    assert!(
        page_in(&runtime, view).1.room.get() + 24.0 < room,
        "the lockup at 720×{taller} takes the list's room: {} then {room}",
        page_in(&runtime, view).1.room.get()
    );
    assert!(
        in_view(&runtime, &compiled, url.as_str()),
        "the focused field stays whole in a room the brand shrank: the list {:?}, its room {} \
         at {}, the field {:?}, its span {:?}",
        list_rect(&compiled),
        page_in(&runtime, view).1.room.get(),
        page_in(&runtime, view).1.scroll.get(),
        painted(&compiled, url.as_str()).map(|node| node.rect),
        page_in(&runtime, view)
            .1
            .spans
            .borrow()
            .iter()
            .find(|span| *span.key == *url.as_str())
            .map(|span| (span.top, span.bottom)),
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
    // A caret move or a selection leaves the disclosure as it stood (Return joins, as Join
    // does: `return_in_the_vault_password_joins_as_join_does`).
    let stood = page_in(&runtime, view).0.vault_open;
    for event in [
        TextInputEvent::Left { extend: false },
        TextInputEvent::Home { extend: true },
        TextInputEvent::SelectAll,
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
    compile_at(&runtime, short);
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
    compile_at(&runtime, short);
    assert!((page_in(&runtime, view).1.scroll.get() - sliver).abs() < 0.01);
    compile_at(&runtime, taller);
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
        let compiled = compile_at(&runtime, taller);
        assert!(painted(&compiled, key).is_none(), "{key} scrolled away");
        runtime.dispatch(instance, view, event).unwrap();
        // A typed address the worker has not read yet leaves the row
        // nothing to wait on; the vault stays open for the draft.
        assert!(
            page_in(&runtime, view).0.vault_shown(),
            "{key} typed into keeps the vault open"
        );
        let compiled = compile_at(&runtime, taller);
        assert!(
            in_view(&runtime, &compiled, key),
            "{key} typed into shows: the list {:?}, its room {} at {}, the field {:?}, its span {:?}",
            list_rect(&compiled),
            page_in(&runtime, view).1.room.get(),
            page_in(&runtime, view).1.scroll.get(),
            painted(&compiled, key).map(|node| node.rect),
            page_in(&runtime, view)
                .1
                .spans
                .borrow()
                .iter()
                .find(|span| *span.key == *key)
                .map(|span| (span.top, span.bottom)),
        );
    }
    let (app, _) = page_in(&runtime, view);
    assert!(app.url.value().contains('x') && app.url.value().contains('y'));
}

/// The headline and the lockup reach a screen reader in words (§4.7): the headline is a Heading
/// named by its words, in one line or wrapped, and the lockup a progress indicator with its
/// count, named "Setup progress".
#[cfg(a11y_tree)]
#[test]
fn the_headline_and_the_progress_reach_a_screen_reader_in_words() {
    use aterm_a11y::model::Role;
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status_of("running"));
    for width in [999.0, 220.0] {
        let compiled = compile(&app, &SystemStateViewState::default(), width, 900.0);
        let projection =
            crate::native_accessibility::project_native_accessibility(&compiled, None).unwrap();
        let node = |key: &str| {
            let id = projection
                .id_for_key(&UiKey::new(key))
                .unwrap_or_else(|| panic!("{width}: {key} is published"));
            projection
                .update()
                .nodes
                .iter()
                .find(|(candidate, _)| *candidate == id)
                .unwrap()
                .1
                .clone()
        };
        let headline = node(HEADLINE);
        assert_eq!(headline.role(), Role::Heading, "{width}");
        assert_eq!(headline.label(), Some("Setting up your Mac"), "{width}");
        let progress = node("system-state/progress");
        assert_eq!(progress.role(), Role::ProgressIndicator, "{width}");
        assert_eq!(progress.label(), Some("Setup progress"), "{width}");
        assert_eq!(progress.numeric_value(), Some(10.0), "{width}");
        assert_eq!(progress.max_numeric_value(), Some(15.0), "{width}");
    }
}

#[test]
fn a_page_owes_an_animation_wake_only_while_a_run_walks_on_a_shown_page() {
    assert!(
        page_owes_animation_wake(true, true, true),
        "a run on a shown page with the hands off the keys walks, so it owes its tick"
    );
    assert!(
        !page_owes_animation_wake(false, true, true),
        "a settled or finished page stands still and owes no tick"
    );
    assert!(
        !page_owes_animation_wake(true, false, true),
        "a hidden page draws no walk, so it owes no tick"
    );
    assert!(!page_owes_animation_wake(false, false, true));
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
                Some("On an admin Mac: aterm secrets approve 7QK2-M9WD-4HTX-PL3B, or retype the join password"),
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
    let scenarios = page_states();
    // Each state at each text size, with the vault open and shut, is one item, whose cuts are
    // keyed by all three, so the items run side by side; a worker prepares its own fonts and
    // text size, a thread's own in tests. Shut, the page holds the open view's words but the
    // vault's fields, with the disclosure's row the open view leads with, so the shut view runs
    // at every 32pt: a word cut in that shut row, in a band narrower than that, goes unseen.
    let pages: Vec<(f32, usize, bool)> = [1.0_f32, 1.5, 2.0]
        .into_iter()
        .flat_map(|scale| {
            (0..scenarios.len())
                .flat_map(move |at| [true, false].map(move |vault_open| (scale, at, vault_open)))
        })
        .collect();
    let found = std::sync::Mutex::new(std::collections::BTreeMap::<String, Vec<String>>::new());
    crate::native_settings::tests::on_workers(&pages, "words whole", |&(scale, at, vault_open)| {
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        let _ = install_preferences(AppearancePreferences {
            text_scale: scale,
            ..AppearancePreferences::default()
        });
        let (name, status, stop) = &scenarios[at];
        let mut cut = std::collections::BTreeMap::<String, Vec<String>>::new();
        let mut width = 320.0_f32;
        while width <= 800.0 {
            if !vault_open && (width - 320.0) % 32.0 != 0.0 {
                width += 8.0;
                continue;
            }
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
            for (key, audit) in
                compiled.text_fit_cuts(|kind, _, clipped| kind != "text-field" && clipped)
            {
                let entry = cut
                    .entry(format!(
                        "{scale}x {name} open={vault_open} :: clip-truncated {}",
                        key.as_str()
                    ))
                    .or_default();
                if entry.len() < 3 {
                    entry.push(format!("w={width}: {audit}"));
                }
            }
            width += 8.0;
        }
        found
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(cut);
    });
    let cut = found
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    let switch = compiled
        .semantic(&UiKey::new("system-state/line/accounts/switch"))
        .and_then(|node| node.state)
        .expect("the line's switch shows");
    assert!(!switch.enabled, "no line takes a press while Restore runs");
    act(&mut app, "system-state/line/accounts/switch");
    assert!(app.take_action().is_none());
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
/// and runs the plan again, and the card says what comes next for each (§4.8), so no note says
/// it again; while the run retries, one note says it did not finish this time.
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
    assert!(status.notes(&plan).is_empty(), "{:?}", status.notes(&plan));
    let mut app = SystemStateApp::new(plan.clone());
    app.publish(status.clone());
    let card = lines::card_items(&app, &app.reviewed_rows());
    assert_eq!(
        card.iter()
            .map(|item| (item.title.as_str(), item.reason.as_deref()))
            .collect::<Vec<_>>(),
        [("Rust in Spotlight failed", Some(lines::TRIES_BY_ITSELF))]
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
    assert!(status.notes(&plan).is_empty(), "{:?}", status.notes(&plan));
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
    for scenario in FIXTURES {
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
        setup_changed_since_entry("Dock"),
        keep_awake_failing_entry("the menu bar refused the mug"),
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
    assert_eq!(
        keep_awake_failing_entry("disk full").key(),
        keep_awake_failing_entry("").key(),
        "a later trouble replaces the entry, and its clear finds it"
    );
    assert_eq!(
        setup_changed_since_entry("Dock").key(),
        setup_changed_since_entry("").key(),
        "a later pass's rows replace the entry, and its clear finds it"
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
    let mut reading_failed = status_of("failed-retrying");
    reading_failed.retrying = false;
    let (failed, notes) = line(&reading_failed);
    assert!(!failed.start && failed.show.is_some(), "{notes:?}");
    let (failed, notes) = line(&status_of("failed"));
    assert!(failed.start && failed.show.is_some(), "{notes:?}");
    let (step, notes) = line(&status_of("named-result-failed"));
    assert!(step.start);
    assert!(notes.is_empty(), "the card says what comes next: {notes:?}");
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
    assert_eq!(notes, [format!("Setup failed. {SET_UP_NOW_NOTE}")]);
    // A Check that failed is pressed again where it is.
    let (check, notes) = line(&status_of("check-failed"));
    assert!(!check.start);
    assert_eq!(notes, [format!("Check again failed. {CHECK_FAILED_NOTE}")]);
    // A row a Check could not confirm says so on the row, which the headline names: no note
    // says it again.
    let (unconfirmed, notes) = line(&status_of("check-unconfirmed"));
    assert_eq!(unconfirmed.text, "Not confirmed: GitHub repositories");
    assert!(notes.is_empty(), "{notes:?}");
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
    // A vault try held after two waits for this Mac's network or vault to change: no Set up
    // now, and its row, not a note, says so.
    let mut held = status_of("vault-failed");
    held.retrying = false;
    held.rows[0].detail = TRIED_TWICE.into();
    let (vault, notes) = line(&held);
    assert!(!vault.start);
    assert!(notes.is_empty(), "{notes:?}");
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
    // A vault try held after two waits, calm, for this Mac's network or vault to change,
    // whether or not the run retries other steps: nothing says it failed.
    let mut held = status_of("vault-failed");
    held.rows[0].detail = TRIED_TWICE.into();
    for retrying in [false, true] {
        held.retrying = retrying;
        assert_eq!(
            line(&held).text,
            "2 steps waiting: Secrets vault, GitHub repositories"
        );
        assert_eq!(
            crate::system_state_access::menu_line(&held, false),
            None,
            "retrying {retrying}"
        );
        assert_eq!(
            row_view(secrets, &held, Selection::all()).chip,
            Some(Chip::Waiting)
        );
    }
    // Once the tries stop (a run that no longer retries), the step failed, and the headline,
    // the menu line and the diamond say so.
    step.retrying = false;
    assert_eq!(line(&step).text, "1 step failed: GitHub repositories");
}

/// Each line's mark says its rows as the headline says them (design §3.1, §4.4): a step the
/// headline names failed takes the failed mark, though its row reads in place, and a step aterm
/// tries again by itself the waiting one, never the done one.
#[test]
fn a_line_marks_a_failed_step_failed_and_a_retried_one_waiting() {
    let failed = status_of("named-result-failed");
    assert_eq!(mark_on(&failed, Setting::Repositories), Mark::Failed);
    let mut retried = failed.clone();
    retried.retrying = true;
    assert_eq!(mark_on(&retried, Setting::Repositories), Mark::Waiting);
    assert_eq!(
        mark_on(&status_of("vault-failed"), Setting::Secrets),
        Mark::Waiting
    );
    assert_eq!(
        mark_on(&status_of("set-up"), Setting::Repositories),
        Mark::Done
    );
}

/// Debugger access offers no Turn On… of its own: setup's one early elevation turns developer
/// mode on (`aterm_machines::root_steps`), and Settings ▸ Security changes it. A row that reads
/// off, as the doctor reads it on an admin account, waits on that elevation with its note: it
/// needs nobody, has no button, and a press of its key starts nothing. Negative control: the
/// same row read as an administrator's (`Needs you`) is the person's, still with no button.
#[test]
fn the_debugger_row_waits_on_the_elevation_without_a_button() {
    let open = "system-state/row/debugger/open";
    let debugger = Subject::Access(Access::Debugger);
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status_of("mixed"));
    let row = app.reviewed_row(debugger);
    assert_eq!(row.standing, Some(Standing::Waiting));
    assert_eq!(
        row.note.as_deref(),
        Some(aterm_types::macos_setup::DEBUGGER_BY_ELEVATION)
    );
    assert!(!row.attention(), "the elevation's, never the person's");
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 900.0);
    assert!(
        compiled.semantic(&UiKey::new(open)).is_none(),
        "no Turn On…"
    );
    app.gesture(true);
    act(&mut app, open);
    assert!(app.take_action().is_none(), "a press starts nothing");

    let mut admin = status_of("mixed");
    if let Some(reading) = admin.observed.iter_mut().find(|o| o.subject == debugger) {
        reading.standing = Standing::NeedsYou;
        reading.note = None;
    }
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(admin);
    assert!(app.reviewed_row(debugger).attention());
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 900.0);
    assert!(compiled.semantic(&UiKey::new(open)).is_none());
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
            "Not confirmed: GitHub repositories",
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
    assert_eq!(line(&ran).text, "Not confirmed: Dock, Rust in Spotlight");
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
/// waiting chip, never `Failed`, and its line marks it waiting; once the tries stop, all of
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
    let station = mark_on;
    let repositories = Subject::Setting(Setting::Repositories);
    // A reading that failed, with its reason on its row.
    let retrying = status_of("failed-retrying");
    assert_eq!(agree(&retrying), "1 step waiting: GitHub repositories");
    assert_eq!(station(&retrying, Setting::Repositories), Mark::Waiting);
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
        (Some(Chip::Waiting), Some(GITHUB_STOPPED)),
        "the row waits with its reason, calm, never red"
    );
    let stopped = SetupStatus {
        retrying: false,
        ..retrying
    };
    assert_eq!(agree(&stopped), "1 step failed: GitHub repositories");
    assert_eq!(station(&stopped, Setting::Repositories), Mark::Failed);
    assert_eq!(
        row_view(repositories, &stopped, Selection::all()).chip,
        Some(Chip::Failed)
    );
    assert!(stopped.notes(&plan).is_empty(), "the row says why");
    // A step the run could not finish keeps its own reason on its row; the page's note says
    // what comes next.
    let mut step = status_of("set-up");
    step.rows = pass_rows(&PassOutcome {
        rolled_back: None,
        unfinished: vec![(Setting::Dock, Unfinished::Failed)],
    });
    step.retrying = true;
    let plan = step.plan.clone().unwrap();
    assert_eq!(agree(&step), "1 step waiting: Dock");
    assert_eq!(station(&step, Setting::Dock), Mark::Waiting);
    assert_eq!(
        step.notes(&plan),
        [format!("Dock did not finish this time. {RETRIES_NOTE}")]
    );
    let dock = Subject::Setting(Setting::Dock);
    assert_eq!(
        row_view(dock, &step, Selection::all()).note.as_deref(),
        Some("The change did not take.")
    );
    step.retrying = false;
    assert_eq!(agree(&step), "1 step failed: Dock");
    assert_eq!(station(&step, Setting::Dock), Mark::Failed);
    // The headline names it with Set up now, and its row keeps its reason: no note says it again.
    assert!(step.notes(&plan).is_empty(), "{:?}", step.notes(&plan));
    assert_eq!(
        row_view(dock, &step, Selection::all()).note.as_deref(),
        Some("The change did not take.")
    );
}

/// What every signal says of one row (design §3.6): the headline, the row's chip, its line's
/// mark, the menu line and the tab's diamond.
fn signals(
    status: &SetupStatus,
    subject: Subject,
) -> (String, Option<Chip>, Mark, Option<String>, bool) {
    let plan = status.plan.clone().unwrap();
    let all = rows(status, plan.selection);
    let station = mark_on(status, subject.setting());
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

/// Every failure has one kind, and each signal says it alike (design §3.6): the headline, the
/// row's chip, its line's mark, the menu line and the tab's diamond agree for a transient
/// failure while aterm tries it again (waiting, calm) and once the tries are spent (failed), and
/// for one only a person ends (needs you, at once, whether or not the run retries).
#[test]
fn every_failure_kind_signals_alike_in_the_headline_the_row_the_line_the_menu_and_the_diamond() {
    let repositories = Subject::Setting(Setting::Repositories);
    let waiting = |name: &str| {
        (
            format!("1 step waiting: {name}"),
            Some(Chip::Waiting),
            Mark::Waiting,
            None,
            false,
        )
    };
    let failed = |name: &str| {
        (
            format!("1 step failed: {name}"),
            Some(Chip::Failed),
            Mark::Failed,
            Some(format!("System State: {name} failed")),
            true,
        )
    };
    let needs_you = |name: &str| {
        (
            format!("Needs you: {name}"),
            Some(Chip::NeedsYou),
            Mark::NeedsYou,
            Some(format!("System State: Needs you: {name}")),
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
        // The row says who renews the token and the line they run (§3.6).
        assert_eq!(
            (row.note.as_deref(), &row.value),
            (
                Some(GITHUB_TOKEN_RENEW_NOTE),
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

    // A transient vault try held after two waits for a change of network or vault, calm,
    // whether or not the run retries.
    // The repositories wait on the vault's token, so the headline names both.
    let mut held = status_of("vault-failed");
    held.rows[0].detail = TRIED_TWICE.into();
    let (_, chip, station, menu, diamond) = waiting("Secrets vault");
    assert_eq!(
        signals(&held, Subject::Setting(Setting::Secrets)),
        (
            "2 steps waiting: Secrets vault, GitHub repositories".to_owned(),
            chip,
            station,
            menu,
            diamond
        )
    );
}

/// No fixture's signals contradict one another (design §3.6): wherever a row's chip says
/// `Failed` the headline says a step failed, the diamond and the menu line show, and the
/// row's line marks it failed, and wherever the headline counts a step waiting no row reads
/// `Failed` and nothing signals.
#[test]
fn no_fixture_says_waiting_and_failed_at_once() {
    for scenario in FIXTURES {
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
    for line in lines::Line::ALL {
        let key = format!("system-state/line/{}/switch", line.slug());
        assert!(enabled(&key), "{key}");
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
        let developer = line_in(&page_reading(&status), Setting::Spotlight);
        assert_eq!(
            developer.phrase.starts_with("Partly on · "),
            workspace == GroupChoice::Partly,
            "{scenario}: {developer:?}"
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
/// (design §3.2): its line's mark waits, its row has no check mark and says `Not
/// confirmed` in its note, where the headline's Show lands; once confirmed it is settled again.
#[test]
fn an_unconfirmed_row_is_drawn_waiting_with_its_note() {
    let station = |app: &SystemStateApp, setting: Setting| line_in(app, setting).mark;
    let repositories = Subject::Setting(Setting::Repositories);
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status_of("check-unconfirmed"));
    let line = app.headline();
    assert_eq!(line.show, Some(repositories));
    assert_eq!(station(&app, Setting::Repositories), Mark::Waiting);
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
    assert_eq!(station(&app, Setting::Dock), Mark::Waiting);
    assert_eq!(
        app.reviewed_row(Subject::Setting(Setting::Dock))
            .note
            .as_deref(),
        Some(UNCONFIRMED_NOTE)
    );
    // Confirmed, the rows are settled again.
    app.publish(status_of("set-up"));
    assert_eq!(station(&app, Setting::Repositories), Mark::Done);
    assert_eq!(station(&app, Setting::Dock), Mark::Done);
    assert_eq!(app.reviewed_row(repositories).chip, Some(Chip::InPlace));
}

/// The launch's offer of a fresh signed Mac, before the page's first tick answers it
/// ([`fresh_with_grants`]): Full Disk Access and Debugger access read as needing a person once
/// Permissions run.
fn in_review() -> (
    SystemStateApp,
    mpsc::Receiver<crate::workstation_setup::SetupAnswer>,
) {
    let (send, receive) = mpsc::channel();
    let mut app = SystemStateApp::new(Plan::default());
    assert!(app.offer(Request::new(Plan::default(), send)));
    app.publish(fresh_with_grants());
    (app, receive)
}

/// While the first launch's offer waits its headline is its one signal (design §3.3, §3.6):
/// no attention diamond, and the headline says setup is starting; once nothing is offered, the
/// same readings light the tab.
#[test]
fn the_review_headline_is_its_only_signal() {
    // A setup an earlier launch could not finish is said once the review is answered.
    let mut failing = fresh_with_grants();
    failing.rows = vec![SetupStatusRow {
        name: SETUP_ROW.into(),
        status: "failed".into(),
        detail: String::new(),
    }];
    for status in [fresh_with_grants(), failing] {
        let (mut app, _receive) = in_review();
        app.publish(status.clone());
        assert_eq!(app.headline().text, STARTING_HEADLINE);
        assert!(!app.needs_attention());
        assert!(
            !app.presentation(&SystemStateViewState::default())
                .indicators
                .attention
        );
        let mut shown = SystemStateApp::new(Plan::default());
        shown.publish(status);
        assert!(
            shown.needs_attention(),
            "out of the review, the same status lights the tab"
        );
    }
}

/// While the offer waits every row reads as what setup does to it (design §3.3): a grant a
/// person gives once setup runs reads `will change to`, with no `Needs you` and no button to
/// open a pane nobody is asked to open yet.
#[test]
fn review_rows_read_as_what_apply_will_do() {
    let (app, _receive) = in_review();
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 1400.0);
    for (access, slug) in [
        (Access::FullDiskAccess, "full-disk-access"),
        (Access::Debugger, "debugger"),
    ] {
        let row = app.reviewed_row(Subject::Access(access));
        assert_eq!(row.chip, Some(Chip::WillChange { busy: false }), "{slug}");
        assert_eq!(row.open, None, "{slug}");
        assert!(row.label.contains("will change to"), "{}", row.label);
        assert!(
            compiled
                .semantic(&UiKey::new(format!("system-state/row/{slug}/open")))
                .is_none(),
            "{slug}"
        );
    }
    for node in &compiled.semantics {
        assert!(
            !node.label.to_lowercase().contains("needs you"),
            "{}: {}",
            node.key.as_str(),
            node.label
        );
    }
    // Out of the review the same reading asks for the person, with its button.
    let mut shown = SystemStateApp::new(Plan::default());
    shown.publish(fresh_with_grants());
    let row = shown.reviewed_row(Subject::Access(Access::FullDiskAccess));
    assert_eq!(row.chip, Some(Chip::NeedsYou));
    assert_eq!(row.open, Some(Access::FullDiskAccess));
}

/// A newer aterm's setup is said once, calmly, in the headline (design §3.2, §3.5): never a
/// check that does not end. The rows read their values as they stand, Check again reads them
/// anew, and the groups, Set up now and Restore are the newer aterm's.
#[test]
fn a_newer_aterms_setup_never_says_checking() {
    let mut app = SystemStateApp::opened();
    app.publish(status_of("newer"));
    let headline = app.headline();
    assert_eq!(headline.text, NEWER_HEADLINE);
    assert!(!headline.start);
    assert!(!app.needs_attention());
    assert!(app.can_check(), "Check again stays live");
    assert!(!app.can_restore());
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 1400.0);
    assert_eq!(
        label(&compiled, "system-state/row/dock/info"),
        "Dock: 9 apps"
    );
    assert_eq!(
        label(&compiled, "system-state/row/messages/info"),
        "Messages: not read"
    );
    // This aterm cannot read the plan the newer aterm keeps: no line shows an on or off it
    // would only guess, and none is this aterm's to change.
    assert!(
        compiled
            .semantic(&UiKey::new("system-state/line/mac/switch"))
            .is_none()
    );
    assert!(
        !compiled
            .semantics
            .iter()
            .any(|node| node.label == NEWER_SETUP_NOTE),
        "the headline says it; no line repeats it"
    );
    act(&mut app, "system-state/check");
    assert!(matches!(
        app.take_action(),
        Some(SystemStateAction::Check(_))
    ));
}

/// When aterm cannot read this Mac's vault settings, after a Join it refused or the first
/// launch's Join it ran without, the Secrets vault row says why, and the page adds no line
/// that repeats the row (design §3.4).
#[test]
fn a_vault_the_row_explains_is_not_said_again_under_the_headline() {
    for scenario in ["join-refused", "vault-not-joined"] {
        let status = status_of(scenario);
        let plan = status.plan.clone().unwrap();
        let mut app = SystemStateApp::new(plan.clone());
        app.publish(status.clone());
        let secrets = app.reviewed_row(Subject::Setting(Setting::Secrets));
        assert_eq!(
            secrets
                .note
                .as_deref()
                .map(|note| note.trim_end_matches('.')),
            Some(VAULT_UNREAD_NOTE.trim_end_matches('.')),
            "{scenario}"
        );
        assert!(
            status.notes(&plan).is_empty(),
            "{scenario}: {:?}",
            status.notes(&plan)
        );
        assert!(
            !app.headline().start && !app.needs_attention(),
            "{scenario}"
        );
    }
    // Where no row says why, the line does.
    let mut read = status_of("set-up");
    read.rows = vec![calm_setup_row(VAULT_NOT_JOINED_NOTE)];
    let plan = read.plan.clone().unwrap();
    assert_eq!(read.notes(&plan), [VAULT_NOT_JOINED_NOTE]);
}

/// The `failed` scenario is a failure aterm reaches and a person can act on (design §3.6): the
/// run's GitHub repositories step, which aterm no longer tries by itself, said by the headline,
/// the row, the diamond and the menu line together, the headline offering Set up now, its
/// remedy; no note says it again.
#[test]
fn the_failed_scenario_names_its_remedy() {
    let status = status_of("failed");
    let plan = status.plan.clone().unwrap();
    let mut app = SystemStateApp::new(plan.clone());
    app.publish(status.clone());
    let headline = app.headline();
    assert_eq!(headline.text, "1 step failed: GitHub repositories");
    assert!(headline.start, "the headline offers its remedy");
    assert!(status.notes(&plan).is_empty(), "{:?}", status.notes(&plan));
    assert_eq!(
        app.reviewed_row(Subject::Setting(Setting::Repositories))
            .chip,
        Some(Chip::Failed)
    );
    assert!(app.needs_attention());
    assert_eq!(
        menu(&status).as_deref(),
        Some("System State: GitHub repositories failed")
    );
}

/// The `older-cancel` scenario, a Cancel an older aterm recorded: every group reads Leave as is,
/// nothing lights, and the page says nothing of a review (design §3.3, init design §6.1).
#[test]
fn the_older_cancel_scenario_reads_every_group_left_as_is() {
    let status = status_of("older-cancel");
    let plan = status.plan.clone().unwrap();
    let mut app = SystemStateApp::new(plan.clone());
    app.publish(status.clone());
    let headline = app.headline();
    assert_eq!(headline.text, EVERYTHING_LEFT_AS_IS);
    assert!(!headline.start);
    assert!(status.notes(&plan).is_empty());
    assert!(!app.needs_attention());
    assert_eq!(menu(&status), None);
    for subject in Subject::all() {
        assert!(!app.reviewed_row(subject).managed, "{subject:?}");
    }
}

/// aterm widens a plan an older aterm saved with its permissions alone to the base plan every
/// Mac gets, with no ALab signal (§3.5, init-on-by-default §8.1): its run names sync, never the
/// ALab profile this Mac did not get.
#[test]
fn a_widen_to_the_base_plan_names_sync_not_the_alab_profile() {
    let base = Plan::from(aterm_types::macos_setup::BASE_PLAN);
    let mut status = status_of("widened");
    status.plan = Some(base.clone());
    assert_eq!(status.notes(&base), [SYNC_ADDED_NOTE]);
    let whole = Plan::default();
    status.plan = Some(whole.clone());
    assert_eq!(status.notes(&whole), [WIDENED_NOTE]);
}

/// Apple's Command Line Tools, which the GitHub repositories wait on (§3.3): the page reads what
/// their install does. Their tab asking for the password needs the person now, as the debugger
/// step's dialog does, and the headline's Go to tab brings that tab to the front; Apple's
/// installer running is a wait that ends by itself, calm; and the tools missing, with no tab
/// open and nothing installing them, need the person, with Install… on the row, which a press
/// asks the window for, and a note that says why.
#[test]
fn the_command_line_tools_row_reads_what_their_install_does() {
    use crate::system_state_access::menu_line;
    let repositories = Subject::Setting(Setting::Repositories);
    let page = |name: &str| {
        let status = status_of(name);
        let mut app = SystemStateApp::new(Plan::default());
        app.publish(status.clone());
        let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 1400.0);
        let button = compiled.semantic(&UiKey::new(INSTALL_TOOLS)).is_some();
        (app, menu_line(&status, false), button)
    };

    let (mut app, line, button) = page("clt-password");
    let row = app.reviewed_row(repositories);
    assert_eq!(app.headline().text, TOOLS_PASSWORD_HEADLINE);
    assert!(app.headline().go_to_tab);
    assert_eq!(row.chip, Some(Chip::NeedsYou));
    assert_eq!(row.note.as_deref(), Some(TOOLS_PASSWORD_NOTE));
    assert!(!row.install_tools && !button, "its tab is open");
    assert!(app.needs_attention());
    let line = line.expect("the person is needed now");
    assert_eq!(line.note.as_deref(), Some(TOOLS_PASSWORD_HEADLINE));
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 1400.0);
    let go = compiled
        .semantic(&UiKey::new(GO_TO_TOOLS_TAB))
        .expect("the card offers the tab");
    assert_eq!(
        go.label, "Go to tab: Command Line Tools",
        "VoiceOver hears where it goes"
    );
    act(&mut app, GO_TO_TOOLS_TAB);
    assert!(matches!(
        app.take_action(),
        Some(SystemStateAction::GoToToolsTab)
    ));

    let (app, line, button) = page("clt-installing");
    let row = app.reviewed_row(repositories);
    assert_eq!(row.standing, Some(Standing::Waiting));
    assert_eq!(row.chip, Some(Chip::Waiting), "calm while it waits");
    assert_eq!(row.note.as_deref(), Some(INSTALLING_TOOLS_NOTE));
    assert!(!row.install_tools && !button);
    assert_eq!(app.headline().text, "1 step waiting: GitHub repositories");
    assert!(!app.needs_attention(), "a wait that ends by itself");
    assert!(line.is_none(), "{line:?}");

    let (mut app, line, button) = page("clt-missing");
    let row = app.reviewed_row(repositories);
    assert_eq!(app.headline().text, "Needs you: GitHub repositories");
    assert!(!app.headline().go_to_tab, "no tab to go to");
    assert_eq!(row.chip, Some(Chip::NeedsYou));
    assert_eq!(
        row.note.as_deref(),
        Some(TOOLS_NEEDED_NOTE),
        "why, beside Install…"
    );
    assert!(row.install_tools && button);
    assert!(app.needs_attention());
    let line = line.expect("the person is needed now");
    assert_eq!(line.label, "System State: Needs you: GitHub repositories");
    assert_eq!(
        line.note.as_deref(),
        Some(aterm_types::macos_setup::COMMAND_LINE_TOOLS_MISSING),
        "the menu, with no button, says what to do"
    );
    app.by_person = true;
    act(&mut app, INSTALL_TOOLS);
    assert!(matches!(
        app.take_action(),
        Some(SystemStateAction::InstallDeveloperTools { by_person: true })
    ));
}

/// Where a press of Install… or Apple's own installer window stands (§3.3): Apple's window
/// waiting for the person's press needs them there, in the page's words and the menu's; a press
/// kept while a macOS dialog is up is a wait that ends by itself, calm and with no second
/// Install…; and a press whose tab did not open says why on the row, with Install… there again.
#[test]
fn the_command_line_tools_row_says_where_a_press_or_apples_window_stands() {
    use crate::system_state_access::menu_line;
    let repositories = Subject::Setting(Setting::Repositories);
    let page = |name: &str| {
        let status = status_of(name);
        let mut app = SystemStateApp::new(Plan::default());
        app.publish(status.clone());
        let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 1400.0);
        let button = compiled.semantic(&UiKey::new(INSTALL_TOOLS)).is_some();
        (app, menu_line(&status, false), button)
    };

    let (app, line, button) = page("clt-apple-window");
    let row = app.reviewed_row(repositories);
    assert_eq!(app.headline().text, "Needs you: GitHub repositories");
    assert!(
        !app.headline().go_to_tab,
        "Apple's window is not aterm's tab"
    );
    assert_eq!(row.chip, Some(Chip::NeedsYou));
    assert_eq!(row.note.as_deref(), Some(APPLE_WINDOW_NOTE));
    assert!(
        !row.install_tools && !button,
        "Apple's installer is already open"
    );
    assert!(app.needs_attention());
    let line = line.expect("the person is needed now");
    assert_eq!(line.note.as_deref(), Some(APPLE_WINDOW_NOTE));

    let (app, line, button) = page("clt-queued");
    let row = app.reviewed_row(repositories);
    assert_eq!(row.standing, Some(Standing::Waiting));
    assert_eq!(row.chip, Some(Chip::Waiting), "calm while it waits");
    assert_eq!(row.note.as_deref(), Some(TOOLS_QUEUED_NOTE));
    assert!(!row.install_tools && !button, "the press is kept");
    assert_eq!(app.headline().text, "1 step waiting: GitHub repositories");
    assert!(!app.needs_attention(), "a wait that ends by itself");
    assert!(line.is_none(), "{line:?}");

    let (app, line, button) = page("clt-not-opened");
    let row = app.reviewed_row(repositories);
    assert_eq!(row.chip, Some(Chip::NeedsYou));
    assert_eq!(row.note.as_deref(), Some(TOOLS_NOT_OPENED_NOTE));
    assert!(row.install_tools && button, "Install… is there again");
    assert!(app.needs_attention());
    assert_eq!(
        line.map(|line| line.label),
        Some("System State: Needs you: GitHub repositories".to_owned())
    );
}

/// Apple's installer window may sit behind other windows (§3.3): while the row reads it, the card
/// beside `Finish in Apple's installer window` offers Go to installer, which only goes there. A
/// press once the row reads otherwise (an install under way, the Mac Setup tab's password) asks
/// for nothing, and no card offers it then.
#[test]
fn the_apple_window_card_goes_to_the_installer_and_a_stale_press_goes_nowhere() {
    let offered = |app: &SystemStateApp| {
        compile(app, &SystemStateViewState::default(), 999.0, 1400.0)
            .semantic(&UiKey::new(GO_TO_TOOLS_INSTALLER))
            .map(|go| go.label.clone())
    };
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status_of("clt-apple-window"));
    assert_eq!(
        offered(&app).as_deref(),
        Some(lines::GO_TO_INSTALLER_LABEL),
        "the card offers Apple's window, and VoiceOver hears where it goes"
    );
    act(&mut app, GO_TO_TOOLS_INSTALLER);
    assert!(matches!(
        app.take_action(),
        Some(SystemStateAction::GoToToolsInstaller)
    ));
    assert!(app.take_action().is_none(), "one press, one action");

    for other in ["clt-installing", "clt-password", "clt-missing"] {
        app.publish(status_of(other));
        assert_eq!(offered(&app), None, "{other}: no window to go to");
        act(&mut app, GO_TO_TOOLS_INSTALLER);
        assert!(
            app.take_action().is_none(),
            "{other}: a stale press asks for nothing"
        );
    }
}

/// Low Data Mode holding the tools' download back is a wait that ends by itself (§3.3): calm,
/// with no menu line, and Install… there for a person who turned it off. An account that may not
/// install them is told so on the row, with no Install… that would only say it again, and no
/// menu line, since nothing here changes it.
#[test]
fn the_command_line_tools_row_reads_low_data_mode_and_a_non_administrator() {
    use crate::system_state_access::menu_line;
    let repositories = Subject::Setting(Setting::Repositories);
    let page = |name: &str| {
        let status = status_of(name);
        let mut app = SystemStateApp::new(Plan::default());
        app.publish(status.clone());
        let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 1400.0);
        let button = compiled.semantic(&UiKey::new(INSTALL_TOOLS)).is_some();
        (app, menu_line(&status, false), button)
    };

    let (app, line, button) = page("clt-low-data");
    let row = app.reviewed_row(repositories);
    assert_eq!(row.standing, Some(Standing::Waiting));
    assert_eq!(
        row.chip,
        Some(Chip::Waiting),
        "calm until the download can run"
    );
    assert_eq!(row.note.as_deref(), Some(TOOLS_LOW_DATA_NOTE));
    assert!(row.install_tools && button, "Install… stays");
    assert_eq!(app.headline().text, "1 step waiting: GitHub repositories");
    assert!(!app.needs_attention(), "a wait that ends by itself");
    assert!(line.is_none(), "{line:?}");

    let (app, line, button) = page("clt-not-admin");
    let row = app.reviewed_row(repositories);
    assert_eq!(row.chip, Some(Chip::NeedsYou));
    assert_eq!(row.note.as_deref(), Some(TOOLS_NOT_ADMIN_NOTE));
    assert!(!row.install_tools && !button, "no Install… to loop on");
    assert!(line.is_none(), "{line:?}");
}

/// The ALab profile aterm adds by itself, by automatic admission with nobody's press, waits for
/// another setup in its own words (§3.5): nobody chose it, so the page never says "your
/// choice", and the wait is calm.
#[test]
fn a_profile_aterm_adds_by_itself_waits_in_its_own_words() {
    let status = status_of("queued-widen");
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status.clone());
    assert_eq!(
        app.headline().text,
        "Waiting to add this Mac's ALab profile…"
    );
    assert_eq!(status.notes(&app.reviewed_plan()), [QUEUED_WIDEN_NOTE]);
    assert!(!app.needs_attention());
    assert!(crate::system_state_access::menu_line(&status, false).is_none());
    let choice = SetupStatus {
        queued_widen: false,
        ..status
    };
    app.publish(choice.clone());
    assert_eq!(app.headline().text, "Waiting to apply your choice…");
    assert_eq!(choice.notes(&app.reviewed_plan()), [QUEUED_CHOICE_NOTE]);
}

/// A restored installer tab owns its unfinished work without claiming a password prompt or
/// offering a second installer. A confirmed live installer remains a calm wait; once that
/// work is gone and the tools are still missing, the row offers a retry.
#[test]
fn a_restored_command_line_tools_tab_continues_before_another_install_is_offered() {
    let repositories = Subject::Setting(Setting::Repositories);
    let mut status = status_of("clt-recovered");
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status.clone());
    let view = SystemStateViewState::default();
    let row = app.reviewed_row(repositories);
    assert_eq!(app.headline().text, TOOLS_RECOVERED_HEADLINE);
    assert!(
        app.headline().go_to_tab,
        "Go to tab brings the restored tab to the front"
    );
    assert_eq!(row.standing, Some(Standing::NeedsYou));
    assert_eq!(row.chip, Some(Chip::NeedsYou));
    assert_eq!(row.note.as_deref(), Some(TOOLS_RECOVERED_NOTE));
    assert!(!row.install_tools);
    assert!(
        compile(&app, &view, 999.0, 1400.0)
            .semantic(&UiKey::new(INSTALL_TOOLS))
            .is_none()
    );
    assert!(app.needs_attention());
    assert_eq!(
        crate::system_state_access::menu_line(&status, false).and_then(|line| line.note),
        Some(TOOLS_RECOVERED_HEADLINE.to_owned())
    );
    app.by_person = true;
    act(&mut app, INSTALL_TOOLS);
    assert!(
        app.take_action().is_none(),
        "a saved Install action is obsolete"
    );

    status.developer_tools = Some(ToolsInstall::Queued);
    app.publish(status.clone());
    let row = app.reviewed_row(repositories);
    assert_eq!(row.standing, Some(Standing::Waiting));
    assert_eq!(row.note.as_deref(), Some(TOOLS_QUEUED_NOTE));
    assert!(!row.install_tools);
    assert!(!app.needs_attention());
    assert!(crate::system_state_access::menu_line(&status, false).is_none());

    // The live process probe can confirm installation after recovery without requiring a
    // person, even though the persisted receipt alone did not say the command started.
    status.developer_tools = Some(ToolsInstall::Installing);
    app.publish(status.clone());
    let row = app.reviewed_row(repositories);
    assert_eq!(row.standing, Some(Standing::Waiting));
    assert_eq!(row.note.as_deref(), Some(INSTALLING_TOOLS_NOTE));
    assert!(!row.install_tools);
    assert!(!app.needs_attention());
    assert!(crate::system_state_access::menu_line(&status, false).is_none());

    status.developer_tools = None;
    app.publish(status);
    assert!(app.reviewed_row(repositories).install_tools);
    assert!(
        compile(&app, &view, 999.0, 1400.0)
            .semantic(&UiKey::new(INSTALL_TOOLS))
            .is_some()
    );
    act(&mut app, INSTALL_TOOLS);
    assert!(matches!(
        app.take_action(),
        Some(SystemStateAction::InstallDeveloperTools { by_person: true })
    ));
    assert!(app.take_action().is_none(), "one explicit retry");
}

/// A step the run could not finish keeps its reason on its row (§3.6), in the words its step
/// decided (`GitHub can't be reached`, never GitHub's own), and the headline names it with Set
/// up now, what comes next: no note says it again. While the run still tries it, the note says
/// that aterm does, which neither the row nor the headline says.
#[test]
fn a_failed_step_keeps_its_reason_on_its_row() {
    let status = status_of("failed");
    let mut app = SystemStateApp::new(Plan::default());
    app.publish(status.clone());
    let row = app.reviewed_row(Subject::Setting(Setting::Repositories));
    assert_eq!(row.chip, Some(Chip::Failed));
    assert_eq!(row.note.as_deref(), Some(GITHUB_UNREACHED));
    assert!(app.notes(&app.reviewed_plan()).is_empty());
    assert_eq!(
        (app.headline().text.as_str(), app.headline().start),
        ("1 step failed: GitHub repositories", true)
    );
    // While the run still tries it again, the row keeps the same reason under its waiting chip.
    let mut retrying = status;
    retrying.retrying = true;
    app.publish(retrying);
    let row = app.reviewed_row(Subject::Setting(Setting::Repositories));
    assert_eq!(row.chip, Some(Chip::Waiting));
    assert_eq!(row.note.as_deref(), Some(GITHUB_UNREACHED));
    assert_eq!(
        app.notes(&app.reviewed_plan()),
        [format!(
            "GitHub repositories did not finish this time. {RETRIES_NOTE}"
        )]
    );
}

/// While a newer aterm's setup is in force (§3.2) each row keeps its reading's own note, and the
/// vault reads as it stands: its address in plain words, with no field, no hint and no Join,
/// since this aterm joins nothing.
#[test]
fn a_newer_aterms_page_keeps_row_notes_and_reads_the_vault_as_it_stands() {
    let mut app = SystemStateApp::opened();
    app.publish(status_of("newer"));
    assert!(app.status.newer());
    assert!(!app.vault_editable());
    app.vault_open = Some(true);
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 1400.0);
    // The page as drawn: this aterm knows no plan, so its rows are the readings as they stand.
    let heard: Vec<&str> = compiled
        .semantics
        .iter()
        .map(|node| node.label.as_str())
        .filter(|label| !label.is_empty())
        .collect();
    let address = compiled
        .semantic(&UiKey::new("system-state/vault/address"))
        .unwrap_or_else(|| panic!("the address is shown in words: {heard:?}"));
    assert_eq!(address.label, app.vault_address_as_it_stands());
    assert!(address.state.is_none(), "words, not a control");
    for key in [
        "system-state/url",
        "system-state/password",
        "system-state/vault/join",
    ] {
        assert!(
            compiled.semantic(&UiKey::new(key)).is_none(),
            "{key} is not drawn: {heard:?}"
        );
    }
    assert!(
        !heard.iter().any(|label| label.contains("Password")),
        "no password and no hint: {heard:?}"
    );
    let before = app.url.value().to_owned();
    let mut view = SystemStateViewState::default();
    app.act(
        &mut view,
        "system-state/url",
        Some(SemanticInput::Text("http://elsewhere.local".to_owned())),
    );
    assert_eq!(app.url.value(), before, "the address stays as it stands");
}

/// A stage the run's ledger parked (aterm stopped in it twice, or it took over an hour;
/// init-on-by-default design §6.5) reaches the page as its stage row (`waiting`, in the ledger's
/// words): the rows it changes say so, calm, with no Will change chip, and the headline names the
/// step waiting. A stage that took over an hour is checked again by the page itself, after its
/// wait; one aterm stopped in twice waits for the person's Check again, since trying it by itself
/// could stop aterm again. Check again reads the rows again and lifts the park. Nothing signals:
/// no diamond, no menu line.
#[test]
fn a_parked_stage_says_so_on_the_page() {
    let repositories = Subject::Setting(Setting::Repositories);
    for (words, by_itself) in [
        (
            format!(
                "Stopped twice while cloning GitHub repositories; {}",
                StageRow::HELD_TAIL
            ),
            false,
        ),
        (
            format!(
                "Took over an hour cloning GitHub repositories; {}",
                StageRow::PARKED_TAIL
            ),
            true,
        ),
    ] {
        let mut status = status_of("parked");
        status.rows[0].detail.clone_from(&words);
        let plan = status.plan.clone().unwrap();
        let row = row_view(repositories, &status, plan.selection);
        assert_eq!(row.note.as_deref(), Some(words.as_str()));
        assert_eq!(row.chip, Some(Chip::Waiting), "{words}");
        assert!(row.label.contains("waiting"), "{}", row.label);
        let all = rows(&status, plan.selection);
        let line = headline(&status, &all, Some(&plan));
        assert_eq!(line.text, "1 step waiting: GitHub repositories");
        assert!(line.check && !line.start && line.show.is_none(), "{line:?}");
        assert!(status.notes(&plan).is_empty(), "the row says it, once");
        assert!(menu(&status).is_none());
        let mut app = SystemStateApp::new(plan.clone());
        app.publish(status.clone());
        assert!(!app.needs_attention());
        let now = aterm_time::Instant::now();
        app.tick_at(now);
        app.tick_at(now + by_itself_after(1) + Duration::from_secs(1));
        let checked = matches!(
            app.actions.pop_front(),
            Some(SystemStateAction::Check(checked)) if checked == plan
        );
        assert_eq!(
            checked, by_itself,
            "the page checks again by itself: {words}"
        );
        // Check again reads the rows again whenever a person presses it.
        let mut app = SystemStateApp::new(plan.clone());
        app.publish(status.clone());
        act(&mut app, "system-state/check");
        assert!(
            matches!(
                app.actions.pop_front(),
                Some(SystemStateAction::Check(checked)) if checked == plan
            ),
            "Check again reads the rows again"
        );
    }
    // CONTROL: the same row with no parked stage row is a change Set up now makes.
    let mut status = status_of("parked");
    status.rows.clear();
    let plan = status.plan.clone().unwrap();
    let line = headline(&status, &rows(&status, plan.selection), Some(&plan));
    assert!(!line.check && line.start, "{line:?}");
}

/// A stage that has run past the hour in the aterm still running it (init-on-by-default design
/// §6.5) reaches the page as a `running` stage row in the ledger's words: the rows it changes say
/// so, calm, and nothing offers Check again, which could change nothing while that aterm runs it;
/// it ends by itself or when aterm quits. Before, it read as parked with Check again, which
/// lifted the park and tried nothing.
#[test]
fn a_stage_still_running_past_the_hour_says_so_with_no_check_again() {
    let repositories = Subject::Setting(Setting::Repositories);
    let words = format!(
        "Still cloning GitHub repositories after more than an hour; {}",
        StageRow::STILL_RUNNING_TAIL
    );
    let mut status = status_of("parked");
    status.rows[0].status = "running".into();
    status.rows[0].detail.clone_from(&words);
    let plan = status.plan.clone().unwrap();
    let row = row_view(repositories, &status, plan.selection);
    assert_eq!(row.note.as_deref(), Some(words.as_str()));
    assert_eq!(row.chip, Some(Chip::Waiting));
    let line = headline(&status, &rows(&status, plan.selection), Some(&plan));
    assert!(!line.check, "no Check again: {line:?}");
    assert!(menu(&status).is_none(), "nothing signals");
    // CONTROL: the same stage once its run has ended is parked, with Check again.
    status.rows[0].status = "waiting".into();
    status.rows[0].detail = format!(
        "Took over an hour cloning GitHub repositories; {}",
        StageRow::PARKED_TAIL
    );
    let line = headline(&status, &rows(&status, plan.selection), Some(&plan));
    assert!(line.check, "{line:?}");
}

/// A stage resumed after aterm stopped in it says so on the rows it changes while it goes, in
/// the ledger's words.
#[test]
fn a_resumed_stage_says_so_on_the_page() {
    let status = status_of("resumed");
    let plan = status.plan.clone().unwrap();
    let dock = row_view(Subject::Setting(Setting::Dock), &status, plan.selection);
    assert_eq!(dock.note.as_deref(), Some(StageRow::RESUMING));
    assert_eq!(dock.chip, Some(Chip::WillChange { busy: true }));
    // A row of the stage already in place keeps its check, and a row of another stage says
    // nothing of it.
    let caps = row_view(Subject::Setting(Setting::CapsLock), &status, plan.selection);
    assert_eq!((caps.chip, caps.note), (Some(Chip::InPlace), None));
    let spotlight = row_view(
        Subject::Setting(Setting::Spotlight),
        &status,
        plan.selection,
    );
    assert_eq!(spotlight.note, None);
    // CONTROL: a running stage row with no words of its own adds none.
    let mut plain = status.clone();
    plain.rows[0].detail.clear();
    assert_eq!(
        row_view(Subject::Setting(Setting::Dock), &plain, plan.selection).note,
        None
    );
}

/// A step aterm tries again by itself waits, calm (§3.5): its chip reads `Waiting`, the word
/// VoiceOver hears for it, never `Will change`, and no change a press makes counts it.
#[test]
fn a_step_aterm_retries_reads_waiting_as_voiceover_hears_it() {
    let mut step = status_of("named-result-failed");
    step.retrying = true;
    let plan = step.plan.clone().unwrap();
    let row = row_view(
        Subject::Setting(Setting::Repositories),
        &step,
        plan.selection,
    );
    assert_eq!(row.chip, Some(Chip::Waiting));
    assert!(
        row.label
            .ends_with(&format!(", {}", Chip::Waiting.spoken()))
            || row
                .label
                .contains(&format!(", {} ", Chip::Waiting.spoken())),
        "{}",
        row.label
    );
    assert!(!row.to_change());
    // A row that waits on something its note names reads `Waiting` the same way.
    let vault = status_of("vault");
    let repositories = row_view(
        Subject::Setting(Setting::Repositories),
        &vault,
        plan.selection,
    );
    assert_eq!(repositories.chip, Some(Chip::Waiting));
    assert!(
        repositories.label.contains("waiting"),
        "{}",
        repositories.label
    );
}

/// A vault try held after two tries waits, calm, for this Mac's network or vault to change
/// (§3.5): its row says why and what comes next, the headline counts it waiting with no Set up
/// now, and neither the diamond nor the menu line says it failed.
#[test]
fn a_vault_try_held_after_two_waits_calmly() {
    let status = status_of("vault-held");
    let plan = status.plan.clone().unwrap();
    let secrets = row_view(Subject::Setting(Setting::Secrets), &status, plan.selection);
    assert_eq!(secrets.chip, Some(Chip::Waiting));
    assert_eq!(secrets.note.as_deref(), Some(TRIED_TWICE));
    assert!(!TRIED_TWICE.contains("Set up now"));
    let line = headline(&status, &rows(&status, plan.selection), Some(&plan));
    assert_eq!(
        line.text,
        "2 steps waiting: Secrets vault, GitHub repositories"
    );
    assert!(!line.start);
    assert!(status.notes(&plan).is_empty(), "{:?}", status.notes(&plan));
    assert!(menu(&status).is_none());
    let mut app = SystemStateApp::new(plan);
    app.publish(status);
    assert!(!app.needs_attention());
}

/// A note says every line of itself (§3.6): a narrow page wraps it, never cutting it short.
#[test]
fn a_note_is_never_cut_mid_sentence() {
    let status = status_of("restore-at-login");
    let plan = status.plan.clone().unwrap();
    let note = status.notes(&plan).remove(0);
    let mut app = SystemStateApp::new(plan);
    app.publish(status);
    let compiled = compile(&app, &SystemStateViewState::default(), 220.0, 4000.0);
    let lines: Vec<String> = compiled
        .paint
        .iter()
        .filter(|node| node.key.as_str().starts_with("system-state/note/0/line-"))
        .filter_map(|node| match &node.content {
            UiContent::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect();
    assert!(lines.len() > 3, "a narrow page wraps it: {lines:?}");
    assert_eq!(lines.join(" "), note);
}

/// A step that failed, whose row keeps its reason, is said by the row and the headline alone
/// (§3.6): no page note repeats it. A row a Check could not confirm likewise.
#[test]
fn a_failed_or_unconfirmed_row_is_said_once() {
    for scenario in ["failed", "check-unconfirmed"] {
        let status = status_of(scenario);
        let plan = status.plan.clone().unwrap();
        let all = rows(&status, plan.selection);
        let line = headline(&status, &all, Some(&plan));
        let row = row_view(
            Subject::Setting(Setting::Repositories),
            &status,
            plan.selection,
        );
        assert!(
            line.text.contains("GitHub repositories"),
            "{scenario}: {line:?}"
        );
        assert!(row.note.is_some(), "{scenario}: the row keeps its reason");
        assert!(
            status.notes(&plan).is_empty(),
            "{scenario}: {:?}",
            status.notes(&plan)
        );
    }
    // CONTROL: a failed step with no reason on its row still says what comes next, in its card
    // item.
    let named = status_of("named-result-failed");
    let plan = named.plan.clone().unwrap();
    assert!(named.notes(&plan).is_empty());
    let mut app = SystemStateApp::new(plan);
    app.publish(named);
    let card = lines::card_items(&app, &app.reviewed_rows());
    assert!(
        card.iter()
            .any(|item| item.title == "GitHub repositories failed"
                && item.reason.as_deref() == Some(lines::TRIES_BY_ITSELF)),
        "{card:?}"
    );
}

/// The ALab profile aterm adds by itself is said in one note (§3.5) while the run it starts goes:
/// the widen applies at once, with no offer of its own to say it again.
#[test]
fn a_widened_profile_says_one_alab_note() {
    let mut app = SystemStateApp::new(permissions_only());
    let mut status = status_of("widened");
    status.widened = true;
    status.busy = true;
    status.running = vec![Setting::Dock];
    app.publish(status);
    let notes = app.notes(&app.reviewed_plan());
    assert_eq!(
        notes.iter().filter(|note| *note == WIDENED_NOTE).count(),
        1,
        "{notes:?}"
    );
}

/// The Command Line Tools row says what it needs in full (§3.3): its note names the tools, and
/// its button shows its whole label, on one line where the card has room for it and wrapped
/// where it has not, VoiceOver hearing the whole label either way.
#[test]
fn the_tools_row_names_them_and_its_button_says_it_whole_where_it_fits() {
    let status = status_of("clt-missing");
    let plan = status.plan.clone().unwrap();
    let row = row_view(
        Subject::Setting(Setting::Repositories),
        &status,
        plan.selection,
    );
    assert_eq!(row.note.as_deref(), Some(TOOLS_NEEDED_NOTE));
    assert!(TOOLS_NEEDED_NOTE.contains("Command Line Tools"));
    let mut app = SystemStateApp::new(plan);
    app.publish(status);
    let face = |width: f32| {
        let compiled = compile(&app, &SystemStateViewState::default(), width, 4000.0);
        let node = compiled
            .paint
            .iter()
            .find(|node| node.key.as_str() == INSTALL_TOOLS)
            .expect("Install… is painted");
        let UiContent::Button(control) = &node.content else {
            panic!("Install… is a button");
        };
        assert_eq!(label(&compiled, INSTALL_TOOLS), INSTALL_TOOLS_LABEL);
        control
            .spec
            .visual_label
            .clone()
            .unwrap_or_else(|| control.spec.label.clone())
    };
    assert_eq!(face(999.0), INSTALL_TOOLS_LABEL, "wide");
    // Narrow, the card's button wraps its whole face rather than cut it.
    assert_eq!(
        face(240.0).replace('\n', " "),
        INSTALL_TOOLS_LABEL,
        "narrow"
    );
}

/// The Mac Setup tab a restore brought back is named as its title and the headline
/// name it.
#[test]
fn a_restored_tools_tab_is_named_as_its_title_names_it() {
    let tab = format!("{} tab", crate::clt_install::TAB_TITLE);
    assert!(
        TOOLS_RECOVERED_NOTE.starts_with(&tab),
        "{TOOLS_RECOVERED_NOTE:?}"
    );
    assert!(
        TOOLS_RECOVERED_HEADLINE.ends_with(&tab),
        "{TOOLS_RECOVERED_HEADLINE:?}"
    );
    let status = status_of("clt-recovered");
    let plan = status.plan.clone().unwrap();
    let row = row_view(
        Subject::Setting(Setting::Repositories),
        &status,
        plan.selection,
    );
    assert_eq!(row.note.as_deref(), Some(TOOLS_RECOVERED_NOTE));
}

/// The Secrets vault row in fluid-swarm §3.6's words: while this Mac asks ALab's members, it
/// reads `Asking ALab` and describes automatic retries; once a member let it
/// in, `Allowed`, calm, with nothing for the person to do.
#[test]
fn the_vault_row_asks_alab_in_the_pages_words() {
    let secrets = Subject::Setting(Setting::Secrets);
    let asking = status_of("vault-asking");
    let plan = asking.plan.clone().unwrap();
    let row = row_view(secrets, &asking, plan.selection);
    assert_eq!(row.chip, Some(Chip::Waiting));
    assert_eq!(
        row.note.as_deref(),
        Some(aterm_types::macos_setup::VAULT_ASKING_NOTE)
    );
    assert!(row.note.unwrap().contains("automatically"));
    assert!(matches!(
        &row.value,
        RowValue::Change { now, .. } if now == aterm_types::macos_setup::VAULT_ASKING
    ));
    let allowed = status_of("vault-allowed");
    let row = row_view(secrets, &allowed, plan.selection);
    assert_eq!(row.chip, Some(Chip::Waiting));
    assert!(matches!(
        &row.value,
        RowValue::Change { now, .. } if now == aterm_types::macos_setup::VAULT_ALLOWED
    ));
    assert!(menu(&allowed).is_none());
    let mut app = SystemStateApp::new(plan);
    app.publish(allowed);
    assert!(!app.needs_attention());
}

#[test]
fn automatic_admission_never_renders_a_phrase_prompt() {
    for state in ["vault-asking", "vault-asking-long", "vault-allowed"] {
        let status = status_of(state);
        let mut app = SystemStateApp::new(status.plan.clone().unwrap());
        app.publish(status);
        let view = SystemStateViewState::default();
        let compiled = compile(&app, &view, 999.0, 4000.0);
        assert!(
            compiled.semantic(&UiKey::new(PHRASE_FIELD)).is_none(),
            "{state}"
        );
        assert!(
            compiled.semantic(&UiKey::new(PHRASE_JOIN)).is_none(),
            "{state}"
        );
    }
}

/// Each thing a phrase can come to has its own words under the field. One that reached no Mac in
/// ALab was asked nothing, so its words stay for another Join under the not-reached line (the
/// broker's failure stays in that line); one refused is spent, the field
/// empties and the refused line shows; one that let this Mac in empties the field too. One a Mac
/// a rotation dropped typed (one-mesh E2, f299) is spent as well, and its line says how that Mac
/// comes back, never to press Allow again.
///
/// NEGATIVE CONTROL: have `phrase_finished` wipe the field for `NotReached` too: the words the
/// person still holds good are gone. Have `phrase_help` read `Dropped` as `Refused`: the line
/// says to press Allow again.
#[test]
fn each_phrase_outcome_has_its_own_words_and_only_a_spent_phrase_empties_the_field() {
    assert_eq!(
        [
            PHRASE_HELP,
            PHRASE_REFUSED,
            PHRASE_NOT_REACHED,
            PHRASE_STOPPED,
            PHRASE_DROPPED,
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len(),
        5,
        "each outcome has its own words"
    );
    let asking = status_of("vault-asking");
    let mut app = SystemStateApp::new(asking.plan.clone().unwrap());
    app.publish(asking);
    let mut view = SystemStateViewState::default();
    let typed = |app: &SystemStateApp| app.field_projection(PHRASE_FIELD).unwrap().text;
    let try_with =
        |app: &mut SystemStateApp, view: &mut SystemStateViewState, outcome: PhraseOutcome| {
            app.act(
                view,
                PHRASE_FIELD,
                Some(SemanticInput::Text("river stone lamp".into())),
            );
            app.act(view, PHRASE_JOIN, None);
            assert!(app.phrase_finished(app.phrase_try().unwrap(), Some(outcome)));
        };

    try_with(&mut app, &mut view, PhraseOutcome::NotReached);
    assert_eq!(app.phrase_help(), PHRASE_NOT_REACHED);
    assert_eq!(typed(&app), "river stone lamp", "the words stay");
    app.act(&mut view, PHRASE_JOIN, None);
    let joins = app
        .actions
        .drain(..)
        .filter(|action| matches!(action, SystemStateAction::JoinPhrase(..)))
        .count();
    assert_eq!(joins, 2, "the same words go again with one press");
    assert!(app.phrase_finished(app.phrase_try().unwrap(), Some(PhraseOutcome::NotReached)));

    try_with(&mut app, &mut view, PhraseOutcome::Refused);
    assert_eq!(app.phrase_help(), PHRASE_REFUSED);
    assert_eq!(typed(&app), "", "a refused phrase is spent");

    try_with(&mut app, &mut view, PhraseOutcome::Joined);
    assert_eq!(app.phrase_help(), PHRASE_HELP);
    assert_eq!(typed(&app), "", "a phrase that let this Mac in is spent");

    try_with(&mut app, &mut view, PhraseOutcome::Dropped);
    assert_eq!(app.phrase_help(), PHRASE_DROPPED);
    assert!(!PHRASE_DROPPED.contains("Allow"), "{PHRASE_DROPPED}");
    assert!(PHRASE_DROPPED.contains("new Mac"), "{PHRASE_DROPPED}");
    assert_eq!(typed(&app), "", "no phrase lets a dropped Mac in");

    // A try whose worker ended before it said what the phrase came to may not have sent it: the
    // line says plainly that the try stopped, and the words stay for Join to try again.
    try_with(&mut app, &mut view, PhraseOutcome::Stopped);
    assert_eq!(app.phrase_help(), PHRASE_STOPPED);
    assert_eq!(typed(&app), "river stone lamp", "the words stay");
}

/// GitHub refusing the vault's token says who renews it (any member can) and the exact
/// line they run, which the row's Copy Command puts on the clipboard (§3.6).
#[test]
fn a_refused_github_token_says_who_renews_it_and_copies_the_line() {
    let repositories = Subject::Setting(Setting::Repositories);
    let status = status_of("github-sign-in");
    let plan = status.plan.clone().unwrap();
    let row = row_view(repositories, &status, plan.selection);
    let note = row.note.clone().unwrap();
    assert!(note.contains("any member of the vault"), "{note}");
    assert!(
        note.ends_with(aterm_types::macos_setup::GITHUB_TOKEN_COMMAND),
        "{note}"
    );
    assert!(row.copy_command);
    let mut app = SystemStateApp::new(plan);
    app.publish(status);
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 4000.0);
    assert_eq!(label(&compiled, COPY_TOKEN_COMMAND), "Copy Command");
    act(&mut app, COPY_TOKEN_COMMAND);
    assert!(matches!(
        app.actions.pop_front(),
        Some(SystemStateAction::CopyCommand { command, .. })
            if command == aterm_types::macos_setup::GITHUB_TOKEN_COMMAND
    ));
    // CONTROL: a row that needs a person for anything else has no Copy Command.
    let tools = status_of("clt-missing");
    let tools_plan = tools.plan.clone().unwrap();
    assert!(!row_view(repositories, &tools, tools_plan.selection).copy_command);
}

/// A setting the person changed after aterm set it is theirs (init-on-by-default design §6.6):
/// the digest's entry says how to have aterm set it again, never to press Set up now, which
/// keeps their change.
#[test]
fn the_changed_since_entry_names_no_set_up_now() {
    let entry = format!("{:?}", setup_changed_since_entry("Dock"));
    assert!(!entry.contains("Set up now"), "{entry}");
    assert!(entry.contains(CHANGED_SINCE_STEP), "{entry}");
    let status = status_of("changed-by-you");
    let plan = status.plan.clone().unwrap();
    let dock = row_view(Subject::Setting(Setting::Dock), &status, plan.selection);
    assert_eq!(
        (dock.chip, dock.note.as_deref()),
        (None, Some(aterm_types::macos_setup::CHANGED_BY_YOU))
    );
}

/// Keyboard focus on a card item that leaves the card (the walk moved on to its next page) moves
/// to the card's first button, its default action, never staying on a control that is gone.
#[test]
fn focus_on_a_card_item_that_leaves_the_card_moves_to_its_first_button() {
    let walk = status_of("walk");
    let (mut runtime, instance, view) = runtime_with_app(page_reading(&walk));
    let first = {
        let Some(NativeApp::SystemState(app)) = runtime.app(instance) else {
            panic!("the page");
        };
        app.card_buttons()
    };
    let open = first
        .first()
        .cloned()
        .expect("the walk's page has its button");
    let Some(AppViewState::SystemState(state)) = runtime.view_state_mut(view) else {
        panic!("the page's view");
    };
    state.common.last_focus = Some(UiKey::new(open.clone()));
    // Full Disk Access granted: the walk moves to its next page.
    let mut next = walk.clone();
    for row in &mut next.observed {
        if row.subject == Subject::Access(Access::FullDiskAccess) {
            row.standing = Standing::InPlace;
            row.now = Some("On".to_owned());
        }
    }
    if let Some(page) = next.walk.as_mut() {
        page.access = Access::DeveloperTools;
        page.at = 2;
    }
    assert!(runtime.publish_system_state(next));
    let Some(AppViewState::SystemState(state)) = runtime.view_state(view) else {
        panic!("the page's view");
    };
    let focus = state.common.last_focus.clone();
    assert_ne!(
        focus,
        Some(UiKey::new(open)),
        "focus stays on a button that is gone"
    );
    assert!(focus.is_some(), "focus goes to the card's next default");
}

/// The hero for `app`, as the page draws it.
fn hero_of(app: &SystemStateApp) -> lines::Hero {
    let rows = page::page_rows(app);
    let card = lines::card_items(app, &rows);
    lines::hero(app, &rows, &card)
}

/// The plan an automatic Apply begins, if `action` is one.
fn automatic_plan(action: Option<SystemStateAction>) -> Option<Plan> {
    match action {
        Some(SystemStateAction::Apply(answer, Admission::Automatic)) => match answer.decision {
            crate::workstation_setup::WorkstationSetupDecision::Begin(plan) => Some(plan),
            _ => None,
        },
        _ => None,
    }
}

/// Revision 4: changes to make that nothing holds start a run by themselves, with no press, and
/// a run that leaves them undone starts again after 1, 2, 4, 8, then every 15 minutes.
#[test]
fn changes_to_make_start_by_themselves_and_try_again_a_little_later_each_time() {
    assert_eq!(
        [0, 1, 2, 3, 4, 5, 9].map(|tries| by_itself_after(tries).as_secs() / 60),
        [0, 1, 2, 4, 8, 15, 15]
    );
    let status = to_change();
    let mut app = page_reading(&status);
    assert!(
        app.starts_by_itself(),
        "changes to make, nothing holding them"
    );
    let hero = hero_of(&app);
    assert_eq!(
        (hero.headline.as_str(), hero.sub.as_str(), hero.start),
        (lines::SETTING_UP, lines::STARTING, false),
        "the page says it sets up, never a press it waits for"
    );
    let now = aterm_time::Instant::now();
    let mut at = now;
    for tries in 1..=3 {
        app.tick_at(at);
        let plan = automatic_plan(app.actions.pop_front())
            .unwrap_or_else(|| panic!("try {tries} starts by itself"));
        assert!(app.actions.is_empty());
        app.apply_accepted(plan);
        // The run ended with the changes still to make.
        app.publish(status.clone());
        assert!(app.retry_waits() && !app.starts_by_itself());
        let next = at + by_itself_after(tries);
        assert_eq!(app.next_by_itself(), Some(next), "try {tries}");
        app.tick_at(next - Duration::from_secs(1));
        assert!(app.actions.is_empty(), "try {} waits its turn", tries + 1);
        at = next;
    }
    // Once nothing is left to change, the count starts again.
    let mut done = status_of("set-up");
    done.plan.clone_from(&status.plan);
    app.publish(done);
    app.tick_at(at);
    assert!(app.actions.is_empty() && app.next_by_itself().is_none());
}

/// A hold (a person's Stop, an older aterm's Cancel) is the one time changes to make wait for a
/// press: the page starts nothing, says setup is stopped, and offers Set up now in its footer.
#[test]
fn a_hold_keeps_changes_to_make_for_set_up_now_in_the_footer() {
    let mut status = to_change();
    status.run = Some(InitialSetupRunPolicy {
        revision: 1,
        allowed: false,
    });
    let mut app = page_reading(&status);
    assert!(app.held() && !app.starts_by_itself());
    app.tick_at(aterm_time::Instant::now());
    assert!(app.actions.is_empty(), "a hold starts nothing by itself");
    let hero = hero_of(&app);
    assert_eq!(hero.headline, lines::STOPPED);
    assert!(hero.start, "Set up now goes on with them");
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 740.0);
    assert_eq!(
        label(&compiled, "system-state/headline/start"),
        "Set up now"
    );
    act(&mut app, "system-state/headline/start");
    assert!(matches!(
        app.actions.pop_front(),
        Some(SystemStateAction::Apply(..))
    ));
    // CONTROL: with nothing holding them the footer offers no Set up now.
    let app = page_reading(&to_change());
    let compiled = compile(&app, &SystemStateViewState::default(), 999.0, 740.0);
    assert!(
        compiled
            .semantic(&UiKey::new("system-state/headline/start"))
            .is_none()
    );
}

/// A Check that failed as a whole is read again by the page itself after a minute, then after
/// longer waits: no Check again for the person to press.
#[test]
fn a_failed_check_is_read_again_by_itself() {
    let status = status_of("check-failed");
    let mut app = page_reading(&status);
    let now = aterm_time::Instant::now();
    app.tick_at(now);
    assert!(
        !app.actions
            .iter()
            .any(|action| matches!(action, SystemStateAction::Check(_))),
        "no Check before its wait"
    );
    app.actions.clear();
    assert_eq!(app.next_by_itself(), Some(now + by_itself_after(1)));
    app.tick_at(now + by_itself_after(1));
    assert!(
        app.actions
            .iter()
            .any(|action| matches!(action, SystemStateAction::Check(_))),
        "the page checks again by itself: {:?}",
        app.actions
    );
}

/// The footer waits for the first readings, shows no Check again while a check runs, and shows
/// Restore only once aterm saved preferences to put back.
#[test]
fn the_footer_offers_only_what_can_be_done_now() {
    let has = |status: &SetupStatus, key: &str| {
        let app = page_reading(status);
        compile(&app, &SystemStateViewState::default(), 999.0, 740.0)
            .semantic(&UiKey::new(key))
            .is_some()
    };
    let checking = status_of("checking");
    assert!(!has(&checking, "system-state/check") && !has(&checking, "system-state/restore"));
    let set_up = status_of("set-up");
    assert!(set_up.restorable);
    assert!(has(&set_up, "system-state/check") && has(&set_up, "system-state/restore"));
    let unsaved = SetupStatus {
        restorable: false,
        ..set_up.clone()
    };
    assert!(
        !has(&unsaved, "system-state/restore"),
        "nothing to put back"
    );
    let reading = SetupStatus {
        busy: true,
        operation: Some(SetupOperation::Check),
        ..set_up
    };
    assert!(!has(&reading, "system-state/check"), "a check runs already");
}

/// Every "needs you" has its card item: a Mac whose Command Line Tools only an administrator
/// installs says so in the card.
#[test]
fn the_tools_an_administrator_installs_are_in_the_card() {
    let app = page_reading(&status_of("clt-not-admin"));
    let (_, card) = lines_of(&app);
    let item = card
        .iter()
        .find(|item| item.subject == Some(Subject::Setting(Setting::Repositories)))
        .expect("the tools' step is in the card");
    assert_eq!(item.mark, Mark::NeedsYou);
    assert_eq!(
        item.title,
        "Ask an administrator of this Mac to install Apple's Command Line Tools"
    );
}

/// A line says what is true now, and where setup changes it, now → wanted (§4.3).
#[test]
fn a_line_says_what_is_true_now_and_now_to_wanted() {
    let app = page_reading(&status_of("set-up"));
    let mac = line_in(&app, Setting::CapsLock);
    assert_eq!(mac.mark, Mark::Done);
    assert!(mac.phrase.starts_with("Caps Lock is "), "{}", mac.phrase);
    let secrets = line_in(&app, Setting::Secrets);
    assert_eq!(secrets.mark, Mark::Done);
    assert_eq!(
        secrets.phrase,
        format!("Joined · {}", Setting::Secrets.wanted().to_lowercase())
    );
    let mut unread = status_of("set-up");
    unread
        .observed
        .iter_mut()
        .find(|row| row.subject == Subject::Setting(Setting::Secrets))
        .unwrap()
        .now = None;
    let without_value = line_in(&page_reading(&unread), Setting::Secrets);
    assert_ne!(without_value.phrase, secrets.phrase);
    let permissions = line_in(&app, Setting::Permissions);
    assert_eq!(permissions.phrase, "Granted");
    // A grant on offer is not granted: its line says Offered (nux round 9, item 12), and the
    // unchanged reading above is the negative control.
    let mut offered = status_of("set-up");
    let debugger = Subject::Access(Access::Debugger);
    let reading = offered
        .observed
        .iter_mut()
        .find(|o| o.subject == debugger)
        .expect("set-up reads the debugger");
    reading.standing = Standing::NotNeeded;
    reading.now = Some("Off".to_owned());
    reading.note = Some(aterm_types::macos_setup::ACCESS_OFFERED.to_owned());
    let permissions = line_in(&page_reading(&offered), Setting::Permissions);
    assert_eq!(permissions.mark, Mark::Waiting);
    assert!(permissions.phrase.contains("macOS grants pending"));
    let hero = hero_of(&page_reading(&offered));
    assert_eq!(hero.headline, "Setup incomplete");
    assert!(hero.done < hero.total);
    let app = page_reading(&status_of("mixed"));
    let (lines, _) = lines_of(&app);
    assert!(
        lines
            .iter()
            .any(|line| line.mark == Mark::Pending && line.phrase.contains(" → ")),
        "{lines:#?}"
    );
}

/// A grant on offer neither needs the person nor starts a run: the page has no card item for it
/// and its one footer press asks for the batch. Negative control: with no offer the same press
/// does nothing.
#[test]
fn grants_on_offer_ask_nothing_of_the_page_but_its_one_press() {
    let mut status = to_change();
    let debugger = Subject::Access(Access::Debugger);
    if let Some(reading) = status.observed.iter_mut().find(|o| o.subject == debugger) {
        reading.standing = Standing::NotNeeded;
        reading.now = Some("Off".to_owned());
        reading.note = Some(aterm_types::macos_setup::ACCESS_OFFERED.to_owned());
    }
    assert_eq!(offered_access(&status), [Access::Debugger]);
    let mut app = page_reading(&status);
    let rows = page::page_rows(&app);
    assert!(
        !lines::card_items(&app, &rows)
            .iter()
            .any(|item| item.subject == Some(debugger)),
        "no card item for a grant on offer"
    );
    act(&mut app, ASK_FOR_ACCESS);
    assert!(matches!(
        app.take_action(),
        Some(SystemStateAction::AskForAccess { .. })
    ));

    let mut none = page_reading(&to_change());
    assert!(offered_access(&none.status).is_empty());
    act(&mut none, ASK_FOR_ACCESS);
    assert!(
        !matches!(
            none.take_action(),
            Some(SystemStateAction::AskForAccess { .. })
        ),
        "CONTROL: no offer, no ask"
    );
}
