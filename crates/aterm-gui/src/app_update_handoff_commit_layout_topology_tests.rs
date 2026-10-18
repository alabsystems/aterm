// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::commit_layout_topology;
use crate::restore::{
    PaneLayout, RestoreManifest, RestoredSplitTree, RestoredTab, RestoredView, TabOrderEntry,
    TerminalLeafRestore, WindowLayout,
};

/// One window, one terminal tab, shaped as `capture_restore_manifest` produces
/// it: the legacy `tabs` mirror and the canonical `restored_tabs` tree carry the
/// same session's cwd/title, so a projection missing either would still reject.
fn captured(position: Option<(i32, i32)>, cwd: Option<&str>, title: &str) -> RestoreManifest {
    RestoreManifest::new(vec![WindowLayout {
        rows: 40,
        cols: 120,
        active_tab: 0,
        outer_x: position.map(|(x, _)| x),
        outer_y: position.map(|(_, y)| y),
        maximized: None,
        inner_px: None,
        presence_row: false,
        show: crate::restore::WindowShow::UNKNOWN,
        tabs: vec![PaneLayout::Leaf {
            cwd: cwd.map(str::to_string),
            title: title.to_string(),
            focused: true,
            local_id: Some(7),
        }],
        native_tabs: Vec::new(),
        tab_order: vec![TabOrderEntry::Terminal { index: 0 }],
        active_item: Some(0),
        restored_tabs: vec![RestoredTab {
            root: RestoredSplitTree::leaf(RestoredView::Terminal(TerminalLeafRestore {
                sid: None,
                lineage: None,
                cwd: cwd.map(str::to_string),
                title: title.to_string(),
                profile: None,
                local_id: Some(7),
                user_title: None,
                description: None,
                icon: None,
                role: None,
                attention: None,
                questions: None,
                autopilot: None,
                land_at: None,
                identity: None,
                theme: None,
                agent: None,
                supervised: None,
                held: false,
                holds: Vec::new(),
            })),
            focused_path: Vec::new(),
            zoomed: false,
        }],
    }])
}

#[test]
fn dragging_the_window_while_the_successor_boots_is_not_a_topology_change() {
    let committed = captured(Some((120, 80)), Some("/work"), "zsh");
    let dragged = captured(Some((640, 310)), Some("/work"), "zsh");
    assert_ne!(
        committed, dragged,
        "PRECONDITION: the derived PartialEq must still see the raw position difference — \
         that is exactly what used to reject the Commit"
    );
    assert_eq!(
        commit_layout_topology(&committed),
        commit_layout_topology(&dragged),
        "a drag during the child's boot must not be reported as changed topology; \
         `WindowEvent::Moved` is classified Tolerated for this very reason"
    );
}

/// The macOS show state is live, like the position: entering full screen,
/// minimizing or focusing another window while the successor boots must not
/// reject the Commit, nor hide a real topology change riding with it.
#[test]
fn a_show_state_change_while_the_successor_boots_is_not_a_topology_change() {
    use crate::restore::WindowShow;
    let shown = |show: WindowShow| {
        let mut layout = captured(Some((120, 80)), Some("/work"), "zsh");
        layout.windows[0].show = show;
        layout
    };
    let committed = shown(WindowShow {
        fullscreen: Some(false),
        minimized: Some(false),
        z_order: Some(0),
        key: Some(true),
    });
    let changes = [
        WindowShow {
            fullscreen: Some(true),
            ..committed.windows[0].show
        },
        WindowShow {
            minimized: Some(true),
            ..committed.windows[0].show
        },
        WindowShow {
            z_order: Some(1),
            key: Some(false),
            ..committed.windows[0].show
        },
        // A parent that captured nothing (headless, or before the field).
        WindowShow::UNKNOWN,
    ];
    for changed in changes {
        let live = shown(changed);
        assert_ne!(
            committed, live,
            "PRECONDITION: the derived PartialEq sees the show-state difference {changed:?}"
        );
        assert_eq!(
            commit_layout_topology(&committed),
            commit_layout_topology(&live),
            "a show-state change ({changed:?}) is not topology and must not reject Commit"
        );
    }
    let mut resized = shown(WindowShow {
        fullscreen: Some(true),
        ..committed.windows[0].show
    });
    resized.windows[0].cols = 200;
    assert_ne!(
        commit_layout_topology(&committed),
        commit_layout_topology(&resized),
        "the normalization is of the show state alone: a re-grid riding with it rejects"
    );
}

#[test]
fn a_contended_session_lock_that_empties_cwd_and_title_is_not_a_topology_change() {
    let committed = captured(Some((120, 80)), Some("/work"), "zsh");
    // Exactly what `restore_session_meta` yields on a `WouldBlock` try_lock: no
    // cwd, empty title, every structural field untouched.
    let degraded = captured(Some((120, 80)), None, "");
    assert_ne!(
        committed, degraded,
        "PRECONDITION: a degraded capture is not equal to the committed one"
    );
    assert_eq!(
        commit_layout_topology(&committed),
        commit_layout_topology(&degraded),
        "a degraded metadata read must never masquerade as a changed layout — it made the \
         rejection nondeterministic for a session that did not change"
    );
}

#[test]
fn a_real_topology_change_still_rejects_the_commit() {
    let committed = captured(Some((120, 80)), Some("/work"), "zsh");

    let mut resized = captured(Some((120, 80)), Some("/work"), "zsh");
    resized.windows[0].cols = 200;
    assert_ne!(
        commit_layout_topology(&committed),
        commit_layout_topology(&resized),
        "the grid the proof committed to is structural and must still reject"
    );

    let mut readopted = captured(Some((120, 80)), Some("/work"), "zsh");
    readopted.windows[0].restored_tabs[0].root =
        RestoredSplitTree::leaf(RestoredView::Terminal(TerminalLeafRestore {
            sid: None,
            lineage: None,
            cwd: Some("/work".to_string()),
            title: "zsh".to_string(),
            profile: None,
            local_id: Some(9),
            user_title: None,
            description: None,
            icon: None,
            role: None,
            attention: None,
            questions: None,
            autopilot: None,
            land_at: None,
            identity: None,
            theme: None,
            agent: None,
            supervised: None,
            held: false,
            holds: Vec::new(),
        }));
    assert_ne!(
        commit_layout_topology(&committed),
        commit_layout_topology(&readopted),
        "`local_id` is the layout↔live-fd bridge the child adopts by, not degradable \
         metadata, so it must survive the projection"
    );

    let mut relabelled = captured(Some((120, 80)), Some("/work"), "zsh");
    let RestoredSplitTree::Leaf {
        view: RestoredView::Terminal(terminal),
    } = &mut relabelled.windows[0].restored_tabs[0].root
    else {
        unreachable!("the fixture builds a single terminal leaf");
    };
    terminal.user_title = Some("deploy".to_string());
    assert_ne!(
        commit_layout_topology(&committed),
        commit_layout_topology(&relabelled),
        "USER metadata is captured under a BLOCKING lock, so it cannot degrade and must \
         not be swept up by the cwd/title normalization"
    );

    let mut extra_tab = captured(Some((120, 80)), Some("/work"), "zsh");
    let tab = extra_tab.windows[0].restored_tabs[0].clone();
    extra_tab.windows[0].restored_tabs.push(tab);
    assert_ne!(
        commit_layout_topology(&committed),
        commit_layout_topology(&extra_tab),
        "a tab that appeared during async preparation is what this gate exists to catch"
    );
}

/// `captured`'s single terminal leaf, edited in place.
fn with_leaf(
    mut layout: RestoreManifest,
    edit: impl FnOnce(&mut TerminalLeafRestore),
) -> RestoreManifest {
    let RestoredSplitTree::Leaf {
        view: RestoredView::Terminal(terminal),
    } = &mut layout.windows[0].restored_tabs[0].root
    else {
        unreachable!("the fixture builds a single terminal leaf");
    };
    edit(terminal);
    layout
}

fn stamp_all_five(terminal: &mut TerminalLeafRestore) {
    terminal.user_title = Some("fable driver".to_string());
    terminal.description = Some("drives the satcomp worker".to_string());
    terminal.icon = Some("🦊".to_string());
    terminal.role = Some("agent:claude-driver-fable".to_string());
    terminal.attention = Some("waiting on a human review".to_string());
}

/// All five user metadata fields stay in the Commit comparison (so a `meta set`
/// answered before the Commit-time re-capture is seen), the cwd/title/position
/// normalization never touches them, and the sidecar the child parses carries
/// exactly what was compared.
#[test]
fn every_user_metadata_field_is_compared_and_rides_the_sidecar() {
    type Clear = fn(&mut TerminalLeafRestore);
    let stamped = with_leaf(
        captured(Some((120, 80)), Some("/work"), "zsh"),
        stamp_all_five,
    );

    let clears: [(&str, Clear); 5] = [
        ("user_title", |terminal| terminal.user_title = None),
        ("description", |terminal| terminal.description = None),
        ("icon", |terminal| terminal.icon = None),
        ("role", |terminal| terminal.role = None),
        ("attention", |terminal| terminal.attention = None),
    ];
    for (field, clear) in clears {
        assert_ne!(
            commit_layout_topology(&stamped),
            commit_layout_topology(&with_leaf(stamped.clone(), clear)),
            "{field} is read under a blocking lock and must stay in the comparison"
        );
    }

    let dragged_and_degraded = with_leaf(captured(Some((640, 310)), None, ""), stamp_all_five);
    assert_eq!(
        commit_layout_topology(&stamped),
        commit_layout_topology(&dragged_and_degraded),
        "the degradable-field normalization must not reach the user fields"
    );

    let wire = stamped.to_toml().expect("the parent serializes its layout");
    let parsed = RestoreManifest::from_toml(&wire)
        .filter(|layout| layout.covers_exact_seamless_ids(&[7]))
        .expect("the child accepts the sidecar");
    assert_eq!(
        commit_layout_topology(&parsed),
        commit_layout_topology(&stamped),
        "the child's layout carries every compared user field"
    );
}

/// A layout the capture's fold cannot bring within the caps is refused at the park as a fact
/// about the desk (`CaptureRefusal`, retried once the desk changes), not as worker trouble. A
/// layout within the caps commits to its usual digest.
#[test]
fn the_park_refuses_a_layout_past_the_caps_as_a_fact_about_the_desk() {
    use super::{CaptureRefusal, commit_handoff_layout};
    let within = captured(Some((120, 80)), Some("/work"), "zsh");
    assert_eq!(
        commit_handoff_layout(&within),
        Ok(crate::seamless::layout_digest(&within).expect("canonical")),
        "within the caps: the digest the park always committed to"
    );

    // Past the window cap, unfolded: the worker's round trip reads 64 of the 65 windows back.
    let one_window = captured(None, Some("/work"), "zsh").windows.remove(0);
    let past = RestoreManifest::new(vec![one_window; crate::restore::MAX_WINDOWS + 1]);
    let wire = past.to_toml().expect("serializes");
    assert_ne!(
        RestoreManifest::from_toml(&wire).as_ref(),
        Some(&past),
        "PRECONDITION: the worker's round trip fails on it"
    );
    let refusal: CaptureRefusal = commit_handoff_layout(&past).expect_err("refused at the park");
    assert_eq!(
        refusal.local_id, None,
        "a fact about the desk, not about one session"
    );
    // …and the fold the capture runs brings that desk within the caps.
    let mut folded = past;
    folded.fold_into_caps();
    assert!(commit_handoff_layout(&folded).is_ok());
}
