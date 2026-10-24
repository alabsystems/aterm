// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The window around each of the merged deck's scenes ([`aterm_vitals::fixtures`]): its tabs,
//! its front tab and that tab's panes, and the needs its agent sessions publish, handed to the
//! deck as the App hands a real window's ([`crate::App::deck_frame`]).

use aterm_vitals::fixtures::{self, DeckScene};
use aterm_vitals::ink::VitalsInks;
use aterm_vitals::model::Snapshot;
use aterm_vitals::words::{Locale, Platform};

use super::DeckInput;
use crate::needs::Need;
use crate::stats_plan::DeckForm;

/// A scene's window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeckContext {
    /// The tab strip's titles, in order.
    pub(crate) titles: Vec<&'static str>,
    /// The front tab's index.
    pub(crate) front: usize,
    /// The front tab's panes' session keys, the focused one first ([`DeckInput::tab_panes`]).
    pub(crate) panes: Vec<u64>,
    /// Each agent session's need as the needs system publishes it, by session key. The busy
    /// mockup's three needs are the jobs engine's and the disk's, so no scene has one.
    pub(crate) needs: Vec<(u64, Need)>,
}

impl DeckContext {
    /// `scene`'s window.
    pub(crate) fn of(scene: DeckScene) -> Self {
        let tabs = fixtures::tabs(scene);
        let front = fixtures::front(scene);
        Self {
            titles: tabs.iter().map(|t| t.title).collect(),
            front,
            panes: tabs[front].panes.to_vec(),
            needs: Vec::new(),
        }
    }

    /// The deck's input for this window over `snap`: its marks are the snapshot's jobs', as
    /// [`crate::App::stats_marks`] reads them.
    pub(crate) fn input<'a>(
        &self,
        snap: Option<&'a Snapshot>,
        form: DeckForm,
        inks: VitalsInks,
    ) -> DeckInput<'a> {
        DeckInput {
            snap,
            tab_panes: self.panes.clone(),
            tab_label: format!("Tab {}", self.front + 1),
            tab_count: self.titles.len(),
            inks,
            dwc: false,
            marks: snap
                .and_then(|s| s.jobs.as_deref())
                .map(crate::stats_paint::marks_of)
                .unwrap_or_default(),
            plate: None,
            form,
            contrast: false,
            locale: Locale {
                platform: Platform::MacOs,
                utc_offset_s: 0,
            },
        }
    }
}

mod tests {
    use super::*;

    /// Each window's front tab is the snapshot's: its panes are the ones the scene's figures
    /// sum as this tab, and every pane the snapshot reads is one of the window's.
    #[test]
    fn the_window_is_the_snapshots() {
        for scene in DeckScene::ALL {
            let ctx = DeckContext::of(scene);
            let snap = fixtures::snapshot(scene);
            let panes: Vec<u64> = fixtures::tabs(scene)
                .iter()
                .flat_map(|t| t.panes.iter().copied())
                .collect();
            for (key, _) in &snap.resources[0].tabs {
                assert!(panes.contains(key), "{scene:?}: pane {key}");
            }
            assert!(ctx.needs.iter().all(|(k, _)| panes.contains(k)));
            let input = ctx.input(
                Some(&snap),
                DeckForm::Full,
                super::super::tests::dark_inks(),
            );
            assert_eq!(input.tab_panes, ctx.panes);
            assert_eq!(input.tab_count, 4);
            assert_eq!(
                !input.marks.is_empty(),
                scene == DeckScene::Busy,
                "{scene:?}"
            );
            if scene == DeckScene::Absent {
                continue;
            }
            for key in &ctx.panes {
                assert!(snap.tab(*key).is_some(), "{scene:?}: front pane {key}");
            }
        }
        assert_eq!(DeckContext::of(DeckScene::Busy).panes, [3, 5]);
        let calm = fixtures::snapshot(DeckScene::Calm);
        let input = DeckContext::of(DeckScene::Calm).input(
            Some(&calm),
            DeckForm::Full,
            super::super::tests::dark_inks(),
        );
        assert_eq!(input.tab_label, "Tab 2");
    }
}
