// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm.log keeps words, kinds, ids and numbers (DESIGN §3.6): a config value, a tab's label, an
//! exception's reason and another program's notice reach it by their length, and each keeps the
//! home it has (the Messages store). One test per family; each plants a sentinel the log must not
//! hold and checks the line itself is still written and the text still kept where it lives.

use aterm_agent::supervise::policy::exception::{Exception, Urgency};
use aterm_agent::supervise::policy::outlook::ExceptionKind;
use aterm_phase::Program;

use crate::App;
use crate::harness_host::HostNote;
use crate::test_log::logged_while;

/// Whether the Messages store (its records and its live rows) holds `needle`.
fn kept(app: &App, needle: &str) -> bool {
    app.messages
        .log()
        .records()
        .any(|r| r.title.contains(needle) || r.detail.iter().any(|d| d.contains(needle)))
        || app.messages.live_rows().any(|l| {
            l.msg.title.contains(needle) || l.msg.detail.iter().any(|d| d.contains(needle))
        })
}

/// The family the person wrote in `font_family` is a config value: the log says it was refused
/// and its length, and the queued Messages record names it.
#[test]
fn a_refused_font_family_reaches_the_log_by_its_length() {
    let _lane = crate::message_inbox::own_lane_for_test();
    let warning = "font_family \"QZP7E7F1 Mono\" is not installed here; using Menlo";
    let logged = logged_while(|| crate::say_font_family_rejected(warning));
    assert!(
        logged.iter().any(|line| line
            == &format!(
                "font_family refused ({}B); the Messages record names it",
                warning.len()
            )),
        "the refusal is logged: {} lines",
        logged.len()
    );
    assert!(
        !logged.iter().any(|line| line.contains("QZP7E7F")),
        "the log keeps the family's name"
    );
    let queued = crate::message_inbox::take_queued();
    assert!(
        queued.iter().any(|q| q.message.title.contains("QZP7E7F1")
            || q.message.detail.iter().any(|d| d.contains("QZP7E7F1"))),
        "CONTROL: the queued Messages record names the family"
    );
}

/// A tab's label falls back to the program's own OSC title: the log names a tab by its session,
/// and a message by its key and its title's length, while the Messages store keeps the title. A
/// title with a space is no word, so it is the one-word title that shows a message's key never
/// holds the label (the Stop Worker row is keyed by its session).
#[test]
fn a_tab_named_by_its_programs_title_reaches_the_log_by_its_session() {
    for (title, planted) in [("QZP7E7G1 build", "QZP7E7G1"), ("QZP7E7G3", "QZP7E7G3")] {
        let mut app = App::headless_for_test();
        app.pool
            .get(0)
            .expect("session 0")
            .term
            .lock()
            .unwrap()
            .process(format!("\x1b]2;{title}\x07").as_bytes());
        let label = app.tab_label_of_session(0);
        assert!(
            label.contains(planted),
            "CONTROL: the tab's label is its program's title"
        );
        let logged = logged_while(|| {
            app.stop_worker_refused(0, crate::app_overview::StopRefused::NoAgent);
            app.read_status_result(&crate::status_item::ResultRef {
                session: 0,
                entered: 0,
                end_serial: None,
                path: std::path::PathBuf::from("QZP7E7G2/result.md"),
                label: "QZP7E7G2".to_string(),
            });
        });
        assert!(
            logged.iter().any(|line| line.starts_with("message ")
                && line.contains(" key=harness.stop-worker.0 title=")),
            "the posted message is logged by its key: {} lines",
            logged.len()
        );
        assert!(
            logged
                .iter()
                .any(|line| line.starts_with("status item: session 0's result did not open: ")),
            "the result that did not open is logged: {} lines",
            logged.len()
        );
        assert!(
            !logged.iter().any(|line| line.contains("QZP7E7G")),
            "the log keeps the tab's label"
        );
        assert!(
            kept(&app, planted),
            "CONTROL: the Messages store keeps the title"
        );
    }
}

/// An exception's reason can be a `/goal` label read off the screen or a repository's finding:
/// the log names the exception's kind and the reason's length, Later and Now alike, and the
/// Messages store keeps the words.
#[test]
fn a_needs_a_person_line_names_the_reason_by_its_length() {
    let mut app = App::headless_for_test();
    let action = "Run /goal again in this tab.";
    let planted = |when: Urgency, why: &str| Exception {
        held: None,
        title: None,
        kind: ExceptionKind::GoalCleared,
        when,
        why: why.to_string(),
        action: action.to_string(),
        clears: "the agent working",
        escalated: false,
        notice: None,
    };
    let later = "Claude cleared the /goal QZP7E7H1 on its own.";
    let now = "Claude cleared the /goal QZP7E7H2 on its own.";
    let noticed = "Claude cleared the /goal QZP7E7H3 on its own.";
    let text = aterm_agent::supervise::policy::notice::LOGIN_SIGN_IN;
    let notice = aterm_agent::supervise::policy::notice::Notice {
        id: text.id,
        code: text.code,
        title: text.title,
        short_title: text.short_title,
        did: text.did.to_string(),
        expect: text.expect.to_string(),
        command: None,
        short: None,
        undo: None,
        back: text.back,
    };
    let logged = logged_while(|| {
        for (sid, standing) in [
            ("s-e7h1", planted(Urgency::Later, later)),
            ("s-e7h2", planted(Urgency::Now, now)),
            (
                "s-e7h3",
                Exception {
                    notice: Some(notice.clone()),
                    ..planted(Urgency::Later, noticed)
                },
            ),
        ] {
            app.note_harness_host(HostNote::ResumeException {
                sid: sid.to_string(),
                agent: Program::Claude,
                standing: Some(standing),
            });
        }
    });
    // A Later exception's notice is named by its id, as a Now one's is.
    assert!(
        logged.iter().any(|line| line.starts_with(&format!(
            "harness @s-e7h3: needs a person (goal-cleared, notice {}): why={}B",
            text.id,
            noticed.len()
        ))),
        "the Later line names its notice: {} lines",
        logged.len()
    );
    for (head, why) in [
        ("harness @s-e7h1: needs a person (", later),
        ("harness @s-e7h2: needs a person now (", now),
    ] {
        let tail = format!("why={}B => {}B", why.len(), action.len());
        assert!(
            logged
                .iter()
                .any(|line| line.starts_with(head) && line.ends_with(&tail)),
            "{head:?} names the reason by its length: {} lines",
            logged.len()
        );
    }
    assert!(
        !logged.iter().any(|line| line.contains("QZP7E7H")),
        "the log keeps the reason"
    );
    assert!(
        kept(&app, "QZP7E7H1")
            || app
                .messages
                .digest()
                .entries
                .iter()
                .any(|e| e.title.contains("QZP7E7H1")
                    || e.why.contains("QZP7E7H1")
                    || e.detail.iter().any(|d| d.contains("QZP7E7H1"))),
        "CONTROL: the Messages store keeps the Later reason"
    );
    assert!(
        kept(&app, "QZP7E7H2"),
        "CONTROL: the Messages store keeps the Now reason"
    );
}

/// `appnotice` text is another program's free words: the log names the lane and the text's
/// length, and messages.log keeps the text.
#[test]
fn an_appnotice_reaches_the_log_by_its_lane_and_length() {
    let mut app = App::headless_for_test();
    let text = "QZP7E7I1: a helper's own words";
    let logged = logged_while(|| {
        assert!(app.app_notice("update", text).is_ok());
    });
    assert!(
        logged
            .iter()
            .any(|line| line == &format!("appnotice update: recorded text={}B", text.len())),
        "the notice is logged: {} lines",
        logged.len()
    );
    assert!(
        !logged.iter().any(|line| line.contains("QZP7E7I")),
        "the log keeps the notice's text"
    );
    assert!(
        kept(&app, "QZP7E7I1"),
        "CONTROL: messages.log keeps the text"
    );
}

/// A dead accessibility publisher's panic can quote the accessibility text, a session's screen:
/// the log names the panic's kind and length, and the queued Messages record keeps the reason.
#[cfg(a11y_tree)]
#[test]
fn a_dead_accessibility_publisher_reaches_the_log_by_its_panics_kind() {
    let _lane = crate::message_inbox::own_lane_for_test();
    let payload = "byte index 2 is not a char boundary; it is inside 'é' of `QZP7E7N1 row`";
    let logged = logged_while(|| crate::a11y_backend::report_failure(payload, "src/e7.rs:1:1"));
    assert!(
        logged.iter().any(|line| line
            == &format!(
                "accessibility publisher failed at src/e7.rs:1:1: str-slice panic ({}B)",
                payload.len()
            )),
        "the dead publisher is logged by its panic's kind: {} lines",
        logged.len()
    );
    assert!(
        !logged.iter().any(|line| line.contains("QZP7E7N")),
        "the log keeps the panic's text"
    );
    let queued = crate::message_inbox::take_queued();
    assert!(
        queued
            .iter()
            .any(|q| q.message.detail.iter().any(|d| d.contains("QZP7E7N1"))),
        "CONTROL: the queued Messages record keeps the reason"
    );
}

/// A message the rule records instead of showing is named in the log by its key where the
/// Messages store filed one, else by its title's length: a title can name a tab by the program's
/// own title.
#[test]
fn a_demoted_message_reaches_the_log_by_its_key_or_its_titles_length() {
    use aterm_messages::{Message, Severity, tags};
    let mut app = App::headless_for_test();
    let title = "Working in QZP7E7O1 build";
    let working = |title: &str| {
        Message::new(tags::HARNESS, Severity::Info, title)
            .working(std::time::Duration::from_secs(5))
    };
    let logged = logged_while(|| {
        app.post_message(working(title));
        app.post_message(working("Working in QZP7E7O2").key("e7.demoted"));
    });
    for last in [
        format!("(last: title={}B)", title.len()),
        "(last: key=e7.demoted)".to_string(),
    ] {
        assert!(
            logged
                .iter()
                .any(|line| line.starts_with("message rule: ") && line.ends_with(&last)),
            "the rule's line names the message {last}: {} lines",
            logged.len()
        );
    }
    assert!(
        !logged.iter().any(|line| line.contains("QZP7E7O")),
        "the log keeps the demoted message's title"
    );
    assert!(
        kept(&app, "QZP7E7O1"),
        "CONTROL: the Messages store keeps the title"
    );
}
