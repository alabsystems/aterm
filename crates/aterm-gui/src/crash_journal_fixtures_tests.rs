// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use crate::restore::{self, RestoreManifest};

fn leaf(local: u64, cwd: Option<&str>, title: &str) -> restore::RestoredSplitTree {
    restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(
        restore::TerminalLeafRestore {
            sid: None,
            lineage: None,
            log_ep: None,
            cwd: cwd.map(str::to_string),
            title: title.to_string(),
            profile: None,
            local_id: Some(local),
            user_title: None,
            description: None,
            icon: None,
            role: None,
            attention: None,
            questions: None,
            autopilot: None,
            land_at: None,
            autopilot_by: None,
            theme: None,
            agent: None,
            supervised: None,
            lost: None,
            held: false,
            holds: Vec::new(),
            claim_ended: false,
            not_claim_worker: false,
            claim_worker: None,
            task: None,
        },
    ))
}

pub(crate) fn layout(tabs: &[(&str, &str)]) -> RestoreManifest {
    RestoreManifest::new(vec![restore::WindowLayout {
        rows: 24,
        cols: 80,
        active_tab: 0,
        outer_x: None,
        outer_y: None,
        maximized: None,
        inner_px: None,
        presence_row: false,
        show: restore::WindowShow::UNKNOWN,
        tabs: Vec::new(),
        native_tabs: Vec::new(),
        tab_order: Vec::new(),
        active_item: Some(0),
        workers_in_front: false,
        restored_tabs: tabs
            .iter()
            .enumerate()
            .map(|(index, (cwd, title))| restore::RestoredTab {
                root: leaf(index as u64, Some(cwd), title),
                focused_path: Vec::new(),
                zoomed: false,
                parked: false,
                ephemeral: false,
                fleet: false,
            })
            .collect(),
    }])
}
