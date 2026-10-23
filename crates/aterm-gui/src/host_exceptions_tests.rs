// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for host exception producers, ripening horizons, and digest lifecycle.

use std::time::{Duration, Instant};

use aterm_agent::supervise::policy::exception::{Exception, Urgency};
use aterm_agent::supervise::policy::notice;
use aterm_agent::supervise::policy::outlook::ExceptionKind;
use aterm_agent::supervise::{StallSeen, StuckSurvey};
use aterm_core::terminal::repaint_nudge::{Gate, GiveUp};
use aterm_messages::exceptions::{Entry, Family};
use aterm_phase::Program;

use crate::App;
use crate::harness_host::{AccountsSaid, HostNote, RelaunchSay};
use crate::message_reporters as mr;
use crate::messages_host::message_regrids;
use crate::update_words::{self, HealthKind};

const DAY: Duration = Duration::from_secs(24 * 3600);

/// The places a tab can be named in: alone, in one of several windows, unknown.
fn places() -> [mr::UpgradeSpot; 3] {
    [
        mr::UpgradeSpot {
            place: "in tab 3".to_string(),
            nav: Some(aterm_messages::Intent::ShowTab { tab: 3, window: 1 }),
        },
        mr::UpgradeSpot {
            place: "in window 12, tab 10".to_string(),
            nav: Some(aterm_messages::Intent::ShowTab {
                tab: 10,
                window: 12,
            }),
        },
        mr::UpgradeSpot {
            place: "in its tab".to_string(),
            nav: None,
        },
    ]
}

/// Every entry the host can write, in every place it can be worded for.
fn roster() -> Vec<Entry> {
    let body = "3 failed checks in a row since 2026-09-14T22:04:36Z: manifests cannot be \
                downloaded.";
    let mut all: Vec<Entry> = HealthKind::ALL
        .into_iter()
        .map(|kind| update_words::update_health_entry(kind, body))
        .collect();
    all.push(update_words::update_health_entry(HealthKind::Install, ""));
    for spot in places() {
        all.push(mr::supervisor_failing_entry(
            "s-3e79b5bde172ad0088ad",
            &spot,
            "boom",
        ));
        all.push(mr::survey_entry("s-3e79", &spot, "a rating card is open"));
        for why in [
            GiveUp::NoAnswer,
            GiveUp::Deferred(Gate::Busy),
            GiveUp::Deferred(Gate::Selection),
            GiveUp::Deferred(Gate::Lease),
            GiveUp::Budget,
        ] {
            all.push(mr::render_unrepaired_entry("s-3e79", &spot, why));
        }
        for source in [
            atpkg::image::ImageSource::Store,
            atpkg::image::ImageSource::Native,
        ] {
            all.push(mr::claude_build_drift_entry(
                "s-3e79b5bde172ad0088ad",
                &spot,
                &crate::session_timeline::RegistryDrift {
                    pgid: 4242,
                    image: "2.1.304".into(),
                    source,
                    reported: "2.1.303".into(),
                },
            ));
        }
    }
    all.push(mr::disk_low_entry(7, 20, &[]));
    for feature in mr::REMOVED_FEATURES {
        all.push(mr::removed_setting_entry(feature, Some(12), &[]));
        all.push(mr::removed_setting_entry(feature, None, &[]));
        let left = feature.left.map(|left| {
            left(
                aterm_toml::from_str::<aterm_toml::Value>(NET_WITH_CREDENTIALS)
                    .expect("the fixture parses")
                    .get("net")
                    .expect("the fixture has [net]"),
            )
        });
        all.push(mr::removed_setting_entry(
            feature,
            Some(12),
            &left.unwrap_or_default(),
        ));
    }
    for cause in [
        crate::session_log_journal::Cause::Full,
        crate::session_log_journal::Cause::Io,
        crate::session_log_journal::Cause::Denied,
        crate::session_log_journal::Cause::Unsupported,
        crate::session_log_journal::Cause::Panic,
    ] {
        all.push(mr::session_journal_paused_entry(cause.word()));
        all.push(mr::session_journal_stopped_entry(cause.word()));
    }
    let foreign = |n: usize| {
        (0..n)
            .map(|i| crate::toolchain_words::Foreign {
                name: "trust".to_string(),
                path: format!("/Users//someone/.rustup/toolchains/trust-{i}"),
                fix: atpkg::seam::DETACH_FIX.to_string(),
            })
            .collect::<Vec<_>>()
    };
    for scope in [atpkg::cli::FOREIGN_SEAM, atpkg::cli::FOREIGN_PATH] {
        for n in [1, 5] {
            all.push(crate::toolchain_words::foreign_tools_entry(
                scope,
                &foreign(n),
            ));
        }
    }
    all.push(mr::disk_low_cleanup_off_entry(2, 10));
    all.push(mr::disk_low_cleanup_off_entry(
        u64::MAX >> 30,
        u64::MAX >> 30,
    ));
    let note = format!(
        "/Users//someone/{}/aterm.toml: cannot be read (Permission denied (os error 13)), so \
         every removal is off (disk.apply false, auto_free_gib 0, dead_build_dirs false) until \
         it is fixed",
        "x".repeat(40)
    );
    all.push(mr::disk_settings_entry(std::slice::from_ref(&note)));
    all.push(mr::disk_settings_entry(
        &std::iter::repeat_n(note, 5).collect::<Vec<_>>(),
    ));
    all.push(mr::upgrade_cannot_run_entry("busy:state-unwritable", None));
    all.push(mr::upgrade_records_unlisted_entry(std::path::Path::new(
        "/Users//someone/.local/state/aterm/harness/upgrade",
    )));
    all.push(mr::upgrade_records_unreadable_entry(&[
        std::path::PathBuf::from("/Users//someone/.local/state/aterm/harness/upgrade/0badf00d.json"),
    ]));
    for step in [
        atpkg::devtools::Step::Add,
        atpkg::devtools::Step::SwitchOn,
        atpkg::devtools::Step::ListedOn,
    ] {
        all.push(mr::devtools_entry(step));
    }
    all.push(mr::palette_refused_entry());
    for labels in [
        vec!["work-2".to_string()],
        ["main", "work-2", "pool-a"].map(str::to_string).to_vec(),
        (0..32)
            .map(|i| format!("{i:02}{}", "x".repeat(62)))
            .collect(),
        vec!["a".repeat(57)],
        vec!["a".repeat(58)],
    ] {
        all.push(mr::accounts_signed_out_entry(&labels).expect("a label is an entry"));
    }
    all.push(mr::accounts_refused_entry());
    all.push(mr::usage_tap_silent_entry());
    for spot in places() {
        for cause in ["held", "shell-prompt", "other-tab"] {
            all.push(mr::relaunch_blocked_entry("s-3e79", &spot, cause, false));
        }
        let thens = [
            None,
            crate::harness_host::cannot_then("refused:history-unsafe"),
            crate::harness_host::cannot_then("refused:no-turn"),
        ];
        for reason in ["its shell is gone", "it ran one prompt and ended"] {
            for new_tab in [false, true] {
                for then in thens {
                    all.push(mr::relaunch_gone_entry(
                        "s-3e79", &spot, reason, false, new_tab, then,
                    ));
                    all.push(mr::relaunch_gone_entry(
                        "s-3e79", &spot, reason, true, new_tab, then,
                    ));
                }
            }
        }
        for restarted in [false, true] {
            for command in [None, Some("HOME=/Users//o claude --resume 0badf00d")] {
                for then in thens {
                    all.push(mr::relaunch_lost_entry(
                        "s-3e79",
                        &spot,
                        "its shell is gone",
                        restarted,
                        command,
                        then,
                    ));
                }
            }
        }
        all.push(mr::relaunch_not_restored_entry("s-3e79", &spot));
    }
    for spot in places() {
        let one = vec![(
            "s-1".to_string(),
            stall_exception(),
            Program::Claude,
            spot.clone(),
        )];
        all.extend(mr::resume_exceptions(&one));
        for agent in [Program::Agy, Program::Muse] {
            let own = vec![(
                "s-1".to_string(),
                Exception::stalled_on_work(),
                agent,
                spot.clone(),
            )];
            all.extend(mr::resume_exceptions(&own));
        }
        for kind in later_kinds() {
            let many: Vec<_> = ["s-1", "s-2", "s-3"]
                .iter()
                .map(|sid| {
                    (
                        sid.to_string(),
                        resume_exception(kind),
                        Program::Claude,
                        spot.clone(),
                    )
                })
                .collect();
            all.extend(mr::resume_exceptions(&many));
            all.extend(mr::resume_exceptions(&many[..1]));
        }
    }
    all
}

/// The kinds of the resume loop's exceptions that wait for the owner's look.
fn later_kinds() -> [ExceptionKind; 7] {
    [
        ExceptionKind::Spend,
        ExceptionKind::Bucket,
        ExceptionKind::GoalCleared,
        ExceptionKind::Futile,
        ExceptionKind::Context,
        ExceptionKind::Dialog,
        ExceptionKind::Refused,
    ]
}

/// The loop's own words for `kind` (`policy/exception.rs`), as it tells them.
fn resume_exception(kind: ExceptionKind) -> Exception {
    let (why, action) = match kind {
        ExceptionKind::Spend => (
            "Its usage credits are used up, and no time window ends that.",
            "Add usage credits in claude.ai settings.",
        ),
        ExceptionKind::Bucket => (
            "Fable has no reset time, and this tab has no other model to fall back on.",
            "Run /model in this tab and pick another model.",
        ),
        ExceptionKind::GoalCleared => (
            "Claude cleared the /goal: organization verification required.",
            "Run /goal again in this tab.",
        ),
        ExceptionKind::Futile => (
            "The limit still stands an hour after its reset.",
            "Check claude.ai/settings/usage for what holds it.",
        ),
        ExceptionKind::Stalled => return stall_exception(),
        ExceptionKind::Dialog => return Exception::dialog(),
        ExceptionKind::Context => return Exception::context(),
        ExceptionKind::Refused => {
            return Exception::refused(Some("tool_history_mismatch"), "API Error: 400");
        }
        ExceptionKind::Login => (
            "Its login is gone, and only a browser sign-in restores it.",
            "Finish signing in on the page /login opened in your browser.",
        ),
    };
    Exception {
        kind,
        when: if kind == ExceptionKind::Login {
            Urgency::Now
        } else {
            Urgency::Later
        },
        why: why.to_string(),
        action: action.to_string(),
        clears: "the agent working",
        escalated: kind == ExceptionKind::Login,
        notice: None,
    }
}

/// The stall canary's third rung, as the loop tells it.
fn stall_exception() -> Exception {
    Exception::stalled()
}

/// EVERY ENTRY THE HOST CAN WRITE passes the door as a `Later` ask (an entry the
/// door refuses is never shown).
#[test]
fn every_host_entry_passes_the_door_and_the_caps() {
    let record_keys: Vec<String> = HealthKind::ALL
        .into_iter()
        .map(|kind| kind.key().to_string())
        .chain([
            mr::supervisor_failing_record("s-1", "in tab 1", "x")
                .key
                .unwrap_or_default(),
            mr::disk_low_record(1, 2, &[]).key.unwrap_or_default(),
            mr::disk_low_cleanup_off_record(1, 2)
                .key
                .unwrap_or_default(),
        ])
        .collect();
    for entry in roster() {
        let msg = entry
            .message()
            .unwrap_or_else(|fault| panic!("{}: {fault:?}", entry.key()));
        let aterm_messages::need::Need::Later(_) = msg.need() else {
            panic!("{}: an entry is a Later ask", entry.key());
        };
        let key = entry.key();
        assert!(key.chars().count() <= 48, "{key}");
        assert!(key.starts_with(&entry.family().prefix()), "{key}");
        assert!(
            !record_keys.contains(&key),
            "{key}: an entry and a record never share a key"
        );
    }
}

/// A REMOVED FEATURE'S SETTINGS ARE ONE ENTRY UNTIL THEY GO (design §7): an
/// `aterm.toml` that still configures the network drive, in any spelling —
/// a `[net]` header, a `[[net.connections]]` array with no header, an inline
/// table, a dotted key — is told once, naming what replaced it, and every load
/// that still finds it says the same entry again rather than a second one; the
/// load that no longer finds it ends it.
#[test]
fn a_removed_features_settings_are_one_entry_until_they_go() {
    let net = &mr::REMOVED_FEATURES[0];
    assert_eq!(net.table, "net");
    for text in [
        "[net]\nlisten = \"0.0.0.0:7100\"\ncert = \"c.der\"\nkey = \"k.der\"\n",
        "[[net.connections]]\nname = \"box\"\nhost = \"box:7100\"\n",
        "net = { listen = \"0.0.0.0:7100\" }\n",
        "net.listen = \"0.0.0.0:7100\"\n",
    ] {
        let mut app = App::headless_for_test();
        app.note_removed_settings(text);
        app.note_removed_settings(text);
        let digest = app.messages.digest();
        assert_eq!(digest.len(), 1, "{text:?}: {digest:?}");
        let entry = &digest.entries[0];
        assert_eq!(entry.key, mr::removed_setting_key(net), "{text:?}");
        assert_eq!(
            entry.then, "Delete the [net] table from aterm.toml",
            "the step is the one that ends it: {text:?}"
        );
        assert!(
            entry.why.contains("the fleet replaced")
                && entry
                    .detail
                    .iter()
                    .any(|line| line.contains("aterm fleet join")),
            "it names what replaced it: {text:?}: {entry:?}"
        );
        app.note_removed_settings("font_px = 14\n");
        assert!(
            app.messages.digest().is_empty(),
            "{text:?}: a load without [net] ends it"
        );
    }
    for text in [
        "",
        "font_px = 14\n",
        "[fleet]\nlisten = \"0.0.0.0:7100\"\n",
        "[net\n",
    ] {
        let mut app = App::headless_for_test();
        app.note_removed_settings(text);
        assert!(app.messages.digest().is_empty(), "{text:?}");
    }
}

/// A `[net]` table that names every credential the network drive kept: the
/// TLS key (whose folder held its persistent drive token), the certificate,
/// and two saved connections, one with a `token_file` and one without.
const NET_WITH_CREDENTIALS: &str = "[net]\nlisten = \"0.0.0.0:7100\"\ncert = \
     \"/keys/net-cert.der\"\nkey = \"/keys/net-key.der\"\n\n[[net.connections]]\nname = \
     \"box\"\nhost = \"box:7100\"\ntoken_file = \"/keys/box.token\"\n\n[[net.connections]]\n\
     name = \"lab\"\nhost = \"lab:7100\"\n";

/// The same at a real home's length: the key and certificate in the config
/// folder, and four connections, two with a `token_file` (one beside the key,
/// one in another folder) and two without.
const NET_AT_HOME: &str = "[net]\nlisten = \"0.0.0.0:7100\"\n\
     cert = \"/home/alexandra/.config/aterm/net/drive-cert.der\"\n\
     key = \"/home/alexandra/.config/aterm/net/drive-key.der\"\n\
     [[net.connections]]\nname = \"build-box\"\nhost = \"build-box:7100\"\n\
     token_file = \"/home/alexandra/.config/aterm/net/build-box.token\"\n\
     [[net.connections]]\nname = \"lab-mac\"\nhost = \"lab-mac:7100\"\n\
     token_file = \"/home/alexandra/secrets/aterm/lab-mac.token\"\n\
     [[net.connections]]\nname = \"render-farm\"\nhost = \"render-farm:7100\"\n\
     [[net.connections]]\nname = \"office-mini\"\nhost = \"office-mini:7100\"\n";

/// Whether a line of `detail` names the file `name` in `folder`: its whole
/// path, ended by the line or a list's `,` or `;`, or `name` in the list after
/// `in <folder>: `.
fn names_file(detail: &[String], folder: &str, name: &str) -> bool {
    let whole = std::path::Path::new(folder)
        .join(name)
        .display()
        .to_string();
    let group = format!("in {folder}: ");
    detail.iter().any(|line| {
        let ended = |at: usize| {
            line[at..]
                .chars()
                .next()
                .is_none_or(|c| c == ',' || c == ';')
        };
        line.match_indices(&whole)
            .any(|(i, _)| ended(i + whole.len()))
            || line.match_indices(&group).any(|(i, _)| {
                line[i + group.len()..]
                    .split("; ")
                    .next()
                    .is_some_and(|list| list.split(", ").any(|n| n == name))
            })
    })
}

/// THE NETWORK DRIVE'S CREDENTIALS ARE NAMED WHILE ITS TABLE STILL SAYS WHERE
/// THEY ARE (design §7): an `aterm.toml` whose `[net]` names a TLS key, a
/// certificate and saved connections gets Details naming each file — the
/// drive token in the key's folder, a connection's `token_file`, the key, the
/// certificate — and, on macOS, the Keychain service and every connection's
/// name; elsewhere every connection's conventional file, which `dial-token`
/// wrote whatever its `token_file` said.
#[test]
fn the_network_drives_left_credentials_are_named_from_its_table() {
    let home = "/home/alexandra/.config/aterm/net";
    for (table, files, connections) in [
        (
            NET_WITH_CREDENTIALS,
            vec![
                ("/keys", "aterm-network-drive.token"),
                ("/keys", "net-key.der"),
                ("/keys", "net-cert.der"),
                ("/keys", "box.token"),
            ],
            &["box", "lab"][..],
        ),
        (
            NET_AT_HOME,
            vec![
                (home, "aterm-network-drive.token"),
                (home, "drive-key.der"),
                (home, "drive-cert.der"),
                (home, "build-box.token"),
                ("/home/alexandra/secrets/aterm", "lab-mac.token"),
            ],
            &["build-box", "lab-mac", "render-farm", "office-mini"][..],
        ),
    ] {
        let mut app = App::headless_for_test();
        app.note_removed_settings(table);
        let digest = app.messages.digest();
        assert_eq!(digest.len(), 1, "{digest:?}");
        let detail = &digest.entries[0].detail;
        for (folder, name) in files {
            assert!(
                names_file(detail, folder, name),
                "names {folder}/{name}: {detail:#?}"
            );
        }
        if cfg!(target_os = "macos") {
            let keychain = detail
                .iter()
                .find(|l| l.contains("-s aterm-net-drive"))
                .unwrap_or_else(|| panic!("the Keychain items: {detail:#?}"));
            assert!(
                keychain.ends_with(&connections.join(", ")),
                "every connection: {keychain}"
            );
        } else {
            for name in connections {
                assert!(
                    names_file(detail, "net beside aterm.toml", &format!("{name}.token")),
                    "{name}'s conventional file: {detail:#?}"
                );
            }
            assert!(
                !detail.iter().any(|l| l.contains("aterm-net-drive")),
                "{detail:#?}"
            );
        }
    }
    let mut app = App::headless_for_test();
    app.note_removed_settings(NET_WITH_CREDENTIALS);
    let lines = app.messages.digest().entries[0].detail.len();
    let mut bare = App::headless_for_test();
    bare.note_removed_settings("[net]\nlisten = \"0.0.0.0:7100\"\n");
    let digest = bare.messages.digest();
    assert_eq!(digest.len(), 1, "{digest:?}");
    let detail = &digest.entries[0].detail;
    assert_eq!(
        detail.len() + 2,
        lines,
        "no credential, no line: {detail:?}"
    );
    assert!(
        !detail
            .iter()
            .any(|l| l.contains(".token") || l.contains("aterm-net-drive")),
        "{detail:?}"
    );
}

/// A TABLE A REMOVED FEATURE LEFT BEHIND (`[operator]`, 2026-10-15) is one
/// line of the digest while `aterm.toml` still writes it — said again at a
/// reload, still one line — its one step the deletion that ends it, its
/// navigation opening the file at the table's line, and its reason and
/// Details naming the manager session that replaced it on every platform;
/// the reload that drops the table ends it.
#[test]
fn a_removed_table_is_one_line_while_the_file_writes_it() {
    let operator = mr::REMOVED_FEATURES
        .iter()
        .find(|feature| feature.table == "operator")
        .expect("the operator is a removed feature");
    let mut app = App::headless_for_test();
    app.note_removed_settings("font_px = 14\n");
    assert!(app.messages.digest().is_empty(), "no table, no line");
    for text in [
        "font_px = 14\n\n# the old switch\n[operator]\nenabled = true\n",
        "font_px = 14\n\n# the old switch\noperator.enabled = true\n",
        "font_px = 14\n\n# the old switch\n[[operator.queue]]\nsid = \"s-1\"\n",
    ] {
        app.note_removed_settings(text);
        app.note_removed_settings(text);
        let digest = app.messages.digest();
        assert_eq!(
            digest.len(),
            1,
            "one line, however often it is said: {text:?}"
        );
        let line = &digest.entries[0];
        assert_eq!(line.key, mr::removed_setting_key(operator));
        assert_eq!(line.then, "Delete the [operator] table from aterm.toml");
        assert_eq!(
            line.actions.first(),
            Some(&aterm_messages::Intent::OpenConfigEditor { line: Some(4) }),
            "{text:?}"
        );
        assert!(
            line.why.contains("a manager session")
                && line
                    .detail
                    .iter()
                    .any(|said| said.contains("spawn agent=claude")),
            "{text:?}: {line:?}"
        );
        app.note_removed_settings("font_px = [\n");
        assert_eq!(
            app.messages.digest().len(),
            1,
            "an unparsed file moves nothing"
        );
        app.note_removed_settings("font_px = 14\n");
        assert!(
            app.messages.digest().is_empty(),
            "the edit that drops the table ends its line"
        );
    }
}

/// A self-mending fact is an entry only once it outlasts its horizon (`except_after`), and an
/// end before then withdraws it unseen.
#[test]
fn a_claim_ripens_at_its_horizon_and_an_end_withdraws_it() {
    let mut app = App::headless_for_test();
    let entry = update_words::update_health_entry(HealthKind::Download, "");
    app.except_after(&entry, Duration::from_secs(3600));
    assert!(app.messages.digest().is_empty(), "not yet");
    let due = app.ripening_deadline().expect("the loop is told when");
    assert!(app.messages_deadline().is_some_and(|at| at <= due));
    app.except_after(&entry, Duration::from_secs(3600));
    assert_eq!(app.ripening_deadline(), Some(due));
    let _ = app.settle_messages(due - Duration::from_secs(1));
    assert!(app.messages.digest().is_empty(), "a second early");
    let _ = app.settle_messages(due);
    assert_eq!(app.messages.digest().len(), 1);
    assert!(app.ripening_deadline().is_none(), "raised: no claim left");
    let mut app = App::headless_for_test();
    app.except_after(&entry, Duration::from_secs(3600));
    app.except_clear(&entry.key());
    assert!(app.ripening_deadline().is_none());
    let _ = app.settle_messages(Instant::now() + 2 * DAY);
    assert!(app.messages.digest().is_empty());
    let mut app = App::headless_for_test();
    app.except_after(&entry, Duration::ZERO);
    assert_eq!(app.messages.digest().len(), 1);
    let level = Entry::new(
        Family::Upgrade,
        "",
        "own-work",
        "Claude is behind",
        aterm_messages::need::Why::OnlyYou(aterm_messages::need::Reason::new("it is busy")),
        aterm_messages::need::Then::step("End that job in the tab"),
    );
    app.except_after(&level, Duration::from_secs(60));
    assert!(app.ripening_deadline().is_none());
}

/// A failing update kind is a record now and one `update.review.<kind>` line once its streak is
/// a day old (dated from the body, so a process starting on day six says it at once).
#[test]
fn updates_failing_for_a_day_are_one_line_and_the_healing_ends_it() {
    let stamp = |ago: u64| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        aterm_types::rfc3339::format_rfc3339(now - ago)
    };
    let body = |ago: u64| {
        format!(
            "20 failed checks in a row since {}: release manifests exist but cannot be \
             downloaded.",
            stamp(ago)
        )
    };
    let title = aterm_update::health_failing_title("pipeline");
    let mut app = App::headless_for_test();
    assert!(app.note_update_health(title, &body(3600)));
    assert!(app.messages.digest().is_empty());
    let due = app.ripening_deadline().expect("a claim");
    let left = due.duration_since(Instant::now());
    assert!(
        left > Duration::from_secs(22 * 3600) && left <= Duration::from_secs(23 * 3600),
        "{left:?}"
    );
    let _ = app.settle_messages(due);
    let keys: Vec<String> = app
        .messages
        .digest()
        .entries
        .iter()
        .map(|e| e.key.clone())
        .collect();
    assert_eq!(keys, ["update.review.download"]);
    app.heal_update_health(crate::messages_host::HealthProof::Checked);
    assert_eq!(
        app.messages.digest().len(),
        1,
        "a check that works downloads nothing"
    );
    app.heal_update_health(crate::messages_host::HealthProof::Downloaded);
    assert!(
        app.messages.digest().is_empty(),
        "downloading works: the line ends"
    );
    let mut app = App::headless_for_test();
    assert!(app.note_update_health(title, &body(6 * 24 * 3600)));
    assert_eq!(app.messages.digest().len(), 1);
    assert!(!app.note_update_health(aterm_update::HEALTH_RECOVERED_TITLE, ""));
    assert!(app.messages.digest().is_empty(), "the ledger healed");
    let historical_body = "20 failed checks in a row since 2026-09-14T22:04:36Z: release \
                           manifests exist but cannot be downloaded.";
    let mut app = App::headless_for_test();
    assert!(app.note_update_health(title, historical_body));
    assert_eq!(app.messages.digest().len(), 1);
    assert!(app.ripening_deadline().is_none());
    let e = &app.messages.digest().entries[0];
    assert!(
        e.detail.iter().any(|l| l.contains("Sep 14")),
        "the streak's own start: {:?}",
        e.detail
    );
}

/// `health_entry_delay` is the day less what the streak has spent, clamped to `0..=1 day`; a
/// body with no readable start counts from now.
#[test]
fn the_entry_delay_is_the_day_less_the_streak_so_far() {
    let now = 1_790_640_000_i64;
    let dated = |ago: i64| {
        format!(
            "3 failed since {}: why.",
            aterm_types::rfc3339::format_rfc3339(u64::try_from(now - ago).unwrap())
        )
    };
    assert_eq!(update_words::health_entry_delay(&dated(0), now), DAY);
    assert_eq!(
        update_words::health_entry_delay(&dated(3600), now),
        Duration::from_secs(23 * 3600)
    );
    assert_eq!(
        update_words::health_entry_delay(&dated(30 * 3600), now),
        Duration::ZERO
    );
    assert_eq!(update_words::health_entry_delay("no date here", now), DAY);
    assert_eq!(update_words::health_entry_delay(&dated(-3600), now), DAY);
}

/// A warning an older parent carried across an update crosses as its excerpt alone
/// (`since Sep 28`), dated to the end of that day in the latest year whose day has begun.
#[test]
fn a_carried_excerpt_dates_the_day_from_the_end_of_its_day() {
    let now = 1_790_640_000_i64;
    let delay = |line: &str, now: i64| update_words::health_entry_delay(line, now);
    assert_eq!(delay("since Sep 29", now), DAY, "today: the whole day");
    assert_eq!(
        delay("since Sep 28", now),
        DAY - Duration::from_secs(1),
        "yesterday, from its last second"
    );
    assert_eq!(delay("since Sep 14", now), Duration::ZERO);
    let new_year = now + 95 * 86_400;
    assert_eq!(delay("since Dec 31", new_year), Duration::ZERO);
    assert_eq!(delay("since Jan 2", new_year), DAY);
    for line in ["since Sept 14", "since Sep", "Sep 14", "since Sep 32"] {
        assert_eq!(delay(line, now), DAY, "{line:?}");
    }
}

/// A failing supervisor is one record per episode and a `harness.supervisor.<tab>.failing` line
/// after an hour of failures; steady again ends it.
#[test]
fn a_supervisor_that_keeps_failing_is_a_record_then_a_line_after_an_hour() {
    let mut app = App::headless_for_test();
    let message = message_regrids();
    let pause = Duration::from_secs(300);
    let failing = |why: &str| HostNote::SupervisorFailing {
        sid: "s-7".to_string(),
        why: why.to_string(),
        pause,
    };
    let records = |app: &App| {
        app.messages
            .log()
            .records()
            .filter(|r| !r.later && r.key.as_deref() == Some("harness.host.supervisor.s-7"))
            .count()
    };
    app.note_harness_host(failing("boom"));
    app.note_harness_host(failing("boom again"));
    assert_eq!(records(&app), 1, "one record per episode");
    assert!(app.messages.digest().is_empty(), "not before the hour");
    let t0 = app.supervisor_failing["s-7"].first;
    app.supervisor_failing.get_mut("s-7").unwrap().last = t0 + Duration::from_secs(55 * 60);
    let _ = app.settle_messages(t0 + Duration::from_secs(59 * 60));
    assert!(app.messages.digest().is_empty(), "a minute early");
    app.supervisor_failing.get_mut("s-7").unwrap().last = t0 + Duration::from_secs(59 * 60);
    let _ = app.settle_messages(t0 + Duration::from_secs(60 * 60));
    let entry = &app.messages.digest().entries[0];
    assert_eq!(entry.key, "harness.supervisor.s-7.failing");
    assert!(
        entry.detail.iter().any(|l| l.contains("boom again")),
        "{:?}",
        entry.detail
    );
    assert!(
        app.ripening_deadline().is_some(),
        "and it is watched until steady"
    );
    app.note_harness_host(HostNote::SupervisorSteady {
        sid: "s-7".to_string(),
    });
    assert!(app.messages.digest().is_empty());
    app.note_harness_host(failing("again"));
    assert_eq!(records(&app), 2);
    let t1 = app.supervisor_failing["s-7"].first;
    let _ = app.settle_messages(t1 + pause + Duration::from_secs(31 * 60));
    assert!(
        app.supervisor_failing.is_empty(),
        "a run that held is steady"
    );
    let _ = app.settle_messages(t1 + 3 * Duration::from_secs(3600));
    assert!(app.messages.digest().is_empty());
    app.note_harness_host(failing("once more"));
    app.note_harness_host(HostNote::SupervisorSteady {
        sid: "s-7".to_string(),
    });
    assert!(app.ripening_deadline().is_none());
    assert!(app.messages.digest().is_empty());
    assert_eq!(message_regrids(), message);
    assert!(app.messages.live_rows().count() == 0, "no band row");
}

/// A rating card the harness will not press: a record now, "press 0 in that tab" an hour on,
/// gone with the card.
#[test]
fn a_card_the_harness_will_not_press_is_a_record_then_a_line_after_an_hour() {
    let mut app = App::headless_for_test();
    app.note_harness_host(HostNote::SurveyStuck {
        sid: "s-8".to_string(),
        kind: StuckSurvey::Unclosed,
        why: "the session survey did not close (3 tries); still trying".to_string(),
    });
    assert!(
        app.messages
            .log()
            .records()
            .any(|r| r.key.as_deref() == Some("harness.host.survey-open.s-8")),
        "a record"
    );
    assert!(
        app.ripening_deadline().is_none(),
        "still trying: no claim on the digest, however long"
    );
    app.note_harness_host(HostNote::SurveyEnded {
        sid: "s-8".to_string(),
    });
    app.note_harness_host(HostNote::SurveyStuck {
        sid: "s-9".to_string(),
        kind: StuckSurvey::Withheld,
        why: "a rating card aterm does not answer is open (odd)".to_string(),
    });
    assert!(app.messages.digest().is_empty(), "not before the hour");
    assert!(
        app.messages
            .log()
            .records()
            .any(|r| r.key.as_deref() == Some("harness.host.survey-card.s-9")),
        "a record"
    );
    let due = app.ripening_deadline().expect("a claim");
    let _ = app.settle_messages(due);
    let entry = &app.messages.digest().entries[0];
    assert_eq!(entry.key, "harness.supervisor.s-9.survey-card");
    app.note_harness_host(HostNote::SurveyEnded {
        sid: "s-9".to_string(),
    });
    assert!(app.messages.digest().is_empty(), "gone with the card");
}

/// Two 6 h passes that freed nothing are one `disk.low` line; the record names the figures and
/// where the space is, and space coming back ends it.
#[test]
fn a_volume_aterm_could_not_help_is_one_line_and_space_ends_it() {
    let mut app = App::headless_for_test();
    let hogs = vec![
        "$HOME/trust/build 94.0 GiB (x.py bootstrap; run scripts/slim_build.py)".to_owned(),
        "$HOME/trust/crates/target.noindex 49.0 GiB (cargo build output: deps/ and build/ are `targo clean`'s)".to_owned(),
        "~/x/target 1.0 GiB (cargo build output: deps/ and build/ are `targo clean`'s)".to_owned(),
    ];
    app.note_harness_host(HostNote::DiskLow {
        free_gib: 7,
        floor_gib: 20,
        hogs: hogs.clone(),
    });
    app.note_harness_host(HostNote::DiskLow {
        free_gib: 6,
        floor_gib: 20,
        hogs: hogs.clone(),
    });
    let keys: Vec<String> = app
        .messages
        .digest()
        .entries
        .iter()
        .map(|e| e.key.clone())
        .collect();
    assert_eq!(keys, ["disk.low"]);
    assert_eq!(
        app.messages
            .log()
            .records()
            .filter(|r| !r.later && r.key.as_deref() == Some("system.disk.low"))
            .count(),
        1,
        "one record per episode"
    );
    let detail = app.messages.digest().entries[0].detail.clone();
    assert!(
        detail.iter().any(|l| l.contains(&hogs[0])) && detail.iter().any(|l| l.contains(&hogs[1])),
        "{detail:?}"
    );
    let record = app
        .messages
        .log()
        .records()
        .find(|r| r.key.as_deref() == Some("system.disk.low"))
        .expect("the record")
        .detail
        .clone();
    assert!(
        hogs.iter().all(|h| record.iter().any(|l| l.contains(h))),
        "{record:?}"
    );
    app.note_harness_host(HostNote::DiskPlenty);
    assert!(app.messages.digest().is_empty());
    app.note_harness_host(HostNote::DiskPlenty);
}

/// THE FULL DISK, ON THE GLASS ONLY WHILE THE OWNER IS NEEDED: what a pass
/// freed is a fact for later (one `disk.freed` line of the digest, nothing
/// standing); a volume the look left at its emergency is the one standing
/// alert (`system.disk.full`), restated in place, and the volume out of its
/// emergency ends it.
#[test]
fn a_full_disk_is_one_standing_alert_and_what_was_freed_is_a_fact() {
    let mut app = App::headless_for_test();
    app.note_harness_host(HostNote::DiskFreed {
        bytes: 31 << 30,
        went: vec!["31.0 GiB ~/Library/Caches/aterm/cargo-build.noindex/x/.cargo-build".into()],
    });
    let keys: Vec<String> = app
        .messages
        .digest()
        .entries
        .iter()
        .map(|e| e.key.clone())
        .collect();
    assert_eq!(keys, ["disk.freed"]);
    assert!(
        app.messages
            .live_by_key(crate::message_reporters::DISK_FULL_KEY)
            .is_none(),
        "a fact is not on the glass"
    );
    let full = || HostNote::DiskFull {
        free: Some(1 << 20),
        freed: 31 << 30,
        hogs: vec!["$HOME/trust/build 94.0 GiB (x.py bootstrap)".into()],
        cause: crate::message_reporters::DiskFullCause::Spent,
        thresholds: aterm_agent::harness::disk::Config::default().thresholds(None),
    };
    app.note_harness_host(full());
    app.note_harness_host(full());
    assert!(
        app.messages
            .live_by_key(crate::message_reporters::DISK_FULL_KEY)
            .is_some(),
        "the alert stands"
    );
    app.note_harness_host(HostNote::DiskFullOver);
    assert!(
        app.messages
            .live_by_key(crate::message_reporters::DISK_FULL_KEY)
            .is_none(),
        "space back ends it"
    );
}

/// THE DEVELOPER TOOLS LIST is one line however often a
/// reading says `not_granted`, whose one press opens the Developer Tools pane and whose
/// second capsule opens aterm's own Settings ▸ Security, in `atpkg::devtools`' words (owner
/// ask 2026-10-15: one spelling on every surface); a reading that finds aterm on the list
/// ends it; `unknown` leaves it as it was.
#[test]
fn the_developer_tools_list_is_one_line_and_a_reading_decides_it() {
    use crate::devtools_watch::Step;
    let mut app = App::headless_for_test();
    let regrids = message_regrids();
    let keys = |app: &App| -> Vec<String> {
        app.messages
            .digest()
            .entries
            .iter()
            .map(|e| e.key.clone())
            .collect()
    };
    assert!(app.apply_devtools_step(Step::Raise { hand_over: true }));
    assert!(!app.apply_devtools_step(Step::Raise { hand_over: false }));
    assert_eq!(
        keys(&app),
        [mr::DEVTOOLS_KEY],
        "one line, however often it is told"
    );
    let line = app.messages.digest().entries[0].clone();
    assert_eq!(
        line.actions,
        [
            aterm_messages::Intent::OpenSystemPane {
                pane: mr::PANE_DEVELOPER_TOOLS.to_string()
            },
            aterm_messages::Intent::OpenSettings {
                route: crate::native_settings::SettingsRoute::Security
                    .path()
                    .to_string()
            },
        ],
        "the one press opens Developer Tools; beside it, aterm's Security page"
    );
    assert_eq!(
        line.actions
            .iter()
            .map(aterm_messages::Intent::label)
            .collect::<Vec<_>>(),
        ["Open Settings", "Security"],
        "two capsules that never read alike"
    );
    assert_eq!(line.then, atpkg::devtools::HELPER_STEP, "the one action");
    let skips = aterm_containment::SpikeEvidence::MEASURED.devtool_skips_first_run_check;
    assert!(
        line.detail.iter().any(|l| l == atpkg::devtools::why(skips)),
        "why, in atpkg's words: {:?}",
        line.detail
    );
    assert!(
        line.detail.iter().any(|l| l
            .starts_with(aterm_containment::consent::DEVTOOLS_SETTINGS_WORDS)
            && l.ends_with(&format!("({})", atpkg::devtools::PLUS_ROUTE))),
        "the path, and the + route beside the drag: {:?}",
        line.detail
    );
    for (list, title, then, causes) in [
        (
            crate::devtools_helper::test_arms::list(
                aterm_containment::consent::DevToolListing::Off,
                aterm_containment::consent::DevToolListLabel::Read,
                Some(2),
            ),
            "aterm is off in Developer Tools",
            atpkg::devtools::STEP_SWITCH_ON,
            false,
        ),
        (
            crate::devtools_helper::test_arms::on(),
            "macOS still checks programs aterm starts",
            atpkg::devtools::STEP_LISTED_ON,
            true,
        ),
    ] {
        let mut app = App::headless_for_test();
        app.devtools_helper.note_reading(
            crate::devtools_helper::test_arms::reading(
                aterm_containment::DevToolGrant::NotGranted,
                list,
            ),
            std::time::Instant::now(),
        );
        let _ = app.apply_devtools_step(Step::Raise { hand_over: false });
        assert_eq!(keys(&app), [mr::DEVTOOLS_KEY], "one line");
        let line = app.messages.digest().entries[0].clone();
        assert_eq!((line.title.as_str(), line.then.as_str()), (title, then));
        assert_eq!(
            line.detail
                .iter()
                .any(|l| l == atpkg::devtools::HELPER_LISTED_ON),
            causes,
            "{:?}",
            line.detail
        );
        assert!(
            line.detail
                .iter()
                .all(|l| !l.contains(atpkg::devtools::PLUS_ROUTE)),
            "no + route for a listed aterm: {:?}",
            line.detail
        );
    }
    assert!(!app.apply_devtools_step(Step::Nothing));
    assert_eq!(keys(&app), [mr::DEVTOOLS_KEY], "unknown leaves it");
    assert!(!app.apply_devtools_step(Step::Clear));
    assert!(app.messages.digest().is_empty(), "on the list: it ends");
    assert_eq!(app.message_band_rows, 0, "never a band row");
    assert_eq!(message_regrids(), regrids, "no re-grid");
    app.on_devtools_probe_ready();
    assert!(app.messages.digest().is_empty());
}

/// An unreadable `[disk]` setting that turned cleanup off is one `disk.settings` line until a
/// look reads the settings; a short volume meanwhile is its own `disk.low-cleanup-off` line.
#[test]
fn a_disk_setting_that_turns_cleanup_off_is_one_line_until_it_reads_again() {
    let mut app = App::headless_for_test();
    let keys = |app: &App| {
        let mut keys: Vec<String> = app
            .messages
            .digest()
            .entries
            .iter()
            .map(|e| e.key.clone())
            .collect();
        keys.sort();
        keys
    };
    let records = |app: &App| {
        app.messages
            .log()
            .records()
            .filter(|r| !r.later && r.key.as_deref() == Some("system.disk.low"))
            .count()
    };
    let notes =
        vec!["disk.apply must be true or false, not a string, so every removal is off".to_string()];
    let short = HostNote::DiskLowCleanupOff {
        free_gib: 2,
        floor_gib: 10,
    };
    for _ in 0..2 {
        app.note_harness_host(HostNote::DiskSettingsOff {
            notes: notes.clone(),
        });
        app.note_harness_host(short.clone());
    }
    assert_eq!(
        keys(&app),
        [mr::DISK_LOW_CLEANUP_OFF_KEY, mr::DISK_SETTINGS_KEY]
    );
    assert_eq!(records(&app), 1, "one record per episode");
    let settings = app
        .messages
        .digest()
        .entries
        .iter()
        .find(|e| e.key == mr::DISK_SETTINGS_KEY)
        .expect("the settings line")
        .detail
        .clone();
    assert!(
        settings.iter().any(|l| l.contains(&notes[0])),
        "{settings:?}"
    );
    app.note_harness_host(HostNote::DiskPlenty);
    assert_eq!(
        keys(&app),
        [mr::DISK_SETTINGS_KEY],
        "space back is not the setting fixed"
    );
    app.note_harness_host(short.clone());
    assert_eq!(records(&app), 2, "a new episode is recorded again");
    app.note_harness_host(HostNote::DiskSettingsRead);
    assert!(
        app.messages.digest().is_empty(),
        "{:?}: the settings read end the line their fault opened",
        keys(&app)
    );
    app.note_harness_host(HostNote::DiskSettingsRead);
    assert!(app.messages.digest().is_empty());

    app.note_harness_host(HostNote::DiskLow {
        free_gib: 2,
        floor_gib: 10,
        hogs: Vec::new(),
    });
    app.note_harness_host(HostNote::DiskSettingsRead);
    assert_eq!(keys(&app), [mr::DISK_LOW_KEY]);
    app.note_harness_host(HostNote::DiskPlenty);
    assert!(app.messages.digest().is_empty());

    let mut app = App::headless_for_test();
    app.except(&mr::disk_low_cleanup_off_entry(2, 10));
    assert!(app.host_noted.is_empty());
    app.note_harness_host(HostNote::DiskSettingsRead);
    assert!(app.messages.digest().is_empty(), "{:?}", keys(&app));
}

/// A short volume is one low-disk line in the form last told (cleanup-off or the passes'), each
/// form recorded once; a change of form keeps what the owner filed until space is back.
#[test]
fn a_short_volume_is_one_low_disk_line_in_the_form_last_told() {
    let mut app = App::headless_for_test();
    let keys = |app: &App| -> Vec<String> {
        app.messages
            .digest()
            .entries
            .iter()
            .map(|e| e.key.clone())
            .collect()
    };
    let passes = HostNote::DiskLow {
        free_gib: 3,
        floor_gib: 10,
        hogs: Vec::new(),
    };
    let off = HostNote::DiskLowCleanupOff {
        free_gib: 2,
        floor_gib: 10,
    };
    app.note_harness_host(passes.clone());
    assert_eq!(keys(&app), [mr::DISK_LOW_KEY]);
    app.note_harness_host(off.clone());
    assert_eq!(keys(&app), [mr::DISK_LOW_CLEANUP_OFF_KEY]);
    app.note_harness_host(off.clone());
    app.note_harness_host(passes.clone());
    assert_eq!(keys(&app), [mr::DISK_LOW_KEY]);
    assert_eq!(
        app.messages
            .log()
            .records()
            .filter(|r| !r.later && r.key.as_deref() == Some("system.disk.low"))
            .count(),
        2,
        "each form once"
    );

    let mut app = App::headless_for_test();
    app.note_harness_host(passes.clone());
    assert_eq!(app.dismiss_exceptions(), 1);
    app.note_harness_host(off);
    assert_eq!(keys(&app), [mr::DISK_LOW_CLEANUP_OFF_KEY]);
    app.note_harness_host(HostNote::DiskSettingsRead);
    assert!(app.messages.digest().is_empty(), "{:?}", keys(&app));
    app.note_harness_host(passes.clone());
    assert!(
        app.messages.digest().is_empty(),
        "{:?}: the owner's filing holds",
        keys(&app)
    );
    app.note_harness_host(HostNote::DiskPlenty);
    app.note_harness_host(passes);
    assert_eq!(keys(&app), [mr::DISK_LOW_KEY]);
}

/// An upgrade that cannot run is one `harness.upgrade-host.cannot-run` line, ended when the last
/// such tab runs again (or its worker ends), never by a pass over the rows.
#[test]
fn an_upgrade_that_cannot_run_is_one_line_until_the_last_tab_runs_again() {
    let mut app = App::headless_for_test();
    for sid in ["s-1", "s-2"] {
        app.note_harness_host(HostNote::UpgradeCannotRun {
            sid: sid.to_string(),
            step: "busy:state-unwritable".to_string(),
            dir: Some(std::path::PathBuf::from("/state/upgrade")),
        });
    }
    assert_eq!(app.messages.digest().len(), 1, "one line, two tabs");
    let entry = &app.messages.digest().entries[0];
    assert_eq!(entry.key, "harness.upgrade-host.cannot-run");
    let said = format!("{} {} {:?}", entry.title, entry.then, entry.detail);
    assert!(said.contains("/state/upgrade"), "{said}");
    assert!(!said.contains("busy:"), "{said}");
    app.except_replace(Family::Upgrade, &[]);
    assert_eq!(
        app.messages.digest().len(),
        1,
        "the rows' pass does not end it"
    );
    app.note_harness_host(HostNote::UpgradeRuns {
        sid: "s-1".to_string(),
    });
    assert_eq!(app.messages.digest().len(), 1, "s-2 still cannot");
    app.note_harness_host(HostNote::UpgradeRuns {
        sid: "s-2".to_string(),
    });
    assert!(app.messages.digest().is_empty());
}

/// THE RECORDS' FOLDER THE UPGRADES' VIEW CANNOT LIST is
/// the host's fact, `harness.upgrade-host.records-unlisted`, in the
/// classifier's words with the folder under Details: until it, the upgrades
/// shown stood at the last whole look with nothing said.
#[test]
fn a_records_folder_the_view_cannot_list_is_one_line_until_it_is_listed() {
    let mut app = App::headless_for_test();
    let dir = std::path::PathBuf::from("/state/harness/upgrade");
    for _ in 0..2 {
        app.note_harness_host(HostNote::UpgradeRecords {
            unlisted: Some(dir.clone()),
        });
    }
    assert_eq!(app.messages.digest().len(), 1, "one line");
    let entry = &app.messages.digest().entries[0];
    assert_eq!(entry.key, mr::UPGRADE_RECORDS_UNLISTED_KEY);
    let said = format!("{} {} {:?}", entry.title, entry.then, entry.detail);
    assert!(said.contains("/state/harness/upgrade"), "{said}");
    app.except_replace(Family::Upgrade, &[]);
    assert_eq!(
        app.messages.digest().len(),
        1,
        "the rows' pass does not end it"
    );
    app.note_harness_host(HostNote::UpgradeRecords { unlisted: None });
    assert!(app.messages.digest().is_empty(), "listed: it ends");
    app.note_harness_host(HostNote::UpgradeCannotRun {
        sid: "s-1".to_string(),
        step: "busy:state-unwritable".to_string(),
        dir: None,
    });
    app.note_harness_host(HostNote::UpgradeRecords { unlisted: None });
    assert_eq!(app.messages.digest().len(), 1);
    assert_eq!(
        app.messages.digest().entries[0].key,
        mr::UPGRADE_CANNOT_RUN_KEY
    );
}

/// THE RECORDS THE UPGRADES' VIEW CANNOT READ (host-8) are
/// the host's fact, `harness.upgrade-host.records-unreadable`, in the
/// classifier's words with each file under Details: until it, a record that
/// could not be read froze its tab's upgrade row and mark with nothing
/// said.
#[test]
fn records_the_view_cannot_read_are_one_line_until_they_read() {
    let mut app = App::headless_for_test();
    let files = vec![
        std::path::PathBuf::from("/state/harness/upgrade/aaa.json"),
        std::path::PathBuf::from("/state/harness/upgrade/bbb.json"),
    ];
    app.note_harness_host(HostNote::UpgradeRecords {
        unlisted: Some(std::path::PathBuf::from("/state/harness/upgrade")),
    });
    for _ in 0..2 {
        app.note_harness_host(HostNote::UpgradeRecordsUnreadable {
            files: files.clone(),
        });
    }
    let keys: Vec<String> = app
        .messages
        .digest()
        .entries
        .iter()
        .map(|e| e.key.clone())
        .collect();
    assert_eq!(
        keys.iter()
            .filter(|k| *k == mr::UPGRADE_RECORDS_UNREADABLE_KEY)
            .count(),
        1,
        "one line: {keys:?}"
    );
    let digest = app.messages.digest();
    let entry = digest
        .entries
        .iter()
        .find(|e| e.key == mr::UPGRADE_RECORDS_UNREADABLE_KEY)
        .expect("said");
    let said = format!("{} {} {:?}", entry.title, entry.then, entry.detail);
    assert!(
        said.contains("aaa.json") && said.contains("bbb.json"),
        "{said}"
    );
    app.note_harness_host(HostNote::UpgradeRecordsUnreadable { files: Vec::new() });
    let keys: Vec<String> = app
        .messages
        .digest()
        .entries
        .iter()
        .map(|e| e.key.clone())
        .collect();
    assert_eq!(
        keys,
        [mr::UPGRADE_RECORDS_UNLISTED_KEY.to_string()],
        "read: it ends, the folder's line stands"
    );
}

/// What a note of the accounts may never move: the band's rows, a tab's
/// attention (its strip's bit, its session's badge), and the band's re-grid
/// count (presence owns no row to re-grid).
fn glass(app: &App) -> (usize, usize, usize, u64) {
    let marked = app
        .windows
        .values()
        .flat_map(|w| w.tab_set.tabs())
        .filter(|tab| tab.presentation.indicators.attention)
        .count();
    let badged = app
        .pool
        .iter()
        .filter(|s| {
            let meta = s.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            meta.attention.is_some()
        })
        .count();
    (
        app.messages.live_rows().count(),
        marked,
        badged,
        message_regrids(),
    )
}

/// THE CLAUDE ACCOUNTS' TWO LINES (`HostNote::Accounts`), walked: an account
/// signed out is ONE line, `harness.accounts.signed-out`, whose key and action
/// hold while its first label does; none signed out ends it.
#[test]
fn the_accounts_lines_follow_what_the_host_says_and_mark_nothing() {
    use crate::messages_host::wall_stamp_now;
    use aterm_messages::PostOutcome;
    use std::time::Instant;
    let mut app = App::headless_for_test();
    let before = glass(&app);
    let records = |app: &App| app.messages.log().records().filter(|r| !r.later).count();
    let recorded = records(&app);
    let listed = |labels: &[&str]| {
        HostNote::Accounts(AccountsSaid::Listed {
            signed_out: labels.iter().map(|l| l.to_string()).collect(),
            credits: Vec::new(),
            window_full: Vec::new(),
            due: Vec::new(),
            none_soon: false,
            wrong: Vec::new(),
        })
    };
    let step = |app: &mut App, note: HostNote| -> Vec<(String, String)> {
        app.note_harness_host(note.clone());
        assert_eq!(glass(app), before, "{note:?}: nothing on the glass");
        assert!(app.host_noted.is_empty(), "{note:?}: no claim held");
        assert_eq!(records(app), recorded, "{note:?}: no record");
        let mut lines: Vec<(String, String)> = app
            .messages
            .digest()
            .entries
            .iter()
            .map(|e| (e.key.clone(), e.then.clone()))
            .collect();
        lines.sort();
        lines
    };
    let shown = |app: &App| -> (bool, Vec<&'static str>) {
        let digest = app.messages.digest();
        let e = digest
            .entries
            .iter()
            .find(|e| e.key == mr::ACCOUNTS_SIGNED_OUT_KEY)
            .expect("the line is open");
        let detail = e.detail[2..].join("\n");
        let labels = ["work-2", "pool-a"]
            .into_iter()
            .filter(|l| detail.contains(l))
            .collect();
        (e.title.starts_with("2 "), labels)
    };
    let logged = |app: &App| -> Vec<String> {
        app.messages
            .log()
            .records()
            .filter(|r| r.later && r.is_live())
            .filter(|r| r.key.as_deref() == Some(mr::ACCOUNTS_SIGNED_OUT_KEY))
            .map(|r| r.detail[1].clone())
            .collect()
    };
    let line = |key: &str, then: &str| (key.to_string(), then.to_string());
    let login = |label: &str| {
        line(
            mr::ACCOUNTS_SIGNED_OUT_KEY,
            &notice::ACCOUNT_SIGNED_OUT.fill(Some(label), None).todo,
        )
    };
    let refused = line(
        mr::ACCOUNTS_REFUSED_KEY,
        "Run aterm harness accounts to read why it is refused",
    );

    let rev = app.messages.digest_revision();
    assert!(step(&mut app, HostNote::Accounts(AccountsSaid::Nothing)).is_empty());
    assert!(step(&mut app, listed(&[])).is_empty());
    assert_eq!(app.messages.digest_revision(), rev, "an empty digest stays");

    assert_eq!(step(&mut app, listed(&["work-2"])), [login("work-2")]);
    assert_eq!(shown(&app), (false, vec!["work-2"]));
    let rev = app.messages.digest_revision();
    assert_eq!(step(&mut app, listed(&["work-2"])), [login("work-2")]);
    assert_eq!(app.messages.digest_revision(), rev, "the same words again");
    let id = app.messages.digest().entries[0].id;
    let both = ["work-2", "pool-a"];
    assert_eq!(step(&mut app, listed(&both)), [login("work-2")]);
    assert_eq!(app.messages.digest().entries[0].id, id, "one entry, kept");
    assert_eq!(shown(&app), (true, vec!["work-2", "pool-a"]), "at once");
    let rev = app.messages.digest_revision();
    let again = mr::accounts_signed_out_entry(&both.map(str::to_string)).expect("an entry");
    let posted = app.messages.post(
        again.message().expect("an entry that passes"),
        wall_stamp_now(),
        Instant::now(),
    );
    assert_eq!(posted.outcome, PostOutcome::Duplicate);
    assert_eq!(app.messages.digest_revision(), rev);
    assert_eq!(app.messages.digest().entries[0].id, id);
    assert_eq!(
        logged(&app),
        [notice::ACCOUNT_SIGNED_OUT.fill(Some("work-2"), None).todo]
    );

    assert_eq!(step(&mut app, listed(&["pool-a"])), [login("pool-a")]);
    assert_ne!(app.messages.digest().entries[0].id, id, "raised anew");
    assert_eq!(shown(&app), (false, vec!["pool-a"]));
    assert_eq!(
        logged(&app),
        [notice::ACCOUNT_SIGNED_OUT.fill(Some("pool-a"), None).todo]
    );
    assert!(step(&mut app, listed(&[])).is_empty());
    assert!(logged(&app).is_empty());

    assert_eq!(step(&mut app, listed(&["work-2"])), [login("work-2")]);
    assert_eq!(
        step(&mut app, HostNote::Accounts(AccountsSaid::Refused)),
        std::slice::from_ref(&refused)
    );
    assert_eq!(step(&mut app, listed(&["work-2"])), [login("work-2")]);
    assert!(step(&mut app, HostNote::Accounts(AccountsSaid::Nothing)).is_empty());

    let keys = |lines: Vec<(String, String)>| -> Vec<String> {
        lines.into_iter().map(|(key, _)| key).collect()
    };
    assert_eq!(step(&mut app, listed(&["work-2"])), [login("work-2")]);
    assert_eq!(
        keys(step(
            &mut app,
            HostNote::Accounts(AccountsSaid::LoginsUnread)
        )),
        [mr::ACCOUNTS_LOGINS_UNREAD_KEY]
    );
    assert_eq!(
        keys(step(&mut app, HostNote::Accounts(AccountsSaid::Refused))),
        [mr::ACCOUNTS_REFUSED_KEY]
    );
    assert_eq!(
        keys(step(
            &mut app,
            HostNote::Accounts(AccountsSaid::LoginsUnread)
        )),
        [mr::ACCOUNTS_LOGINS_UNREAD_KEY]
    );
    assert_eq!(step(&mut app, listed(&["work-2"])), [login("work-2")]);
    assert!(step(&mut app, HostNote::Accounts(AccountsSaid::Nothing)).is_empty());

    assert_eq!(step(&mut app, listed(&["work-2"])), [login("work-2")]);
    assert_eq!(app.dismiss_exceptions(), 1);
    assert_eq!(
        step(&mut app, HostNote::Accounts(AccountsSaid::Refused)),
        [refused]
    );
    assert!(
        step(&mut app, listed(&["work-2"])).is_empty(),
        "still filed"
    );
    assert!(step(&mut app, listed(&["pool-a"])).is_empty());
    assert!(step(&mut app, listed(&[])).is_empty());
    assert_eq!(step(&mut app, listed(&["work-2"])), [login("work-2")]);

    let adopting = || {
        use aterm_messages::{LogLine, MessageCenter, MessageLog};
        let mut earlier = App::headless_for_test();
        earlier.except(&mr::accounts_signed_out_entry(&both.map(str::to_string)).expect("one"));
        earlier.except(&mr::accounts_refused_entry());
        let lines: Vec<LogLine> = earlier
            .messages
            .drain_shelved_for_persist()
            .into_iter()
            .map(|(line, _)| line)
            .collect();
        let mut log = MessageLog::empty();
        log.replay_all(lines);
        let now = Instant::now();
        let mut center = MessageCenter::new(log, now);
        assert_eq!(center.adopt_digest(wall_stamp_now(), now), 2);
        let mut app = App::headless_for_test();
        app.messages = center;
        app
    };
    let mut app = adopting();
    let before = glass(&app);
    assert!(shown(&app).0, "the adopted line counts two");
    app.note_harness_host(listed(&["pool-a"]));
    assert_eq!(
        shown(&app),
        (false, vec!["pool-a"]),
        "the adopted words gave way"
    );
    assert_eq!(app.messages.digest().len(), 1, "and the refused line ended");
    assert_eq!(
        logged(&app),
        [notice::ACCOUNT_SIGNED_OUT.fill(Some("pool-a"), None).todo]
    );
    assert_eq!(glass(&app), before);
    let mut app = adopting();
    app.note_harness_host(HostNote::Accounts(AccountsSaid::Nothing));
    assert!(app.messages.digest().is_empty(), "both ended");
    assert_eq!(glass(&app), before);
}

/// THE ACCOUNT'S USAGE LINES (`HostNote::Accounts`, accounts v2 S5): usage
/// credits carrying an account's sessions are ONE line,
/// `harness.accounts.credits`, raised once — the same note again moves
/// nothing — and ended when nothing names it (credits over); the week nearly
/// used with credits off is ONE line, `harness.accounts.window-full`, whose
/// action is to turn credits on, or to raise their cap where the vendor said
/// they ran out — under `[harness] usage_credits = false`, the default, with
/// that key set too, since the harness keeps the session on the plan until
/// the reset whatever the account allows.
#[test]
fn the_accounts_usage_lines_follow_what_the_host_says_and_mark_nothing() {
    let mut app = App::headless_for_test();
    let before = glass(&app);
    let said = |credits: &[&str], full: &[(&str, &str)]| {
        HostNote::Accounts(AccountsSaid::Listed {
            signed_out: Vec::new(),
            credits: credits.iter().map(|l| l.to_string()).collect(),
            window_full: full
                .iter()
                .map(|(l, why)| (l.to_string(), why.to_string()))
                .collect(),
            due: Vec::new(),
            none_soon: false,
            wrong: Vec::new(),
        })
    };
    let step = |app: &mut App, note: HostNote| -> Vec<(String, String)> {
        app.note_harness_host(note.clone());
        assert_eq!(glass(app), before, "{note:?}: nothing on the glass");
        assert!(app.host_noted.is_empty(), "{note:?}: no claim held");
        let mut lines: Vec<(String, String)> = app
            .messages
            .digest()
            .entries
            .iter()
            .map(|e| (e.key.clone(), e.then.clone()))
            .collect();
        lines.sort();
        lines
    };
    let credits = (
        mr::ACCOUNTS_CREDITS_KEY.to_string(),
        "Review the usage credit cap in claude.ai settings".to_string(),
    );
    let full_line = |then: &str| (mr::ACCOUNTS_WINDOW_FULL_KEY.to_string(), then.to_string());
    let turn_on =
        full_line("Set [harness] usage_credits = true and turn on usage credits in claude.ai");
    let raise =
        full_line("Set [harness] usage_credits = true and raise the usage credit cap in claude.ai");
    assert!(step(&mut app, said(&[], &[])).is_empty());
    assert_eq!(
        step(&mut app, said(&["main"], &[])),
        std::slice::from_ref(&credits)
    );
    let rev = app.messages.digest_revision();
    assert_eq!(
        step(&mut app, said(&["main"], &[])),
        std::slice::from_ref(&credits)
    );
    assert_eq!(app.messages.digest_revision(), rev, "the same words again");
    let e = app.messages.digest().entries[0].clone();
    assert_eq!(e.title, "Claude is running on usage credits");
    assert!(
        e.detail.iter().any(|l| l == "On credits: main"),
        "{:?}",
        e.detail
    );
    assert!(step(&mut app, said(&[], &[])).is_empty());
    assert_eq!(
        step(&mut app, said(&[], &[("main", "org_level_disabled")])),
        std::slice::from_ref(&turn_on)
    );
    assert!(step(&mut app, said(&[], &[])).is_empty());
    assert_eq!(
        step(&mut app, said(&[], &[("main", "out_of_credits")])),
        [raise]
    );
    assert!(step(&mut app, said(&[], &[])).is_empty());
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\nusage_credits = true\n"),
    );
    assert_eq!(
        step(&mut app, said(&[], &[("main", "org_level_disabled")])),
        [full_line("Turn on usage credits in claude.ai settings")]
    );
    assert!(step(&mut app, said(&[], &[])).is_empty());
    assert_eq!(
        step(&mut app, said(&[], &[("main", "out_of_credits")])),
        [full_line(
            "Raise the usage credit cap in claude.ai settings"
        )]
    );
    assert_eq!(
        step(&mut app, said(&["main"], &[("main", "org_level_disabled")])).len(),
        2
    );
    assert!(step(&mut app, HostNote::Accounts(AccountsSaid::Nothing)).is_empty());
    let spent = [("main".to_string(), "out_of_credits".to_string())];
    let off = [("main".to_string(), String::new())];
    for entry in [
        mr::accounts_credits_entry(&["main".to_string(), "work-2".to_string()]),
        mr::accounts_window_full_entry(&spent, true),
        mr::accounts_window_full_entry(&off, true),
        mr::accounts_window_full_entry(&spent, false),
        mr::accounts_window_full_entry(&off, false),
    ] {
        let entry = entry.expect("an entry");
        assert_eq!(entry.family(), Family::Accounts);
        assert!(entry.message().is_ok(), "{:?}", entry.message().err());
    }
    assert!(mr::accounts_credits_entry(&[]).is_none());
    assert!(mr::accounts_window_full_entry(&[], false).is_none());
}

/// A `Later` point only a person can answer is one `harness.resume.<kind>` line naming its tabs,
/// a stall one `harness.resume.<tab>.stalled` line per tab; each ends ten minutes after the last
/// look that named it.
#[test]
fn a_resume_exception_is_one_line_per_kind_and_a_stall_one_per_tab_and_each_ends_with_its_fact() {
    let mut app = App::headless_for_test();
    let told = |app: &mut App, sid: &str, standing: Option<Exception>| {
        app.note_harness_host(HostNote::ResumeException {
            sid: sid.to_string(),
            agent: Program::Claude,
            standing,
        });
    };
    let keys = |app: &App| -> Vec<String> {
        let mut keys: Vec<String> = app
            .messages
            .digest()
            .entries
            .iter()
            .map(|e| e.key.clone())
            .collect();
        keys.sort();
        keys
    };
    told(
        &mut app,
        "s-1",
        Some(resume_exception(ExceptionKind::Login)),
    );
    assert!(app.messages.digest().is_empty());
    told(
        &mut app,
        "s-1",
        Some(resume_exception(ExceptionKind::Spend)),
    );
    told(
        &mut app,
        "s-2",
        Some(resume_exception(ExceptionKind::Spend)),
    );
    told(
        &mut app,
        "s-3",
        Some(resume_exception(ExceptionKind::GoalCleared)),
    );
    assert_eq!(
        keys(&app),
        ["harness.resume.goal-cleared", "harness.resume.spend"]
    );
    told(&mut app, "s-2", Some(stall_exception()));
    assert_eq!(
        keys(&app),
        [
            "harness.resume.goal-cleared",
            "harness.resume.s-2.stalled",
            "harness.resume.spend"
        ],
        "the stall replaced tab 2's spend; tab 1 still has it"
    );
    told(&mut app, "s-1", None);
    assert!(keys(&app).contains(&"harness.resume.spend".to_string()));
    told(&mut app, "s-3", None);
    told(&mut app, "s-2", None);
    assert_eq!(app.messages.digest().len(), 3, "one look is a flicker");
    let due = app.messages_deadline().expect("the clock is armed");
    let _ = app.settle_messages(due + Duration::from_secs(1));
    assert!(
        app.messages.digest().is_empty(),
        "no fact stands: {:?}",
        keys(&app)
    );
    told(
        &mut app,
        "s-4",
        Some(resume_exception(ExceptionKind::Futile)),
    );
    assert_eq!(app.messages.digest().len(), 1);
    told(&mut app, "s-4", None);
    let due = app.messages_deadline().expect("armed");
    let _ = app.settle_messages(due + Duration::from_secs(1));
    assert!(app.messages.digest().is_empty());
}

/// A failing relaunch is a record, and a line only after an hour for a cause a person clears;
/// `relaunch = false` is a record; an agent that cannot come back is the tab's mark when its
/// command is known, else one line; the agent running again ends every line.
#[test]
fn a_relaunch_is_a_record_and_only_what_a_person_alone_can_mend_is_a_line() {
    let mut app = App::headless_for_test();
    let tab = front_tab_sid(&mut app);
    let say = |app: &mut App, said| {
        app.note_harness_host(HostNote::Relaunch {
            sid: tab.clone(),
            said,
        });
    };
    let records = |app: &App| {
        app.messages
            .log()
            .records()
            .filter(|r| {
                !r.later && r.key.as_deref() == Some(&*format!("harness.host.relaunch.{tab}"))
            })
            .count()
    };
    let command = Some("claude --resume 0badf00d-1111-2222-3333-444455556666".to_string());
    say(
        &mut app,
        RelaunchSay::Failing {
            blocked_by: None,
            restarted: false,
            command: command.clone(),
        },
    );
    assert_eq!(records(&app), 1);
    assert!(app.ripening_deadline().is_none(), "nothing ripens");
    let _ = app.settle_messages(Instant::now() + 2 * DAY);
    assert!(app.messages.digest().is_empty());
    say(
        &mut app,
        RelaunchSay::Failing {
            blocked_by: Some("held"),
            restarted: false,
            command,
        },
    );
    assert!(app.messages.digest().is_empty(), "not yet");
    let due = app.ripening_deadline().expect("armed for the hour");
    let _ = app.settle_messages(due);
    let keys: Vec<String> = app
        .messages
        .digest()
        .entries
        .iter()
        .map(|e| e.key.clone())
        .collect();
    assert_eq!(keys, [mr::relaunch_keys(&tab)[0].clone()]);
    say(&mut app, RelaunchSay::Ended);
    assert!(app.messages.digest().is_empty());
    say(
        &mut app,
        RelaunchSay::Limited {
            why: "[harness] relaunch = false".to_string(),
        },
    );
    assert!(app.messages.digest().is_empty());
    let cannot_at =
        |command: Option<&str>, marked: bool, lost: bool, new_tab: bool| RelaunchSay::Cannot {
            reason: "its shell is gone".to_string(),
            restarted: false,
            command: command.map(str::to_string),
            marked,
            lost,
            new_tab,
            then: None,
        };
    let cannot =
        |command: Option<&str>, marked: bool, lost: bool| cannot_at(command, marked, lost, false);
    say(
        &mut app,
        cannot(Some("claude --resume 0badf00d"), true, true),
    );
    assert!(app.messages.digest().is_empty(), "the tab carries the ask");
    say(&mut app, cannot(None, false, false));
    let gone = app.messages.digest().entries[0].clone();
    assert_eq!(gone.key, mr::relaunch_keys(&tab)[1]);
    assert!(
        gone.then.starts_with("Start the agent in that tab"),
        "{}",
        gone.then
    );
    say(&mut app, RelaunchSay::Ended);
    assert!(app.messages.digest().is_empty());
    say(&mut app, cannot_at(None, false, false, true));
    let gone = app.messages.digest().entries[0].clone();
    assert!(
        gone.then.starts_with("Start the agent in a new tab"),
        "{}",
        gone.then
    );
    say(&mut app, RelaunchSay::Ended);
    assert!(app.messages.digest().is_empty());
    say(
        &mut app,
        cannot(Some("HOME=/o claude --resume 0badf00d"), false, true),
    );
    let lost = app.messages.digest().entries[0].clone();
    assert_eq!(lost.key, mr::relaunch_keys(&tab)[2]);
    assert!(
        lost.then.starts_with("Run the command below"),
        "{}",
        lost.then
    );
    say(&mut app, RelaunchSay::Ended);
    assert_eq!(
        app.messages.digest().entries.len(),
        1,
        "not ended by a program"
    );
    say(&mut app, RelaunchSay::Back);
    assert!(app.messages.digest().is_empty(), "held again");
    say(
        &mut app,
        RelaunchSay::SteppedDown {
            reason: "its shell is gone".to_string(),
            restarted: false,
            command: Some("claude --resume 0badf00d".to_string()),
        },
    );
    assert_eq!(
        app.messages.digest().entries[0].key,
        mr::relaunch_keys(&tab)[2]
    );
    say(&mut app, RelaunchSay::Back);
    assert!(app.messages.digest().is_empty());
    say(
        &mut app,
        cannot(Some("HOME=/o claude --resume 0badf00d"), false, true),
    );
    assert_eq!(
        app.messages.digest().entries[0].key,
        mr::relaunch_keys(&tab)[2]
    );
    say(
        &mut app,
        cannot(Some("claude --resume 5ca1ab1e"), true, true),
    );
    assert!(
        app.messages.digest().is_empty(),
        "the mark is the tab's one word: {:?}",
        app.messages.digest().entries
    );
    say(&mut app, RelaunchSay::NotRestored);
    let entries = app.messages.digest().entries.clone();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0].key, mr::relaunch_not_restored_key(&tab));
    assert!(!mr::relaunch_keys(&tab).contains(&entries[0].key));
    assert!(
        entries[0].then.starts_with("Look at the tab"),
        "{}",
        entries[0].then
    );
    say(&mut app, RelaunchSay::Ended);
    assert_eq!(app.messages.digest().entries.len(), 1, "Ended leaves it");
}

/// The sid of a headless App's front tab, registered in the store at real length (22
/// characters, as `aterm ctl ls` prints): its digest keys are shortened to the key cap, the
/// shape the closed-tab and adoption decisions work on.
fn front_tab_sid(app: &mut App) -> String {
    let local = app
        .focused_session_id(crate::WindowId(0))
        .expect("the front session");
    let mut handle = crate::session_store::test_handle(local);
    handle.sid = aterm_session::SessionId::new(REAL_SID);
    app.store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .register(handle);
    REAL_SID.to_string()
}

/// A session id of the length real ones have.
const REAL_SID: &str = "s-3e79b5bde172ad0088ad";
/// The sid of a tab that is not there.
const CLOSED_SID: &str = "s-9f8e7d6c5b4a39281706";

/// A RELAUNCH LINE ENDS WITH ITS TAB (review of 2026-09-29): the worker of a
/// closed tab has no later word, so a claim that is still ripening, and a line
/// already standing, end when no tab shows the session — the line of what a
/// restart did not bring back too (`not-restored`, the decision review's
/// RR-LC-2: it has no ending word at all, and stood with a Show-tab link to a
/// tab that was gone).
#[test]
fn a_relaunch_claim_and_line_end_when_their_tab_is_gone() {
    let mut app = App::headless_for_test();
    let tab = front_tab_sid(&mut app);
    let blocked = |sid: &str| HostNote::Relaunch {
        sid: sid.to_string(),
        said: RelaunchSay::Failing {
            blocked_by: Some("held"),
            restarted: false,
            command: Some("claude --resume 0badf00d-1111-2222-3333-444455556666".to_string()),
        },
    };
    app.note_harness_host(blocked(CLOSED_SID));
    app.note_harness_host(blocked(&tab));
    let due = app.ripening_deadline().expect("two claims");
    let _ = app.settle_messages(due + Duration::from_secs(1));
    let keys: Vec<String> = app
        .messages
        .digest()
        .entries
        .iter()
        .map(|e| e.key.clone())
        .collect();
    assert_eq!(keys, [mr::relaunch_keys(&tab)[0].clone()]);
    app.note_harness_host(HostNote::Relaunch {
        sid: CLOSED_SID.to_string(),
        said: RelaunchSay::Cannot {
            reason: "its shell is gone".to_string(),
            restarted: false,
            command: None,
            marked: false,
            lost: false,
            new_tab: false,
            then: None,
        },
    });
    let _ = app.settle_messages(due + Duration::from_secs(2));
    let keys: Vec<String> = app
        .messages
        .digest()
        .entries
        .iter()
        .map(|e| e.key.clone())
        .collect();
    assert_eq!(keys, [mr::relaunch_keys(&tab)[0].clone()], "{keys:?}");
    for sid in [CLOSED_SID, tab.as_str()] {
        app.note_harness_host(HostNote::Relaunch {
            sid: sid.to_string(),
            said: RelaunchSay::NotRestored,
        });
    }
    app.note_harness_host(HostNote::Relaunch {
        sid: tab.clone(),
        said: RelaunchSay::Ended,
    });
    let _ = app.settle_messages(due + Duration::from_secs(3));
    let keys: Vec<String> = app
        .messages
        .digest()
        .entries
        .iter()
        .map(|e| e.key.clone())
        .collect();
    assert_eq!(keys, [mr::relaunch_not_restored_key(&tab)], "{keys:?}");
}

/// A RESUME THAT WOULD LOSE TURNS IS NEVER TOLD TO BE RESUMED (the Gemini
/// CLI lane's pre-check, `refused:history-unsafe`): its Messages line's step
/// is its own, with its conversation known and without; so is a session that
/// took no turn.
#[test]
fn a_resume_that_would_lose_turns_is_never_told_to_be_resumed() {
    use crate::harness_host::cannot_then;
    let unsafe_then = cannot_then("refused:history-unsafe");
    assert!(unsafe_then.is_some());
    assert!(cannot_then("refused:no-turn").is_some());
    assert_eq!(cannot_then("refused:shell-gone"), None);
    for (step, lost) in [
        ("refused:history-unsafe", true),
        ("refused:history-unsafe", false),
        ("refused:no-turn", true),
        ("refused:shell-gone", true),
    ] {
        let mut app = App::headless_for_test();
        let tab = front_tab_sid(&mut app);
        app.note_harness_host(HostNote::Relaunch {
            sid: tab.clone(),
            said: RelaunchSay::Cannot {
                reason: "why".to_string(),
                restarted: false,
                command: None,
                marked: false,
                lost,
                new_tab: false,
                then: cannot_then(step),
            },
        });
        let thens: Vec<String> = app
            .messages
            .digest()
            .entries
            .iter()
            .map(|e| e.then.clone())
            .collect();
        let want = cannot_then(step)
            .unwrap_or("Start the agent in that tab and resume its conversation")
            .to_string();
        assert_eq!(thens, [want], "{step} lost={lost}");
    }
}

/// A `lost` LINE ENDS WITH ITS TAB TOO (the review of LC-4):
/// it says "run the command below in that tab" and shows that tab, and its
/// host forgets the tab's word with it, so nothing would end it once the
/// tab is gone — a Warn that resurfaces for the rest of the process, its
/// action pointing at a tab that is gone or another tab now.
#[test]
fn a_lost_line_ends_when_its_tab_is_gone() {
    let mut app = App::headless_for_test();
    let tab = front_tab_sid(&mut app);
    let lost = |sid: &str| HostNote::Relaunch {
        sid: sid.to_string(),
        said: RelaunchSay::Cannot {
            reason: "no try could type at its prompt in time".to_string(),
            restarted: false,
            command: Some("HOME=/Users//o claude --resume 0badf00d".to_string()),
            marked: false,
            lost: true,
            new_tab: false,
            then: None,
        },
    };
    app.note_harness_host(lost(CLOSED_SID));
    app.note_harness_host(lost(&tab));
    let _ = app.settle_messages(Instant::now() + Duration::from_secs(1));
    let keys: Vec<String> = app
        .messages
        .digest()
        .entries
        .iter()
        .map(|e| e.key.clone())
        .collect();
    assert_eq!(keys, [mr::relaunch_keys(&tab)[2].clone()], "{keys:?}");
}

/// TWO LOSSES OF ONE TAB, EACH ENDED BY ITS OWN END (the merge of LC-4 into
/// XREV-LC-2): what a restart did not bring back (`not-restored`, a monitor
/// the resumed build said is gone) and an agent that cannot come back on a
/// known conversation (`lost`) stand side by side, neither writing over the
/// other's words.
#[test]
fn a_lost_word_and_what_a_restart_did_not_bring_back_end_each_on_its_own() {
    let mut app = App::headless_for_test();
    let tab = front_tab_sid(&mut app);
    let say = |app: &mut App, sid: &str, said| {
        app.note_harness_host(HostNote::Relaunch {
            sid: sid.to_string(),
            said,
        });
    };
    let cannot = |marked: bool| RelaunchSay::Cannot {
        reason: "no try could type at its prompt in time".to_string(),
        restarted: false,
        command: Some("HOME=/Users//o claude --resume 0badf00d".to_string()),
        marked,
        lost: true,
        new_tab: false,
        then: None,
    };
    let lines = |app: &App| -> Vec<(String, String)> {
        let mut lines: Vec<(String, String)> = app
            .messages
            .digest()
            .entries
            .iter()
            .map(|e| (e.key.clone(), e.then.clone()))
            .collect();
        lines.sort();
        lines
    };
    let [_, _, lost] = mr::relaunch_keys(&tab);
    let not_restored = mr::relaunch_not_restored_key(&tab);
    assert_ne!(lost, not_restored, "two keys");
    let lost_line = (
        lost.clone(),
        "Run the command below in that tab to resume its conversation".to_string(),
    );
    let not_restored_line = (
        not_restored.clone(),
        "Look at the tab and restore what it relied on".to_string(),
    );
    let both = {
        let mut both = vec![lost_line.clone(), not_restored_line.clone()];
        both.sort();
        both
    };
    say(&mut app, &tab, RelaunchSay::NotRestored);
    say(&mut app, &tab, cannot(false));
    assert_eq!(lines(&app), both, "side by side");
    say(&mut app, &tab, RelaunchSay::NotRestored);
    assert_eq!(lines(&app), both, "a second loss rewrites its own line");
    say(&mut app, &tab, RelaunchSay::Ended);
    assert_eq!(lines(&app), both, "an agent running ends neither");
    say(&mut app, &tab, RelaunchSay::Back);
    assert_eq!(
        lines(&app),
        std::slice::from_ref(&not_restored_line),
        "held again"
    );
    say(&mut app, &tab, cannot(false));
    say(&mut app, &tab, cannot(true));
    assert_eq!(
        lines(&app),
        std::slice::from_ref(&not_restored_line),
        "a newer mark"
    );
    say(&mut app, &tab, cannot(false));
    assert_eq!(lines(&app), both);
    say(&mut app, CLOSED_SID, RelaunchSay::NotRestored);
    say(&mut app, CLOSED_SID, cannot(false));
    assert_eq!(app.messages.digest().entries.len(), 4);
    let _ = app.settle_messages(Instant::now() + Duration::from_secs(1));
    assert_eq!(lines(&app), both, "the closed tab's two lines end");
    let mut successor = App::headless_for_test();
    let spot = &places()[0];
    successor.except(&mr::relaunch_lost_entry(
        &tab,
        spot,
        "its shell is gone",
        false,
        Some("claude --resume 0badf00d"),
        None,
    ));
    successor.except(&mr::relaunch_not_restored_entry(&tab, spot));
    successor.sweep_adopted_supervisor_lines();
    let keys: Vec<String> = successor
        .messages
        .digest()
        .entries
        .iter()
        .map(|e| e.key.clone())
        .collect();
    assert_eq!(keys, [not_restored]);
}

/// The closed-tab sweep runs on every loop turn and judges only what is
/// there to judge: with no relaunch line or claim open, a told mark alone
/// still ends with its tab, and every other fact the host holds stands.
#[test]
fn a_told_relaunch_mark_alone_ends_with_its_tab_and_nothing_else_moves() {
    let mut app = App::headless_for_test();
    let tab = front_tab_sid(&mut app);
    app.host_noted.insert("stalled:s-1".to_string());
    app.host_noted.insert(format!("relaunch:{tab}"));
    app.host_noted.insert(format!("relaunch:{CLOSED_SID}"));
    let digest_before = app.messages.digest_revision();
    app.end_relaunch_lines_of_closed_tabs();
    let mut noted: Vec<&str> = app.host_noted.iter().map(String::as_str).collect();
    noted.sort_unstable();
    let open_mark = format!("relaunch:{tab}");
    assert_eq!(noted, [open_mark.as_str(), "stalled:s-1"]);
    assert_eq!(app.messages.digest_revision(), digest_before);
    app.host_noted.remove(&open_mark);
    app.end_relaunch_lines_of_closed_tabs();
    assert_eq!(
        app.host_noted.iter().collect::<Vec<_>>(),
        [&"stalled:s-1".to_string()]
    );
    assert_eq!(app.messages.digest_revision(), digest_before);
}

/// A relaunch line an earlier process left is ended by the adoption sweep unless this process's
/// worker told of it, for short and real-length ids (whose keys carry a shortened subject).
#[test]
fn the_adoption_sweep_ends_relaunch_lines_this_process_did_not_tell() {
    for (old, told) in [("s-old", "s-told"), ("s-1a2b3c4d5e6f708192a3", REAL_SID)] {
        let mut app = App::headless_for_test();
        let spot = &places()[0];
        for sid in [old, told] {
            app.except(&mr::relaunch_gone_entry(
                sid,
                spot,
                "its shell is gone",
                false,
                false,
                None,
            ));
            app.except(&mr::relaunch_blocked_entry(sid, spot, "held", false));
            app.except(&mr::relaunch_lost_entry(
                sid,
                spot,
                "its shell is gone",
                false,
                Some("claude --resume 0badf00d-1111-2222-3333-444455556666"),
                None,
            ));
        }
        app.host_noted.insert(format!("relaunch:{told}"));
        app.sweep_adopted_supervisor_lines();
        let mut keys: Vec<String> = app
            .messages
            .digest()
            .entries
            .iter()
            .map(|e| e.key.clone())
            .collect();
        keys.sort();
        let mut want = mr::relaunch_keys(told).to_vec();
        want.sort();
        assert_eq!(keys, want, "{told}");
    }
}

/// The loop's notice for Claude Code's lost login carried to the browser
/// sign-in (`policy/exception.rs`): `Now`, and no escalation of the loop's
/// carries it.
fn stuck_login_exception() -> Exception {
    Exception::of_notice(&notice::LOGIN_SIGN_IN, None, None, "the agent working")
}

/// A NOW NO ESCALATION OF THE LOOP'S CARRIES IS THE OWNER'S ASK ON THE GLASS
/// (the alert rule: the owner is needed now, aterm cannot settle it, and one
/// step changes it): a tab's lost login carried to the owner's sign-in is ONE
/// standing row, `harness.resume-ask.<tab>.<code>`, `Now`-class, in the
/// owner's four parts — the one step first, then what happened, what aterm
/// did and what follows — and NO line of the digest beside it: Settings > Messages lists
/// the ask once (the review's messages-double).
#[test]
fn a_now_no_escalation_carries_is_a_standing_ask_until_its_point_moves_on() {
    use aterm_messages::need::Need;
    let mut app = App::headless_for_test();
    let told = |app: &mut App, sid: &str, standing: Option<Exception>| {
        app.note_harness_host(HostNote::ResumeException {
            sid: sid.to_string(),
            agent: Program::Claude,
            standing,
        });
    };
    let asks = |app: &App| -> Vec<String> {
        app.messages
            .live_rows()
            .filter(|row| matches!(row.msg.need(), Need::Now(_)))
            .map(|row| row.msg.key.clone().unwrap_or_default())
            .collect()
    };
    let sign_in = |sid: &str| mr::notice_ask_key(sid, Some(notice::LOGIN_SIGN_IN.code));
    told(
        &mut app,
        "s-1",
        Some(resume_exception(ExceptionKind::Login)),
    );
    assert!(asks(&app).is_empty(), "the loop's escalation says it");
    assert!(app.messages.digest().is_empty());
    told(
        &mut app,
        "s-1",
        Some(resume_exception(ExceptionKind::Spend)),
    );
    assert!(asks(&app).is_empty());
    assert_eq!(app.messages.digest().len(), 1, "a line, no row");
    told(&mut app, "s-1", None);
    let _ = app.settle_messages(Instant::now() + DAY);
    assert!(app.messages.digest().is_empty());
    let e = stuck_login_exception();
    let n = e.notice.clone().expect("a notice");
    told(&mut app, "s-1", Some(e.clone()));
    let key = sign_in("s-1");
    assert_eq!(asks(&app), std::slice::from_ref(&key));
    let row = app.messages.live_by_key(&key).expect("the ask");
    assert!(matches!(row.msg.hold, aterm_messages::Hold::Standing(_)));
    assert_eq!(
        row.msg.detail,
        [
            e.action.clone(),
            e.why.clone(),
            n.did.clone(),
            n.expect.clone()
        ],
        "the step, what happened, what aterm did, what follows"
    );
    assert!(
        app.messages.digest().is_empty(),
        "the ask is listed once: {:?}",
        app.messages.digest().entries
    );
    let id = row.id;
    told(&mut app, "s-1", Some(stuck_login_exception()));
    assert_eq!(
        app.messages.live_by_key(&key).map(|row| row.id),
        Some(id),
        "the same row"
    );
    told(
        &mut app,
        "s-1",
        Some(Exception::of_notice(
            &notice::LOGIN_CANCELLED,
            None,
            None,
            "the agent working",
        )),
    );
    assert_eq!(
        asks(&app),
        [mr::notice_ask_key(
            "s-1",
            Some(notice::LOGIN_CANCELLED.code)
        )]
    );
    told(&mut app, "s-1", None);
    assert!(asks(&app).is_empty(), "the login back");
    told(&mut app, "s-2", Some(stuck_login_exception()));
    assert_eq!(asks(&app), [sign_in("s-2")]);
    told(
        &mut app,
        "s-2",
        Some(resume_exception(ExceptionKind::Login)),
    );
    assert!(asks(&app).is_empty(), "said once, by the escalation");
    let mut app = App::headless_for_test();
    app.post_message(mr::resume_ask(
        "s-9",
        &places()[0],
        &stuck_login_exception(),
    ));
    told(&mut app, "s-3", Some(stuck_login_exception()));
    app.ripen_exceptions(Instant::now());
    assert_eq!(asks(&app).len(), 2, "not yet settled");
    app.ripen_exceptions(Instant::now() + DAY);
    assert_eq!(asks(&app), [sign_in("s-3")]);
}

/// ONE LOSS, ONE ROW IN SETTINGS ▸ MESSAGES (the owner's capture, 2026-10-19:
/// "Claude's login expired in tab 1" listed three times, beside "Claude
/// sign-in cancelled"): a tab's login notices are one story.
#[test]
fn a_lost_logins_notices_are_one_row_in_their_latest_words_closed_fixed_once_it_is_back() {
    use aterm_messages::need::Need;
    let mut app = App::headless_for_test();
    let told = |app: &mut App, sid: &str, standing: Option<Exception>| {
        app.note_harness_host(HostNote::ResumeException {
            sid: sid.to_string(),
            agent: Program::Claude,
            standing,
        });
    };
    let notice = |text: &notice::NoticeText| {
        Exception::of_notice(
            text,
            Some("main"),
            text.whys.first().copied(),
            "the agent working",
        )
    };
    let back = |app: &mut App, sid: &str| {
        app.note_harness_host(HostNote::LoginBack {
            sid: sid.to_string(),
        });
    };
    let rows = |app: &App, sid: &str| -> Vec<aterm_messages::page::MessageView> {
        let base = mr::resume_ask_key(sid);
        app.messages_state()
            .entries
            .into_iter()
            .filter(|row| {
                aterm_messages::MessageId::from_raw(row.id)
                    .and_then(|id| app.messages.log().get(id))
                    .and_then(|rec| rec.key.as_deref())
                    .is_some_and(|key| key.starts_with(&base))
            })
            .collect()
    };
    let asks = |app: &App| -> Vec<String> {
        app.messages
            .live_rows()
            .filter(|row| matches!(row.msg.need(), Need::Now(_)))
            .map(|row| row.msg.key.clone().unwrap_or_default())
            .collect()
    };
    let step_of =
        |text: &notice::NoticeText| text.fill(Some("main"), text.whys.first().copied()).todo;

    told(&mut app, "s-1", Some(notice(&notice::LOGIN_SIGN_IN)));
    told(&mut app, "s-1", Some(notice(&notice::LOGIN_CHOOSE)));
    let row = rows(&app, "s-1");
    assert_eq!(row.len(), 1, "{row:?}");
    assert_eq!(
        row[0].detail[0],
        step_of(&notice::LOGIN_CHOOSE),
        "its latest words"
    );
    assert_eq!(
        asks(&app),
        [mr::notice_ask_key("s-1", Some("ch"))],
        "one live ask"
    );
    let read = app
        .messages
        .live_by_key(&asks(&app)[0])
        .expect("the ask")
        .id;
    assert!(app.messages.mark_seen(read, Instant::now()));
    assert!(asks(&app).is_empty(), "read: off the band");
    told(&mut app, "s-1", Some(notice(&notice::LOGIN_SIGN_IN)));
    assert_eq!(
        asks(&app),
        [mr::notice_ask_key("s-1", Some("si"))],
        "news again"
    );
    told(&mut app, "s-1", None);
    assert!(asks(&app).is_empty());
    let row = rows(&app, "s-1");
    assert_eq!((row.len(), row[0].state), (1, "withdrawn"), "{row:?}");
    told(&mut app, "s-1", Some(notice(&notice::LOGIN_CANCELLED)));
    let row = rows(&app, "s-1");
    assert_eq!(row.len(), 1, "{row:?}");
    assert_eq!(row[0].state, "live");
    let cancelled_words = notice::LOGIN_CANCELLED.fill(Some("main"), None);
    assert_eq!(
        row[0].detail,
        [
            cancelled_words.todo,
            cancelled_words.happened,
            cancelled_words.did,
            cancelled_words.expect
        ],
        "the step, what happened, what aterm did, what follows"
    );
    let cancelled = row[0].title.clone();
    told(&mut app, "s-1", None);
    back(&mut app, "s-1");
    let row = rows(&app, "s-1");
    assert_eq!(row.len(), 1, "{row:?}");
    assert_eq!(row[0].state, "resolved-ok");
    assert_eq!(row[0].severity, "success");
    assert!(row[0].title.starts_with(notice::BACK), "{:?}", row[0].title);
    assert_eq!(row[0].detail[0], cancelled, "the state it ended in");
    told(&mut app, "s-1", Some(notice(&notice::LOGIN_SIGN_IN)));
    assert_eq!(asks(&app), [mr::notice_ask_key("s-1", Some("si"))]);
    assert_eq!(rows(&app, "s-1").len(), 2, "a new loss, a new row");
    back(&mut app, "s-1");
    assert!(asks(&app).is_empty(), "resolved off the band");
    let row = rows(&app, "s-1");
    assert_eq!(
        (row.len(), row[0].state, row[0].severity),
        (2, "resolved-ok", "success")
    );

    told(&mut app, "s-2", Some(notice(&notice::LOGIN_SIGN_IN)));
    told(&mut app, "s-2", None);
    let row = rows(&app, "s-2");
    assert_eq!(
        (row.len(), row[0].state, row[0].severity),
        (1, "withdrawn", "warn"),
        "its end alone claims no fix"
    );
    let before = app.messages_state().entries;
    back(&mut app, "s-3");
    assert_eq!(
        app.messages_state().entries,
        before,
        "no story: nothing moves"
    );
}

/// Tab `sid`'s loop tells `text`'s notice (`None`: its point moved on).
fn tell_notice(app: &mut App, sid: &str, text: Option<&notice::NoticeText>) {
    app.note_harness_host(HostNote::ResumeException {
        sid: sid.to_string(),
        agent: Program::Claude,
        standing: text.map(|text| {
            Exception::of_notice(
                text,
                Some("main"),
                text.whys.first().copied(),
                "the agent working",
            )
        }),
    });
}

/// The keys of the live asks on the band.
fn notice_asks(app: &App) -> Vec<String> {
    app.messages
        .live_rows()
        .filter(|row| matches!(row.msg.need(), aterm_messages::need::Need::Now(_)))
        .map(|row| row.msg.key.clone().unwrap_or_default())
        .collect()
}

/// The ids of the page's rows of tab `sid`'s notices, newest first.
fn notice_rows(app: &App, sid: &str) -> Vec<aterm_messages::MessageId> {
    let base = mr::resume_ask_key(sid);
    app.messages_state()
        .entries
        .into_iter()
        .filter_map(|row| aterm_messages::MessageId::from_raw(row.id))
        .filter(|id| {
            app.messages
                .log()
                .get(*id)
                .and_then(|rec| rec.key.as_deref())
                .is_some_and(|key| key.starts_with(&base))
        })
        .collect()
}

/// A NOTICE THE OWNER READ STAYS QUIET WHILE ITS LOSS STANDS — the commit's
/// own rule for the per-notice keys — even after the loss's next notice told
/// its story on (the read row replaced, so the page lists the loss once): the
/// sign-in read, then cancelled, then the sign-in again is no news, and the
/// cancelled ask leaves the band as the sign-in's quiet return withdraws it,
/// as on a tab whose read row was never replaced.
#[test]
fn a_read_notice_stays_quiet_after_the_next_notice_tells_its_story_on() {
    let mut app = App::headless_for_test();
    let si = mr::notice_ask_key("s-1", Some("si"));
    let ca = mr::notice_ask_key("s-1", Some("ca"));
    tell_notice(&mut app, "s-1", Some(&notice::LOGIN_SIGN_IN));
    let read = app.messages.live_by_key(&si).expect("the ask").id;
    assert!(
        app.messages.mark_seen(read, Instant::now()),
        "the owner reads it"
    );
    tell_notice(&mut app, "s-1", Some(&notice::LOGIN_CANCELLED));
    assert_eq!(
        notice_asks(&app),
        std::slice::from_ref(&ca),
        "a new notice is news"
    );
    assert_eq!(notice_rows(&app, "s-1").len(), 1, "one loss, one row");
    tell_notice(&mut app, "s-1", Some(&notice::LOGIN_SIGN_IN));
    assert!(
        notice_asks(&app).is_empty(),
        "the sign-in the owner read is not raised again: {:?}",
        notice_asks(&app)
    );
    assert_eq!(notice_rows(&app, "s-1").len(), 1, "still one row");
    tell_notice(&mut app, "s-1", Some(&notice::LOGIN_CANCELLED));
    assert_eq!(notice_asks(&app), [ca]);
    tell_notice(&mut app, "s-1", None);
    app.note_harness_host(HostNote::LoginBack {
        sid: "s-1".to_string(),
    });
    tell_notice(&mut app, "s-1", Some(&notice::LOGIN_SIGN_IN));
    assert_eq!(notice_asks(&app), [si], "the next loss is news");
    assert_eq!(notice_rows(&app, "s-1").len(), 2, "a row of its own");
}

/// A LOSS WHOSE END NO LOOP TOLD IS NEVER SWALLOWED BY THE NEXT: a renewed
/// login's held move (`renewed-draft`) that then landed under a loop that
/// ended, say, tells no login back, and the tab's next loss a day later is a
/// loss of its own — its own row beside the earlier one, which keeps its
/// words, and the login back closes the new one alone.
#[test]
fn a_loss_with_no_word_for_its_span_is_not_swallowed_by_the_next() {
    use crate::exceptions_host::NOTICE_STORY_SPAN;
    let span = u64::try_from(NOTICE_STORY_SPAN.as_millis()).unwrap();
    let mut app = App::headless_for_test();
    let renewed = |app: &mut App, sid: &str, age: u64| {
        tell_notice(app, sid, Some(&notice::RENEWED_DRAFT));
        tell_notice(app, sid, None);
        let story = app.resume_stories.get_mut(sid).expect("its story");
        story.last = story.last.saturating_sub(age);
        notice_rows(app, sid)
    };
    let first = renewed(&mut app, "s-1", span);
    assert_eq!(first.len(), 1);
    tell_notice(&mut app, "s-1", Some(&notice::LOGIN_SIGN_IN));
    let rows = notice_rows(&app, "s-1");
    assert_eq!(rows.len(), 2, "a loss of its own: {rows:?}");
    assert_eq!(rows[1], first[0], "the earlier loss keeps its row");
    assert_eq!(
        app.messages.log().get(first[0]).and_then(|r| r.retired()),
        Some(&aterm_messages::Retired::Withdrawn),
        "in its own words, never replaced"
    );
    tell_notice(&mut app, "s-1", None);
    app.note_harness_host(HostNote::LoginBack {
        sid: "s-1".to_string(),
    });
    let state = |id| {
        app.messages
            .log()
            .get(id)
            .and_then(|r| r.retired())
            .cloned()
    };
    assert_eq!(
        (state(rows[0]), state(rows[1])),
        (
            Some(aterm_messages::Retired::Resolved(
                aterm_messages::Outcome::Ok
            )),
            Some(aterm_messages::Retired::Withdrawn)
        ),
        "the login back closes the loss it ends"
    );
    let first = renewed(&mut app, "s-2", span - 60_000);
    tell_notice(&mut app, "s-2", Some(&notice::LOGIN_SIGN_IN));
    let rows = notice_rows(&app, "s-2");
    assert_eq!(rows.len(), 1, "one story: {rows:?}");
    assert_ne!(rows[0], first[0], "in its latest words");
}

/// ANOTHER EXCEPTION OF THE TAB'S IS NO WORD OF ITS LOSS: a stall told a day
/// after a loss whose end no loop told (its ask long withdrawn) does not keep
/// that loss's story open, so the tab's next loss is a row of its own beside
/// it, as with no tell at all.
#[test]
fn another_exception_of_the_tab_keeps_no_loss_open() {
    use crate::exceptions_host::NOTICE_STORY_SPAN;
    let span = u64::try_from(NOTICE_STORY_SPAN.as_millis()).unwrap();
    for ask_stood in [false, true] {
        let mut app = App::headless_for_test();
        tell_notice(&mut app, "s-1", Some(&notice::RENEWED_DRAFT));
        if !ask_stood {
            tell_notice(&mut app, "s-1", None);
        }
        let story = app.resume_stories.get_mut("s-1").expect("its story");
        story.last = story.last.saturating_sub(span);
        app.note_harness_host(HostNote::ResumeException {
            sid: "s-1".to_string(),
            agent: Program::Claude,
            standing: Some(Exception::stalled()),
        });
        tell_notice(&mut app, "s-1", Some(&notice::LOGIN_SIGN_IN));
        let rows = notice_rows(&app, "s-1");
        if ask_stood {
            assert_eq!(rows.len(), 1, "the ask's end was its word: {rows:?}");
        } else {
            assert_eq!(rows.len(), 2, "a stall is no word of the loss: {rows:?}");
        }
    }
}

/// A LOSS AN EARLIER PROCESS CARRIED HERE CLOSES FIXED WHEN THIS ONE SEES THE
/// LOGIN BACK: a seamless update carries a tab's standing notice, and the
/// successor's own loop tells the same notice in the same words (a duplicate
/// of the carried row, so the story is told in it), then its end, then the
/// login back: the carried row's record reads `✓` in the notice's fix words.
#[test]
fn a_carried_notice_told_again_closes_fixed_when_the_login_is_back() {
    for back in [true, false] {
        let mut parent = App::headless_for_test();
        tell_notice(&mut parent, "s-1", Some(&notice::LOGIN_SIGN_IN));
        let _ = parent
            .messages
            .settle(Instant::now() + aterm_messages::PROGRESS_GRACE, true);
        let carry = parent.messages.carried();
        let mut app = App::headless_for_test();
        app.messages.seed_carried(
            &carry,
            crate::messages_host::wall_stamp_now(),
            Instant::now(),
        );
        let key = mr::notice_ask_key("s-1", Some("si"));
        let carried = app.messages.live_by_key(&key).expect("carried").id;
        tell_notice(&mut app, "s-1", Some(&notice::LOGIN_SIGN_IN));
        assert_eq!(
            app.messages.live_by_key(&key).map(|row| row.id),
            Some(carried),
            "premise: the same notice is the carried row"
        );
        tell_notice(&mut app, "s-1", None);
        if back {
            app.note_harness_host(HostNote::LoginBack {
                sid: "s-1".to_string(),
            });
        }
        let rec = app.messages.log().get(carried).expect("its record");
        if back {
            assert_eq!(
                rec.retired(),
                Some(&aterm_messages::Retired::Resolved(
                    aterm_messages::Outcome::Ok
                )),
                "{rec:?}"
            );
            assert!(rec.title.starts_with(notice::BACK), "{:?}", rec.title);
        } else {
            assert_eq!(
                (rec.retired(), rec.severity),
                (
                    Some(&aterm_messages::Retired::Withdrawn),
                    aterm_messages::Severity::Warn
                ),
                "its end alone claims no fix"
            );
        }
    }
}

/// A USAGE LIMIT'S RECORD CLOSES AS THE SWITCH IT WAS, NEVER AS A LOGIN BACK
/// (the review of the login carry, r8c-7: once a move onto another account
/// landed, every usage limit's record in Settings ▸ Messages read `Claude's
/// login is back`, though no login was lost): the host's login-back note
/// closes the story in its notice's own fix words, its row still standing or
/// already withdrawn.
#[test]
fn a_usage_limits_record_closes_as_a_switch_never_as_a_login_back() {
    for (text, words) in [
        (&notice::LIMIT_RENEW, notice::SWITCHED),
        (&notice::LIMIT_ROSTER, notice::SWITCHED),
        (&notice::LOGIN_SIGN_IN, notice::BACK),
    ] {
        for withdrawn in [false, true] {
            let mut app = App::headless_for_test();
            tell_notice(&mut app, "s-1", Some(text));
            let key = mr::notice_ask_key("s-1", Some(text.code));
            let id = app.messages.live_by_key(&key).expect("the ask").id;
            if withdrawn {
                tell_notice(&mut app, "s-1", None);
            }
            app.note_harness_host(HostNote::LoginBack {
                sid: "s-1".to_string(),
            });
            let rec = app.messages.log().get(id).expect("its record");
            assert_eq!(
                rec.retired(),
                Some(&aterm_messages::Retired::Resolved(
                    aterm_messages::Outcome::Ok
                )),
                "{}: {rec:?}",
                text.id
            );
            assert!(
                rec.title.starts_with(words),
                "{} (withdrawn {withdrawn}): {:?}",
                text.id,
                rec.title
            );
            assert_eq!(
                rec.title.starts_with(notice::BACK),
                words == notice::BACK,
                "{}: {:?}",
                text.id,
                rec.title
            );
        }
    }
}

/// A USAGE LIMIT THAT ENDED BY ITSELF CLOSES AS NO SWITCH (the review of
/// the login carry, r9h-3): the row withdrawn and the story kept whatever
/// ended the ask, a login back told later — a lost login's, an account move
/// days on — closed the limit's record `Claude switched accounts`, though
/// its reset had ended it.
#[test]
fn a_usage_limit_that_ended_by_itself_closes_as_no_switch() {
    use crate::exceptions_host::NOTICE_STORY_SPAN;
    let span = u64::try_from(NOTICE_STORY_SPAN.as_millis()).unwrap();
    for end in ["moved-on", "limit-over", "past-span"] {
        let mut app = App::headless_for_test();
        tell_notice(&mut app, "s-1", Some(&notice::LIMIT_ROSTER));
        let key = mr::notice_ask_key("s-1", Some(notice::LIMIT_ROSTER.code));
        let id = app.messages.live_by_key(&key).expect("the ask").id;
        if end == "limit-over" {
            app.note_harness_host(HostNote::LimitOver {
                sid: "s-1".to_string(),
            });
        }
        tell_notice(&mut app, "s-1", None);
        if end == "past-span" {
            let story = app.resume_stories.get_mut("s-1").expect("its story");
            story.last = story.last.saturating_sub(span);
        }
        app.note_harness_host(HostNote::LoginBack {
            sid: "s-1".to_string(),
        });
        let rec = app.messages.log().get(id).expect("its record");
        if end == "moved-on" {
            assert_eq!(
                rec.retired(),
                Some(&aterm_messages::Retired::Resolved(
                    aterm_messages::Outcome::Ok
                )),
                "{end}: {rec:?}"
            );
            assert!(rec.title.starts_with(notice::SWITCHED), "{end}: {rec:?}");
        } else {
            assert_eq!(
                rec.retired(),
                Some(&aterm_messages::Retired::Withdrawn),
                "{end}: {rec:?}"
            );
            assert!(!rec.title.starts_with(notice::SWITCHED), "{end}: {rec:?}");
        }
        assert!(!app.resume_stories.contains_key("s-1"), "{end}: over");
    }
}

/// A resume line an earlier process left open, which no loop here tells of, ends with the first
/// whole answer after the settle.
#[test]
fn a_resume_line_an_earlier_process_left_open_ends_after_the_settle() {
    let mut app = App::headless_for_test();
    let stale = mr::resume_exceptions(&[(
        "s-9".to_string(),
        resume_exception(ExceptionKind::Spend),
        Program::Claude,
        places()[0].clone(),
    )]);
    app.except_replace(Family::Resume, &stale);
    assert_eq!(app.messages.digest().len(), 1, "adopted from the log");
    app.ripen_exceptions(Instant::now());
    assert!(!app.resume_swept);
    assert_eq!(app.messages.digest().len(), 1);
    app.ripen_exceptions(Instant::now() + DAY);
    assert!(app.resume_swept);
    let due = app.messages_deadline().expect("the clock is armed");
    let _ = app.settle_messages(due + Duration::from_secs(1));
    assert!(app.messages.digest().is_empty());
    let mut app = App::headless_for_test();
    app.note_harness_host(HostNote::ResumeException {
        sid: "s-9".to_string(),
        agent: Program::Claude,
        standing: Some(resume_exception(ExceptionKind::Spend)),
    });
    app.ripen_exceptions(Instant::now() + DAY);
    let _ = app.settle_messages(Instant::now() + DAY + DAY);
    assert_eq!(app.messages.digest().len(), 1, "still told of");
}

/// A LIMIT'S LINE NAMES THE AGENT ITS TABS RUN, OR NONE: a limit held unread
/// past its reset (`Exception::unread_limit`) on a screen no reader names is
/// titled `Limit still holds in tab 2`, tabs running different agents
/// likewise, and a spend or bucket line `… limit reached`; one agent's tabs
/// are named where the glass's six words hold the name and the place
/// (`Codex`), the name dropped before the place where they do not (`Gemini
/// CLI`, `Antigravity CLI`). Measured before: every one of them said
/// `Claude`. NEGATIVE CONTROLS: Claude Code's tabs keep `Claude`, and
/// `/goal`'s line, Claude Code's alone, keeps its words on any agent.
#[test]
fn a_limits_line_names_the_agent_its_tabs_run() {
    let spot = |n: u16| mr::UpgradeSpot {
        place: format!("in tab {n}"),
        nav: None,
    };
    let title = |facts: &[(&str, u16, ExceptionKind, Program)]| {
        let facts: Vec<_> = facts
            .iter()
            .map(|&(sid, n, kind, agent)| {
                let exception = match kind {
                    ExceptionKind::Futile => Exception::unread_limit(),
                    _ => resume_exception(kind),
                };
                (sid.to_string(), exception, agent, spot(n))
            })
            .collect();
        let entries = mr::resume_exceptions(&facts);
        assert_eq!(entries.len(), 1);
        entries[0].message().expect("passes the door").title
    };
    let futile = ExceptionKind::Futile;
    assert_eq!(
        title(&[("s-2", 2, futile, Program::Generic)]),
        "Limit still holds in tab 2"
    );
    assert_eq!(
        title(&[("s-2", 2, futile, Program::Codex)]),
        "Limit still holds Codex in tab 2"
    );
    assert_eq!(
        title(&[("s-2", 2, futile, Program::Gemini)]),
        "Limit still holds in tab 2",
        "`… Gemini CLI in tab 2` is seven words"
    );
    assert_eq!(
        title(&[("s-2", 2, ExceptionKind::Spend, Program::Codex)]),
        "Account limit stops Codex in tab 2"
    );
    assert_eq!(
        title(&[("s-2", 2, ExceptionKind::Spend, Program::Generic)]),
        "Account limit reached in tab 2"
    );
    assert_eq!(
        title(&[("s-2", 2, ExceptionKind::Bucket, Program::Agy)]),
        "Model limit reached in tab 2"
    );
    assert_eq!(
        title(&[
            ("s-1", 1, futile, Program::Claude),
            ("s-3", 3, futile, Program::Muse),
        ]),
        "Limit still holds in 2 tabs"
    );
    // CONTROLS: Claude Code's tabs say Claude; `/goal` is Claude Code's.
    assert_eq!(
        title(&[("s-2", 2, futile, Program::Claude)]),
        "Limit still holds Claude in tab 2"
    );
    assert_eq!(
        title(&[("s-2", 2, ExceptionKind::Spend, Program::Claude)]),
        "Account limit stops Claude in tab 2"
    );
    assert_eq!(
        title(&[
            ("s-1", 1, futile, Program::Claude),
            ("s-3", 3, futile, Program::Claude),
        ]),
        "Limit still holds Claude in 2 tabs"
    );
    assert_eq!(
        title(&[("s-2", 2, ExceptionKind::GoalCleared, Program::Generic)]),
        "Claude lost its goal in tab 2"
    );
}

/// The pure half: the same kind in two tabs is one entry whose title counts the tabs and whose
/// detail names them, one tab keeps its own place and its `Show tab` button, and a lost login
/// (`Now`) is no entry at all.
#[test]
fn a_kind_in_two_tabs_is_one_entry_that_names_them() {
    let spot = |n: u16| mr::UpgradeSpot {
        place: format!("in tab {n}"),
        nav: Some(aterm_messages::Intent::ShowTab { tab: n, window: 1 }),
    };
    let fact = |sid: &str, n: u16, kind| {
        (
            sid.to_string(),
            resume_exception(kind),
            Program::Claude,
            spot(n),
        )
    };
    let two = mr::resume_exceptions(&[
        fact("s-1", 1, ExceptionKind::Spend),
        fact("s-3", 3, ExceptionKind::Spend),
        fact("s-2", 2, ExceptionKind::Login),
    ]);
    assert_eq!(two.len(), 1, "the login is not an entry");
    let msg = two[0].message().expect("passes the door");
    assert!(msg.title.contains("2 tabs"), "{}", msg.title);
    assert!(
        msg.detail.iter().any(|l| l.contains("tab 1, tab 3")),
        "{:?}",
        msg.detail
    );
    let one = mr::resume_exceptions(&[fact("s-1", 1, ExceptionKind::GoalCleared)]);
    let msg = one[0].message().expect("passes the door");
    assert!(
        msg.actions
            .iter()
            .any(|a| matches!(a, aterm_messages::Intent::ShowTab { tab: 1, .. })),
        "{:?}",
        msg.actions
    );
    let stalled = mr::resume_exceptions(&[fact("s-2", 2, ExceptionKind::Stalled)]);
    assert_eq!(stalled[0].key(), "harness.resume.s-2.stalled");
    let dialog = mr::resume_exceptions(&[fact("s-2", 2, ExceptionKind::Dialog)]);
    assert_eq!(dialog[0].key(), "harness.resume.s-2.dialog");
    let both = mr::resume_exceptions(&[
        fact("s-1", 1, ExceptionKind::Context),
        fact("s-3", 3, ExceptionKind::Context),
        fact("s-2", 2, ExceptionKind::Futile),
    ]);
    let mut keys: Vec<String> = both.iter().map(|e| e.key()).collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "harness.resume.futile",
            "harness.resume.s-1.context",
            "harness.resume.s-3.context"
        ]
    );
    let entry = both
        .iter()
        .find(|e| e.key() == "harness.resume.s-3.context")
        .expect("its own entry");
    assert_eq!(entry.log_line_text(), Some("resume context sid=s-3"));
    let msg = entry.message().expect("passes the door");
    assert_eq!(msg.severity, aterm_messages::Severity::Info);
    assert!(
        msg.actions
            .iter()
            .any(|a| matches!(a, aterm_messages::Intent::ShowTab { tab: 3, .. })),
        "{:?}",
        msg.actions
    );
}

/// The stall canary's 90-minute rung is one record and one `aterm.log` error per episode, never
/// a digest line or a tab mark (the digest waits for the third hour,
/// [`ExceptionKind::Stalled`]); its end lets the next episode be told.
#[test]
fn a_stall_at_ninety_minutes_is_a_record_once_and_no_line() {
    let mut app = App::headless_for_test();
    let told = |app: &mut App, minutes: Option<u64>| {
        app.note_harness_host(HostNote::ResumeUnread {
            sid: "s-1".to_string(),
            agent: Program::Claude,
            stall: minutes.map(|minutes| StallSeen::Unread { minutes }),
        });
    };
    told(&mut app, Some(90));
    told(&mut app, Some(91));
    assert!(app.messages.digest().is_empty(), "no line at 90 minutes");
    assert!(app.host_noted.contains("stalled:s-1"));
    let recorded = |app: &App| {
        app.messages
            .log()
            .records()
            .filter(|r| !r.later && r.key.as_deref() == Some("harness.host.stalled.s-1"))
            .count()
    };
    assert_eq!(recorded(&app), 1, "one record per episode");
    told(&mut app, None);
    assert!(!app.host_noted.contains("stalled:s-1"));
    told(&mut app, Some(95));
    assert_eq!(recorded(&app), 2, "a new episode is told again");
}

/// A STALL ON THE AGENT'S OWN BACKGROUND WORK SPEAKS OF THAT AGENT AND ITS
/// WORK (agy's and Muse Code's ladder told the
/// window "Claude has stalled" over "Claude's own title has said idle … over
/// a screen this build does not recognise", where no title was read, the
/// screen was read, and the agent was not Claude).
#[test]
fn a_stall_on_an_agents_own_work_names_that_agent_and_reads_no_title() {
    let record = |agent: Program, stall: StallSeen| {
        let mut app = App::headless_for_test();
        app.note_harness_host(HostNote::ResumeUnread {
            sid: "s-1".to_string(),
            agent,
            stall: Some(stall),
        });
        let r = app
            .messages
            .log()
            .records()
            .find(|r| r.key.as_deref() == Some("harness.host.stalled.s-1"))
            .expect("the record");
        (r.title.clone(), r.detail.join(" "))
    };
    for (agent, name) in [
        (Program::Muse, "Muse Code"),
        (Program::Agy, "Antigravity CLI"),
    ] {
        let (title, detail) = record(agent, StallSeen::OwnWork { minutes: 90 });
        assert!(title.starts_with(&format!("{name} has stalled")), "{title}");
        assert!(
            detail.contains(&format!("{name}'s turn ended"))
                && detail.contains("background work has run 90 min under words that stood still"),
            "{detail}"
        );
        for claim in ["Claude", "title", "does not recognise"] {
            assert!(
                !title.contains(claim) && !detail.contains(claim),
                "{claim}: {title} / {detail}"
            );
        }
    }
    let (title, detail) = record(Program::Claude, StallSeen::Unread { minutes: 90 });
    assert!(title.starts_with("Claude has stalled"), "{title}");
    assert!(
        detail.contains("Claude's own title has said idle for 90 min"),
        "{detail}"
    );
    let spot = mr::UpgradeSpot {
        place: "in tab 2".to_string(),
        nav: None,
    };
    let title = |exception: Exception, agent: Program| {
        let entries = mr::resume_exceptions(&[("s-2".to_string(), exception, agent, spot.clone())]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key(), "harness.resume.s-2.stalled");
        entries[0].message().expect("passes the door").title
    };
    assert_eq!(
        title(Exception::stalled_on_work(), Program::Muse),
        "Muse Code in tab 2 has stalled"
    );
    assert_eq!(
        title(Exception::stalled_on_work(), Program::Agy),
        "Antigravity CLI in tab 2 has stalled"
    );
    assert_eq!(
        title(Exception::stalled(), Program::Claude),
        "Claude in tab 2 has stalled"
    );
    let mut app = App::headless_for_test();
    app.note_harness_host(HostNote::ResumeException {
        sid: "s-3".to_string(),
        agent: Program::Muse,
        standing: Some(Exception::stalled_on_work()),
    });
    let digest = app.messages.digest();
    let line = digest
        .entries
        .iter()
        .find(|e| e.key == "harness.resume.s-3.stalled")
        .expect("the tab's line");
    assert!(line.title.starts_with("Muse Code"), "{}", line.title);
}

/// A STALL ON A BACKGROUND CALL THAT MADE NO PROGRESS speaks of that call (the
/// hung-call design's R1: the canary said "a screen this build does not
/// recognise" over a test asleep on its own mutex for 26 h).
#[test]
fn a_stall_on_a_call_that_made_no_progress_names_its_pid_and_passes_the_door() {
    use aterm_agent::supervise::BackgroundCall;
    let mut app = App::headless_for_test();
    app.note_harness_host(HostNote::ResumeUnread {
        sid: "s-1".to_string(),
        agent: Program::Claude,
        stall: Some(StallSeen::NoProgress {
            minutes: 31,
            pid: 27098,
        }),
    });
    let r = app
        .messages
        .log()
        .records()
        .find(|r| r.key.as_deref() == Some("harness.host.stalled.s-1"))
        .expect("the record");
    let detail = r.detail.join(" ");
    assert!(r.title.starts_with("Claude has stalled"), "{}", r.title);
    assert!(
        detail.contains("pid 27098") && detail.contains("31 min"),
        "{detail}"
    );
    let call = BackgroundCall {
        pid: 27098,
        pgid: 27090,
        exe: "aterm_gui-e0964a".to_string(),
        task: Some("bplle5dxj".to_string()),
        quiet_ms: 61 * 60_000,
        wait: None,
    };
    let spot = mr::UpgradeSpot {
        place: "in tab 2".to_string(),
        nav: None,
    };
    let entries = mr::resume_exceptions(&[(
        "s-2".to_string(),
        Exception::no_progress(&call),
        Program::Claude,
        spot,
    )]);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].key(), "harness.resume.s-2.stalled");
    let line = entries[0].message().expect("passes the door");
    assert_eq!(line.title, "Claude in tab 2 has stalled");
}

/// A PALETTE SET THE OWNER'S CONFIG REFUSED is one digest line for every tab,
/// naming the setting: never a band row, and a session's later refusals (or
/// another session's first) add nothing.
#[test]
fn a_refused_palette_set_is_one_line_naming_the_setting() {
    let mut app = App::headless_for_test();
    assert!(app.messages.digest().is_empty());
    app.note_palette_refused();
    let rev = app.messages.digest_revision();
    app.note_palette_refused();
    assert_eq!(
        app.messages.digest_revision(),
        rev,
        "a second refusal moves nothing"
    );
    let digest = app.messages.digest();
    assert_eq!(digest.entries.len(), 1);
    assert_eq!(digest.entries[0].key, "terminal.palette-refused");
    let msg = mr::palette_refused_entry()
        .message()
        .expect("the entry is well formed");
    let words = format!("{msg:?}");
    assert!(
        words.contains("allow_palette_reconfigure"),
        "the line names the setting that refused it: {words}"
    );
}

/// TURNING PALETTE SETS BACK ON ends the refusal line: its one action is
/// done.
#[test]
fn turning_palette_sets_on_clears_the_refusal_line() {
    fn apply(app: &mut App, dir: &std::path::Path, text: &str) {
        let path = dir.join("aterm.toml");
        std::fs::write(&path, text).expect("write config");
        let observation =
            crate::native_config_service::VersionedConfigService::observe_path(&path, false)
                .expect("observe");
        let config: crate::app_config::Config = aterm_toml::from_str(text).expect("valid toml");
        app.apply_prepared_config_generation(
            crate::native_font_catalog::PreparedConfigGeneration {
                observation,
                values: std::collections::BTreeMap::new(),
                disk_faults: std::sync::Arc::default(),
                assets: config.resolve_asset_catalog(),
                path_feed_fps: config.path_feed_fingerprints(),
                sparkle: config.prepare_sparkle_runtime(),
                fonts: None,
                warnings: Vec::new(),
                font_refusals: Vec::new(),
                reason: crate::app_colorscheme::ThemeChange::ConfigReload,
                config,
            },
        );
    }
    let refused = |app: &App| {
        app.messages
            .digest()
            .entries
            .iter()
            .any(|e| e.key == mr::palette_refused_key())
    };
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-palette-refused")
        .tempdir()
        .expect("scratch dir");
    let mut app = App::headless_for_test();
    apply(
        &mut app,
        scratch.path(),
        "allow_palette_reconfigure = false\n",
    );
    app.note_palette_refused();
    assert!(refused(&app), "precondition: the line is filed");

    let unrelated = "allow_palette_reconfigure = false\nfaint_opacity = 0.4\n";
    apply(&mut app, scratch.path(), unrelated);
    assert!(refused(&app), "still refused: the line stays");

    apply(
        &mut app,
        scratch.path(),
        "allow_palette_reconfigure = true\n",
    );
    assert!(!refused(&app), "allowed again: the line ends");
}

/// A repaint that gave up is one `render.<tab>.unrepaired` line ("Tab N may be one row off"),
/// with no tab mark, band row or re-grid, and only while its epoch is the tab's open, at-risk
/// one: a give-up for a healed epoch, or a session the store does not know, raises nothing.
#[test]
fn a_repaint_that_gave_up_is_one_line_and_none_for_a_healed_epoch() {
    let mut app = App::headless_for_test();
    let message = message_regrids();
    app.note_render_unrepaired(404, 7, GiveUp::NoAnswer);
    assert!(app.messages.digest().is_empty());
    let session = app.pool.iter().map(|s| s.id).next().expect("session 0");
    let rev = app.messages.digest_revision();
    app.note_render_unrepaired(session, 7, GiveUp::NoAnswer);
    assert!(
        app.messages.digest().is_empty(),
        "epoch 7 is not at risk: no line"
    );
    assert_eq!(app.messages.digest_revision(), rev, "nothing moved");
    assert_eq!(message_regrids(), message);
}

/// A pool session the store knows (so `render.<sid>.unrepaired` can name it) whose timeline
/// holds an open, at-risk render epoch (a restored engine's synthetic epoch), and its sid.
fn session_with_open_epoch(app: &mut App, local: u64) -> (u64, String) {
    use crate::resize_ledger::EngineProbe;
    let session = crate::stub_session(local);
    let probe = EngineProbe {
        lifetime: 1,
        debt: aterm_core::terminal::RenderDebt::of_rows(1, 0),
        geom: (24, 80),
        ..EngineProbe::default()
    };
    session
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .evaluate_resizes(probe, 1_000);
    app.pool.insert(session);
    let handle = crate::session_store::test_handle(local);
    let sid = handle.sid.as_str().to_string();
    app.store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .register(handle);
    let open = app.pool.get(local).is_some_and(|s| {
        s.ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .resize_epoch_at_risk(1)
    });
    assert!(open, "the fixture's epoch 1 is open and at risk");
    (local, sid)
}

/// A render line stands through every nudge step while its epoch is at risk.
#[test]
fn a_render_line_stands_while_its_epoch_is_at_risk_and_ends_when_it_heals() {
    let mut app = App::headless_for_test();
    let (session, sid) = session_with_open_epoch(&mut app, 41);
    app.note_render_unrepaired(session, 1, GiveUp::NoAnswer);
    assert_eq!(app.messages.digest().len(), 1);
    for _ in 0..3 {
        app.check_render_repairs();
    }
    assert_eq!(
        app.messages.digest().len(),
        1,
        "still at risk: the line stands through every nudge step"
    );
    assert_eq!(
        app.messages.digest().entries[0].key,
        mr::render_unrepaired_key(&sid)
    );
    app.pool
        .get(session)
        .expect("the session")
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .evaluate_resizes(
            crate::resize_ledger::EngineProbe {
                lifetime: 1,
                full_clears: 1,
                geom: (24, 80),
                ..crate::resize_ledger::EngineProbe::default()
            },
            2_000,
        );
    app.check_render_repairs();
    assert!(app.messages.digest().is_empty(), "healed: the line ends");
}

/// An adoption nudge (a blank agent tab) names no epoch of the ledger, so it is a log line,
/// never a digest line that would flash for one loop turn.
#[test]
fn an_adoption_nudge_that_gave_up_raises_no_digest_line() {
    let mut app = App::headless_for_test();
    let (session, _) = session_with_open_epoch(&mut app, 42);
    let rev = app.messages.digest_revision();
    app.note_render_unrepaired(session, crate::app_nudge::ADOPTION_EPOCH, GiveUp::NoAnswer);
    app.check_render_repairs();
    assert!(app.messages.digest().is_empty());
    assert_eq!(app.messages.digest_revision(), rev, "nothing moved");
}

/// Every event family's end ends a line an earlier process raised, adopted from the log with
/// none of the in-memory facts (`supervisor_failing`, `host_noted`, `render_unrepaired`).
#[test]
fn the_ends_of_the_event_families_end_lines_an_earlier_process_raised() {
    let mut app = App::headless_for_test();
    let spot = &places()[0];
    let (session, sid) = session_with_open_epoch(&mut app, 43);
    app.except(&mr::disk_low_entry(7, 20, &[]));
    app.except(&mr::disk_settings_entry(&[
        "disk.apply is not read".to_string()
    ]));
    app.except(&mr::upgrade_cannot_run_entry("busy:state-unwritable", None));
    app.except(&mr::supervisor_failing_entry(&sid, spot, "boom"));
    app.except(&mr::survey_entry(&sid, spot, "a rating card is open"));
    app.except(&mr::render_unrepaired_entry(&sid, spot, GiveUp::NoAnswer));
    assert_eq!(app.messages.digest().len(), 6);
    assert!(app.host_noted.is_empty() && app.supervisor_failing.is_empty());
    assert!(app.render_unrepaired.is_empty());
    let _ = session;
    for note in [
        HostNote::DiskPlenty,
        HostNote::DiskSettingsRead,
        HostNote::UpgradeRuns { sid: sid.clone() },
        HostNote::SupervisorSteady { sid: sid.clone() },
        HostNote::SurveyEnded { sid: sid.clone() },
    ] {
        app.note_harness_host(note);
    }
    app.check_render_repairs();
    let left: Vec<String> = app
        .messages
        .digest()
        .entries
        .iter()
        .map(|e| e.key.clone())
        .collect();
    assert!(left.is_empty(), "STALE after the end events: {left:?}");
}

/// A render line whose epoch this process holds at risk is not an orphan: the adoption sweep of
/// `check_render_repairs` leaves it to its epoch.
#[test]
fn an_adopted_render_line_ends_but_a_held_one_stands() {
    let mut app = App::headless_for_test();
    let (session, sid) = session_with_open_epoch(&mut app, 44);
    let spot = &places()[0];
    app.except(&mr::render_unrepaired_entry(&sid, spot, GiveUp::NoAnswer));
    app.check_render_repairs();
    assert!(
        app.messages.digest().is_empty(),
        "no epoch is held for an adopted line"
    );
    app.note_render_unrepaired(session, 1, GiveUp::NoAnswer);
    app.check_render_repairs();
    assert_eq!(app.messages.digest().len(), 1, "held: stands");
}

/// The supervisor lines an earlier process left that this one has heard nothing of for its
/// steady time end; the ones it holds stand.
#[test]
fn the_adoption_sweep_ends_supervisor_lines_nothing_has_told_of() {
    for (old, held) in [("s-old", "s-held"), ("s-1a2b3c4d5e6f708192a3", REAL_SID)] {
        let mut app = App::headless_for_test();
        let spot = &places()[0];
        app.except(&mr::supervisor_failing_entry(old, spot, "boom"));
        app.except(&mr::supervisor_failing_entry(held, spot, "boom"));
        app.except(&mr::survey_entry(old, spot, "did not close"));
        app.except(&mr::survey_entry(held, spot, "a card"));
        let now = Instant::now();
        app.supervisor_failing.insert(
            held.to_string(),
            crate::exceptions_host::SupervisorFail {
                first: now,
                last: now,
                pause: Duration::from_secs(1),
                why: "boom".to_string(),
                raised: true,
            },
        );
        app.host_noted.insert(format!("survey:{held}:Withheld"));
        app.sweep_adopted_supervisor_lines();
        let mut keys: Vec<String> = app
            .messages
            .digest()
            .entries
            .iter()
            .map(|e| e.key.clone())
            .collect();
        keys.sort();
        let mut want = vec![mr::supervisor_failing_key(held), mr::survey_key(held)];
        want.sort();
        assert_eq!(keys, want, "{held}");
    }
}

/// The Full Disk Access card, in the order it is used: the owner presses the button
/// (`Answered`), the probe then reads the grant, and the regression after the next update is
/// news again.
#[test]
fn a_verified_grant_after_the_owners_answer_makes_the_next_regression_news() {
    use aterm_messages::ActionIndex;
    let mut app = App::headless_for_test();
    let words = crate::consent_card::NeedWords {
        cause: crate::consent_card::NeedCause::PromptObserved {
            service: None,
            session: None,
        },
        tab_number: None,
        regressed: true,
    };
    let regressed = mr::file_access_entry(&words);
    assert_eq!(regressed.key(), mr::FDA_ENTRY_KEYS[1]);
    app.except(&regressed);
    let id = app.messages.digest().entries[0].id;
    let _ = app.messages.act_digest(
        id,
        ActionIndex(0),
        crate::messages_host::wall_stamp_now(),
        Instant::now(),
    );
    assert!(app.messages.digest().is_empty(), "answered: closed");
    app.clear_fda_entries();
    app.except(&regressed);
    assert_eq!(
        app.messages.digest().len(),
        1,
        "the second regression is news after a verified grant"
    );
}

/// atpkg's `foreign:` lines (a foreign rustup `trust` link, a managed tool shadowed on PATH) are
/// one entry per look carrying atpkg's fix, re-said as the same entry and ended by that look's
/// first report that sees nothing.
#[test]
fn foreign_tools_are_one_entry_per_look_with_atpkgs_fix_ended_when_it_sees_none() {
    use atpkg::cli::{FOREIGN_MARKER, FOREIGN_NONE, FOREIGN_PATH, FOREIGN_SEAM};
    let mut app = App::headless_for_test();
    let key = |scope: &str| crate::toolchain_words::foreign_tools_entry(scope, &[]).key();
    assert_eq!(key(FOREIGN_SEAM), "toolchain.seam.foreign");
    assert_eq!(key(FOREIGN_PATH), "toolchain.path.foreign");
    let feed = |app: &mut App, line: &str| match crate::parse_seed_line(line) {
        Some(crate::Wake::PkgForeign(body)) => app.note_foreign_tools(&body),
        other => panic!("{line}: {other:?}"),
    };
    let entry = |app: &App, scope: &str| {
        app.messages
            .digest()
            .entries
            .into_iter()
            .find(|e| e.key == key(scope))
    };
    let seam = format!(
        "atpkg: {FOREIGN_MARKER}{FOREIGN_SEAM} trust /u/.rustup/toolchains/trust \u{2014} {}",
        atpkg::seam::DETACH_FIX
    );
    let path = format!(
        "atpkg: {FOREIGN_MARKER}{FOREIGN_PATH} trust /u/.local/bin/trustdoc \u{2014} type \
         alab-trustdoc for the managed one"
    );
    let none = |scope: &str| format!("atpkg: {FOREIGN_MARKER}{scope} {FOREIGN_NONE}");
    feed(&mut app, &seam);
    let open = entry(&app, FOREIGN_SEAM).expect("the seam's entry");
    assert!(
        open.detail
            .iter()
            .any(|l| l.contains(atpkg::seam::DETACH_FIX)
                && l.contains("/u/.rustup/toolchains/trust")),
        "atpkg's fix, whole: {:?}",
        open.detail
    );
    feed(&mut app, &path);
    let open = entry(&app, FOREIGN_PATH).expect("the PATH look's entry");
    assert!(
        open.detail
            .iter()
            .any(|l| l.contains("/u/.local/bin/trustdoc")
                && l.contains("type alab-trustdoc for the managed one")),
        "atpkg's fix, whole: {:?}",
        open.detail
    );
    assert_eq!(app.messages.digest().len(), 2, "one per look");
    let revision = app.messages.digest_revision();
    feed(&mut app, &seam);
    feed(&mut app, &path);
    assert_eq!(app.messages.digest_revision(), revision, "no news");
    feed(
        &mut app,
        &format!("atpkg: {FOREIGN_MARKER}elsewhere {FOREIGN_NONE}"),
    );
    assert_eq!(app.messages.digest().len(), 2);
    feed(&mut app, &none(FOREIGN_SEAM));
    assert!(!app.owes_exception(&key(FOREIGN_SEAM)), "ended");
    assert!(
        app.owes_exception(&key(FOREIGN_PATH)),
        "the other look's stands"
    );
    feed(&mut app, &none(FOREIGN_PATH));
    assert!(app.messages.digest().is_empty());
    feed(
        &mut app,
        &format!("atpkg: {FOREIGN_MARKER}{FOREIGN_PATH} gh /u/.local/bin/gh"),
    );
    assert!(app.messages.digest().is_empty(), "no fix, no entry");
}

/// A pass that laid programs down on a machine without a C toolchain says
/// `c-toolchain: missing`: ONE entry naming atpkg's fix (on macOS `aterm init --only
/// base.clt`), a fact for later that is never on screen, filed once however many passes say
/// it, and ended by a pass that finds one that builds (docs/DESIGN-init-system-2026-10-20.md
/// §4.2 `base.clt`).
#[test]
fn a_missing_c_toolchain_is_one_entry_naming_atpkgs_fix_ended_when_one_builds() {
    use crate::toolchain_words::{CToolchain, c_toolchain_entry};
    use atpkg::cli::{C_TOOLCHAIN_MARKER, C_TOOLCHAIN_MISSING, C_TOOLCHAIN_READY};
    let mut app = App::headless_for_test();
    let key = c_toolchain_entry(CToolchain::Missing).key();
    let feed = |app: &mut App, line: &str| match crate::parse_seed_line(line) {
        Some(crate::Wake::PkgCToolchain(body)) => app.note_c_toolchain(&body),
        other => panic!("{line}: {other:?}"),
    };
    let missing = format!("atpkg: {C_TOOLCHAIN_MARKER}{C_TOOLCHAIN_MISSING}");
    let before = glass(&app);
    feed(&mut app, &missing);
    assert_eq!(
        glass(&app),
        before,
        "a fact for later: nothing on the glass"
    );
    let digest = app.messages.digest();
    assert_eq!(digest.len(), 1, "{digest:?}");
    let open = &digest.entries[0];
    assert_eq!(open.key, key);
    let said = format!("{open:?}");
    assert!(said.contains(atpkg::prereq::act()), "atpkg's fix: {said}");
    let revision = app.messages.digest_revision();
    feed(&mut app, &missing);
    feed(&mut app, &missing);
    assert_eq!(app.messages.digest_revision(), revision, "no news");
    assert_eq!(app.messages.digest().len(), 1);
    feed(&mut app, &format!("atpkg: {C_TOOLCHAIN_MARKER}sideways"));
    assert!(app.owes_exception(&key));
    feed(
        &mut app,
        &format!("atpkg: {C_TOOLCHAIN_MARKER}{C_TOOLCHAIN_READY}"),
    );
    assert!(!app.owes_exception(&key), "ended");
    assert!(app.messages.digest().is_empty());
}

/// `c-toolchain: broken` (off macOS, a compiler without the headers an install supplies)
/// is its own entry, worded as a compiler that cannot build, never "no compiler is
/// installed"; one cause's verdict ends the other's, and `ready` ends whichever is open.
#[test]
fn a_broken_c_toolchain_is_its_own_entry_and_each_verdict_ends_the_other() {
    use crate::toolchain_words::{CToolchain, c_toolchain_entry};
    use atpkg::cli::{
        C_TOOLCHAIN_BROKEN, C_TOOLCHAIN_MARKER, C_TOOLCHAIN_MISSING, C_TOOLCHAIN_READY,
    };
    let mut app = App::headless_for_test();
    let missing = c_toolchain_entry(CToolchain::Missing).key();
    let broken = c_toolchain_entry(CToolchain::Broken).key();
    assert_ne!(missing, broken);
    let feed = |app: &mut App, body: &str| match crate::parse_seed_line(&format!(
        "atpkg: {C_TOOLCHAIN_MARKER}{body}"
    )) {
        Some(crate::Wake::PkgCToolchain(body)) => app.note_c_toolchain(&body),
        other => panic!("{body}: {other:?}"),
    };
    feed(&mut app, C_TOOLCHAIN_BROKEN);
    let digest = app.messages.digest();
    assert_eq!(digest.len(), 1, "{digest:?}");
    let said = format!("{:?}", digest.entries[0]);
    assert!(said.contains("The C compiler cannot build"), "{said}");
    assert!(
        !said.contains("is installed") && !said.contains("missing"),
        "{said}"
    );
    feed(&mut app, C_TOOLCHAIN_MISSING);
    assert!(app.owes_exception(&missing) && !app.owes_exception(&broken));
    feed(&mut app, C_TOOLCHAIN_BROKEN);
    assert!(app.owes_exception(&broken) && !app.owes_exception(&missing));
    feed(&mut app, C_TOOLCHAIN_READY);
    assert!(!app.owes_exception(&broken) && !app.owes_exception(&missing));
    assert!(app.messages.digest().is_empty());
}

/// A RELAUNCH THAT KEEPS FAILING WITH NO COMMAND THE TAB'S MARK COULD NAME
/// (a Codex, a launch no resume repeats) is the digest's line AT ONCE (LC-5 of
/// at attention, where it was a record only): the agent is
/// down and only the owner can start it sooner; an agent running again ends
/// it.
#[test]
fn a_failing_relaunch_with_no_command_is_the_digests_line_at_once() {
    let mut app = App::headless_for_test();
    let tab = front_tab_sid(&mut app);
    let say = |app: &mut App, said| {
        app.note_harness_host(HostNote::Relaunch {
            sid: tab.clone(),
            said,
        });
    };
    say(
        &mut app,
        RelaunchSay::Failing {
            blocked_by: None,
            restarted: false,
            command: Some("claude --resume 0badf00d-1111-2222-3333-444455556666".to_string()),
        },
    );
    assert!(app.messages.digest().is_empty(), "the tab's mark says it");
    say(
        &mut app,
        RelaunchSay::Failing {
            blocked_by: None,
            restarted: false,
            command: None,
        },
    );
    let keys: Vec<String> = app
        .messages
        .digest()
        .entries
        .iter()
        .map(|e| e.key.clone())
        .collect();
    assert_eq!(keys, [mr::relaunch_keys(&tab)[0].clone()], "at once");
    say(&mut app, RelaunchSay::Ended);
    assert!(app.messages.digest().is_empty(), "an agent back ends it");
    say(
        &mut app,
        RelaunchSay::Failing {
            blocked_by: Some("other-tab"),
            restarted: false,
            command: None,
        },
    );
    let line = app.messages.digest().entries[0].clone();
    assert_eq!(line.key, mr::relaunch_keys(&tab)[0], "at once");
    assert!(
        line.then
            .starts_with("Close the conversation in its other tab"),
        "{}",
        line.then
    );
    say(&mut app, RelaunchSay::Ended);
    say(
        &mut app,
        RelaunchSay::Failing {
            blocked_by: None,
            restarted: true,
            command: None,
        },
    );
    assert!(app.messages.digest().is_empty(), "a record, its row speaks");
}

/// A LOST LOGIN IS ITS TAB'S NOTICE, NEVER A MACHINE'S RENEW ASK: an earlier
/// build's machine ask (`harness.accounts.renew-ask`) an earlier process left
/// is withdrawn at the resume family's settle, whatever its tabs tell, and a
/// tab's lost login carried to the owner's sign-in is its own ask.
#[test]
fn a_lost_login_is_its_tabs_ask_and_the_accounts_say_lines() {
    use aterm_messages::need::Need;
    let mut app = App::headless_for_test();
    let told = |app: &mut App, sid: &str, standing: Option<Exception>| {
        app.note_harness_host(HostNote::ResumeException {
            sid: sid.to_string(),
            agent: Program::Claude,
            standing,
        });
    };
    let asks = |app: &App| -> Vec<String> {
        app.messages
            .live_rows()
            .filter(|row| matches!(row.msg.need(), Need::Now(_)))
            .map(|row| row.msg.key.clone().unwrap_or_default())
            .collect()
    };
    {
        use aterm_messages::need::{Ask, Reason, StandingWhy, Then};
        app.post_message(
            aterm_messages::Message::new(
                aterm_messages::tags::HARNESS,
                aterm_messages::Severity::Warn,
                "Claude login ended",
            )
            .line("Claude Code's login ended.")
            .now(Ask::standing(
                StandingWhy::OnlyYou(Reason::new("Claude Code's login ended")),
                Then::step("Run aterm harness accounts renew main"),
            ))
            .key(mr::RENEW_ASK_KEY),
        );
    }
    assert_eq!(asks(&app), [mr::RENEW_ASK_KEY]);
    told(&mut app, "s-4", Some(stuck_login_exception()));
    app.ripen_exceptions(Instant::now() + DAY);
    assert_eq!(
        asks(&app),
        [mr::notice_ask_key("s-4", Some(notice::LOGIN_SIGN_IN.code))],
        "the old machine ask swept; the tab's own ask stands"
    );
    told(&mut app, "s-4", None);
    told(
        &mut app,
        "s-5",
        Some(resume_exception(ExceptionKind::Login)),
    );
    assert!(asks(&app).is_empty(), "said once, by the escalation");

    let listed = |due: &[&str], none_soon: bool, wrong: &[&str]| {
        HostNote::Accounts(AccountsSaid::Listed {
            signed_out: Vec::new(),
            credits: Vec::new(),
            window_full: Vec::new(),
            due: due.iter().map(|l| l.to_string()).collect(),
            none_soon,
            wrong: wrong.iter().map(|l| l.to_string()).collect(),
        })
    };
    let keys = |app: &App| -> Vec<String> {
        app.messages
            .digest()
            .entries
            .iter()
            .map(|e| e.key.clone())
            .collect()
    };
    let _ = app.settle_messages(Instant::now() + DAY);
    app.note_harness_host(listed(&["m7-a"], false, &[]));
    assert_eq!(keys(&app), [mr::ACCOUNTS_DUE_KEY]);
    assert!(asks(&app).is_empty(), "a due login is a line, never an ask");
    app.note_harness_host(listed(&["m7-a"], true, &["m7-a"]));
    let mut want = vec![
        mr::ACCOUNTS_DUE_KEY.to_string(),
        mr::ACCOUNTS_NONE_SOON_KEY.to_string(),
        mr::ACCOUNTS_WRONG_KEY.to_string(),
    ];
    want.sort();
    let mut have = keys(&app);
    have.sort();
    assert_eq!(have, want);
    app.note_harness_host(listed(&[], false, &[]));
    let _ = app.settle_messages(Instant::now() + DAY);
    assert!(keys(&app).is_empty(), "renewed: every line ends");
}

/// A silent usage tap is one `harness.accounts.no-tap` line for the machine, held while any tab
/// is silent and raised at most once a day; never a band row or a tab mark.
#[test]
fn a_silent_usage_tap_is_one_line_once_a_day_and_ends_with_the_last_tab() {
    let mut app = App::headless_for_test();
    let before = glass(&app);
    let note = |app: &mut App, sid: &str, silent: bool| {
        app.note_harness_host(HostNote::UsageTap {
            sid: sid.to_string(),
            silent,
        });
        assert_eq!(glass(app), before, "nothing on the glass");
        app.messages
            .digest()
            .entries
            .iter()
            .map(|e| e.key.clone())
            .collect::<Vec<_>>()
    };
    assert!(note(&mut app, "s-1", false).is_empty(), "nothing to end");
    assert_eq!(note(&mut app, "s-1", true), [mr::USAGE_TAP_SILENT_KEY]);
    assert_eq!(note(&mut app, "s-2", true), [mr::USAGE_TAP_SILENT_KEY]);
    assert_eq!(app.messages.digest().entries.len(), 1, "one line");
    assert_eq!(
        note(&mut app, "s-1", false),
        [mr::USAGE_TAP_SILENT_KEY],
        "s-2 stands"
    );
    assert!(note(&mut app, "s-2", false).is_empty(), "the last is heard");
    assert!(note(&mut app, "s-3", true).is_empty(), "once a day");
    assert!(note(&mut app, "s-3", false).is_empty());
}

/// THE OWNER'S `occupancy = "prompt"` IS ONE ENTRY WHILE IT STANDS (occupancy 1e; the
/// red-team's F14, R9): the policy wake alone — the watcher's post, which comes at every
/// launch and on a read no config generation admits — whose root `[harness]` table says it
/// raises the entry naming that the harness types occupancy lines, once however many posts say
/// it, and the board takes the mode; a later post that no longer says it clears it. No config
/// generation is applied here: the launch's unchanged file sends none. NEGATIVE CONTROLS:
/// `observe`, `off`, a word that is no mode, and no key raise nothing, and a stale stamp
/// changes nothing.
#[test]
fn the_owners_prompt_is_one_entry_while_the_policy_in_force_says_it() {
    use crate::app_config::HarnessPolicy;
    use crate::harness_policy_cell::HarnessStampSource;
    use aterm_agent::supervise::Occupancy;
    let mut app = App::headless_for_test();
    let mut stamps = HarnessStampSource::new();
    let prompt = "[harness]\noccupancy = \"prompt\"\n";
    let stale = stamps.stamp(HarnessPolicy::read("[harness]\noccupancy = \"observe\"\n"));
    app.adopt_harness_policy(stamps.stamp(HarnessPolicy::read(prompt)));
    app.adopt_harness_policy(stamps.stamp(HarnessPolicy::read(prompt)));
    let digest = app.messages.digest();
    assert_eq!(digest.len(), 1, "{digest:?}");
    let entry = &digest.entries[0];
    assert_eq!(entry.key, mr::occupancy_prompt_key());
    assert!(entry.key.starts_with(&Family::Occupancy.prefix()));
    assert_eq!(crate::occupancy_host::mode(), Occupancy::Prompt);
    mr::occupancy_prompt_entry()
        .message()
        .expect("the words pass the rule");
    app.adopt_harness_policy(stale);
    assert_eq!(
        app.messages.digest().len(),
        1,
        "a stale stamp changes nothing"
    );
    assert_eq!(crate::occupancy_host::mode(), Occupancy::Prompt);
    app.adopt_harness_policy(
        stamps.stamp(HarnessPolicy::read("[harness]\noccupancy = \"observe\"\n")),
    );
    assert!(app.messages.digest().is_empty(), "observe ends it");
    assert_eq!(crate::occupancy_host::mode(), Occupancy::Observe);
    for (text, mode) in [
        ("", Occupancy::Observe),
        ("font_px = 14\n", Occupancy::Observe),
        ("[harness]\noccupancy = \"off\"\n", Occupancy::Off),
        ("[harness]\noccupancy = \"yes\"\n", Occupancy::Observe),
        ("[harness]\nautopilot = \"cruise\"\n", Occupancy::Observe),
    ] {
        let mut app = App::headless_for_test();
        app.adopt_harness_policy(stamps.stamp(HarnessPolicy::read(text)));
        assert!(app.messages.digest().is_empty(), "{text:?}");
        assert_eq!(crate::occupancy_host::mode(), mode, "{text:?}");
    }
    crate::occupancy_host::set_mode(Occupancy::Observe);
}

/// A TAB'S SPENT CAP UNDER `prompt` IS ITS HOLD ENTRY until its loop says otherwise (design §6,
/// `occupancy.hold`): told through the host's note, a word under `prompt` with a rule spent
/// raises the tab's entry — once while what is spent stands — and a word with nothing spent
/// clears it. NEGATIVE CONTROL: the same spent word under `observe` raises nothing (nothing the
/// owner sees was typed).
#[test]
fn a_spent_cap_under_prompt_is_the_tabs_hold_entry() {
    use aterm_agent::supervise::Occupancy;
    use aterm_agent::supervise::occupancy::{
        OccupancyReading, OccupancySaid, RuleId, SaidHold, SaidRule, SaidVerdict,
    };
    let word = |mode: Occupancy, verdicts: [SaidVerdict; 2]| OccupancySaid {
        mode,
        hold: Some(SaidHold {
            since_ms: 1_792_600_000_000,
            declared: true,
        }),
        ready: false,
        guards: None,
        rules: [
            SaidRule {
                rule: RuleId::WaitNudge,
                verdict: verdicts[0],
                at_ms: 1_792_600_000_000,
            },
            SaidRule {
                rule: RuleId::HoldRoom,
                verdict: verdicts[1],
                at_ms: 1_792_600_000_000,
            },
        ],
        at_ms: 1_792_600_000_000,
        reading: OccupancyReading::default(),
    };
    let sid = "s-occupancy-hold";
    let mut app = App::headless_for_test();
    let spent = [SaidVerdict::Quiet, SaidVerdict::Spent];
    for _ in 0..2 {
        app.note_harness_host(HostNote::OccupancySaid {
            sid: sid.to_string(),
            said: word(Occupancy::Prompt, spent),
        });
    }
    let digest = app.messages.digest();
    assert_eq!(digest.len(), 1, "{digest:?}");
    assert_eq!(digest.entries[0].key, mr::occupancy_hold_key(sid));
    mr::occupancy_hold_entry(sid, &[RuleId::WaitNudge, RuleId::HoldRoom])
        .message()
        .expect("the words pass the rule");
    app.note_occupancy_said(sid, word(Occupancy::Prompt, [SaidVerdict::Spent; 2]));
    assert_eq!(app.messages.digest().len(), 1, "one entry for the tab");
    app.note_occupancy_said(
        sid,
        word(Occupancy::Prompt, [SaidVerdict::Quiet, SaidVerdict::Typed]),
    );
    assert!(app.messages.digest().is_empty(), "nothing spent ends it");

    let mut observe = App::headless_for_test();
    observe.note_occupancy_said(sid, word(Occupancy::Observe, [SaidVerdict::Spent; 2]));
    assert!(
        observe.messages.digest().is_empty(),
        "observe raises nothing"
    );
    crate::occupancy_host::told(sid, None);
}
