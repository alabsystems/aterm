// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The walker at rest ([`Engine::look_due`]): with no activity for [`LIFT_FOR`] it looks at the
//! panes in a pair of looks [`DISCOVER_EVERY`] apart once every [`REST_EVERY`], a pane's
//! folders join and leave on two looks in a row as they do at [`DISCOVER_EVERY`], it sleeps from
//! one pair to the next, it rests only while its mount watch's key on `/` is armed, and a mount
//! or unmount it hears rereads the mount table at once. Each law drives the production
//! [`Engine::apply`], [`Engine::discover_if_due`] and [`Engine::wakes_at`] on a clock the test
//! steps [`DISCOVER_EVERY`] at a time, with the drawn deck's glance asks every
//! [`DECK_ASK_EVERY`] that keep its scopes demanded, or the mount watch's own [`MountEars`] and
//! [`mounts_heard`].

use super::*;

/// One tab whose one pane sits in a repository, `w`, beside another, `x`, under a scratch home,
/// demanded as a drawn deck demands it, on a walker whose mount watch's key is armed (`ears`),
/// whose mount table reads the test counts.
struct Resting {
    rig: Rig,
    _tmp: aterm_tempfile::TempDir,
    home: PathBuf,
    w: PathBuf,
    x: PathBuf,
    t0: Instant,
    /// The mount table's reads.
    tables: Rc<Cell<usize>>,
    /// A volume the table lists once the test mounts it.
    mounted: Rc<RefCell<Option<Mount>>>,
}

impl Resting {
    fn new() -> Self {
        Self::hearing(Arc::new(AtomicBool::new(true)))
    }

    /// [`Self::new`] on a walker whose mount watch says whether its key is armed by `ears`.
    fn hearing(ears: Arc<AtomicBool>) -> Self {
        let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
        let home = base(&tmp).join("home");
        let (w, x) = (home.join("w"), home.join("x"));
        mkdir(&w.join(".git"));
        mkdir(&x.join(".git"));
        let mut rig = Rig::new(&home);
        rig.engine.ears = Some(ears);
        let tables: Rc<Cell<usize>> = Rc::default();
        let mounted: Rc<RefCell<Option<Mount>>> = Rc::default();
        let read = rig.engine.policy.as_ref().expect("a policy").mounts.clone();
        rig.engine.table = Box::new({
            let (n, mounted) = (Rc::clone(&tables), Rc::clone(&mounted));
            move || {
                n.set(n.get() + 1);
                let mut table = read.clone();
                table.mounts.extend(mounted.borrow().clone());
                table
            }
        });
        rig.facts(&[(1, 1, &w)], &[], &[]);
        Self {
            rig,
            _tmp: tmp,
            home,
            w,
            x,
            t0: Instant::now(),
            tables,
            mounted,
        }
    }

    /// Scripted second `s`.
    fn at(&self, s: u64) -> Instant {
        self.t0 + Duration::from_secs(s)
    }

    /// One turn of the walker's loop at second `s`: the deck's glance asks where one is due,
    /// then the discovery where one is due. True when a periodic look ran.
    fn turn(&mut self, s: u64, asks: bool) -> bool {
        let now = self.at(s);
        if asks && s.is_multiple_of(DECK_ASK_EVERY.as_secs()) {
            for scope in [DiskScope::Tab(tab_id(1)), DiskScope::Aterm] {
                self.rig.demand(scope, Pace::Glance, now);
            }
        }
        self.look(now)
    }

    /// The discovery where one is due at `now`: true when a periodic look ran.
    fn look(&mut self, now: Instant) -> bool {
        let round = self.rig.engine.round;
        self.rig.engine.discover_if_due(now);
        self.rig.engine.round > round
    }

    /// Turns every [`DISCOVER_EVERY`] from second `from` to before `until`, `act` taken before
    /// each: the seconds at which a periodic look ran.
    fn run(&mut self, from: u64, until: u64, mut act: impl FnMut(&mut Self, u64)) -> Vec<u64> {
        let mut looked = Vec::new();
        for s in (from..until).step_by(5) {
            act(self, s);
            if self.turn(s, true) {
                looked.push(s);
            }
        }
        looked
    }

    /// The pane's members now.
    fn members(&self) -> Vec<PathBuf> {
        self.rig.engine.panes[&1]
            .membership
            .members()
            .cloned()
            .collect()
    }
}

/// Every [`DISCOVER_EVERY`] from `from` to before `until`.
fn every(from: u64, until: u64) -> impl Iterator<Item = u64> {
    (from..until).step_by(5)
}

/// THE WALKER AT REST LOOKS TWICE A MINUTE, AND A GLANCE ASK DOES NOT WAKE IT: over 300
/// scripted seconds with the deck's glance asks every 30 s and nothing else, it looks every
/// [`DISCOVER_EVERY`] for [`LIFT_FOR`] after the scopes were first asked (0 to 55, 12 looks),
/// then in pairs, 110 and 115, 170 and 175, 230 and 235, 290 and 295: 20 looks, where it took
/// 60. A program that comes to the pane's front at 100 (a `Facts` message that changed) is
/// activity: looks every [`DISCOVER_EVERY`] from 100 to 155, then pairs again from 210: 28. So is
/// a look-pace ask at 100 (`sys disk`, a Disk lens): 28.
///
/// Negative controls, each applied once by hand: [`REST_EVERY`] taken as two
/// [`DISCOVER_EVERY`] in [`Engine::look_due`] (a look every 5 s, 60); a glance ask counted as
/// activity (60).
#[test]
fn the_walker_at_rest_looks_twice_a_minute_and_a_glance_ask_does_not_wake_it() {
    assert_eq!(
        (DISCOVER_EVERY, REST_EVERY, LIFT_FOR, DECK_ASK_EVERY),
        (
            Duration::from_secs(5),
            Duration::from_secs(60),
            Duration::from_secs(60),
            Duration::from_secs(30)
        )
    );
    let mut quiet = Resting::new();
    let looked = quiet.run(0, 300, |_, _| {});
    let resting: Vec<u64> = every(0, 60)
        .chain([110, 115, 170, 175, 230, 235, 290, 295])
        .collect();
    assert_eq!(resting.len(), 20);
    assert_eq!(looked, resting, "glance asks alone");
    assert_eq!(
        quiet.rig.engine.look_due(),
        Some(quiet.at(350)),
        "the next pair opens a minute after the last opened"
    );

    let woken: Vec<u64> = every(0, 60)
        .chain(every(100, 160))
        .chain([210, 215, 270, 275])
        .collect();
    assert_eq!(woken.len(), 28);
    let mut fronted = Resting::new();
    let looked = fronted.run(0, 300, |r, s| {
        if s == 100 {
            let mut facts = r.rig.engine.facts.clone();
            facts.panes[0].front_pid += 1;
            r.rig.engine.apply(ToWalker::Facts(facts));
        }
    });
    assert_eq!(looked, woken, "a program came to the front at 100");
    let mut asked = Resting::new();
    let looked = asked.run(0, 300, |r, s| {
        if s == 100 {
            let at = r.at(s);
            r.rig.demand(DiskScope::Tab(tab_id(1)), Pace::Look, at);
        }
    });
    assert_eq!(looked, woken, "a look-pace ask at 100");
}

/// A PANE'S FOLDERS KEEP THEIR TWO LOOKS AT REST: a program's `cd` away from `w` that the
/// resting look at 110 sees and the one at 115 does not (no `Facts` message: the kernel's cwd
/// moved, no report) leaves `w` the pane's member at both looks, and `x` never joins; a `cd` to
/// `x` that stays from 160 joins `x` and drops `w` at 175, the second look of the next pair.
///
/// Negative controls, each applied once by hand: a pane unseen for [`PANE_STALE_AFTER`] started
/// again at rest too (the look at 110, 55 s after the one at 55, is a first: the members are
/// `x` alone there); no second look in a pair (looks at 110 and 165 alone after 55, a minute
/// apart).
#[test]
fn a_panes_folders_keep_their_two_looks_at_rest() {
    let mut r = Resting::new();
    let (w, x) = (r.w.clone(), r.x.clone());
    let mut members: BTreeMap<u64, Vec<PathBuf>> = BTreeMap::new();
    // The members after each turn, taken before the next.
    let looked = r.run(0, 180, |r, s| {
        if s > 0 {
            members.insert(s - 5, r.members());
        }
        match s {
            110 | 160 => r.rig.cd(1, &x),
            115 => r.rig.cd(1, &w),
            _ => {}
        }
    });
    members.insert(175, r.members());
    assert_eq!(
        looked,
        every(0, 60).chain([110, 115, 170, 175]).collect::<Vec<_>>()
    );
    assert_eq!(members[&110], vec![w.clone()], "a cd one resting look saw");
    assert_eq!(members[&115], vec![w.clone()], "and the next did not");
    assert_eq!(members[&170], vec![w], "a cd that stays, seen once");
    assert_eq!(members[&175], vec![x], "seen twice: it joins, w leaves");
}

/// A MOUNT HEARD AT REST REREADS THE TABLE AT ONCE: at 130, between two resting pairs, a volume
/// mounted under the home is heard (`ToWalker::Mounts`), and that turn reads the mount table
/// once and holds the volume in its policy, where the next resting look would have read it at
/// 170. With no scope demanded (the deck's asks lapsed) the heard mount rereads the table too.
///
/// Negative control, applied once by hand: the mount heard and ignored (no look at 130).
#[test]
fn a_mount_heard_at_rest_rereads_the_table_at_once() {
    let mut r = Resting::new();
    let looked = r.run(0, 130, |_, _| {});
    assert_eq!(looked, every(0, 60).chain([110, 115]).collect::<Vec<_>>());
    assert_eq!(r.tables.get(), 14, "one read a look");
    let volume = r.volume();
    *r.mounted.borrow_mut() = Some(volume.clone());
    r.rig.engine.apply(ToWalker::Mounts);
    assert!(r.turn(130, true), "the mount is a look at once");
    assert_eq!(r.tables.get(), 15, "read at the mount");
    assert!(r.holds(&volume), "the policy holds the volume");

    let mut lapsed = Resting::new();
    assert!(lapsed.turn(0, true));
    let reads = lapsed.tables.get();
    *lapsed.mounted.borrow_mut() = Some(volume.clone());
    lapsed.rig.engine.apply(ToWalker::Mounts);
    assert!(!lapsed.turn(200, false), "no scope is demanded: no look");
    assert_eq!(lapsed.tables.get(), reads + 1, "read at the mount");
    assert!(lapsed.holds(&volume), "the policy holds the volume");
}

impl Resting {
    /// A removable volume mounted under the home.
    fn volume(&self) -> Mount {
        Mount {
            on: self.home.join("disk"),
            from: "/dev/disk9s1".into(),
            fstype: "apfs".into(),
            local: true,
            automounted: false,
            removable: true,
            read_only: false,
            container: Some("disk9".into()),
        }
    }

    /// Whether the walker's policy holds `volume`.
    fn holds(&self, volume: &Mount) -> bool {
        self.rig
            .engine
            .policy
            .as_ref()
            .expect("a policy")
            .mounts
            .mounts
            .contains(volume)
    }
}

/// A MOUNT HEARD BETWEEN LOOKS READS THE TABLE AND COUNTS NO LOOK: while the walker looks every
/// [`DISCOVER_EVERY`], a program sits in `x` from 49 to 51 (no report), so the look at 50 sees
/// `x`; a volume mounted at 50.3 is heard, and the turn at 50.4 reads the mount table once and
/// holds the volume, with no look (the round does not move); the look at 55 sees `w` again, and
/// `w` stays the pane's member: a stay shorter than one look never joins, as without the mount.
/// The mount is activity: the walker looks every [`DISCOVER_EVERY`] to 110 (where with no mount
/// its pair would be 110 and 115), then rests, a pair at 165 and 170.
///
/// Negative control, applied once by hand: a heard mount runs a periodic look at once, however
/// soon after the last (a look at 50.4: `x` seen twice in a row, 0.4 s apart, joins).
#[test]
fn a_mount_heard_between_looks_reads_the_table_and_counts_no_look() {
    let mut r = Resting::new();
    let (w, x) = (r.w.clone(), r.x.clone());
    let looked = r.run(0, 50, |_, _| {});
    assert_eq!(looked, every(0, 50).collect::<Vec<_>>());
    r.rig.cd(1, &x);
    assert!(r.turn(50, true), "the look at 50 sees x");
    let reads = r.tables.get();
    let volume = r.volume();
    *r.mounted.borrow_mut() = Some(volume.clone());
    r.rig.engine.apply(ToWalker::Mounts);
    let heard = r.at(50) + Duration::from_millis(400);
    assert!(!r.look(heard), "no look 0.4 s after the last");
    assert_eq!(
        r.tables.get(),
        reads + 1,
        "the table read at the mount alone"
    );
    assert!(r.holds(&volume), "the policy holds the volume");
    r.rig.cd(1, &w);
    let mut members = BTreeMap::new();
    let looked = r.run(55, 180, |r, s| {
        if s == 60 {
            members.insert(55, r.members());
        }
    });
    assert_eq!(members[&55], vec![w], "x was seen at one look");
    assert_eq!(
        looked,
        every(55, 115).chain([165, 170]).collect::<Vec<_>>(),
        "the mount is activity"
    );
}

/// A SCOPE THE LAST LOOK DID NOT COVER WAKES THE WALKER: two tabs, one pane each (`w`, `x`);
/// the deck asks the first tab and aterm at glance pace every [`DECK_ASK_EVERY`] until 100,
/// when the window shows the second tab instead, whose scope it then asks every
/// [`DECK_ASK_EVERY`] (no `Facts` message: the tabs are as they were). The new scope is
/// activity: looks 0 to 55, then 100 to 155, then pairs at 210 and 215, 270 and 275 (28), as a
/// `Facts` change at 100 gives.
///
/// Negative control, applied once by hand: only what stirred the walker is activity (the tab
/// switch is none: looks 0 to 55, then pairs from 110, 20).
#[test]
fn a_scope_the_last_look_did_not_cover_wakes_the_walker() {
    let mut r = Resting::new();
    let (w, x) = (r.w.clone(), r.x.clone());
    r.rig.facts(&[(1, 1, &w), (2, 2, &x)], &[], &[]);
    let mut looked = Vec::new();
    for s in every(0, 300) {
        let now = r.at(s);
        let shown = if s < 100 { 1 } else { 2 };
        let since = if s < 100 { 0 } else { 100 };
        if (s - since).is_multiple_of(DECK_ASK_EVERY.as_secs()) {
            for scope in [DiskScope::Tab(tab_id(shown)), DiskScope::Aterm] {
                r.rig.demand(scope, Pace::Glance, now);
            }
        }
        if r.look(now) {
            looked.push(s);
        }
    }
    let woken: Vec<u64> = every(0, 60)
        .chain(every(100, 160))
        .chain([210, 215, 270, 275])
        .collect();
    assert_eq!(looked, woken, "the second tab shown at 100");
}

/// A WALKER AT REST SLEEPS FROM ONE PAIR TO THE NEXT: with its walks done (none owed), a walker
/// that sleeps after the look at 50 wakes for the look at 55; after the look at 55, which ends
/// its active minute, for the look at 110, the first of the next pair; after 110 for 115, and
/// after 115 for 170. It never wakes at 60 to find no look due.
///
/// Negative control, applied once by hand: the sleep bounded by [`DISCOVER_EVERY`] from now
/// (wakes at 60 after the look at 55).
#[test]
fn a_walker_at_rest_sleeps_from_one_pair_to_the_next() {
    let mut r = Resting::new();
    let mut wakes = Vec::new();
    for s in every(0, 120) {
        let looked = r.turn(s, true);
        r.rig.settle(r.at(s));
        if looked && [50, 55, 110, 115].contains(&s) {
            let at = r.rig.engine.wakes_at(r.at(s));
            wakes.push((s, at.duration_since(r.t0).as_secs()));
        }
    }
    assert_eq!(
        wakes,
        vec![(50, 55), (55, 110), (110, 115), (115, 170)],
        "each sleep's end"
    );
}

/// A test stand-in for the platform's file events whose every arm takes, or fails as one does
/// when descriptors or inotify watches ran out, as `take` says.
struct Arms {
    take: Arc<AtomicBool>,
}

impl FileEvents for Arms {
    fn arm(&mut self, _key: u64, _path: &Path) -> std::io::Result<()> {
        if self.take.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(std::io::Error::other("no watch left"))
        }
    }

    fn arm_directory(&mut self, key: u64, path: &Path) -> std::io::Result<()> {
        self.arm(key, path)
    }

    fn disarm(&mut self, _key: u64) {}

    fn wait(&mut self, _timeout: Option<Duration>, _touched: &mut Vec<u64>) -> std::io::Result<()> {
        Ok(())
    }

    fn waker(&self) -> Waker {
        Arc::new(|| {})
    }
}

/// A WALKER RESTS ONLY WHILE ITS MOUNT WATCH'S KEY ON `/` IS ARMED: a mount or unmount looks
/// again only at an armed key, so one whose arm did not take hears none. Under the mount
/// watch's own [`MountEars`], an arm of `/` that fails leaves the walker looking every
/// [`DISCOVER_EVERY`] over 300 s of glance asks (60 looks); one that takes lets it rest (0 to
/// 55, then 110 and 115), and the arm again that fails at 130, after one of the key's events
/// (whose look the watch tells: [`ToWalker::Mounts`]), puts it back to a look every
/// [`DISCOVER_EVERY`] from 130. A disarm leaves the key unarmed.
///
/// Negative control, applied once by hand: [`MountEars`] keeping the key armed whatever the arm
/// did (the walker whose arm failed rests: 20 looks).
#[test]
fn a_walker_rests_only_while_its_mount_watchs_key_on_root_is_armed() {
    let root = Path::new("/");
    let take = Arc::new(AtomicBool::new(false));
    let mut ears = MountEars::over(Box::new(Arms {
        take: Arc::clone(&take),
    }));
    assert!(ears.arm_directory(MOUNTS_KEY, root).is_err());
    let mut unarmed = Resting::hearing(Arc::clone(&ears.armed));
    assert_eq!(
        unarmed.run(0, 300, |_, _| {}),
        every(0, 300).collect::<Vec<_>>(),
        "the key on / did not arm: no rest"
    );

    take.store(true, Ordering::Release);
    assert!(ears.arm_directory(MOUNTS_KEY, root).is_ok());
    let mut armed = Resting::hearing(Arc::clone(&ears.armed));
    let looked = armed.run(0, 130, |_, _| {});
    assert_eq!(
        looked,
        every(0, 60).chain([110, 115]).collect::<Vec<_>>(),
        "armed: it rests"
    );
    take.store(false, Ordering::Release);
    assert!(ears.arm_directory(MOUNTS_KEY, root).is_err());
    armed.rig.engine.apply(ToWalker::Mounts);
    assert_eq!(
        armed.run(130, 300, |_, _| {}),
        every(130, 300).collect::<Vec<_>>(),
        "the arm lost at 130: no rest from there"
    );

    take.store(true, Ordering::Release);
    assert!(ears.arm(MOUNTS_KEY, root).is_ok());
    assert!(ears.armed.load(Ordering::Acquire), "a file arm that takes");
    ears.disarm(MOUNTS_KEY);
    assert!(!ears.armed.load(Ordering::Acquire), "disarmed");
}

/// THE MOUNT WATCH'S TELL IS ONE `Mounts` DOWN THE WALKER'S INBOX: [`hear_mounts`] hands the
/// watch [`mounts_heard`], and each tell it makes sends the walker one [`ToWalker::Mounts`]
/// through the inbox [`spawn_walker`] sets.
///
/// Negative control, applied once by hand: the tell sends nothing (none in the inbox).
#[test]
fn the_mount_watchs_tell_is_one_mounts_down_the_walkers_inbox() {
    let shared: &'static Shared = Box::leak(Box::new(Shared::new()));
    let (tx, inbox) = std::sync::mpsc::channel();
    assert!(shared.inbox.set(tx).is_ok());
    let mut tell = mounts_heard(shared);
    assert!(inbox.try_recv().is_err(), "nothing before the tell");
    tell(MOUNTS_KEY);
    let sent: Vec<ToWalker> = inbox.try_iter().collect();
    assert_eq!(sent.len(), 1, "one message a tell");
    assert!(matches!(sent[0], ToWalker::Mounts), "and it is Mounts");
}
