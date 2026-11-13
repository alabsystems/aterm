// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_vitals::model::{
    ClusterSpec, GpuSample, MemSample, Pressure, ProcSample, Sample, TabRoot, Ticks, VolumeSample,
};

#[cfg(unix)]
#[test]
fn place_only_changes_rekey_and_repaint_the_deck_from_one_captured_caption() {
    let _attached = attach_stats();
    let _place = crate::net_place::hold_scoped(Some(aterm_netplace::sample::home(1000, 1)));
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    app.config
        .sys
        .get_or_insert_with(Default::default)
        .stats_bars = Some("deck".into());
    let head = reserved_deck_px(
        StatsBars::Deck,
        app.windows[&wid].deck_form,
        app.windows[&wid].scale,
        app.win_cell_size(wid).1,
    ) + 14;
    {
        let metrics = &mut app.windows.get_mut(&wid).unwrap().metrics;
        metrics.head = head;
        metrics.pad = 12;
        metrics.pad_top = 2;
    }
    app.backend.set_head(head);
    app.backend.set_pad(12);
    app.backend.set_pad_top(2);
    let grid = (app.windows[&wid].cols, app.windows[&wid].rows);
    let network = app.network_snapshot.clone();
    let before = app.deck_frame(wid).unwrap();
    let before_key = app.deck_input_key(&before);
    let before_prims = prims_fp(&deck_prims(&before.geom, &app.deck_input(&before)));
    app.splice_stats_deck(wid);
    let before_pixels = app.windows[&wid].deck_card.as_ref().unwrap().rgba.clone();

    let mut away = aterm_netplace::sample::hotspot(2000, 2000);
    crate::net_place::hold(Some(away.clone()));
    // The same frame must keep its original words even if the next observation arrives.
    assert_eq!(app.deck_input_key(&before), before_key);
    assert_eq!(
        prims_fp(&deck_prims(&before.geom, &app.deck_input(&before))),
        before_prims
    );
    let after = app.deck_frame(wid).unwrap();
    assert_ne!(app.deck_input_key(&after), before_key);
    assert_ne!(
        prims_fp(&deck_prims(&after.geom, &app.deck_input(&after))),
        before_prims
    );
    app.splice_stats_deck(wid);
    assert_ne!(
        app.windows[&wid].deck_card.as_ref().unwrap().rgba,
        before_pixels
    );
    assert!(std::sync::Arc::ptr_eq(&app.network_snapshot, &network));
    assert!(app.vitals.snapshot.is_none());
    assert_eq!((app.windows[&wid].cols, app.windows[&wid].rows), grid);

    // Times not included in either rendered caption do not invalidate the card.
    let after_key = app.deck_input_key(&after);
    let after_pixels = app.windows[&wid].deck_card.as_ref().unwrap().rgba.clone();
    away.since_ms = Some(9000);
    away.alab = aterm_netplace::Alab::Away {
        since_ms: Some(8000),
    };
    crate::net_place::hold(Some(away));
    let unchanged = app.deck_frame(wid).unwrap();
    assert_eq!(unchanged.network_place, after.network_place);
    assert_eq!(app.deck_input_key(&unchanged), after_key);
    app.splice_stats_deck(wid);
    assert_eq!(
        app.windows[&wid].deck_card.as_ref().unwrap().rgba,
        after_pixels
    );

    // A resource-only sill has no Network row whose caption could move.
    app.config.sys.as_mut().unwrap().stats_bars = Some("sill".into());
    let sill = app.deck_frame(wid).unwrap();
    assert_eq!(sill.form, StatsBars::Sill);
    assert!(sill.network_place.is_none());
    let sill_key = app.deck_input_key(&sill);
    app.splice_stats_deck(wid);
    let sill_pixels = app.windows[&wid].deck_card.as_ref().unwrap().rgba.clone();
    crate::net_place::hold(Some(aterm_netplace::sample::home(1000, 2)));
    let same_sill = app.deck_frame(wid).unwrap();
    assert_eq!(same_sill.form, StatsBars::Sill);
    assert!(same_sill.network_place.is_none());
    assert_eq!(app.deck_input_key(&same_sill), sill_key);
    app.splice_stats_deck(wid);
    assert_eq!(
        app.windows[&wid].deck_card.as_ref().unwrap().rgba,
        sill_pixels
    );
    assert!(std::sync::Arc::ptr_eq(&app.network_snapshot, &network));
    assert!(app.vitals.snapshot.is_none());
    assert_eq!((app.windows[&wid].cols, app.windows[&wid].rows), grid);
}

#[test]
fn changing_the_relay_source_invalidates_the_resource_deck() {
    let mut app = crate::App::headless_for_test();
    let wid = *app.windows.keys().next().expect("window");
    let frame = app.network_lens_frame(wid).expect("network frame");
    let before = app.deck_input_key(&frame);
    let mut snapshot = app.network_snapshot.as_ref().clone();
    snapshot.network.via_local_process = Some(42);
    app.network_snapshot = std::sync::Arc::new(snapshot);
    assert_ne!(app.deck_input_key(&frame), before);
}

#[test]
fn retention_boundary_invalidates_the_deck_without_a_revision_change() {
    let mut app = crate::App::headless_for_test();
    let wid = *app.windows.keys().next().expect("window");
    let frame = app.network_lens_frame(wid).expect("network frame");
    let before = app.deck_input_key(&frame);
    let original = app.network_snapshot.as_ref().clone();
    let mut snapshot = original.clone();
    snapshot.network.retention_limited = !snapshot.network.retention_limited;
    assert_eq!(snapshot.revision, original.revision);
    assert_eq!(snapshot.network.revision, original.network.revision);
    app.network_snapshot = std::sync::Arc::new(snapshot);
    assert_ne!(app.deck_input_key(&frame), before);
}

#[test]
fn service_expiry_changes_the_resource_deck_cache_key_without_a_source_revision() {
    let mut app = crate::App::headless_for_test();
    let wid = *app.windows.keys().next().expect("window");
    let mut snapshot = aterm_types::network_state::NetworkStateSnapshot::default();
    snapshot.services.secrets.availability =
        aterm_types::network_state::ServiceAvailability::Cached;
    snapshot.services.secrets.observed_at_ms = 1000;
    app.network_snapshot = std::sync::Arc::new(snapshot);
    let frame = app.network_lens_frame(wid).expect("network frame");
    let before = app.deck_input_key(&frame);
    app.network_snapshot =
        crate::network_deck::fresh_snapshot(app.network_snapshot.clone(), Some(61_001));
    assert_eq!(app.network_snapshot.revision, 0);
    assert_ne!(app.deck_input_key(&frame), before);
}

#[test]
fn peer_expiry_changes_the_resource_deck_cache_key_without_a_source_revision() {
    use aterm_types::network_state::{MachineIdentity, PeerObservation, Reachability, Visibility};
    let mut app = crate::App::headless_for_test();
    let wid = *app.windows.keys().next().expect("window");
    let mut snapshot = aterm_types::network_state::NetworkStateSnapshot::default();
    snapshot.network.visibility = Visibility::Active;
    snapshot.network.observed_at_ms = 10_000;
    snapshot.network.peers.push(PeerObservation {
        slot: 1,
        identity: MachineIdentity {
            machine_id: [1; 16],
            incarnation: [2; 16],
            label: None,
        },
        first_seen_ms: 1_000,
        last_seen_ms: 2_000,
        last_checked_ms: Some(2_000),
        last_repair: None,
        last_authenticated_endpoint: None,
        reachability: Reachability::Reachable,
        instances: None,
        samples: Vec::new(),
        lease_ms: None,
    });
    app.network_snapshot = std::sync::Arc::new(snapshot);
    let frame = app.network_lens_frame(wid).expect("network frame");
    let before = app.deck_input_key(&frame);
    app.network_snapshot =
        crate::network_deck::fresh_snapshot(app.network_snapshot.clone(), Some(17_001));
    assert_eq!(app.network_snapshot.revision, 0);
    assert_eq!(app.network_snapshot.network.revision, 0);
    assert_ne!(app.deck_input_key(&frame), before);
}

/// Gives this test thread's windows the stats bars, as glass gives a
/// real window ([`stats_drawn_on`]), until the guard drops.
#[must_use]
pub(crate) fn attach_stats() -> Attached {
    TESTS_ATTACHED.with(|c| c.set(true));
    Attached
}

/// [`attach_stats`]'s guard: dropping it takes the bars away again.
pub(crate) struct Attached;

impl Drop for Attached {
    fn drop(&mut self) {
        TESTS_ATTACHED.with(|c| c.set(false));
    }
}

#[test]
fn only_a_change_into_or_out_of_the_deck_moves_the_head() {
    const CELL: usize = 28;
    let head = |bars, deck| chrome_head_px(28.0, 2.0, bars, deck, CELL);
    for deck in [DeckForm::Folded, DeckForm::Full] {
        for a in StatsBars::ALL {
            for b in StatsBars::ALL {
                let moved = head(a, deck) != head(b, deck);
                assert_eq!(
                    a.takes_room() != b.takes_room(),
                    moved,
                    "{a:?} to {b:?}, {deck:?}"
                );
            }
        }
    }
    for bars in StatsBars::ALL {
        let unfold = head(bars, DeckForm::Full) - head(bars, DeckForm::Folded);
        assert_eq!(unfold != 0, bars.takes_room(), "{bars:?}");
        assert_eq!(unfold % CELL, 0, "{bars:?}");
    }
    assert_eq!(
        head(StatsBars::Sill, DeckForm::Folded),
        head(StatsBars::Off, DeckForm::Full)
    );
}

fn set_size(app: &mut crate::App, pref: DeckSizePref) {
    app.config
        .sys
        .get_or_insert_with(Default::default)
        .stats_deck_size = Some(pref.word().into());
}

#[test]
fn head_sites_agree() {
    use aterm_window::dpi::PhysicalSize;
    let _attached = attach_stats();
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let band_pts = 28.0;
    let mut cells = std::collections::BTreeSet::new();
    for scale in [1.0, 2.0] {
        let band = band_px(band_pts, scale);
        for font_px in [8.0, 11.0, 14.0, 18.0, 23.0, 28.0, 35.0] {
            {
                let ws = app.windows.get_mut(&wid).expect("the window");
                ws.scale = scale;
                ws.head_pts = band_pts;
                ws.metrics.font_px = font_px;
            }
            let (_, ch) = app.win_cell_size(wid);
            cells.insert(ch);
            let at = format!("{ch} px cells at {scale}×");
            let mut heads = Vec::new();
            // Full only where chosen, whatever the window's height.
            let frames = [
                (DeckSizePref::Auto, DeckForm::Folded),
                (DeckSizePref::Full, DeckForm::Full),
            ]
            .map(|(pref, want)| (band + ((FOLDED_PT + 900.0) * scale) as usize, pref, want));
            for (frame, pref, want) in frames {
                set_size(&mut app, pref);
                let head = app.deck_head_px(wid, band_pts, scale, frame as u32);
                let ws = app.windows.get_mut(&wid).expect("the window");
                assert_eq!(ws.deck_form, want, "{at}");
                ws.metrics.head = head;
                app.backend.set_head(head);
                let f = app.deck_frame(wid).expect("the Deck shows");
                let (above, below) = if crate::tab_bar::STRIP_IS_CHROME_BAND {
                    (0, band)
                } else {
                    (band, 0)
                };
                assert_eq!(f.dy as usize, above, "{at}");
                assert_eq!(f.geom.h as usize + band, head, "{at}");
                assert_eq!(
                    f.geom.h as usize,
                    reserved_deck_px(StatsBars::Deck, want, scale, ch),
                    "{at}"
                );
                assert_eq!(app.deck_drawn_px(wid), f.geom.h as usize, "{at}");
                assert_eq!(
                    f.dy as usize + f.geom.h as usize + below,
                    app.win_head(wid),
                    "the grid starts under the card and the band, {at}"
                );
                if crate::tab_bar::STRIP_IS_CHROME_BAND {
                    assert_eq!(
                        app.strip_band_top_px(wid),
                        app.win_pad_top(wid) + band,
                        "the band starts where the card ends, {at}"
                    );
                }
                heads.push(head);
            }
            let (folded, full) = (heads[0], heads[1]);
            assert_eq!((full - folded) % ch, 0, "{at}");
            let lost = (full - folded) / ch;
            let fit = |app: &mut crate::App, head: usize, h: usize| {
                app.windows.get_mut(&wid).expect("the window").metrics.head = head;
                app.backend.set_head(head);
                let size = PhysicalSize::new(800, h as u32);
                let (rows, _) = app.grid_dims_for(wid, size);
                (rows, app.least_window_size(wid).height)
            };
            let low = full + 8 * ch;
            for h in low..low + 3 * ch {
                let (folded_rows, folded_min) = fit(&mut app, folded, h);
                let (full_rows, full_min) = fit(&mut app, full, h);
                assert_eq!(
                    usize::from(folded_rows - full_rows),
                    lost,
                    "a {h} px frame, {at}"
                );
                assert_eq!((full_min - folded_min) % ch as u32, 0, "{at}");
            }
        }
    }
    assert!(
        cells.first().is_some_and(|c| *c <= 12) && cells.last().is_some_and(|c| *c >= 40),
        "the cells span 12 to 40 px: {cells:?}"
    );
}

/// `stats_deck_size` forces the form whatever the window's height; auto keeps the Deck
/// Folded at every height; a frame of 0 keeps the form.
#[test]
fn the_deck_size_setting_forces_the_form() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let form = |app: &crate::App| app.windows[&wid].deck_form;
    let (_, ch) = app.win_cell_size(wid);
    let (tall, short) = (56 + 124 + 1800, 56 + 124 + 1400);
    for (word, frame, want) in [
        ("folded", tall, DeckForm::Folded),
        ("full", short, DeckForm::Full),
        ("auto", 0, DeckForm::Full),
        ("auto", short, DeckForm::Folded),
        ("full", tall, DeckForm::Full),
        ("auto", tall, DeckForm::Folded),
        ("auto", 0, DeckForm::Folded),
        ("auto", short, DeckForm::Folded),
    ] {
        let at = format!("{word} at {frame} px");
        app.config
            .sys
            .get_or_insert_with(Default::default)
            .stats_deck_size = Some(word.into());
        let head = app.deck_head_px(wid, 28.0, 2.0, frame);
        assert_eq!(form(&app), want, "{at}");
        assert_eq!(
            head,
            56 + reserved_deck_px(StatsBars::Deck, want, 2.0, ch),
            "{at}"
        );
    }
}

/// A restored window keeps the frame the Deck sized for it (Folded at 62 pt, or Full). It
/// takes the form the setting gives, Folded unless full, whatever form sized it, and its
/// grid follows the frame: a window Full sized under the old Automatic gets the rows Full
/// took back.
#[test]
fn a_restored_window_keeps_its_size() {
    use aterm_window::dpi::PhysicalSize;
    let _attached = attach_stats();
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let (band_pts, scale) = (28.0, 2.0);
    {
        let ws = app.windows.get_mut(&wid).expect("the window");
        ws.scale = scale;
        ws.head_pts = band_pts;
    }
    let (_, cell) = app.win_cell_size(wid);
    let cols = 80;
    let px = |deck| deck_px(StatsBars::Deck, deck, scale, cell);
    let full_rows = (px(DeckForm::Full) - px(DeckForm::Folded)) / cell;
    assert!(full_rows > 0, "PRECONDITION: Full takes whole rows");
    let rows_in = |form: DeckForm| match form {
        DeckForm::Full => full_rows as i32,
        DeckForm::Folded => 0,
    };
    let form = |app: &crate::App| app.windows[&wid].deck_form;
    let frame_in = |app: &mut crate::App, deck: DeckForm, rows: u16| {
        app.windows.get_mut(&wid).expect("the window").deck_form = deck;
        let head = app.deck_head_px(wid, band_pts, scale, 0);
        app.backend.set_head(head);
        app.window_frame_px_for(wid, rows, cols)
    };
    let attach = |app: &mut crate::App, rows: u16, carried: Option<PhysicalSize<u32>>| {
        app.windows.get_mut(&wid).expect("the window").deck_form = DeckForm::Folded;
        let head = app.attach_deck_head(wid, band_pts, scale);
        app.windows.get_mut(&wid).expect("the window").metrics.head = head;
        app.attach_frame_px(wid, rows, cols, carried)
    };
    let rem = (cell / 2) as u32;
    for rows in [20, 57, 120] {
        for pref in DeckSizePref::ALL {
            set_size(&mut app, pref);
            for deck in [DeckForm::Full, DeckForm::Folded] {
                let frame = frame_in(&mut app, deck, rows);
                let carried = PhysicalSize::new(frame.width, frame.height + rem);
                let at = format!("{pref:?}: a {deck:?} frame of {rows} rows");
                let size = attach(&mut app, rows, Some(carried));
                let want = match pref {
                    DeckSizePref::Full => DeckForm::Full,
                    DeckSizePref::Auto | DeckSizePref::Folded => DeckForm::Folded,
                };
                assert_eq!(form(&app), want, "{at}");
                assert_eq!(size, carried, "the window keeps its size, {at}");
                let held = app.grid_dims_for(wid, size);
                assert_eq!(held.1, cols, "{at}");
                let gained = rows_in(deck) - rows_in(want);
                assert_eq!(
                    i32::from(held.0) - i32::from(rows),
                    gained,
                    "the grid takes back what Full took, {at}"
                );
                let _ = attach(&mut app, rows, None);
                assert_eq!(form(&app), want, "a new window of that grid, {at}");
            }
        }
    }
}

/// A carried frame no deck sized (one from another display scale, twice or half the size
/// its grid takes here, or one with no deck in its height) is never kept: the window opens
/// in the form and at the size it would with nothing carried. CONTROL: a frame the Full
/// deck sized, under Automatic, is kept at its own size.
#[test]
fn a_frame_no_deck_sized_sizes_the_deck_as_nothing_carried() {
    use aterm_window::dpi::PhysicalSize;
    let _attached = attach_stats();
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let (band_pts, scale, cols) = (28.0, 2.0, 80);
    {
        let ws = app.windows.get_mut(&wid).expect("the window");
        ws.scale = scale;
        ws.head_pts = band_pts;
    }
    let (_, cell) = app.win_cell_size(wid);
    let band = band_px(band_pts, scale);
    let attach = |app: &mut crate::App, rows, carried: Option<PhysicalSize<u32>>| {
        app.windows.get_mut(&wid).expect("the window").deck_form = DeckForm::Folded;
        let head = app.attach_deck_head(wid, band_pts, scale);
        app.windows.get_mut(&wid).expect("the window").metrics.head = head;
        let size = app.attach_frame_px(wid, rows, cols, carried);
        (app.windows[&wid].deck_form, size)
    };
    for rows in 3..=16u16 {
        let (form, size) = attach(&mut app, rows, None);
        let bare = size.height as usize - app.backend.head();
        let doubled = PhysicalSize::new(size.width * 2, size.height * 2);
        let halved = PhysicalSize::new(size.width / 2, size.height / 2);
        let no_deck = PhysicalSize::new(size.width, (bare + band) as u32);
        for carried in [doubled, halved, no_deck] {
            let at = format!("{rows} rows, carried {carried:?}");
            assert_eq!(attach(&mut app, rows, Some(carried)), (form, size), "{at}");
        }
        let full =
            (bare + chrome_head_px(band_pts, scale, StatsBars::Deck, DeckForm::Full, cell)) as u32;
        let full = PhysicalSize::new(size.width, full);
        assert_eq!(
            attach(&mut app, rows, Some(full)),
            (form, full),
            "CONTROL: {rows} rows, a frame the Full deck sized is kept"
        );
    }
}

/// A new window (nothing carried) opens Folded under Automatic and holds the grid it was
/// opened for.
#[test]
fn a_new_window_opens_folded_and_holds_its_grid() {
    let _attached = attach_stats();
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let (band_pts, scale, cols) = (28.0, 2.0, 80);
    {
        let ws = app.windows.get_mut(&wid).expect("the window");
        ws.scale = scale;
        ws.head_pts = band_pts;
    }
    for rows in 3..=16u16 {
        let at = format!("a new window of {rows} rows");
        app.windows.get_mut(&wid).expect("the window").deck_form = DeckForm::Full;
        let head = app.attach_deck_head(wid, band_pts, scale);
        app.windows.get_mut(&wid).expect("the window").metrics.head = head;
        assert_eq!(app.windows[&wid].deck_form, DeckForm::Folded, "{at}");
        let size = app.attach_frame_px(wid, rows, cols, None);
        assert_eq!(app.grid_dims_for(wid, size), (rows, cols), "{at}");
    }
}

/// A window the Deck sized Full, attached under Automatic (which is Folded), keeps its
/// frame, and every tab of it re-grids once to what that frame holds, booked `site=deck`.
/// A window sized in the form it takes re-grids nothing.
#[test]
fn a_frame_the_other_form_sized_regrids_each_tab_once_at_its_attach() {
    for deck in [DeckForm::Full, DeckForm::Folded] {
        let at = format!("a {deck:?} frame attached under Automatic");
        let _attached = attach_stats();
        let (mut app, wid, sessions) = three_panes();
        let (band_pts, scale) = (28.0, 2.0);
        {
            let ws = app.windows.get_mut(&wid).expect("the window");
            ws.scale = scale;
            ws.head_pts = band_pts;
        }
        let (rows, cols) = (app.windows[&wid].rows, app.windows[&wid].cols);
        app.windows.get_mut(&wid).expect("the window").deck_form = deck;
        let head = app.deck_head_px(wid, band_pts, scale, 0);
        app.backend.set_head(head);
        let carried = app.window_frame_px_for(wid, rows, cols);
        let before = sites_of(&app, &sessions);
        let head = app.attach_deck_head(wid, band_pts, scale);
        app.windows.get_mut(&wid).expect("the window").metrics.head = head;
        let size = app.attach_frame_px(wid, rows, cols, Some(carried));
        assert_eq!(size, carried, "the window keeps its frame, {at}");
        let held = app.grid_dims_for(wid, size);
        app.regrid_to_attach_frame(wid, size);
        app.regrid_to_attach_frame(wid, size);
        let ws = &app.windows[&wid];
        assert_eq!((ws.rows, ws.cols), held, "{at}");
        let booked = new_sites(&before, &sites_of(&app, &sessions));
        assert_eq!(app.windows[&wid].deck_form, DeckForm::Folded, "{at}");
        if deck == DeckForm::Folded {
            assert_eq!(held, (rows, cols), "{at}");
            assert!(booked.iter().all(Vec::is_empty), "{booked:?}, {at}");
        } else {
            assert_ne!(held, (rows, cols), "PRECONDITION: the head moved, {at}");
            assert!(
                booked.iter().all(|b| b == &["site=deck"]),
                "one re-grid each, booked deck: {booked:?}, {at}"
            );
        }
    }
}

/// A v0.147.0 parent (the last whose message band took rows) sized its window for its grid
/// plus the band's 1 to 3 rows, which no carry now names. Where those rows come to exactly
/// what the Full deck adds to the Folded one, the attach reads the excess as a Full deck over
/// a window that takes Folded: the frame is kept, and each tab re-grids once to the rows it
/// holds, booked `site=deck`. Every other count takes the exact frame and re-grids nothing.
#[test]
fn a_v0_147_band_excess_reads_as_a_deck_only_where_it_matches_the_full_form() {
    use aterm_window::dpi::PhysicalSize;
    let (mut misread, mut exact_taken) = (0, 0);
    for scale in [1.0, 2.0] {
        for band in 1..=aterm_messages::MAX_ROWS {
            let at = format!("{band} band row(s) at {scale}x");
            let _attached = attach_stats();
            let (mut app, wid, sessions) = three_panes();
            let band_pts = 28.0;
            {
                let ws = app.windows.get_mut(&wid).expect("the window");
                ws.scale = scale;
                ws.head_pts = band_pts;
            }
            let (rows, cols) = (app.windows[&wid].rows, app.windows[&wid].cols);
            app.windows.get_mut(&wid).expect("the window").deck_form = DeckForm::Folded;
            let head = app.deck_head_px(wid, band_pts, scale, 0);
            app.backend.set_head(head);
            let (_, cell) = app.win_cell_size(wid);
            let exact = app.window_frame_px_for(wid, rows, cols);
            let band_px = u32::from(band) * u32::try_from(cell).expect("a cell's height");
            let carried = PhysicalSize::new(exact.width, exact.height + band_px);
            let full_over_folded =
                chrome_head_px(band_pts, scale, StatsBars::Deck, DeckForm::Full, cell)
                    - chrome_head_px(band_pts, scale, StatsBars::Deck, DeckForm::Folded, cell);
            let before = sites_of(&app, &sessions);
            let head = app.attach_deck_head(wid, band_pts, scale);
            app.windows.get_mut(&wid).expect("the window").metrics.head = head;
            let size = app.attach_frame_px(wid, rows, cols, Some(carried));
            app.regrid_to_attach_frame(wid, size);
            let booked = new_sites(&before, &sites_of(&app, &sessions));
            let grid = (app.windows[&wid].rows, app.windows[&wid].cols);
            if usize::from(band) * cell == full_over_folded {
                misread += 1;
                assert_eq!(size, carried, "the frame is kept, {at}");
                assert_eq!(
                    grid,
                    (rows + band, cols),
                    "the band's rows join the grid, {at}"
                );
                assert!(
                    booked.iter().all(|b| b == &["site=deck"]),
                    "one re-grid each, booked deck: {booked:?}, {at}"
                );
            } else {
                exact_taken += 1;
                assert_eq!(size, exact, "the exact frame, {at}");
                assert_eq!(grid, (rows, cols), "{at}");
                assert!(booked.iter().all(Vec::is_empty), "{booked:?}, {at}");
            }
        }
    }
    assert!(
        misread > 0 && exact_taken > 0,
        "PRECONDITION: both readings reached: {misread} misread, {exact_taken} exact"
    );
}

/// A window of two tabs, the second split: three panes, each its own session.
fn three_panes() -> (crate::App, crate::WindowId, Vec<u64>) {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let first = app
        .focused_session_id(wid)
        .expect("the first tab's session");
    let second = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(second));
    let split = app.split_active_stub_tab(wid);
    let sessions = vec![first, second, split];
    assert_eq!(app.windows[&wid].tab_set.tabs().len(), 2, "PRECONDITION");
    (app, wid, sessions)
}

/// Each session's resize rows' `site=` words, oldest first.
fn sites_of(app: &crate::App, sessions: &[u64]) -> Vec<Vec<String>> {
    sessions
        .iter()
        .map(|id| {
            let reply = app
                .pool
                .get(*id)
                .expect("a live session")
                .ctx
                .timeline
                .lock()
                .unwrap()
                .resizes()
                .reply(0, None);
            reply
                .lines()
                .filter(|l| l.starts_with("resize "))
                .filter_map(|l| l.split(' ').find(|w| w.starts_with("site=")))
                .map(str::to_owned)
                .collect()
        })
        .collect()
}

/// What each session booked since `before`.
fn new_sites(before: &[Vec<String>], after: &[Vec<String>]) -> Vec<Vec<String>> {
    before
        .iter()
        .zip(after)
        .map(|(b, a)| a[b.len()..].to_vec())
        .collect()
}

/// A switch of the deck's size in the config re-grids every tab of the window once,
/// booked `site=deck`, and a reload that keeps the size re-grids none. A headless
/// window's head is the backend's, so the test moves it to the new size's deck as
/// `refresh_all_window_metrics` moves an attached window's during the reload.
#[test]
fn a_size_switch_regrids_each_tab_once_booked_deck() {
    use aterm_window::dpi::PhysicalSize;
    let _attached = attach_stats();
    let (mut app, wid, sessions) = three_panes();
    let scratch = aterm_tempfile::tempdir().expect("scratch dir");
    let dir = scratch.path().to_path_buf();
    let text = |pref: DeckSizePref, bars: &str| {
        format!("[sys]\n{bars}stats_deck_size = \"{}\"\n", pref.word())
    };
    let apply = |app: &mut crate::App, text: String| {
        crate::app_colorscheme::install_resolved_colors::apply_config_text(app, &dir, &text);
    };
    let head_of = |app: &mut crate::App, pref: DeckSizePref| {
        let (_, cell) = app.win_cell_size(wid);
        let ws = app.windows.get_mut(&wid).expect("the window");
        ws.deck_form = plan::deck_form_for(pref);
        let head = chrome_head_px(0.0, ws.scale, StatsBars::Deck, ws.deck_form, cell);
        ws.metrics.head = head;
        app.backend.set_head(head);
    };
    let size = PhysicalSize::new(1600, 1400);
    app.windows.get_mut(&wid).expect("the window").win_px = Some(size);
    apply(&mut app, text(DeckSizePref::Auto, ""));
    head_of(&mut app, DeckSizePref::Auto);
    app.on_resize(wid, size);
    let folded_grid = app.grid_dims_for(wid, size);
    assert_eq!(
        (app.windows[&wid].rows, app.windows[&wid].cols),
        folded_grid
    );
    let deck = "stats_bars = \"deck\"\n";
    for (pref, bars, regrids) in [
        (DeckSizePref::Full, "", true),
        (DeckSizePref::Full, deck, false),
        (DeckSizePref::Auto, deck, true),
        (DeckSizePref::Auto, "", false),
    ] {
        let at = format!("to {pref:?} {bars:?}");
        let before = sites_of(&app, &sessions);
        let grid = (app.windows[&wid].rows, app.windows[&wid].cols);
        head_of(&mut app, pref);
        apply(&mut app, text(pref, bars));
        assert_eq!(app.deck_size_pref(), pref, "{at}");
        let booked = new_sites(&before, &sites_of(&app, &sessions));
        let now = (app.windows[&wid].rows, app.windows[&wid].cols);
        if regrids {
            assert_eq!(now, app.grid_dims_for(wid, size), "{at}");
            assert_ne!(now, grid, "PRECONDITION: the head moved the grid, {at}");
            assert!(
                booked.iter().all(|b| b == &["site=deck"]),
                "one re-grid each, booked deck: {booked:?}, {at}"
            );
        } else {
            assert_eq!(now, grid, "{at}");
            assert!(booked.iter().all(Vec::is_empty), "{booked:?}, {at}");
        }
    }
    assert_eq!(
        (app.windows[&wid].rows, app.windows[&wid].cols),
        folded_grid,
        "back where it started"
    );
}

/// A snapshot after three readings: ten cores in two clusters, memory, a
/// GPU, a volume, aterm with one tab running claude.
pub(crate) fn snapshot() -> Snapshot {
    let mut v = aterm_vitals::Vitals::new();
    let mk = sample_at;
    let _ = v.absorb(&mk(10, &[0; 10], &[0, 0]));
    let _ = v.absorb(&mk(11, &[20, 30, 10, 0, 5, 5, 5, 5, 5, 5], &[100, 400]));
    v.absorb(&mk(
        12,
        &[120, 130, 60, 10, 10, 10, 10, 10, 10, 10],
        &[200, 1200],
    ))
}

/// [`snapshot`]'s reading at `t` s: each core's busy ticks `busy`, aterm's and claude's CPU
/// ms `cpu`, the GPU 12 % busy, and no GPU client read.
pub(crate) fn sample_at(t: u64, busy: &[u64], cpu: &[u64]) -> Sample {
    let tabs = vec![TabRoot {
        key: 7,
        tab: 7,
        shell_pid: 200,
        label: "Tab 1".into(),
        program: "claude".into(),
        tab_id: None,
        agent_leader: None,
    }];
    Sample {
        at_ns: t * 1_000_000_000,
        cores: Some(
            busy.iter()
                .map(|&b| Ticks {
                    busy: b,
                    total: t * 100,
                })
                .collect(),
        ),
        clusters: vec![
            ClusterSpec {
                name: "Super".into(),
                logical: vec![0, 1, 2, 3],
            },
            ClusterSpec {
                name: "Efficiency".into(),
                logical: vec![4, 5, 6, 7, 8, 9],
            },
        ],
        mem: Some(MemSample {
            total: 32 << 30,
            app: 12 << 30,
            wired: 4 << 30,
            compressed: Some(1 << 30),
            cached: 8 << 30,
            free: 7 << 30,
            swap_used: 0,
            swap_total: 0,
            pressure: Pressure::Normal,
            commit: None,
            ..MemSample::default()
        }),
        gpu: Some(GpuSample {
            device_pct: Some(12),
            cores: Some(10),
            ..GpuSample::default()
        }),
        volume: Some(VolumeSample {
            total: 2_000_000_000_000,
            free: 1_810_000_000_000,
            reserved: 0,
            mount: "/".into(),
        }),
        procs: Some(vec![
            ProcSample {
                start: Some(1),
                pid: 100,
                ppid: 1,
                cpu_ns: Some(cpu[0] * 1_000_000),
                footprint: Some(300 << 20),
                name: "aterm".into(),
                disk_written: None,
                uid: None,
                ..ProcSample::default()
            },
            ProcSample {
                start: Some(1),
                pid: 200,
                ppid: 100,
                cpu_ns: Some(0),
                footprint: Some(5 << 20),
                name: "zsh".into(),
                disk_written: None,
                uid: None,
                ..ProcSample::default()
            },
            ProcSample {
                start: Some(1),
                pid: 201,
                ppid: 200,
                cpu_ns: Some(cpu[1] * 1_000_000),
                footprint: Some(400 << 20),
                name: "claude".into(),
                disk_written: None,
                uid: None,
                ..ProcSample::default()
            },
        ]),
        self_pid: 100,
        tabs,
        ..Sample::default()
    }
}

pub(crate) fn dark_inks() -> VitalsInks {
    aterm_vitals::ink::vitals_inks([0x30, 0x31, 0x35], [0xF8, 0xF8, 0xF2], [0x50, 0xFA, 0x7B])
}

#[test]
fn deck_inks_hold_on_every_built_in_band() {
    let blends = matches!(
        crate::chrome_band::PLATFORM_BAR_BASE,
        aterm_messages::ink::BarBase::Blend
    );
    let pinned = if blends { &PINNED_ON_BLENDS[..] } else { &[] };
    const PINNED_ON_BLENDS: [(&str, &str, f32); 5] = [
        ("Catppuccin Mocha", "heat_ic steps", 0.047),
        ("Dracula", "heat_ic steps", 0.038),
        ("Gruvbox Dark", "heat_ic steps", 0.042),
        ("Nord", "heat_ic steps", 0.038),
        ("One Dark", "heat_ic steps", 0.049),
    ];
    let bands = built_in_bands();
    assert!(bands.len() > 2, "CONTROL: themes to judge");
    let (mut failed, mut seen) = (Vec::new(), Vec::new());
    for (name, inks) in bands {
        for miss in inks.deck_floor_misses() {
            let line = format!("{name} (band {:?}): {}", inks.ground, miss.what);
            let pin = pinned
                .iter()
                .find(|(theme, floor, _)| *theme == name && miss.what.starts_with(floor));
            match pin {
                Some(&(theme, floor, least)) if miss.out_of_reach => {
                    seen.push((theme, floor));
                    if miss.measured < least {
                        failed.push(format!("{line}, pinned at {least} or more"));
                    }
                }
                _ if miss.out_of_reach => failed.push(format!("{line}, out of reach, unpinned")),
                _ => failed.push(line),
            }
        }
    }
    for &(theme, floor, _) in pinned {
        if !seen.contains(&(theme, floor)) {
            failed.push(format!("{theme}: {floor} is pinned out of reach but holds"));
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// Each built-in theme's name and the inks solved on its band.
fn built_in_bands() -> Vec<(&'static str, VitalsInks)> {
    aterm_types::scheme::builtin_names()
        .into_iter()
        .map(|name| {
            let scheme = aterm_types::scheme::builtin(name).expect("listed scheme exists");
            let band = crate::chrome_band::band_colors(crate::chrome_band::ChromePalette {
                theme: aterm_render::Theme::from(&scheme),
                ansi: Some(crate::chrome_band::ansi_of_scheme(&scheme)),
                authored: aterm_messages::tokens::AuthoredChrome::NONE,
            });
            (name, ink::vitals_inks(band.bar_bg, band.value, band.accent))
        })
        .collect()
}

pub(crate) fn input(snap: &Snapshot) -> DeckInput<'_> {
    DeckInput {
        network: None,
        network_place: None,
        network_target: crate::network_deck::Target::Overview,
        lab: None,
        fleet: None,
        snap: Some(snap),
        prev_snap: None,
        tab_panes: vec![7],
        tab_label: "Tab 1".into(),
        tab_count: 1,
        inks: dark_inks(),
        dwc: false,
        marks: Vec::new(),
        plate: None,
        form: DeckForm::Full,
        contrast: false,
        locale: Locale {
            platform: Platform::MacOs,
            utc_offset_s: 0,
            utc_offset_at: None,
        },
        live: DeckLive::STILL,
    }
}

/// Tab 1 split in two panes after three readings a second apart: claude in pane `a` (1 core,
/// 400 MiB) and cargo in pane `b` (2 cores, 600 MiB), each under its own zsh, on ten cores.
pub(crate) fn split_tab(a: u64, b: u64) -> Snapshot {
    split_tab_running(a, b, ["claude", "cargo"])
}

/// [`split_tab`] with the panes' programs named `names`.
pub(crate) fn split_tab_running(a: u64, b: u64, names: [&str; 2]) -> Snapshot {
    use aterm_vitals::model::TaskRoom;
    let pane = |key: u64, shell_pid: u32| TabRoot {
        key,
        tab: a.min(b),
        shell_pid,
        label: "Tab 1".into(),
        program: String::new(),
        tab_id: None,
        agent_leader: None,
    };
    let proc = |pid: u32, ppid: u32, cpu_ms: u64, mib: u64, name: &str| ProcSample {
        start: Some(1),
        pid,
        ppid,
        cpu_ns: Some(cpu_ms * 1_000_000),
        footprint: Some(mib << 20),
        name: name.into(),
        ..ProcSample::default()
    };
    let mk = |t: u64| Sample {
        at_ns: t * 1_000_000_000,
        at_unix_ms: 1_792_000_800_000 + t as i64 * 1000,
        mono_ms: t * 1000,
        cadence_ms: 1000,
        cores: Some(vec![
            Ticks {
                busy: t * 50,
                total: t * 100,
            };
            10
        ]),
        mem: Some(MemSample {
            total: 32 << 30,
            app: 12 << 30,
            ..MemSample::default()
        }),
        procs: Some(vec![
            proc(100, 1, 0, 300, "aterm"),
            proc(200, 100, 0, 5, "zsh"),
            proc(201, 200, t * 1000, 400, names[0]),
            proc(300, 100, 0, 5, "zsh"),
            proc(301, 300, t * 2000, 600, names[1]),
        ]),
        self_pid: 100,
        tabs: vec![pane(a, 200), pane(b, 300)],
        room: TaskRoom {
            procs: Some(900),
            procs_max: Some(1000),
            user_procs: Some(400),
            user_procs_max: Some(500),
            ..TaskRoom::default()
        },
        ..Sample::default()
    };
    let mut v = aterm_vitals::Vitals::new();
    let _ = v.absorb(&mk(10));
    let _ = v.absorb(&mk(11));
    v.absorb(&mk(12))
}

/// The deck of a split tab, focused on pane 8, reads the whole tab: each scoped row's "this
/// tab" share is both panes', inside all tabs, and its words say so. CONTROL: the focused
/// pane alone is less.
#[test]
fn a_split_tabs_deck_reads_every_pane() {
    let snap = split_tab(7, 8);
    let m = deck_metrics();
    let deck = |panes: Vec<u64>| DeckInput {
        tab_panes: panes,
        tab_count: 2,
        ..input(&snap)
    };
    let bar = |input: &DeckInput<'_>, res: Resource| {
        DeckWords::new(input, &m)
            .rows
            .into_iter()
            .find(|r| r.res == res)
            .and_then(|r| r.bar)
            .expect("a scoped row's bar")
    };
    let near = |a: Option<f32>, b: f64| a.is_some_and(|a| (f64::from(a) - b).abs() < 1e-4);
    let gib = (32u64 << 30) as f64;
    for (res, tab, focused) in [
        (Resource::Cpu, 0.3, 0.2),
        (
            Resource::Memory,
            (1010u64 << 20) as f64 / gib,
            (605u64 << 20) as f64 / gib,
        ),
        (Resource::Processes, 0.004, 0.002),
    ] {
        let (whole, alone) = (bar(&deck(vec![8, 7]), res), bar(&deck(vec![8]), res));
        assert!(near(whole.tab, tab), "{res:?}: {:?} vs {tab}", whole.tab);
        assert!(near(alone.tab, focused), "CONTROL {res:?}: {:?}", alone.tab);
        assert!(whole.tab <= whole.aterm, "{res:?}: this tab ≤ all tabs");
        assert_eq!(
            bar(&deck(vec![7, 8]), res),
            whole,
            "{res:?}: whichever pane leads"
        );
    }
    let cpu =
        |panes: &[u64]| words::row_sentence(Resource::Cpu, &snap, panes, &input(&snap).locale);
    assert!(
        cpu(&[8, 7]).contains("Tab 1 3.0 cores."),
        "{}",
        cpu(&[8, 7])
    );
    assert!(
        cpu(&[8]).contains("Tab 1 2.0 cores."),
        "CONTROL: {}",
        cpu(&[8])
    );
    let tab = snap.tab_of(&[8, 7]).expect("the tab");
    assert_eq!(
        (tab.key, tab.scope.procs),
        (8, 4),
        "the sill's lanes and the chip's meter"
    );
    assert!(
        (tab.scope.cores.raw - 3.0).abs() < 1e-4,
        "{:?}",
        tab.scope.cores
    );
}

const GEOM: DeckGeom = DeckGeom {
    w: 2238.0,
    h: 208.0,
    pad: 24.0,
    scale: 2.0,
    font_px: 26.0,
};

/// The Deck sits at the head's foot under macOS's titlebar; where the strip is the
/// window's band (Linux, Windows), at the head's top, so the band keeps its whole height
/// under it.
#[cfg(unix)]
#[test]
fn the_deck_takes_the_heads_foot_under_a_titlebar_and_its_top_over_a_band() {
    let _attached = attach_stats();
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    app.config
        .sys
        .get_or_insert_with(Default::default)
        .stats_bars = Some("deck".into());
    let deck = reserved_deck_px(
        StatsBars::Deck,
        app.windows[&wid].deck_form,
        app.windows[&wid].scale,
        app.win_cell_size(wid).1,
    );
    let head = deck + 14;
    app.windows.get_mut(&wid).expect("window").metrics.head = head;
    app.backend.set_head(head);
    let pad_top = app.win_pad_top(wid);
    let rows = app.deck_rows_px(wid).expect("the head holds the Deck");
    assert_eq!(rows.len(), deck);
    let (want, band_top) = if crate::tab_bar::STRIP_IS_CHROME_BAND {
        (0..deck, pad_top + head - deck)
    } else {
        (head - deck..head, pad_top + head)
    };
    assert_eq!(rows, want);
    assert_eq!(app.strip_band_top_px(wid), band_top);
    let f = app.deck_frame(wid).expect("the Deck draws");
    assert_eq!(
        f.dy as usize, rows.start,
        "the card is drawn where its rows are"
    );
    app.windows.get_mut(&wid).expect("window").metrics.head = deck - 1;
    app.backend.set_head(deck - 1);
    assert_eq!(app.deck_rows_px(wid), None);
    assert_eq!(app.strip_band_top_px(wid), pad_top + deck - 1);
    app.config
        .sys
        .get_or_insert_with(Default::default)
        .stats_bars = Some("off".into());
    app.windows.get_mut(&wid).expect("window").metrics.head = head;
    app.backend.set_head(head);
    assert_eq!(app.deck_rows_px(wid), None);
    assert_eq!(app.strip_band_top_px(wid), pad_top + head);
}

/// A point over the Deck is not a strip column, though a point above the
/// grid clamps onto the strip's row: a press there is the Deck's
/// (`App::stats_press`), and a pointer resting on it lights no tab and
/// steps no strip drag.
#[cfg(unix)]
#[test]
fn a_point_over_the_deck_is_no_strip_column() {
    let _attached = attach_stats();
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    app.tab_strip_rows = 1;
    app.splice_tab_strip(wid);
    app.config
        .sys
        .get_or_insert_with(Default::default)
        .stats_bars = Some("deck".into());
    let deck = reserved_deck_px(
        StatsBars::Deck,
        app.windows[&wid].deck_form,
        app.windows[&wid].scale,
        app.win_cell_size(wid).1,
    );
    let head = deck + 14;
    app.windows.get_mut(&wid).expect("window").metrics.head = head;
    app.backend.set_head(head);
    let rows = app.deck_rows_px(wid).expect("the head holds the Deck");
    let (cw, ch) = app.win_cell_size(wid);
    let (fx, fy) = app.frame_origin(wid);
    let col = 3u16;
    let x = fx as f64 + (app.win_pad(wid) + usize::from(col) * cw + cw / 2) as f64;
    let over_deck = fy as f64 + (rows.start + rows.len() / 2) as f64;
    let on_strip = fy as f64 + (app.win_pad_top(wid) + head + ch / 2) as f64;
    assert_eq!(app.strip_col_at(wid, x, over_deck), None, "over the Deck");
    assert_eq!(app.strip_col_at(wid, x, on_strip), Some(col), "the strip");
    app.config
        .sys
        .get_or_insert_with(Default::default)
        .stats_bars = Some("off".into());
    assert_eq!(app.deck_rows_px(wid), None);
    assert_eq!(
        app.strip_col_at(wid, x, over_deck),
        Some(col),
        "control: with the Deck off the same point clamps onto the strip"
    );
}

#[test]
fn the_stats_bars_setting_reads_its_own_words() {
    assert_eq!(StatsBars::parse(None), StatsBars::Deck);
    assert_eq!(StatsBars::parse(Some("off")), StatsBars::Off);
    assert_eq!(StatsBars::parse(Some("sill")), StatsBars::Sill);
    assert_eq!(StatsBars::parse(Some(" Sill ")), StatsBars::Sill);
    assert_eq!(StatsBars::parse(Some("nonsense")), StatsBars::Deck);
    for f in StatsBars::ALL {
        assert_eq!(StatsBars::from_word(f.word()), Some(f));
        assert_eq!(StatsBars::parse(Some(f.word())), f);
    }
}

#[test]
fn the_same_snapshot_lays_the_same_pixels() {
    let snap = snapshot();
    for form in [DeckForm::Full, DeckForm::Folded] {
        let mut i = input(&snap);
        i.form = form;
        let a = deck_prims(&GEOM, &i);
        let b = deck_prims(&GEOM, &i);
        assert_eq!(prims_fp(&a), prims_fp(&b), "{form:?}");
    }
}

#[test]
fn a_panel_one_ulp_over_or_two_prims_swapped_is_another_deck() {
    let snap = snapshot();
    let laid = deck_prims(&GEOM, &input(&snap));
    let mut moved = laid.clone();
    let x = moved
        .iter_mut()
        .find_map(|p| match p {
            DrawPrim::Panel { x, .. } => Some(x),
            _ => None,
        })
        .expect("the deck lays a panel");
    *x = f32::from_bits(x.to_bits() + 1);
    assert_ne!(prims_fp(&moved), prims_fp(&laid));
    let mut swapped = laid.clone();
    swapped.swap(0, laid.len() - 1);
    assert_ne!(prims_fp(&swapped), prims_fp(&laid));
}

/// A snapshot of a large Mac (18 cores, an M5 Max GPU of 40 cores holding 1.3 GiB, 128 GiB of
/// memory, an 8 TB volume that keeps 160 GB from this user): what the capacity captions once
/// said over the GPU, Memory and Disk strips.
fn big_mac() -> Snapshot {
    let mut snap = snapshot();
    let cpu = snap.cpu.as_mut().expect("fixture CPU");
    let old_count = cpu.total();
    cpu.cores.resize_with(18, Default::default);
    cpu.clusters
        .last_mut()
        .expect("fixture cluster")
        .cores
        .extend(old_count..18);
    snap.resources[Resource::Cpu.index()].capacity = Some(18.0);
    let memory = &mut snap.mem.as_mut().expect("fixture memory").sample;
    memory.total = 128 << 30;
    memory.app = 80 << 30;
    memory.wired = 12 << 30;
    memory.compressed = Some(4 << 30);
    memory.cached = 24 << 30;
    memory.free = 8 << 30;
    let memory_view = &mut snap.resources[Resource::Memory.index()];
    memory_view.now = Some((96_u64 << 30) as f64);
    memory_view.capacity = Some((128_u64 << 30) as f64);
    snap.volume = Some(VolumeSample {
        total: 8_000_000_000_000,
        free: 240_000_000_000,
        reserved: 160_000_000_000,
        mount: "/".into(),
    });
    let disk_view = &mut snap.resources[Resource::Disk.index()];
    disk_view.now = Some(7_760_000_000_000.0);
    disk_view.capacity = Some(8_000_000_000_000.0);
    let gpu = snap.facts.gpu.as_mut().expect("fixture GPU");
    gpu.name = Some("Apple M5 Max".into());
    gpu.cores = Some(40);
    gpu.in_use_bytes = Some(13 * (1_u64 << 30) / 10);
    snap
}

/// The table that lays row `r` of `form` at `width` pt: Full's, or the Folded block that
/// holds it, with the row's index in that table.
fn table_of(form: DeckForm, width: f64, r: usize, m: &FaceMetrics) -> Option<(Table, u32)> {
    match form {
        DeckForm::Full => {
            let plan = plan::plan_full(width, m);
            (!plan.ground_only).then(|| (Table::of_full(&plan), r as u32))
        }
        DeckForm::Folded => {
            let plan = plan::plan_folded(width, m);
            (!plan.ground_only).then(|| (Table::of_block(&plan.blocks[r / 4]), (r % 4) as u32))
        }
    }
}

/// No row captions its capacity over its strip (`docs/DESIGN-stats-deck-flow-2026-11-02.md`
/// §3): at every Full and Folded width, at 1× and 2×, on a Mac whose GPU, memory and disk the
/// captions once named, no word stands over a row's strip, and the GPU's name, its cores and
/// the memory's and disk's available figures are nowhere in the deck. CONTROL: a row that
/// says it is not read does stand words over its strip, so the check sees them there.
#[test]
fn no_row_captions_its_capacity_over_its_strip() {
    let m = FaceMetrics::helvetica_neue();
    let snap = big_mac();
    let mut checked = 0;
    for form in [DeckForm::Full, DeckForm::Folded] {
        for width in (300..=2_560).step_by(13).map(f64::from) {
            for scale in [1.0_f32, 2.0] {
                let mut i = input(&snap);
                i.form = form;
                let g = DeckGeom {
                    w: width as f32 * scale,
                    scale,
                    ..GEOM
                };
                let laid = lay(&g, &i, &DeckWords::new(&i, &m), &m, true);
                for (r, &res) in DECK_RESOURCES.iter().enumerate() {
                    let Some((t, tr)) = table_of(form, width, r, &m) else {
                        continue;
                    };
                    let (top, foot) = (t.rows.top(tr), t.rows.top(tr) + t.rows.pitch);
                    let over = laid.boxes.iter().find(|b| {
                        b.kind == BoxKind::Text
                            && b.r[0] < t.strip.end()
                            && b.r[2] > t.strip.x
                            && b.r[3] > top
                            && b.r[3] <= foot
                    });
                    assert!(
                        over.is_none(),
                        "{form:?} {width} pt {scale}×: {res:?}'s strip carries {over:?}"
                    );
                    checked += 1;
                }
                let said: Vec<&str> = laid
                    .boxes
                    .iter()
                    .filter(|b| b.kind == BoxKind::Text)
                    .map(|b| b.text.as_str())
                    .collect();
                for caption in ["M5 Max", "40 cores", "available", "avail", "in use"] {
                    assert!(
                        !said.iter().any(|t| t.contains(caption)),
                        "{form:?} {width} pt {scale}×: {caption:?} in {said:?}"
                    );
                }
            }
        }
    }
    assert!(checked > 2_000, "CONTROL: {checked} rows checked");
    let mut unread = big_mac();
    unread.resources[Resource::Gpu.index()] = ResourceView::default();
    let mut i = input(&unread);
    i.form = DeckForm::Folded;
    let laid = lay(&GEOM, &i, &DeckWords::new(&i, &m), &m, true);
    let width = f64::from(GEOM.w / GEOM.scale);
    let (t, tr) = table_of(DeckForm::Folded, width, 1, &m).expect("Folded lays");
    assert!(
        laid.boxes.iter().any(|b| b.kind == BoxKind::Text
            && b.text.contains("no GPU reading")
            && b.r[0] >= t.strip.x
            && b.r[3] > t.rows.top(tr)
            && b.r[3] <= t.rows.top(tr) + t.rows.pitch),
        "CONTROL: an unread GPU says so over its strip"
    );
}

/// The GPU, Memory and Disk rows draw their whole strips (`docs/DESIGN-stats-deck-flow-
/// 2026-11-02.md` §3, "bring back the time series"): at every Folded width that lays the
/// deck, at 1× and 2×, each draws one strip across its table's strip column, its cells the
/// row's full cell height from the row's top. CONTROL: an unread GPU draws no strip, and the
/// check finds none for it.
#[test]
fn the_gpu_memory_and_disk_rows_draw_full_height_strips_at_every_folded_width() {
    let m = FaceMetrics::helvetica_neue();
    let snap = big_mac();
    let mut laid_any = 0;
    let strips = |laid: &Laid, t: &Table, tr: u32| -> Vec<[f64; 4]> {
        laid.boxes
            .iter()
            .filter(|b| {
                b.kind == BoxKind::Strip
                    && b.r[0] == t.strip.x
                    && b.r[2] == t.strip.end()
                    && b.r[3] > t.rows.top(tr)
                    && b.r[3] <= t.rows.top(tr) + t.rows.pitch
            })
            .map(|b| b.r)
            .collect()
    };
    for width in 300..=2_560 {
        let width = f64::from(width);
        if plan::plan_folded(width, &m).ground_only {
            continue;
        }
        for scale in [1.0_f32, 2.0] {
            let mut i = input(&snap);
            i.form = DeckForm::Folded;
            let g = DeckGeom {
                w: width as f32 * scale,
                scale,
                ..GEOM
            };
            let laid = lay(&g, &i, &DeckWords::new(&i, &m), &m, false);
            for res in [Resource::Gpu, Resource::Memory, Resource::Disk] {
                let r = DECK_RESOURCES.iter().position(|&x| x == res).expect("row");
                let (t, tr) = table_of(DeckForm::Folded, width, r, &m).expect("lays");
                let found = strips(&laid, &t, tr);
                assert_eq!(found.len(), 1, "{width} pt {scale}×: {res:?} {found:?}");
                let [_, y0, _, y1] = found[0];
                assert!(
                    y0 <= t.rows.top(tr) && y1 == t.rows.top(tr) + t.rows.cell,
                    "{width} pt {scale}×: {res:?}'s strip spans {y0}..{y1}, its cell {}..{}",
                    t.rows.top(tr),
                    t.rows.top(tr) + t.rows.cell
                );
            }
            laid_any += 1;
        }
    }
    assert!(laid_any > 3_000, "CONTROL: {laid_any} decks laid");
    let mut unread = big_mac();
    unread.resources[Resource::Gpu.index()] = ResourceView::default();
    let mut i = input(&unread);
    i.form = DeckForm::Folded;
    let laid = lay(&GEOM, &i, &DeckWords::new(&i, &m), &m, false);
    let width = f64::from(GEOM.w / GEOM.scale);
    let (t, tr) = table_of(DeckForm::Folded, width, 1, &m).expect("lays");
    assert!(strips(&laid, &t, tr).is_empty(), "CONTROL: no strip unread");
}

/// Text primitives whose baseline belongs to this resource's actual row.
fn drawn_row_text(laid: &Laid, table: &Table, r: u32, scale: f32) -> String {
    let top = table.rows.top(r) as f32 * scale;
    let foot = table.rows.top(r + 1) as f32 * scale;
    let left = table.name.x as f32 * scale;
    let right = table.end as f32 * scale;
    laid.prims
        .iter()
        .filter_map(|p| match p {
            DrawPrim::Text {
                x, baseline: y, s, ..
            } if *x >= left && *x < right && *y >= top && *y < foot => Some(s.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The CPU's core count stands while its use is unread: raw counters still count the cores
/// for the gauge's caption and the Cores row, and no row says "0%" for what it did not read.
/// With no counter at all, the gauge says the count is unknown.
#[test]
fn an_unread_cpu_still_counts_its_cores_from_the_raw_counters() {
    let snap = big_mac();
    let m = FaceMetrics::helvetica_neue();
    for form in [DeckForm::Full, DeckForm::Folded] {
        for columns in [80, 120] {
            let g = DeckGeom {
                w: columns as f32 * 8.0 * GEOM.scale,
                ..GEOM
            };
            let width = f64::from(g.w / g.scale);
            let table = match form {
                DeckForm::Full => Table::of_full(&plan::plan_full(width, &m)),
                DeckForm::Folded => Table::of_block(&plan::plan_folded(width, &m).blocks[0]),
            };
            let mut unread = snap.clone();
            unread.cpu = None;
            unread.cpu_counter_sample = Some(vec![Ticks { busy: 0, total: 0 }; 18]);
            unread.resources[Resource::Cpu.index()] = ResourceView::default();
            unread.gpu = None;
            unread.resources[Resource::Gpu.index()] = ResourceView::default();
            unread
                .facts
                .gpu
                .as_mut()
                .expect("fixture hardware")
                .device_pct = None;
            let mut i = input(&unread);
            i.form = form;
            let laid = lay(&g, &i, &DeckWords::new(&i, &m), &m, true);
            assert!(
                laid.prims.iter().any(
                    |p| matches!(p, DrawPrim::Text { s, .. } if s == "18 cores" || s == "18 ")
                ),
                "{form:?}/{columns}: raw counters still report the core count"
            );
            assert!(
                !laid
                    .prims
                    .iter()
                    .any(|p| matches!(p, DrawPrim::Text { s, .. } if s.contains("cores unknown")))
            );
            for r in [0, 1] {
                let said = drawn_row_text(&laid, &table, r, g.scale);
                assert!(said.contains('–') && !said.contains("0%"), "{said}");
            }
        }
    }
    let mut none = snap;
    none.cpu = None;
    none.cpu_counter_sample = None;
    none.resources[Resource::Cpu.index()].capacity = None;
    let mut i = input(&none);
    i.form = DeckForm::Full;
    let g = DeckGeom {
        w: 80.0 * 8.0 * GEOM.scale,
        ..GEOM
    };
    let (prims, _) = deck_paint(&g, &i);
    assert!(
        prims.iter().any(|p| matches!(p, DrawPrim::Text { s, .. } if s.contains("cores") && s.contains("unknown"))),
        "an unread CPU core count must stay explicit"
    );
}

/// The GPU's bar draws aterm's and this tab's segments from their share of the device's busy
/// time (`docs/DESIGN-stats-deck-flow-2026-11-02.md` §3), each only while a per-app read is
/// held and the device's busy is read: a scope whose figure is `NotPerApp` draws no segment,
/// absent and never zero, and no hit picks it, even while its 10 s mean still averages the
/// shares it held before they expired; and with the device unread the row draws no bar at
/// all, though a client read is held. CONTROL: with a held read and the device read both
/// draw, nested in the device's fill, and each is a hit. The same through the model: shares
/// held one sweep before they expire draw; one sweep after, the scope's mean still reads,
/// and neither draws.
#[test]
fn a_gpu_scope_without_a_per_app_read_draws_no_segment() {
    use crate::stats_paint::HitKind;
    use aterm_vitals::model::NoFigure;
    let m = FaceMetrics::helvetica_neue();
    let r = DECK_RESOURCES
        .iter()
        .position(|&x| x == Resource::Gpu)
        .expect("GPU row");
    // (case, the scope's mean reads, a per-app read is held, the device's busy is read)
    let cases = [
        ("held", true, true, true),
        ("not per app", false, false, true),
        ("expired", true, false, true),
        ("device unread", true, true, false),
    ];
    for (case, mean, held, device) in cases {
        let mut snap = snapshot();
        let view = &mut snap.resources[Resource::Gpu.index()];
        let fig = |now: f64| ScopeFig {
            now: mean.then_some(now),
            missing: (!held).then_some(NoFigure::NotPerApp),
            ..ScopeFig::default()
        };
        view.aterm = fig(9.0);
        view.tabs = vec![(7, fig(4.0))];
        if !device {
            view.now = None;
            view.heat_now = None;
            if let Some(gpu) = snap.facts.gpu.as_mut() {
                gpu.device_pct = None;
            }
        }
        let draws = held && device;
        let mut i = input(&snap);
        i.form = DeckForm::Folded;
        let words = DeckWords::new(&i, &m);
        match words.rows[r].bar {
            Some(fill) => {
                assert!(device, "{case}: an unread device draws no bar: {fill:?}");
                assert!(fill.global.is_some_and(|g| g > 0.0), "{case}: {fill:?}");
                let scopes = (fill.aterm, fill.tab);
                let want = if draws {
                    (Some(0.09), Some(0.04))
                } else {
                    (None, None)
                };
                assert_eq!(scopes, want, "{case}: {fill:?}");
            }
            None => assert!(!device, "{case}: the GPU's bar"),
        }
        let laid = lay(&GEOM, &i, &words, &m, true);
        let width = f64::from(GEOM.w / GEOM.scale);
        let (t, tr) = table_of(DeckForm::Folded, width, r, &m).expect("lays");
        let col = t.meter.bar.expect("the bar column at 1119 pt");
        let s = GEOM.scale;
        let (x, y0, y1) = (
            (col.x * f64::from(s) + 0.5).floor() as f32,
            t.rows.top(tr) as f32 * s,
            (t.rows.top(tr) + t.rows.pitch) as f32 * s,
        );
        let drawn = |ink: Rgb| {
            laid.prims.iter().any(|p| {
                matches!(*p, DrawPrim::Panel { x: px, y, fill, .. }
                    if (px - x).abs() < 0.01 && y >= y0 && y < y1 && fill == rgba(ink, 0xFF))
            })
        };
        assert_eq!(drawn(i.inks.machine), device, "{case}: the device's fill");
        assert_eq!(drawn(i.inks.aterm), draws, "{case}: aterm's segment");
        assert_eq!(drawn(i.inks.tab), draws, "{case}: the tab's segment");
        let scoped = laid
            .hits
            .iter()
            .any(|h| matches!(h.kind, HitKind::Row(Instrument::Gpu, Bar::Tab | Bar::Aterm)));
        assert_eq!(scoped, draws, "{case}: a scoped hit");
    }

    // Through the model: aterm's and claude's GPU time read at 10 s and 11 s, then a sweep a
    // second with no client read until the held shares expire, `WINDOW_MAX_NS` after 11 s.
    let read = |t: u64, ran: Option<u64>| {
        let mut s = sample_at(t, &[0; 10], &[0, 0]);
        let row = |entry: u64, pid: u32, name: &str, ms: u64| aterm_vitals::GpuClientRow {
            entry,
            pid,
            name: name.into(),
            gpu_ns: ms * 1_000_000,
        };
        s.gpu_clients = ran.map(|k| aterm_vitals::GpuClientsSample {
            at_ns: s.at_ns,
            rows: vec![row(1, 100, "aterm", 30 * k), row(2, 201, "claude", 20 * k)],
            entries: vec![1, 2],
            complete: true,
        });
        s
    };
    let mut v = aterm_vitals::Vitals::new();
    let _ = v.absorb(&read(10, Some(0)));
    let _ = v.absorb(&read(11, Some(1)));
    let expire_s = 11 + aterm_vitals::explain::WINDOW_MAX_NS / 1_000_000_000;
    for t in 12..expire_s {
        let _ = v.absorb(&read(t, None));
    }
    for (t, held) in [(expire_s, true), (expire_s + 1, false)] {
        let snap = v.absorb(&read(t, None));
        let view = snap.resource(Resource::Gpu, &[7]);
        assert!(
            view.aterm.now.is_some_and(|n| n > 0.0),
            "{t} s: aterm's 10 s mean reads: {:?}",
            view.aterm
        );
        assert_eq!(
            view.aterm.missing,
            (!held).then_some(NoFigure::NotPerApp),
            "{t} s"
        );
        let mut i = input(&snap);
        i.form = DeckForm::Folded;
        let fill = DeckWords::new(&i, &m).rows[r].bar.expect("the GPU's bar");
        assert!(fill.global.is_some_and(|g| g > 0.0), "{t} s: {fill:?}");
        assert_eq!(
            (fill.aterm.is_some(), fill.tab.is_some()),
            (held, held),
            "{t} s: {fill:?}"
        );
    }
}

#[test]
fn before_the_first_snapshot_the_deck_says_it_is_reading() {
    let empty = Snapshot::default();
    for form in [DeckForm::Full, DeckForm::Folded] {
        let mut i = input(&empty);
        i.snap = None;
        i.form = form;
        let (prims, hits) = deck_paint(&GEOM, &i);
        let said: Vec<String> = prims
            .iter()
            .filter_map(|p| match p {
                DrawPrim::Text { s, .. } => Some(s.clone()),
                _ => None,
            })
            .collect();
        for name in ["CPU", "Memory", "Disk", "Processes"] {
            assert!(said.iter().any(|s| s == name), "{form:?}: {said:?}");
        }
        let reading = said.iter().filter(|s| *s == "reading\u{2026}").count();
        assert_eq!(reading, DECK_RESOURCES.len(), "{form:?}: {said:?}");
        assert!(hits.is_empty(), "{form:?}: {hits:?}");
    }
}

/// Each scope's head carries its ink as a swatch where the heads have room for one, on the
/// face the deck draws.
#[test]
fn every_bar_label_carries_its_ink_as_a_key() {
    let snap = snapshot();
    let inks = dark_inks();
    let m = FaceMetrics::helvetica_neue();
    for (form, w) in [(DeckForm::Full, 2238.0), (DeckForm::Folded, 2238.0)] {
        let mut i = input(&snap);
        i.form = form;
        let g = DeckGeom { w, ..GEOM };
        let prims = lay(&g, &i, &DeckWords::new(&i, &m), &m, true).prims;
        for ink in [inks.machine, inks.aterm, inks.tab] {
            assert!(
                prims
                    .iter()
                    .any(|p| matches!(p, DrawPrim::Panel { fill, w, y, .. }
                    if fill[..3] == ink[..] && *w <= 12.0 && *y < 28.0)),
                "{form:?}: {ink:?}"
            );
        }
    }
}

/// A bar's fills are whole px and nested (the machine's ⊇ aterm's ⊇ this tab's), and the
/// machine's fill is exactly its share, the row's % (`docs/DESIGN-stats-deck-units-
/// 2026-11-04.md` §4.6): a scope inside it that reads more (Memory's footprints) is clamped
/// to it and never widens it. A scope above zero keeps the least it is given however small
/// its share, where the fill around it leaves room, and a scope with no figure draws nothing,
/// its room passing to the scope inside it. The bar's column nests them in height too, so
/// there a scope that rounds to the length of the one around it shows that one's rim; the
/// bar under a % nests them by length alone, so there (`apart`) a fill ends a px short of
/// the one around it, and the fills inside give way where it has no px to spare.
/// CONTROL: the old rule, which widened a fill to the one inside it, draws the machine's
/// fill past its share wherever a scope inside reads more, and the check sees it. A Memory
/// footprint larger than the figure of the scope around it, which the bar therefore cannot
/// show, is said in speech and drawn after its figure in the Memory lens's Biggest now
/// exactly then.
#[test]
fn the_this_mac_fill_is_the_percent_and_nested_fills_never_widen_it() {
    let fill = |global, aterm, tab| BarFill {
        global,
        aterm,
        tab,
        ticks: None,
        warn_tick: None,
    };
    let w = 52.0;
    // The machine's, aterm's and this tab's shares; the least; apart; the widths; why.
    type Case = (
        (Option<f32>, Option<f32>, Option<f32>),
        f32,
        f32,
        [f32; 3],
        &'static str,
    );
    let cases: [Case; 18] = [
        (
            (Some(0.7), Some(0.4), Some(0.1)),
            1.0,
            0.0,
            [36.0, 21.0, 5.0],
            "nested",
        ),
        (
            (Some(0.3), Some(0.001), Some(0.0005)),
            1.0,
            0.0,
            [16.0, 1.0, 1.0],
            "a px each",
        ),
        (
            (Some(0.3), Some(0.001), Some(0.0005)),
            2.0,
            0.0,
            [16.0, 2.0, 2.0],
            "the least",
        ),
        (
            (Some(0.0), Some(0.0), Some(0.0)),
            1.0,
            0.0,
            [0.0; 3],
            "zero is none",
        ),
        (
            (Some(f32::NAN), None, Some(0.2)),
            1.0,
            0.0,
            [0.0, 0.0, 10.0],
            "unread draws none",
        ),
        (
            (None, Some(0.2), Some(0.3)),
            1.0,
            0.0,
            [0.0, 10.0, 10.0],
            "the tab clamps to aterm",
        ),
        (
            (Some(1.4), None, None),
            1.0,
            0.0,
            [52.0, 0.0, 0.0],
            "never past the bar",
        ),
        (
            (Some(0.4), Some(0.6), Some(0.5)),
            1.0,
            0.0,
            [21.0, 21.0, 21.0],
            "footprints clamp",
        ),
        (
            (Some(0.7), Some(0.605), Some(0.6)),
            1.0,
            0.0,
            [36.0, 31.0, 31.0],
            "the column's rim",
        ),
        (
            (Some(0.7), Some(0.605), Some(0.6)),
            1.0,
            1.0,
            [36.0, 31.0, 30.0],
            "a px apart",
        ),
        (
            (Some(0.61), Some(0.605), Some(0.6)),
            1.0,
            1.0,
            [32.0, 31.0, 30.0],
            "the % holds",
        ),
        (
            (Some(0.3), Some(0.001), Some(0.0005)),
            1.0,
            1.0,
            [16.0, 1.0, 0.0],
            "give way",
        ),
        (
            (Some(1.0), Some(0.995), Some(0.99)),
            1.0,
            1.0,
            [52.0, 51.0, 50.0],
            "at the end",
        ),
        (
            (Some(0.5), Some(0.5), Some(0.5)),
            1.0,
            1.0,
            [26.0, 25.0, 24.0],
            "equal shares",
        ),
        (
            (Some(0.5), Some(0.5), Some(0.5)),
            1.0,
            0.0,
            [26.0; 3],
            "the column draws alike",
        ),
        (
            (None, Some(0.2), Some(0.3)),
            1.0,
            1.0,
            [0.0, 10.0, 9.0],
            "aterm shows past the tab",
        ),
        (
            (Some(0.0), Some(0.2), Some(0.0)),
            1.0,
            1.0,
            [0.0; 3],
            "inside zero is none",
        ),
        (
            (Some(0.4), None, Some(0.399)),
            1.0,
            1.0,
            [21.0, 0.0, 20.0],
            "past the tab",
        ),
    ];
    for ((g, a, t), least, apart, want, why) in cases {
        assert_eq!(fill(g, a, t).widths_px(w, least, apart), want, "{why}");
    }
    assert_eq!(
        fill(Some(0.5), None, None).widths_px(0.5, 1.0, 0.0),
        [0.5, 0.0, 0.0],
        "the least a scope keeps is never wider than the bar"
    );
    assert_eq!(
        fill(Some(0.9), Some(0.8), Some(0.7)).widths_px(2.0, 1.0, 1.0),
        [2.0, 1.0, 0.0],
        "a bar too short for every px keeps the outer scopes'"
    );
    // The law over a grid of shares, both bars: the machine's fill is its share's px (at
    // least `least` while above zero), and each fill ends inside the one around it.
    let law = |f: &BarFill, [gw, aw, tw]: [f32; 3], least: f32, apart: f32| -> bool {
        let g = f.global.unwrap_or(0.0);
        let own = if g > 0.0 {
            (g.min(1.0) * w).round().max(least).min(w)
        } else {
            0.0
        };
        let inside = |outer: f32, inner: f32| inner == 0.0 || inner <= outer - apart;
        gw == own && inside(gw, aw) && inside(aw, tw)
    };
    let steps = [0.0, 0.001, 0.1, 0.33, 0.5, 0.505, 0.6, 0.9, 1.0];
    let mut widened = 0;
    for &g in &steps {
        for &a in &steps {
            for &t in &steps {
                let f = fill(Some(g), Some(a), Some(t));
                for apart in [0.0, 1.0] {
                    assert!(
                        law(&f, f.widths_px(w, 1.0, apart), 1.0, apart),
                        "{f:?} {apart}"
                    );
                    widened += usize::from(!law(&f, old_widths(&f, w, 1.0, apart), 1.0, apart));
                }
            }
        }
    }
    assert!(
        widened > 100,
        "CONTROL: the widening rule breaks the law {widened} times"
    );

    // In words (§4.6): Memory's aterm and Tab fills are footprints, which count pages
    // differently from the machine's used memory, so one can read larger than the scope
    // around it, whose fill stops it. Speech and the Memory lens say each such footprint,
    // and only then: aterm's against used memory and this tab's against aterm's, each below,
    // equal to and above it; and with aterm unread, this tab's against this Mac's. The lens
    // says it whether its processes report footprints, Biggest now listing them, or report
    // none, Biggest now one line of the scopes' figures (the owner's scene, and an idle tab
    // under Bar::Tab: the R3 lens fixes' review, 2026-11-06).
    use crate::stats_lens::{Lens, LensGeom, Opened, lay_lens};
    const ATERM: &str = "aterm's footprint is larger than this Mac's used memory";
    const TAB: &str = "Tab 1's footprint is larger than aterm's";
    const TAB_MAC: &str = "Tab 1's footprint is larger than this Mac's used memory";
    let m = FaceMetrics::helvetica_neue();
    let used = snapshot().resources[Resource::Memory.index()]
        .now
        .expect("CONTROL: memory read");
    // aterm's and this tab's footprints as shares of used memory (aterm's `None`: unread),
    // and what is said.
    let cases: [(Option<f64>, f64, &[&str]); 9] = [
        (Some(0.8), 0.3, &[]),
        (Some(1.0), 1.0, &[]),
        (Some(1.25), 0.3, &[ATERM]),
        (Some(0.5), 0.5, &[]),
        (Some(0.5), 0.6, &[TAB]),
        (Some(1.25), 1.3, &[ATERM, TAB]),
        (None, 0.9, &[]),
        (None, 1.0, &[]),
        (None, 1.1, &[TAB_MAC]),
    ];
    let fig = |v: f64| ScopeFig {
        now: Some(v),
        ..ScopeFig::default()
    };
    for ((ka, kt, want), procs) in cases
        .into_iter()
        .flat_map(|case| [(case, true), (case, false)])
    {
        let at = format!(
            "aterm {ka:?} and Tab 1 {kt} of used memory, {}",
            if procs {
                "its processes' footprints read"
            } else {
                "no process's footprint read"
            }
        );
        let mut snap = snapshot();
        if !procs {
            for t in &mut snap.top {
                t.footprint = None;
            }
        }
        if ka.is_none() {
            snap.aterm = None;
        }
        let view = &mut snap.resources[Resource::Memory.index()];
        view.aterm = ka.map_or_else(ScopeFig::default, |k| fig(used * k));
        view.tabs = vec![(7, fig(used * kt))];
        let i = input(&snap);
        let spoken = words::row_sentence(Resource::Memory, &snap, &i.tab_panes, &i.locale);
        let lens = lay_lens(
            &LensGeom {
                dx: 0,
                dy: 0,
                w: 1024.0 * 2.0 + 24.0,
                h: 250.0 * 2.0 + 24.0,
                margin: 12.0,
                scale: 2.0,
                font_px: 26.0,
            },
            &i,
            &Lens {
                focus: Instrument::Memory,
                bar: Bar::Global,
                how: Opened::Pinned,
            },
            &m,
            true,
        );
        let wells = lens
            .wells
            .iter()
            .map(|(name, line)| format!("{name}: {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let biggest = lens
            .wells
            .iter()
            .find(|(name, _)| name == "Biggest now")
            .map(|(_, line)| line.as_str())
            .unwrap_or_else(|| panic!("{at}: Biggest now in {wells}"));
        assert_eq!(
            biggest.contains("claude"),
            procs,
            "PRECONDITION {at}: Biggest now lists the processes exactly when they report \
             footprints: {biggest}"
        );
        for clause in want {
            assert!(
                biggest.contains(clause),
                "{at}: Biggest now's words say {clause:?}: {biggest}"
            );
        }
        for (surface, text) in [("speech", &spoken), ("the lens", &wells)] {
            assert_eq!(
                text.matches("footprint is larger").count(),
                want.len(),
                "{at}, {surface}: {text}"
            );
            for clause in want {
                assert!(
                    text.contains(clause),
                    "{at}, {surface}: {clause:?} in {text}"
                );
            }
        }
        let drawn: Vec<&str> = lens.boxes.iter().map(|b| b.text.as_str()).collect();
        let larger: Vec<&str> = drawn
            .iter()
            .copied()
            .filter(|t| t.contains("larger than"))
            .collect();
        assert_eq!(
            larger.len(),
            usize::from(!want.is_empty()),
            "{at}: the lens draws it beside the footprints' figures: {drawn:?}"
        );
        for clause in want {
            let than = &clause[clause.find("larger than").expect("a clause")..];
            assert!(
                larger.iter().any(|t| t.contains(than)),
                "{at}: {than:?} drawn in {larger:?}"
            );
        }
    }
}

/// The bar's widths as they were drawn before the clamp (A1 §4.6): each scope never
/// narrower than the one nested inside it, the machine's fill widened to aterm's footprint.
fn old_widths(f: &BarFill, w: f32, least: f32, apart: f32) -> [f32; 3] {
    let scopes = [f.tab, f.aterm, f.global];
    let (mut ends, mut past) = ([0.0_f32; 3], [0.0_f32; 3]);
    let mut inner_end = 0.0_f32;
    for (i, v) in scopes.into_iter().enumerate() {
        if v.is_none() {
            continue;
        }
        if let Some(s) = v.filter(|v| v.is_finite() && *v > 0.0) {
            past[i] = apart;
            ends[i] = (s.clamp(0.0, 1.0) * w).round().max(least);
        }
        ends[i] = ends[i].max(inner_end + past[i]);
        inner_end = ends[i];
    }
    let mut room = w;
    for i in (0..scopes.len()).rev() {
        if scopes[i].is_some() {
            ends[i] = ends[i].min(room).max(0.0);
            room = ends[i] - past[i];
        }
    }
    [ends[2], ends[1], ends[0]]
}

/// The Memory bar's this-Mac fill is the Memory % as drawn: with aterm's footprints above
/// the machine's used memory, the machine's fill still ends at the % of the bar, and
/// aterm's and this tab's end inside it. CONTROL: the footprints do read above the used
/// memory, so the old rule would have widened the machine's fill.
#[test]
fn the_memory_bars_machine_fill_is_its_percent_however_big_the_footprints() {
    let m = FaceMetrics::helvetica_neue();
    let mut snap = snapshot();
    let view = &mut snap.resources[Resource::Memory.index()];
    let total = view.capacity.expect("memory capacity");
    let heat = view.heat_now.expect("memory read");
    view.aterm.now = Some(total * (f64::from(heat) + 0.2).min(1.0));
    view.tabs = vec![(
        7,
        ScopeFig {
            now: Some(total * (f64::from(heat) + 0.1).min(1.0)),
            ..ScopeFig::default()
        },
    )];
    let mut i = input(&snap);
    i.form = DeckForm::Folded;
    let words = DeckWords::new(&i, &m);
    let r = DECK_RESOURCES
        .iter()
        .position(|&x| x == Resource::Memory)
        .expect("Memory row");
    let fill = words.rows[r].bar.expect("the Memory bar");
    assert!(
        fill.aterm.zip(fill.global).is_some_and(|(a, g)| a > g),
        "CONTROL: aterm's footprint reads past the machine's used: {fill:?}"
    );
    let laid = lay(&GEOM, &i, &words, &m, true);
    let width = f64::from(GEOM.w / GEOM.scale);
    let (t, tr) = table_of(DeckForm::Folded, width, r, &m).expect("lays");
    let col = t.meter.bar.expect("the bar column");
    let s = GEOM.scale;
    let x = (col.x * f64::from(s) + 0.5).floor() as f32;
    let bar_w = ((col.x + col.w) * f64::from(s) + 0.5).floor() as f32 - x;
    let (y0, y1) = (
        t.rows.top(tr) as f32 * s,
        (t.rows.top(tr) + t.rows.pitch) as f32 * s,
    );
    let width_of = |ink: Rgb| {
        laid.prims.iter().find_map(|p| match *p {
            DrawPrim::Panel {
                x: px, y, w, fill, ..
            } if (px - x).abs() < 0.01 && y >= y0 && y < y1 && fill == rgba(ink, 0xFF) => Some(w),
            _ => None,
        })
    };
    let machine = width_of(i.inks.machine).expect("the machine's fill");
    assert_eq!(
        machine,
        (heat * bar_w).round(),
        "the machine's fill is the %"
    );
    let aterm = width_of(i.inks.aterm).expect("aterm's fill");
    let tab = width_of(i.inks.tab).expect("the tab's fill");
    assert!(
        tab <= aterm && aterm <= machine,
        "{tab} ≤ {aterm} ≤ {machine}"
    );
}

/// A meter's value is the share as a whole percent up to 100, but a
/// share above 0 that the % column prints as "<1%" keeps a tenth, so it
/// is not 0 beside words that say "less than 1 percent".
#[cfg(a11y_tree)]
#[test]
fn a_meter_under_half_a_percent_is_not_zero() {
    assert_eq!(super::meter_pct(Some(0.004)), Some(0.4));
    assert_eq!(super::meter_pct(Some(0.000_1)), Some(0.1));
    assert_eq!(super::meter_pct(Some(0.0)), Some(0.0));
    assert_eq!(super::meter_pct(Some(0.006)), Some(1.0));
    assert_eq!(super::meter_pct(Some(0.424)), Some(42.0));
    assert_eq!(super::meter_pct(Some(1.3)), Some(100.0));
    assert_eq!(super::meter_pct(Some(f32::NAN)), None);
    assert_eq!(super::meter_pct(None), None);
    assert_eq!(words::pct(Some(0.004)), "<1%");
}

/// T-G12 (Stats Deck v2 §12): the deck reads as one group of six OS resource meters and
/// its CPU cores, each meter saying its row's words.
#[cfg(a11y_tree)]
#[test]
fn a11y_meters() {
    use super::paint_tests::{FRONT, Scene, scene};
    use aterm_vitals::model::NoFigure;
    const MAC: Locale = Locale {
        platform: Platform::MacOs,
        utc_offset_s: 0,
        utc_offset_at: None,
    };
    let said = |snap: &Snapshot| stats_a11y_group(Some(snap), &[FRONT], &MAC);
    let meter = |snap: &Snapshot, res: Resource| {
        let want = words::resource_name(res);
        said(snap)
            .meters
            .into_iter()
            .find(|m| m.name == want)
            .expect("meter in DECK_RESOURCES")
    };

    let calm = scene(Scene::Calm);
    let g = said(&calm);
    assert_eq!(g.name, "Stats deck, newest first");
    let names: Vec<&str> = g.meters.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(
        names,
        ["CPU", "GPU", "Memory", "Disk", "Network", "Processes"]
    );
    for (m, &res) in g.meters.iter().zip(&DECK_RESOURCES) {
        assert_eq!(m.words, words::row_sentence(res, &calm, &[FRONT], &MAC));
    }
    assert_eq!(g.meters[0].value, Some(98.0));
    assert_eq!(
        g.meters[0].words,
        "CPU 98 percent of capacity: 17.7 of 18 cores. aterm 6.8 cores, Tab 2 0.1 cores. \
         Rose from about 49 percent. Over capacity 6 of the last 10 minutes. 8 waiting for \
         a core, load 25.7 on 18 cores. Busiest rustc, 9 processes in Tab 3, 5.9 cores."
    );
    let trouble = scene(Scene::Trouble);
    let cpu = meter(&trouble, Resource::Cpu);
    assert_eq!(
        cpu.words,
        words::row_sentence(Resource::Cpu, &trouble, &[FRONT], &MAC)
    );
    assert!(
        cpu.words.contains(" Why: Spotlight, after start-up. "),
        "{}",
        cpu.words
    );
    assert_eq!(g.cores_name, "18 CPU cores");
    assert_eq!(g.cores.len(), 18);
    assert_eq!(g.cores[0].name, "Super core 1");
    assert_eq!(g.cores[6].name, "Performance core 1");
    let core_names: std::collections::BTreeSet<&str> =
        g.cores.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(core_names.len(), 18, "every core's name is its own");
    for m in g.meters.iter().chain(&g.cores) {
        let v = m.value.expect("read");
        assert!((0.0..=100.0).contains(&v), "{m:?}");
    }
    assert_eq!(
        g.cores[7].words,
        format!("{:.0} percent busy", g.cores[7].value.unwrap())
    );

    let disk = meter(&scene(Scene::Trouble), Resource::Disk);
    assert_eq!(disk.value, Some(98.0));
    assert_eq!(
        disk.words,
        "Disk 98 percent of capacity: 1.96 terabytes used, 41.0 gigabytes free, 2.00 \
         terabytes total. aterm writes 410 megabytes per second, Tab 2 writes 0.2 megabytes per second. Steady near 98 \
         percent. Disk full in about 34 minutes, filling 1.2 gigabytes per minute. Disk low, \
         41 gigabytes free. aterm freed 12 gigabytes today. Used grew 12 gigabytes in the \
         last 10 minutes. Most in ~/aterm/target, 212 gigabytes."
    );

    let mut first = scene(Scene::FirstRun);
    first.facts.net_read_s = 3_600;
    let net = meter(&first, Resource::Network);
    assert_eq!(net.value, Some(9.0));
    assert_eq!(
        net.words,
        "Network 9 percent of its link rate: 2.1 of 22.9 megabytes per second, downloading. \
         Learning its peak: 1 of 24 hours read. No per-app figure. Not read for 9 of the \
         last 10 minutes. Sending 0.3 of 22.9 megabytes per second. Wi-Fi en0, link 22.9 \
         megabytes per second."
    );

    let no_agent = ScopeFig {
        missing: Some(NoFigure::NoAgent),
        ..ScopeFig::default()
    };
    first.resources[Resource::Tokens.index()].tabs = vec![(FRONT, no_agent)];
    assert_eq!(
        words::row_sentence(Resource::Tokens, &first, &[FRONT], &MAC),
        "Tokens: learning aterm's peak, 1 of 10 minutes. aterm 3.1 thousand per minute. Tab \
         2 has no agent. Not read for 9 of the last 10 minutes. Input 410 thousand tokens \
         per minute, cache re-reads included. Plan limit not read."
    );

    let mut unread = scene(Scene::Calm);
    unread.resources[Resource::Gpu.index()] = ResourceView::default();
    let gpu = meter(&unread, Resource::Gpu);
    assert_eq!((gpu.value, gpu.words.as_str()), (None, "GPU: not read."));

    let reading = stats_a11y_group(None, &[FRONT], &MAC);
    for (m, &res) in reading.meters.iter().zip(&DECK_RESOURCES) {
        assert_eq!(m.value, None, "{m:?}");
        assert_eq!(m.words, format!("{}: reading.", words::resource_name(res)));
    }
    assert_eq!(reading.meters.len(), DECK_RESOURCES.len());
    assert!(reading.cores.is_empty());
}
