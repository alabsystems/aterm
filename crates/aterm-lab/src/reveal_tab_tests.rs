// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn passing() -> Evidence {
    Evidence {
        imported: true,
        outside_printed: true,
        outside_said: String::new(),
        editor_opened: true,
        file_holds_values: true,
        refusals: vec![(
            "text".to_owned(),
            "aterm-ctl: ERR refused private view: the front tab shows a private document"
                .to_owned(),
        )],
        doc_read: "aterm-ctl: ERR refused private doc".to_owned(),
        tab_names_editor: true,
        tab_text_clean: true,
        blocks_hold_command: true,
        blocks_clean: true,
        log_clean: true,
        closed_ends_command: true,
        folder_removed: true,
        after_close_clean: true,
        ax: Some(AxEvidence {
            grid_read: true,
            logins_read: true,
            clean: true,
        }),
    }
}

/// Every check holds on a passing run, and each fails on its own: an answer that is not the
/// refusal, a value in the tab, a refused `doc read` turned served.
#[test]
fn each_check_fails_alone() {
    assert!(judge(&passing()).iter().all(|(_, held)| *held));
    let mut served = passing();
    served.refusals[0].1 = "OK 24".to_owned();
    let mut leaked = passing();
    leaked.tab_text_clean = false;
    let mut read = passing();
    read.doc_read = "OK 120 doc=1".to_owned();
    let mut spoken = passing();
    spoken.ax = Some(AxEvidence {
        clean: false,
        ..AxEvidence::default()
    });
    let mut unpublished = passing();
    unpublished.ax = Some(AxEvidence {
        grid_read: false,
        ..passing().ax.unwrap()
    });
    for ev in [served, leaked, read, unpublished] {
        assert_eq!(judge(&ev).iter().filter(|(_, held)| !*held).count(), 1);
    }
    assert_eq!(judge(&spoken).iter().filter(|(_, held)| !*held).count(), 3);
    assert!(skipped(&passing()).is_empty());
}

/// Without an Accessibility grant the tree's checks are a named skip, never a pass.
#[test]
fn an_unread_tree_is_a_skip_not_a_pass() {
    let mut unread = passing();
    unread.ax = None;
    let checks = judge(&unread);
    assert!(
        checks
            .iter()
            .all(|(name, _)| !name.contains("accessibility"))
    );
    assert_eq!(skipped(&unread).len(), 1);
}

/// A report's evidence reads back as the run it was, the tree unread too, and a report missing
/// a field (another build's) reads as none rather than as a run that found nothing. A text
/// holding a value is withheld on the way, and a refusal withheld that way no longer reads as
/// one, so the host fails it as the guest would have.
#[test]
fn a_reports_evidence_reads_back_and_carries_no_value() {
    let mut unread = passing();
    unread.ax = None;
    for ev in [passing(), unread] {
        let wire = evidence_json(&ev);
        assert_eq!(evidence_from(&wire), Some(ev.clone()));
        let text = aterm_json::to_string(&wire).unwrap();
        let parsed = aterm_json::from_str::<Value>(&text).unwrap();
        assert_eq!(evidence_from(&parsed), Some(ev));
    }
    let mut wire = evidence_json(&passing());
    if let Value::Object(map) = &mut wire {
        map.remove("log_clean");
    }
    assert_eq!(evidence_from(&wire), None);

    let mut leaked = passing();
    leaked.refusals[0].1 = format!("{NEEDLE}01");
    leaked.outside_said = format!("box01@proton.example {NEEDLE}01");
    let wire = evidence_json(&leaked);
    assert!(!aterm_json::to_string(&wire).unwrap().contains(NEEDLE));
    let back = evidence_from(&wire).unwrap();
    assert!(!refused_view(&back.refusals[0].1));
    assert_eq!(judge(&back).iter().filter(|(_, held)| !*held).count(), 1);
}

/// The guest helper's reading is its strings, or no grant; anything else is no reading.
#[test]
fn the_helpers_reading_is_its_strings_or_no_grant() {
    assert_eq!(
        helper_reading(
            "{\"trusted\":true,\"texts\":[\"hidden value\",\"box01@proton.example\"]}\n"
        ),
        Some(Some(vec![
            "hidden value".to_owned(),
            "box01@proton.example".to_owned()
        ]))
    );
    assert_eq!(
        helper_reading("{\"trusted\":false,\"texts\":[]}"),
        Some(None)
    );
    for other in [
        "",
        "trusted=yes",
        "{\"texts\":[]}",
        "{\"trusted\":true}",
        "{\"trusted\":true,\"texts\":[1]}",
    ] {
        assert_eq!(helper_reading(other), None, "{other}");
    }
}

/// The negative control reaches the instance by name, and only when asked.
#[test]
fn the_plain_publication_is_the_instances_alone_and_only_when_asked() {
    let world = World::new(CONFIG_OFF).unwrap();
    let named = |env: &[(String, String)]| {
        env.iter()
            .filter(|(key, _)| key == crate::mesh_churn::NEGATIVE_CONTROL)
            .map(|(_, value)| value.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(named(&instance_env(&world, false)), Vec::<String>::new());
    assert_eq!(named(&instance_env(&world, true)), [PLAIN_PUBLICATION]);
    assert_eq!(named(&zsh_env(&world)), Vec::<String>::new());
    assert!(
        instance_env(&world, true)
            .iter()
            .any(|(key, value)| key == "SHELL" && value == "/bin/zsh")
    );
}

/// The tree's three checks are judged under the names a host reads them by.
#[test]
fn the_trees_checks_are_named_as_judged() {
    let names: Vec<String> = judge_tab(&passing())
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(names[names.len() - 3..], TREE_CHECKS.map(str::to_owned));
}

#[test]
fn each_verbs_refusal_reads_as_one() {
    for first in [
        "aterm-ctl: ERR refused private view: the front tab shows a private document",
        "aterm-ctl: ERR video: refused private view: the front tab shows a private document",
        "aterm-ctl: ERR pointer command failed: refused private view: the front tab shows a \
         private document",
    ] {
        assert!(refused_view(first), "{first}");
    }
    for first in [
        "OK at=-",
        "aterm-ctl: ERR pointer command failed: no window",
        "aterm-ctl: ERR refused private doc",
        "refused private view",
    ] {
        assert!(!refused_view(first), "{first}");
    }
}

#[test]
fn the_mailbox_file_names_each_synthetic_login_once() {
    let file = mailbox_file(3);
    assert_eq!(file.matches("[[mailboxes]]").count(), 3);
    assert_eq!(file.matches(NEEDLE).count(), 3);
    assert!(file.contains("login = \"box03@proton.example\"\npassword = \"synthetic-pass-03\""));
}

#[test]
fn the_rc_accepts_its_line_once() {
    let rc = zshrc("/b/aterm db emails --show-passwords");
    assert!(rc.contains("print -z -- '/b/aterm db emails --show-passwords'"));
    assert!(rc.contains("zle -N zle-line-init __a17_accept"));
    assert!(rc.contains("__a17_go=0; zle accept-line"));
}
