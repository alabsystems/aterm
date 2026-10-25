// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The window around each of the merged deck's scenes ([`aterm_vitals::fixtures`]): its tabs,
//! its front tab and that tab's panes, and the needs its agent sessions publish, handed to the
//! deck as the App hands a real window's ([`crate::App::deck_frame`]). The tests' deck reads it,
//! as do a debug build's deck scene (`ATERM_DEBUG_SCENE`, [`crate::icon_scene`]) and the
//! `deck_frame_cost` bench.

use aterm_vitals::fixtures::{self, DeckScene};
use aterm_vitals::glance::needs::{self, DeckNeed, DeckWindow};
use aterm_vitals::model::Snapshot;
#[cfg(any(test, feature = "bench-support"))]
use aterm_vitals::{
    ink::VitalsInks,
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
    /// The front tab's panes' session keys, the focused one first
    /// ([`super::DeckInput::tab_panes`]).
    pub(crate) panes: Vec<u64>,
    /// Every tab's panes, in the strip's order.
    pub(crate) strip: Vec<Vec<u64>>,
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
            strip: tabs.iter().map(|t| t.panes.to_vec()).collect(),
            needs: Vec::new(),
        }
    }

    /// What needs the owner in this window over `snap`, as [`crate::App::deck_needs`] reads a
    /// real window's: busy's disk, Tab 1's password and the front tab's stuck `node`.
    pub(crate) fn deck_needs(&self, snap: Option<&Snapshot>) -> Vec<DeckNeed> {
        let asks: Vec<_> = self
            .needs
            .iter()
            .map(|(pane, need)| crate::vitals_host::deck_ask(*pane, need.word))
            .collect();
        needs::of(
            snap,
            &DeckWindow {
                tabs: &self.strip,
                front: Some(self.front),
                asks: &asks,
            },
        )
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
            needs: self.deck_needs(snap),
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
            assert_eq!(
                ctx.strip.concat(),
                panes,
                "{scene:?}: every tab's panes, in order"
            );
            assert_eq!(ctx.strip[ctx.front], ctx.panes, "{scene:?}");
            let input = ctx.input(
                Some(&snap),
                DeckForm::Full,
                super::super::tests::dark_inks(),
            );
            assert_eq!(input.tab_panes, ctx.panes);
            assert_eq!(input.tab_count, 4);
            assert_eq!(input.needs, ctx.deck_needs(Some(&snap)), "{scene:?}");
            assert_eq!(
                input.needs.len(),
                if scene == DeckScene::Busy { 3 } else { 0 },
                "{scene:?}: busy's disk, Tab 1's password and the front tab's stuck node"
            );
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
            super::super::DeckStyle::V2,
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

    /// Draw `wid`'s Deck in `style`, its head sized for it.
    #[cfg(unix)]
    fn restyle(app: &mut crate::App, wid: crate::WindowId, style: super::super::DeckStyle) {
        app.config
            .sys
            .get_or_insert_with(Default::default)
            .stats_deck_style = Some(style.word().into());
        let (_, cell) = app.win_cell_size(wid);
        let ws = app.windows.get_mut(&wid).expect("window");
        let deck = super::super::deck_px(
            super::super::StatsBars::Deck,
            style,
            ws.deck_form,
            ws.scale,
            cell,
        );
        ws.metrics.head = deck;
        app.backend.set_head(deck);
    }

    /// A headless App whose one window draws the merged Deck.
    #[cfg(unix)]
    fn merged_app() -> (crate::App, crate::WindowId) {
        let (mut app, wid) = deck_app();
        restyle(&mut app, wid, super::super::DeckStyle::Merged);
        app.vitals.deck_scene = None;
        (app, wid)
    }

    /// Publish `word` as session 0's need (the headless window's one pane), or none.
    #[cfg(unix)]
    fn publish(app: &crate::App, word: Option<aterm_types::needs::NeedWord>) {
        let verdict = crate::needs::Verdict {
            need: word.map(|word| Need {
                word,
                key: 1,
                what: String::new(),
                harness: None,
            }),
            ..crate::needs::Verdict::default()
        };
        app.pool
            .get(0)
            .expect("session 0")
            .ctx
            .timeline
            .lock()
            .expect("the timeline")
            .set_need(verdict);
    }

    /// The merged deck's needs and its input key, as the next frame reads them.
    #[cfg(unix)]
    fn needs_and_key(app: &crate::App, wid: crate::WindowId) -> (Vec<DeckNeed>, u64) {
        let f = app.deck_frame(wid).expect("the Deck draws");
        let key = app.deck_input_key(&f);
        (f.needs, key)
    }

    /// A deck scene hands the merged deck its window's needs, as the gallery's input has them;
    /// v2 reads none.
    #[cfg(unix)]
    #[test]
    fn a_deck_scene_hands_the_merged_deck_its_windows_needs() {
        let _attached = super::super::tests::attach_stats();
        let (mut app, wid) = merged_app();
        for scene in DeckScene::ALL {
            let snap = std::sync::Arc::new(fixtures::snapshot(scene));
            app.vitals.snapshot = Some(std::sync::Arc::clone(&snap));
            app.vitals.deck_scene = Some(scene);
            let ctx = DeckContext::of(scene);
            let (needs, _) = needs_and_key(&app, wid);
            assert_eq!(needs, ctx.deck_needs(Some(&snap)), "{scene:?}");
            let lanes: Vec<_> = needs.iter().map(|n| n.lane).collect();
            match scene {
                DeckScene::Busy => assert_eq!(lanes, aterm_vitals::words::DeckScope::ALL),
                DeckScene::Calm | DeckScene::Absent => assert!(lanes.is_empty(), "{scene:?}"),
            }
        }
        restyle(&mut app, wid, super::super::DeckStyle::V2);
        assert_eq!(
            app.deck_frame(wid).map(|f| f.needs),
            Some(Vec::new()),
            "v2 draws no needs"
        );
    }

    /// A need the needs system publishes for a pane, then clears, re-keys the merged deck
    /// though no new reading arrived; the same needs keep the same key, and the cleared deck's is
    /// the calm one's again.
    #[cfg(unix)]
    #[test]
    fn a_cleared_need_rekeys_the_deck() {
        use aterm_types::needs::NeedWord;
        use aterm_vitals::words::DeckScope;

        let _attached = super::super::tests::attach_stats();
        let (app, wid) = merged_app();
        let seq = || app.vitals.snapshot.as_ref().map(|s| s.seq);
        let at = seq();
        let (calm, calm_key) = needs_and_key(&app, wid);
        assert_eq!(calm, Vec::new());
        publish(&app, Some(NeedWord::Approval));
        let (needs, key) = needs_and_key(&app, wid);
        let read: Vec<_> = needs
            .iter()
            .map(|n| (n.lane, n.tab, n.pane, n.pill().full))
            .collect();
        assert_eq!(
            read,
            [(
                DeckScope::ThisTab,
                Some(1),
                Some(0),
                "Tab 1 approval".to_string()
            )]
        );
        assert_ne!(key, calm_key, "the need keys the deck");
        assert_eq!(
            needs_and_key(&app, wid).1,
            key,
            "the same needs, the same key"
        );
        publish(&app, Some(NeedWord::Login));
        assert_ne!(needs_and_key(&app, wid).1, key, "another need, another key");
        publish(&app, None);
        let (cleared, cleared_key) = needs_and_key(&app, wid);
        assert_eq!(cleared, Vec::new());
        assert_ne!(cleared_key, key, "the cleared need re-keys the deck");
        assert_eq!(cleared_key, calm_key);
        assert_eq!(seq(), at, "no reading arrived");
    }

    /// A stall the needs system names (`frozen`) is the tab's lock until the jobs engine reads
    /// the pane's job stuck, then one pill, the engine's (`stuck`, spec §9), never both.
    /// CONTROL: an approval on the same pane is a pill of its own.
    #[cfg(unix)]
    #[test]
    fn a_frozen_pane_is_one_pill_in_the_engines_word() {
        use aterm_types::needs::NeedWord;
        use aterm_vitals::glance::needs::NeedKind;
        use aterm_vitals::jobs::{JobsView, Verdict};

        let _attached = super::super::tests::attach_stats();
        let (mut app, wid) = merged_app();
        app.vitals.snapshot = None;
        let read = |app: &crate::App| -> Vec<(NeedKind, Option<u64>, &'static str)> {
            let f = app.deck_frame(wid).expect("the Deck draws");
            f.needs.iter().map(|n| (n.kind, n.pane, n.word)).collect()
        };
        publish(&app, Some(NeedWord::Frozen));
        assert_eq!(read(&app), [(NeedKind::Lock, Some(0), "frozen")]);
        let mut snap = fixtures::snapshot(DeckScene::Busy);
        snap.facts.filling = None;
        let mut jobs = JobsView::clone(snap.jobs.as_deref().expect("busy's jobs"));
        jobs.rows.retain(|row| row.verdict == Verdict::Stuck);
        for row in &mut jobs.rows {
            row.tab = 0;
        }
        snap.jobs = Some(std::sync::Arc::new(jobs));
        app.vitals.snapshot = Some(std::sync::Arc::new(snap));
        assert_eq!(read(&app), [(NeedKind::Hourglass, Some(0), "stuck")]);
        publish(&app, Some(NeedWord::Approval));
        assert_eq!(
            read(&app),
            [
                (NeedKind::Lock, Some(0), "approval"),
                (NeedKind::Hourglass, Some(0), "stuck")
            ],
            "CONTROL"
        );
    }

    /// Needs are deck marks, never alerts (AGENTS.md; spec §6): as they appear and clear, and the
    /// deck lays and rasterizes them, nothing is posted, as a `Message::now` or an exceptions
    /// entry, and nothing ripens to be.
    #[cfg(unix)]
    #[test]
    fn a_need_posts_nothing() {
        use aterm_types::needs::NeedWord;
        use aterm_vitals::glance::needs::NeedKind;

        let _attached = super::super::tests::attach_stats();
        let (mut app, wid) = merged_app();
        let posted = |app: &crate::App| {
            (
                app.messages.log().records().count(),
                app.messages.live_rows().count(),
                app.ripening.len(),
            )
        };
        let before = posted(&app);
        let busy = std::sync::Arc::new(fixtures::snapshot(DeckScene::Busy));
        let lay = |app: &mut crate::App| {
            let _ = app.stats_deck_fp(wid);
            app.splice_stats_deck(wid);
            app.fold_stats_deck(wid);
            assert!(app.windows[&wid].deck_card.is_some(), "the deck was laid");
            let kinds: Vec<NeedKind> = app
                .deck_frame(wid)
                .expect("the Deck draws")
                .needs
                .iter()
                .map(|n| n.kind)
                .collect();
            assert_eq!(posted(app), before, "{kinds:?}: nothing posted");
            kinds
        };
        assert!(lay(&mut app).is_empty());
        publish(&app, Some(NeedWord::Approval));
        assert_eq!(lay(&mut app), [NeedKind::Lock]);
        app.vitals.snapshot = Some(std::sync::Arc::clone(&busy));
        assert_eq!(lay(&mut app), [NeedKind::Drive, NeedKind::Lock]);
        app.vitals.deck_scene = Some(DeckScene::Busy);
        assert_eq!(
            lay(&mut app),
            [NeedKind::Drive, NeedKind::Lock, NeedKind::Hourglass]
        );
        app.vitals.deck_scene = None;
        app.vitals.snapshot = None;
        publish(&app, None);
        assert!(lay(&mut app).is_empty());
    }
}
