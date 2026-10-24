// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The window around each of the merged deck's scenes ([`aterm_vitals::fixtures`]): its tabs,
//! its front tab and that tab's panes, and the needs its agent sessions publish, handed to the
//! deck as the App hands a real window's ([`crate::App::deck_frame`]). The tests' deck reads it,
//! as do a debug build's deck scene (`ATERM_DEBUG_SCENE`, [`crate::icon_scene`]) and the
//! `deck_frame_cost` bench.

use aterm_vitals::fixtures::{self, DeckScene};
#[cfg(any(test, feature = "bench-support"))]
use aterm_vitals::{
    ink::VitalsInks,
    model::Snapshot,
    words::{Locale, Platform},
};

#[cfg(any(test, feature = "bench-support"))]
use super::DeckInput;
use crate::needs::Need;
#[cfg(any(test, feature = "bench-support"))]
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

    /// The front tab as the deck takes it ([`super::DeckFrame`]): its panes, the focused one
    /// first; its label (`Tab 3`); and how many tabs the strip holds.
    pub(crate) fn front_tab(&self) -> (Vec<u64>, String, usize) {
        (
            self.panes.clone(),
            format!("Tab {}", self.front + 1),
            self.titles.len(),
        )
    }

    /// The deck's input for this window over `snap`: its marks are the snapshot's jobs', as
    /// [`crate::App::stats_marks`] reads them.
    #[cfg(any(test, feature = "bench-support"))]
    pub(crate) fn input<'a>(
        &self,
        snap: Option<&'a Snapshot>,
        form: DeckForm,
        inks: VitalsInks,
    ) -> DeckInput<'a> {
        let (tab_panes, tab_label, tab_count) = self.front_tab();
        DeckInput {
            snap,
            tab_panes,
            tab_label,
            tab_count,
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

#[cfg(test)]
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

    /// A headless App whose one window draws the Deck.
    #[cfg(unix)]
    fn deck_app() -> (crate::App, crate::WindowId) {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.config
            .sys
            .get_or_insert_with(Default::default)
            .stats_bars = Some("deck".into());
        let deck = super::super::deck_px(
            super::super::StatsBars::Deck,
            app.windows[&wid].deck_form,
            app.windows[&wid].scale,
            app.win_cell_size(wid).1,
        );
        app.windows.get_mut(&wid).expect("window").metrics.head = deck;
        app.backend.set_head(deck);
        (app, wid)
    }

    /// A deck scene hands the deck its window's front tab, the one its readings name
    /// (`ATERM_DEBUG_SCENE`, [`crate::icon_scene`]); with none the deck takes the window's own.
    #[cfg(unix)]
    #[test]
    fn a_deck_scene_hands_the_deck_its_windows_front_tab() {
        let _attached = super::super::tests::attach_stats();
        let (mut app, wid) = deck_app();
        let tab = |app: &crate::App| {
            let f = app.deck_frame(wid).expect("the Deck draws");
            (f.tab_panes, f.tab_label, f.tab_count)
        };
        app.vitals.deck_scene = None;
        let own = tab(&app);
        assert_eq!(own.1, "Tab 1");
        assert_eq!(own.2, 1);
        for scene in DeckScene::ALL {
            app.vitals.deck_scene = Some(scene);
            assert_eq!(tab(&app), DeckContext::of(scene).front_tab(), "{scene:?}");
        }
        assert_eq!(tab(&app), (vec![3, 5], "Tab 3".to_string(), 4));
        app.vitals.deck_scene = None;
        assert_eq!(tab(&app), own);
    }

    /// The lens's sentence (⌘C) names the deck scene's front tab, as the drawn lens does.
    /// CONTROL: the window's own pane, read against the scene, names another tab.
    #[cfg(unix)]
    #[test]
    fn a_deck_scenes_lens_sentence_names_its_front_tab() {
        use crate::stats_paint::{Bar, Instrument};
        let _attached = super::super::tests::attach_stats();
        let (mut app, wid) = deck_app();
        app.vitals.deck_scene = None;
        let own = app.deck_tab_panes(wid);
        let snap = std::sync::Arc::new(fixtures::snapshot(DeckScene::Busy));
        app.vitals.snapshot = Some(std::sync::Arc::clone(&snap));
        app.vitals.deck_scene = Some(DeckScene::Busy);
        app.open_stats_lens(
            wid,
            Instrument::Cpu,
            Bar::Global,
            crate::stats_lens::Opened::Pinned,
        );
        let said = app.stats_lens_sentence(wid).expect("the lens's sentence");
        let loc = Locale {
            platform: Platform::host(),
            utc_offset_s: crate::presence::local_offset_s().unwrap_or(0),
        };
        let of =
            |panes: &[u64]| crate::stats_lens::lens_sentence(&snap, Instrument::Cpu, panes, &loc);
        assert_eq!(said, of(&DeckContext::of(DeckScene::Busy).panes));
        assert!(said.contains("Tab 3"), "{said}");
        assert_ne!(said, of(&own), "CONTROL: the window's own {own:?}");
    }
}
