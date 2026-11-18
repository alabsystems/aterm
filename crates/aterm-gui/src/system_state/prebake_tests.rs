// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::*;
use crate::system_state::LockupSpec;
use crate::system_state::lockup::Baked;
use aterm_effects::rainbow_road::{CatPose, RoadArt};

/// A stand-in oven: it records every key it is asked to bake, how many bakes run at once, the
/// most that ever did, and what it kept, each in order.
#[derive(Default)]
struct Recorder {
    running: AtomicUsize,
    most: AtomicUsize,
    /// Every key passed to [`Oven::bake`], in the order the bakes began, kept or not.
    baked: Mutex<Vec<Key>>,
    kept: Mutex<Vec<Key>>,
    landed: AtomicUsize,
    /// A bake under way waits here until the test lets it go (a release mid-bake).
    gate: Mutex<()>,
}

impl Oven for Arc<Recorder> {
    fn kept(&self, key: Key) -> bool {
        self.kept.lock().unwrap().contains(&key)
    }

    fn bake(&self, key: Key) -> Arc<[u8]> {
        // Recorded before it counts as running, so a test that saw it run sees it here.
        self.baked.lock().unwrap().push(key);
        let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.most.fetch_max(now, Ordering::SeqCst);
        drop(self.gate.lock().unwrap());
        self.running.fetch_sub(1, Ordering::SeqCst);
        vec![0; (key.1 * key.2 * 4) as usize].into()
    }

    fn keep(&self, key: Key, _: Arc<[u8]>) {
        self.kept.lock().unwrap().push(key);
    }

    fn landed(&self) {
        self.landed.fetch_add(1, Ordering::SeqCst);
    }

    fn retain(&self, wanted: &[Key]) {
        self.kept.lock().unwrap().retain(|key| wanted.contains(key));
    }
}

fn road(permille: u16) -> Key {
    (
        ImageArt::Road(RoadArt {
            length: 400,
            permille,
            done: false,
            dark: true,
        }),
        4,
        2,
    )
}

/// The worker is done: a hang detector, not a budget.
fn idle(baker: &Baker<Arc<Recorder>>) {
    let end = Instant::now() + Duration::from_secs(60);
    while baker.busy() {
        assert!(Instant::now() < end, "the worker never went idle");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// A bake is under way, waiting at the gate the test holds: a hang detector, not a budget.
fn under_way(recorder: &Recorder) {
    let end = Instant::now() + Duration::from_secs(60);
    while recorder.running.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < end, "the bake never began");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A DRAG BAKES WHERE IT ENDS (§4.5): of forty requests, the first one's bake under way while the
/// other thirty-nine arrive as a live resize makes them, one worker bakes one at a time, and
/// after the bake under way only the newest request's images, every one of them: the requests in
/// between bake nothing. The bake under way, which the newest request no longer wants, is not
/// kept when it lands. NEGATIVE CONTROL: each bake kept woke the window, and asked again for what
/// is kept, nothing bakes.
#[test]
fn rapid_requests_bake_one_at_a_time_and_end_on_the_newest() {
    let recorder = Arc::new(Recorder::default());
    let baker = Baker::new(Arc::clone(&recorder));
    let requests: Vec<[Key; 2]> = (0..40)
        .map(|at| [road(at * 10), road(at * 10 + 1)])
        .collect();
    // The first request's first bake holds at the gate while the rest arrive.
    let gate = recorder.gate.lock().unwrap();
    baker.want(1, &requests[0]);
    under_way(&recorder);
    for keys in &requests[1..] {
        baker.want(1, keys);
    }
    drop(gate);
    idle(&baker);
    assert_eq!(
        recorder.most.load(Ordering::SeqCst),
        1,
        "one bake at a time"
    );
    let newest = requests[requests.len() - 1];
    assert_eq!(
        *recorder.baked.lock().unwrap(),
        vec![road(0), newest[0], newest[1]],
        "the bake under way, then the newest request's, and nothing in between"
    );
    let kept = recorder.kept.lock().unwrap().clone();
    assert_eq!(
        kept, newest,
        "the newest request is kept, the bake it replaced is not"
    );
    assert_eq!(recorder.landed.load(Ordering::SeqCst), kept.len());
    // Asked again for what is kept, nothing bakes.
    baker.want(1, &newest);
    idle(&baker);
    assert_eq!(
        recorder.baked.lock().unwrap().len(),
        3,
        "nothing baked again"
    );
}

/// A BAKE UNDER WAY THAT THE NEWER REQUEST STILL WANTS IS KEPT ONCE: the view's moment moved on
/// while one of its images baked, and the new moment wants that image too (a step's cat whose
/// pose and lean did not change). Not kept yet when the newer request came, it was queued again;
/// it is kept as it lands, and when its turn in the queue comes it is not baked a second time.
/// NEGATIVE CONTROL: the newer request's other image bakes after it and is kept, each kept image
/// waking the window once.
#[test]
fn a_bake_under_way_the_newer_request_still_wants_is_kept_once() {
    let recorder = Arc::new(Recorder::default());
    let baker = Baker::new(Arc::clone(&recorder));
    let gate = recorder.gate.lock().unwrap();
    baker.want(1, &[road(3)]);
    under_way(&recorder);
    // road(3) is at the gate, not kept: the newer request queues it again.
    baker.want(1, &[road(3), road(4)]);
    drop(gate);
    idle(&baker);
    assert_eq!(
        *recorder.baked.lock().unwrap(),
        vec![road(3), road(4)],
        "each baked once"
    );
    assert_eq!(
        *recorder.kept.lock().unwrap(),
        vec![road(3), road(4)],
        "the bake under way kept as it landed, then the newer request's other image"
    );
    assert_eq!(recorder.landed.load(Ordering::SeqCst), 2, "one wake each");
}

/// ONCE NO PAGE IS SHOWN NOTHING IS KEPT: a release drops what is queued and what is kept, and a
/// bake under way when it came is not kept when it lands.
#[test]
fn a_release_drops_the_queue_the_bakes_and_a_bake_under_way() {
    let recorder = Arc::new(Recorder::default());
    let baker = Baker::new(Arc::clone(&recorder));
    baker.want(1, &[road(1)]);
    idle(&baker);
    assert_eq!(*recorder.kept.lock().unwrap(), vec![road(1)]);
    let gate = recorder.gate.lock().unwrap();
    baker.want(1, &[road(1), road(2), road(3)]);
    // The worker is inside road(2)'s bake, waiting at the gate.
    under_way(&recorder);
    baker.release();
    assert!(recorder.kept.lock().unwrap().is_empty(), "the bakes went");
    drop(gate);
    idle(&baker);
    assert!(
        recorder.kept.lock().unwrap().is_empty(),
        "neither the bake under way nor the queued one was kept"
    );
}

/// TWO VIEWS NEVER DROP EACH OTHER'S BAKES (§4.5): one view's newest request replaces its own
/// older one alone. A second view whose images are all kept asks while the first's bakes are
/// queued, and the first's still bake; the worker bakes the newest request first, then what the
/// other view still wants, and never what is kept. NEGATIVE CONTROL: the first view's own newer
/// request does replace its older one: its queued images never bake, and its bake under way is
/// not kept.
#[test]
fn two_views_keep_each_others_queued_bakes() {
    let recorder = Arc::new(Recorder::default());
    let baker = Baker::new(Arc::clone(&recorder));
    baker.want(2, &[road(500)]);
    idle(&baker);
    // View 1's bakes queue behind one under way; view 2 asks for what it has.
    let gate = recorder.gate.lock().unwrap();
    baker.want(1, &[road(1), road(2), road(3)]);
    under_way(&recorder);
    baker.want(2, &[road(500)]);
    // View 2 then wants a new moment: it bakes before what view 1 still waits for.
    baker.want(2, &[road(600)]);
    drop(gate);
    idle(&baker);
    assert_eq!(
        *recorder.baked.lock().unwrap(),
        vec![road(500), road(1), road(600), road(2), road(3)],
        "view 1's bake under way, the newest request, then what view 1 still waits for"
    );
    let kept = recorder.kept.lock().unwrap().clone();
    for key in [road(1), road(2), road(3), road(600)] {
        assert!(kept.contains(&key), "{key:?} kept: {kept:?}");
    }
    // View 1's newer request replaces its own older one: its queued images never bake, and the
    // one under way is not kept when it lands.
    let gate = recorder.gate.lock().unwrap();
    let from = recorder.baked.lock().unwrap().len();
    baker.want(1, &[road(7), road(8), road(9)]);
    under_way(&recorder);
    baker.want(1, &[road(10)]);
    drop(gate);
    idle(&baker);
    assert_eq!(
        recorder.baked.lock().unwrap()[from..],
        [road(7), road(10)],
        "the bake under way, then the newer request's"
    );
    let kept = recorder.kept.lock().unwrap().clone();
    assert!(kept.contains(&road(10)));
    assert!(!kept.contains(&road(7)), "replaced: {kept:?}");
}

/// WHAT IS KEPT IS WHAT THE VIEWS WANT AND DREW: a view's drawing pass that records what it drew
/// drops every bake no live view wants or drew; a view that closes takes its own with it.
#[test]
fn a_drawing_pass_drops_what_no_view_wants_or_drew_and_a_closed_view_takes_its_own() {
    let recorder = Arc::new(Recorder::default());
    let baker = Baker::new(Arc::clone(&recorder));
    baker.want(1, &[road(1)]);
    baker.want(2, &[road(50)]);
    idle(&baker);
    baker.drew(1, &[road(1)]);
    baker.drew(2, &[road(50)]);
    baker.want(1, &[road(2)]);
    idle(&baker);
    baker.drew(1, &[road(1)]);
    assert_eq!(
        sorted(&recorder),
        vec![road(1), road(2), road(50)],
        "the moment drawn, the one wanted, the other view's"
    );
    baker.drew(1, &[road(2)]);
    assert_eq!(
        sorted(&recorder),
        vec![road(2), road(50)],
        "the last moment went"
    );
    baker.forget(2);
    assert_eq!(sorted(&recorder), vec![road(2)], "the closed view's went");
}

/// ONCE NO PAGE IS ON SCREEN EACH VIEW KEEPS ONLY WHAT IT DREW, AND NOTHING BAKES: hidden, every
/// bake but each view's last drawn moment goes and a request bakes nothing; shown again, what
/// each view last asked for bakes. NEGATIVE CONTROL: shown, a request bakes.
#[test]
fn a_hidden_page_keeps_only_what_it_drew_and_bakes_nothing_until_it_shows() {
    let recorder = Arc::new(Recorder::default());
    let baker = Baker::new(Arc::clone(&recorder));
    baker.want(1, &[road(1)]);
    idle(&baker);
    baker.drew(1, &[road(1)]);
    baker.want(1, &[road(2)]);
    idle(&baker);
    baker.want(2, &[road(50)]);
    idle(&baker);
    assert_eq!(sorted(&recorder), vec![road(1), road(2), road(50)]);
    baker.set_shown(false);
    assert_eq!(
        sorted(&recorder),
        vec![road(1)],
        "hidden, only the moment drawn stays"
    );
    baker.want(1, &[road(3)]);
    idle(&baker);
    assert_eq!(sorted(&recorder), vec![road(1)], "and nothing bakes");
    baker.set_shown(true);
    idle(&baker);
    assert_eq!(
        sorted(&recorder),
        vec![road(1), road(3), road(50)],
        "shown, each view's moment bakes"
    );
}

/// A VIEW THAT DRAWS NO IMAGES WANTS AND KEEPS NONE: a view that drew the road and then draws
/// the words alone (Serious Mode, High Contrast, a pane too narrow, a window too short) keeps
/// nothing, and showing the page again bakes nothing for it. NEGATIVE CONTROL: a view still
/// asking for its road, its moment not kept, has it baked on the show.
#[test]
fn a_view_that_draws_no_images_keeps_and_bakes_none() {
    let recorder = Arc::new(Recorder::default());
    let baker = Baker::new(Arc::clone(&recorder));
    baker.want(1, &[road(1)]);
    idle(&baker);
    baker.drew(1, &[road(1)]);
    baker.set_shown(false);
    // Control: a view still asking for its road has it baked again once shown.
    baker.want(2, &[road(7)]);
    baker.set_shown(true);
    idle(&baker);
    assert_eq!(sorted(&recorder), vec![road(1), road(7)]);
    baker.drew(2, &[road(7)]);
    baker.none(1);
    assert_eq!(sorted(&recorder), vec![road(7)], "its road went");
    baker.set_shown(false);
    baker.set_shown(true);
    idle(&baker);
    let landed = recorder.landed.load(Ordering::SeqCst);
    assert_eq!(sorted(&recorder), vec![road(7)], "and nothing baked for it");
    assert_eq!(landed, 2, "the two bakes before, none since");
}

fn sorted(recorder: &Recorder) -> Vec<Key> {
    let mut kept = recorder.kept.lock().unwrap().clone();
    kept.sort_by_key(|(art, _, _)| match art {
        ImageArt::Road(road) => road.permille,
        ImageArt::Cat(_) => u16::MAX,
    });
    kept
}

/// The app's cache keeps a view's working set: two moments of the longest road at Mono 22 on a
/// 2x display, each a road and a cat, fit the byte budget. NEGATIVE CONTROL: past it, the bake
/// drawn least recently goes first, never the one just kept.
#[test]
fn the_cache_keeps_two_moments_of_the_longest_road_within_its_budget() {
    let spec = |done| LockupSpec {
        road: true,
        px: 22.0,
        scale: 2.0,
        alab: true,
        dark: true,
        done,
        total: 8,
        pose: CatPose::Walk,
    };
    let wide = 20_000.0;
    let moments = [spec(3).wanted(wide), spec(4).wanted(wide)];
    let keys: Vec<Key> = moments.iter().flat_map(|moment| moment.keys()).collect();
    let bytes: usize = keys.iter().map(|(_, w, h)| (w * h * 4) as usize).sum();
    assert!(
        bytes <= crate::widget::IMAGES_BUDGET,
        "{bytes} bytes over the budget"
    );
    for (art, w, h) in &keys {
        art.keep(*w, *h, vec![0; (w * h * 4) as usize].into());
    }
    assert!(moments.iter().all(|moment| moment.ready()));
    // Draw the first moment's road, so the second's is the least recently drawn.
    let (art, w, h) = keys[0];
    assert!(art.kept(w, h).is_some());
    let filler = |permille| {
        ImageArt::Road(RoadArt {
            length: 3000,
            permille,
            done: false,
            dark: false,
        })
    };
    let mut n = 0;
    while moments[1].ready() {
        let (w, h) = (keys[0].1, keys[0].2);
        filler(n).keep(w, h, vec![0; (w * h * 4) as usize].into());
        assert!(filler(n).is_kept(w, h), "the bake just kept stays");
        n += 1;
        assert!(n < 16, "the budget never evicted");
    }
    assert!(art.is_kept(w, h), "the road drawn last stays longest");
    assert!(crate::widget::images_kept_bytes() <= crate::widget::IMAGES_BUDGET);
    crate::widget::retain_images(&[]);
    assert_eq!(crate::widget::images_kept_bytes(), 0);
}

/// A RUN KEEPS ABOUT TWO MOMENTS (§4.5): a view 900 points wide on a 2x display drawn through a
/// 16-step run, each step's bake landing a pass after it is asked for, keeps at most the moment
/// it shows and the one just baked as that bake lands, and the one it shows once it draws it;
/// never the run's past. NEGATIVE CONTROL: a view whose passes never say what they drew keeps
/// the run's past, past two moments by its third (one pass that says nothing is not enough: the
/// next pass's record of the moment it still shows drops the one before).
#[test]
fn a_sixteen_step_run_keeps_about_two_moments() {
    let view = crate::system_state::SystemStateViewState::default();
    let requester = &view.lockup_requester;
    let width = 900.0 - 2.0 * 24.0;
    let moment = |done| {
        LockupSpec {
            road: true,
            px: 22.0,
            scale: 2.0,
            alab: true,
            dark: true,
            done,
            total: 16,
            pose: CatPose::Walk,
        }
        .wanted(width)
    };
    let bytes = |baked: Baked| -> usize {
        baked
            .keys()
            .iter()
            .map(|(_, w, h)| (w * h * 4) as usize)
            .sum()
    };
    let two = 2 * bytes(moment(16));
    hold(true);
    let mut drawn: Option<Baked> = None;
    for done in 0..=16 {
        let wanted = moment(done);
        requester.want(&wanted.keys());
        // The pass draws what is kept: the last moment while this one bakes.
        let paint = wanted.spec.paint(width, drawn);
        drawn = paint.images.or(drawn);
        requester.drew(&drawn.map_or_else(Vec::new, |drawn| drawn.keys().to_vec()));
        settle();
        // The most it ever keeps: the moment it shows and the one that just landed.
        let landed = crate::widget::images_kept_bytes();
        assert!(
            landed <= two,
            "{done}: at most two moments as its bake lands: {landed} of {two}"
        );
        let paint = wanted.spec.paint(width, drawn);
        assert_eq!(paint.images, Some(wanted), "{done}: its bake landed");
        drawn = paint.images;
        requester.drew(&wanted.keys());
        assert!(
            crate::widget::images_kept_bytes() <= bytes(wanted),
            "{done}: one moment kept once it is drawn"
        );
    }
    drop(view);
    assert_eq!(
        crate::widget::images_kept_bytes(),
        0,
        "the closed view's went"
    );
    // Control: a view that asks for each moment and never says what it drew.
    let careless = crate::system_state::SystemStateViewState::default();
    for done in 0..3 {
        careless.lockup_requester.want(&moment(done).keys());
        settle();
    }
    let piled = crate::widget::images_kept_bytes();
    assert!(
        piled > two,
        "control: its past moments pile up: {piled} of {two}"
    );
    drop(careless);
    hold(false);
}
