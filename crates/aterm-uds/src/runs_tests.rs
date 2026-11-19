// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Two readers that both found the launcher dead each appended a `dead_seen` line; the first
/// stands. Negative control: let a later line replace the first, and the later (wider)
/// horizon reads.
#[test]
fn a_record_with_two_dead_seen_lines_keeps_the_first() {
    let rec = Record::parse(
        "run v=1 boot=b id=71-1792163800331520 uniqueid=5000 start=1 sid=-\n\
         dead_seen uniqueid=9000 at=1792163900000\n\
         dead_seen uniqueid=9500 at=1792164000000\n",
    );
    assert_eq!(rec.dead_seen, Some(9000));
    assert_eq!(rec.dead_seen_ms, Some(1_792_163_900_000));
}

/// A record's numbers are digits alone: a sign `str::parse` would take names no pid, group
/// or window edge. NEGATIVE CONTROL: parse the fields with `str::parse`, and the plus-signed
/// ones read as numbers.
#[test]
fn a_signed_number_in_a_record_is_no_number() {
    assert_eq!(RunId::parse("+41983-100"), None);
    assert_eq!(RunId::parse("41983-+100"), None);
    let rec = Record::parse(
        "run v=1 boot=b id=71-100 uniqueid=+5000 start=+1 sid=-\n\
         group pgid=+4000 leader_birth=+9 leader_uniqueid=+3000 stage=t\n\
         group pgid=4001 leader_birth=+9 leader_uniqueid=+3000 stage=t\n\
         closed pgid=+4001\n\
         dead_seen uniqueid=+9000 at=1\n",
    );
    assert_eq!((rec.uniqueid, rec.start_ms), (0, 0));
    assert_eq!(rec.groups.len(), 1, "a signed pgid names no group");
    let g = &rec.groups[0];
    assert_eq!(
        (g.pgid, g.leader_birth, g.leader_uniqueid, g.closed),
        (4001, Birth(0), 0, false)
    );
    assert_eq!(rec.dead_seen, None);
}

/// A kernel laid out by a W-kin test: each process with its lineage and, where the kernel names
/// a parent by its unique id, its hop; each one's thread has run, its verb names no program
/// kept by design, and its executable is `/bin/sh` unless laid out otherwise. A pid not laid
/// out reads as absent unless a presence is laid out for it.
#[derive(Default)]
struct KinKernel {
    lineages: std::collections::HashMap<i32, Lineage>,
    hops: std::collections::HashMap<i32, Hop>,
    groups: std::collections::HashMap<u32, Vec<i32>>,
    exes: std::collections::HashMap<i32, PathBuf>,
    sessions: std::collections::HashMap<i32, u32>,
    /// Whose verb names a program launchd keeps by design.
    designed: std::collections::HashSet<i32>,
    /// Whose thread macOS's program check holds.
    held: std::collections::HashSet<i32>,
    /// How a pid not laid out reads ([`Kernel::presence`]).
    presences: std::collections::HashMap<i32, crate::process::ProcessPresence>,
    /// The lineage a pid reads as once its executable was read: another process took it.
    taken: std::collections::HashMap<i32, Lineage>,
    /// The program a group's leader runs once its group was listed ([`Kernel::members`]).
    execs_when_listed: std::collections::HashMap<i32, PathBuf>,
    /// The pids whose executable was read.
    exe_read: std::cell::RefCell<std::collections::HashSet<i32>>,
    /// The groups listed.
    listed: std::cell::RefCell<std::collections::HashSet<u32>>,
}

impl KinKernel {
    /// Lays out `pid`, born at `uniqueid` (its birth too), in group `pgid` under `parent`,
    /// whose unique id it names as `parent_uniqueid`.
    fn put(&mut self, pid: i32, uniqueid: u64, pgid: u32, parent: u32, parent_uniqueid: u64) {
        let identity = Identity {
            pid,
            birth: Birth(uniqueid),
            idversion: None,
        };
        self.lineages.insert(
            pid,
            Lineage {
                identity,
                uniqueid,
                parent,
                pgid,
                launcher_version: None,
                tty: None,
            },
        );
        self.hops.insert(
            pid,
            Hop {
                identity,
                uniqueid,
                parent_uniqueid,
                parent,
                pgid,
                tty: None,
            },
        );
        self.groups.entry(pgid).or_default().push(pid);
    }

    fn kin(&self, pid: i32) -> Kin {
        Kin::of(&self.lineages[&pid])
    }

    /// `pid` runs `exe`.
    fn runs(&mut self, pid: i32, exe: &str) {
        self.exes.insert(pid, PathBuf::from(exe));
    }

    /// Lays out `pids` below `top`, each in group 900 and the parent of the next, born from
    /// `uniqueid` on in that order: the last one's pid and unique id.
    fn chain(&mut self, top: i32, pids: std::ops::Range<i32>, mut uniqueid: u64) -> (u32, u64) {
        let mut parent = (top.unsigned_abs(), self.lineages[&top].uniqueid);
        for pid in pids {
            self.put(pid, uniqueid, 900, parent.0, parent.1);
            parent = (pid.unsigned_abs(), uniqueid);
            uniqueid += 1;
        }
        parent
    }
}

impl Kernel for KinKernel {
    fn birth(&self, pid: i32) -> Option<Birth> {
        self.lineages.get(&pid).map(|l| l.identity.birth)
    }
    fn presence(&self, pid: i32) -> crate::process::ProcessPresence {
        if self.lineages.contains_key(&pid) {
            crate::process::ProcessPresence::Present
        } else {
            self.presences
                .get(&pid)
                .copied()
                .unwrap_or(crate::process::ProcessPresence::Absent)
        }
    }
    fn lineage(&self, pid: i32) -> Option<Lineage> {
        if self.exe_read.borrow().contains(&pid)
            && let Some(taken) = self.taken.get(&pid)
        {
            return Some(*taken);
        }
        self.lineages.get(&pid).copied()
    }
    fn members(&self, pgid: u32) -> Option<GroupMembers> {
        self.listed.borrow_mut().insert(pgid);
        self.groups.get(&pgid).map(|pids| GroupMembers {
            pids: pids.clone(),
            complete: true,
        })
    }
    fn exe(&self, pid: i32) -> Option<PathBuf> {
        self.exe_read.borrow_mut().insert(pid);
        let listed = u32::try_from(pid).is_ok_and(|pgid| self.listed.borrow().contains(&pgid));
        self.lineages.contains_key(&pid).then(|| {
            listed
                .then(|| self.execs_when_listed.get(&pid))
                .flatten()
                .or_else(|| self.exes.get(&pid))
                .cloned()
                .unwrap_or_else(|| PathBuf::from("/bin/sh"))
        })
    }
    fn hop(&self, pid: i32) -> Option<Hop> {
        self.hops.get(&pid).copied()
    }
    fn by_design(&self, pid: i32) -> Option<bool> {
        self.lineages
            .contains_key(&pid)
            .then(|| self.designed.contains(&pid))
    }
    fn session(&self, pid: i32) -> Option<u32> {
        self.sessions.get(&pid).copied()
    }
    fn markers(&self, _: i32) -> Option<Markers> {
        None
    }
    fn held(&self, pid: i32) -> Option<bool> {
        self.lineages
            .contains_key(&pid)
            .then(|| self.held.contains(&pid))
    }
    fn held_now(&self) -> Option<HeldNow> {
        None
    }
    fn launchd_version(&self) -> Option<i32> {
        None
    }
    fn tty_master_held(&self, _: u32) -> Option<bool> {
        None
    }
    fn uid_pids(&self) -> Option<Vec<i32>> {
        let mut pids: Vec<i32> = self.lineages.keys().copied().collect();
        pids.sort_unstable();
        Some(pids)
    }
    fn now_ms(&self) -> u64 {
        0
    }
    fn end(&self, _: Identity, _: &dyn Fn(&Identity) -> bool) -> Result<Ended, EndRefusal> {
        Err(EndRefusal::NotProven)
    }
}

/// A kernel that reads as `before` until the program of `at` was read whole (its executable,
/// then its lineage again, as `program_of` reads them), and as `after` from then on: what
/// changed between the two happened just after that read.
struct Turning {
    before: KinKernel,
    after: KinKernel,
    at: i32,
    /// 0 until `at`'s executable is read, 1 until its lineage is read after it, 2 since.
    read: std::cell::Cell<u8>,
}

impl Turning {
    fn now(&self) -> &KinKernel {
        if self.read.get() == 2 {
            &self.after
        } else {
            &self.before
        }
    }
}

impl Kernel for Turning {
    fn birth(&self, pid: i32) -> Option<Birth> {
        self.now().birth(pid)
    }
    fn presence(&self, pid: i32) -> crate::process::ProcessPresence {
        self.now().presence(pid)
    }
    fn lineage(&self, pid: i32) -> Option<Lineage> {
        let read = self.now().lineage(pid);
        if pid == self.at && self.read.get() == 1 {
            self.read.set(2);
        }
        read
    }
    fn members(&self, pgid: u32) -> Option<GroupMembers> {
        self.now().members(pgid)
    }
    fn exe(&self, pid: i32) -> Option<PathBuf> {
        let read = self.now().exe(pid);
        if pid == self.at && self.read.get() == 0 {
            self.read.set(1);
        }
        read
    }
    fn hop(&self, pid: i32) -> Option<Hop> {
        self.now().hop(pid)
    }
    fn by_design(&self, pid: i32) -> Option<bool> {
        self.now().by_design(pid)
    }
    fn session(&self, pid: i32) -> Option<u32> {
        self.now().session(pid)
    }
    fn markers(&self, pid: i32) -> Option<Markers> {
        self.now().markers(pid)
    }
    fn held(&self, pid: i32) -> Option<bool> {
        self.now().held(pid)
    }
    fn held_now(&self) -> Option<HeldNow> {
        self.now().held_now()
    }
    fn launchd_version(&self) -> Option<i32> {
        self.now().launchd_version()
    }
    fn tty_master_held(&self, dev: u32) -> Option<bool> {
        self.now().tty_master_held(dev)
    }
    fn uid_pids(&self) -> Option<Vec<i32>> {
        self.now().uid_pids()
    }
    fn now_ms(&self) -> u64 {
        self.now().now_ms()
    }
    fn end(&self, token: Identity, still: &dyn Fn(&Identity) -> bool) -> Result<Ended, EndRefusal> {
        self.now().end(token, still)
    }
}

/// The test binary 500 (unique id 5000) leads its group; its child 600 left for a group of its
/// own and 700, 600's child, sits in 600's. Each binds by the unique id its hop names, the
/// grandchild through its booked parent; a process whose pid the chain names but whose unique id
/// is another's (the parent's pid taken since) binds to nothing. NEGATIVE CONTROL: bind by pid
/// alone (`Step::names` without its unique-id arm), and 800, which names another binary at the
/// binary's pid, binds.
#[test]
fn a_child_binds_to_its_test_binary_by_the_unique_ids_of_its_chain() {
    let mut k = KinKernel::default();
    k.put(500, 5000, 500, 1, 10);
    k.put(600, 6000, 600, 500, 5000);
    k.put(700, 7000, 600, 600, 6000);
    // 800 took a pid its parent's chain names, then started 801: 801 names 800's unique id,
    // and 800 names a parent (pid 500) that is not the binary (unique id 4000).
    k.put(800, 8000, 800, 500, 4000);
    k.put(801, 8100, 800, 800, 8000);
    let root = k.kin(500);
    let none = BTreeSet::new();
    let mut reads = 0;
    let child = bind(&k, 600, Some(root), &none, &[], Link::UniqueId, &mut reads);
    assert_eq!(child, Some(k.kin(600)));
    assert_eq!(
        bind(&k, 700, Some(root), &none, &[], Link::UniqueId, &mut reads),
        Some(k.kin(700)),
        "a chain of two hops reaches the binary"
    );
    assert_eq!(
        bind(
            &k,
            700,
            Some(root),
            &BTreeSet::from([k.kin(600)]),
            &[],
            Link::UniqueId,
            &mut reads
        ),
        Some(k.kin(700)),
        "a booked parent ends the climb"
    );
    assert_eq!(
        bind(&k, 800, Some(root), &none, &[], Link::UniqueId, &mut reads),
        None
    );
    assert_eq!(
        bind(&k, 801, Some(root), &none, &[], Link::UniqueId, &mut reads),
        None
    );
    // A booked parent that no longer reads as booked (its pid taken) ends nothing.
    let stale = Kin {
        uniqueid: 1,
        ..k.kin(600)
    };
    let mut gone = KinKernel::default();
    gone.put(500, 5000, 500, 1, 10);
    gone.put(600, 9000, 600, 1, 10);
    gone.put(700, 9100, 600, 600, 9000);
    assert_eq!(
        bind(
            &gone,
            700,
            Some(root),
            &BTreeSet::from([stale]),
            &[],
            Link::UniqueId,
            &mut reads
        ),
        None
    );
    assert!(reads > 0);
}

/// Without unique ids naming parents (Linux), a child binds to a parent born no later than it:
/// one whose parent's pid now names a younger process binds to nothing.
#[test]
fn a_child_binds_by_birth_where_no_parent_is_named_by_unique_id() {
    let mut k = KinKernel::default();
    k.put(500, 5000, 500, 1, 0);
    k.put(600, 6000, 600, 500, 0);
    let mut younger = KinKernel::default();
    younger.put(500, 5000, 500, 1, 0);
    younger.put(510, 6500, 510, 1, 0);
    younger.put(600, 6000, 600, 510, 0);
    let none = BTreeSet::new();
    let mut reads = 0;
    assert_eq!(
        bind(
            &k,
            600,
            Some(k.kin(500)),
            &none,
            &[],
            Link::Birth,
            &mut reads
        ),
        Some(k.kin(600))
    );
    assert_eq!(
        bind(
            &younger,
            600,
            Some(younger.kin(500)),
            &none,
            &[],
            Link::Birth,
            &mut reads
        ),
        None,
        "a parent born after its child is no parent of it"
    );
}

/// What launchd adopted is found by the parent unique ids the binary's descendants name, in
/// unique-id order, through a member of the binary's own group (which is not returned); a
/// process older than the binary or naming another's id is not. NEGATIVE CONTROL: skip the
/// sort, and 699 (listed before its parent 701 by pid) is not found.
#[test]
fn the_binarys_end_finds_its_adopted_descendants_by_their_parent_unique_ids() {
    let mut k = KinKernel::default();
    k.put(500, 5000, 500, 1, 10);
    // In the binary's group: links its own children, is the group end's.
    k.put(650, 6500, 500, 1, 5000);
    // Its adopted child, in a session of its own.
    k.put(690, 6900, 690, 1, 6500);
    // 701 adopted, and 699 its child, listed first by pid.
    k.put(701, 7100, 701, 1, 5000);
    k.put(699, 7200, 701, 701, 7100);
    // Older than the binary, naming an id it does not hold.
    k.put(400, 4000, 400, 1, 5000);
    k.put(800, 8000, 800, 1, 4321);
    let mut reads = 0;
    let found = adopted_kin(&k, k.kin(500), 500, &BTreeSet::new(), &[], &mut reads);
    assert_eq!(found, vec![k.kin(690), k.kin(701), k.kin(699)]);
    assert_eq!(
        adopted_kin(
            &k,
            k.kin(500),
            500,
            &BTreeSet::from([k.kin(701)]),
            &[],
            &mut reads
        ),
        vec![k.kin(690), k.kin(699)],
        "what is booked already is not returned again"
    );
}

/// A booked leader's group gives only the members proven its descendants, born at or after it,
/// while the leader reads as booked: in a session the leader made (`setsid`), each member of
/// that session, its parent living or exited (adopted, gone, or its pid taken since); in the
/// session of verify's launcher (`process_group(0)`), only a member whose chain of parents
/// reaches a booked kin. A process of the launcher's session that joined the group by its own
/// `setpgid`, whether its parent exited or lives, is never given. NEGATIVE CONTROLS: give every
/// member as before (drop both proofs from `group_kin`), and the joiners 720 and 730 are given;
/// let `spares_no_living_parent` fail at a parent that does not read, and 611 is not given; let
/// it read whatever holds a parent's pid as that parent, and 613 is not given.
#[test]
fn a_booked_leaders_group_gives_only_the_members_its_session_or_their_chain_proves() {
    for link in [Link::UniqueId, Link::Birth] {
        let mut k = KinKernel::default();
        // 600 made a session of its own; 610, adopted, is in it; 620 is older than 600; 611's
        // parent 699 is gone; 613's parent's pid is held now by 640, a `trustd` younger than
        // 613, which is no parent of it.
        k.put(600, 6000, 600, 1, 5000);
        k.put(610, 6100, 600, 1, 6000);
        k.put(620, 5900, 600, 1, 1);
        k.put(611, 6110, 600, 699, 6990);
        k.put(640, 6400, 640, 1, 1);
        k.runs(640, "/opt/other/bin/trustd");
        k.put(613, 6130, 600, 640, 6050);
        for pid in [600, 610, 620, 611, 613] {
            k.sessions.insert(pid, 600);
        }
        // 700 made only a group, in the launcher's session 100: 710 is its child; 720 and 730
        // joined its group from that session, 720 adopted and 730 under 300, a live shell.
        k.put(300, 3000, 300, 1, 1);
        k.runs(300, "/bin/zsh");
        k.put(700, 7000, 700, 1, 5000);
        k.put(710, 7100, 700, 700, 7000);
        k.put(720, 7200, 700, 1, 1);
        k.put(730, 7300, 700, 300, 3000);
        for pid in [300, 700, 710, 720, 730] {
            k.sessions.insert(pid, 100);
        }
        let mut reads = 0;
        let booked = BTreeSet::from([k.kin(600), k.kin(700)]);
        assert_eq!(
            group_kin(&k, &booked, &[], link, &mut reads),
            vec![k.kin(610), k.kin(611), k.kin(613), k.kin(710)],
            "{link:?}"
        );
        let stale = Kin {
            birth: Birth(1),
            ..k.kin(600)
        };
        assert!(group_kin(&k, &BTreeSet::from([stale]), &[], link, &mut reads).is_empty());
        assert!(reads > 0);
    }
}

/// No spared program is booked, and none links what it started, at a tree read
/// ([`bind`], through a parent or at a booked kin) or at the binary's end ([`adopted_kin`],
/// [`group_kin`]): a `trustd` (650, leading a session its child 651 is in), an aterm image
/// (660, booked), a program of a spared directory (670) and a verb launchd keeps by design
/// (690); `/bin/sh` (680) and its child are booked. In the session a `/bin/sh` kin (600) made,
/// its child 603 is given, and neither 601, a `trustd` of the spared directory, nor 602, that
/// `trustd`'s child. NEGATIVE CONTROLS: read no program as spared (`program` without its two
/// spared arms), and the `trustd` 650 binds; let `adopted_kin` link through a hop whatever its
/// program, and 650, 651, 670, 671, 690 and 691 are found; let it keep a booked spared kin's
/// unique id known, and the aterm image's child 661 is found; give a session member without
/// climbing its parents (`spares_no_living_parent` holding at once), and 602 is given.
#[test]
fn no_spared_program_is_booked_or_links_what_it_started() {
    let spared = [PathBuf::from("/store/trust/27009.noindex")];
    // Each of 650..=690 a child of the binary and its child in its group, or, `adopted`, both
    // adopted, each naming its parent's unique id.
    let lay = |adopted: bool| {
        let mut k = KinKernel::default();
        k.put(500, 5000, 500, 1, 10);
        for (pid, uniqueid, exe) in [
            (650_i32, 6500, "/opt/other/bin/trustd"),
            (660, 6600, "/Applications/aterm.app/Contents/MacOS/aterm"),
            (670, 6700, "/store/trust/27009.noindex/bin/trustc"),
            (680, 6800, "/bin/sh"),
            (690, 6900, "/bin/sh"),
        ] {
            let pgid = pid.unsigned_abs();
            k.put(pid, uniqueid, pgid, if adopted { 1 } else { 500 }, 5000);
            k.runs(pid, exe);
            k.put(
                pid + 1,
                uniqueid + 10,
                pgid,
                if adopted { 1 } else { pgid },
                uniqueid,
            );
        }
        k.sessions.insert(650, 650);
        k.sessions.insert(651, 650);
        k.designed.insert(690);
        k
    };
    let mut reads = 0;
    for link in [Link::UniqueId, Link::Birth] {
        let mut k = lay(false);
        // 600, the binary's child, made a session: 601, a `trustd` of the spared directory, and
        // 603 are its children in it, and 602 is the `trustd`'s.
        k.put(600, 6000, 600, 500, 5000);
        k.put(601, 6010, 600, 600, 6000);
        k.runs(601, "/store/trust/27009.noindex/bin/trustd");
        k.put(602, 6020, 600, 601, 6010);
        k.put(603, 6030, 600, 600, 6000);
        for pid in [600, 601, 602, 603] {
            k.sessions.insert(pid, 600);
        }
        assert_eq!(
            group_kin(&k, &BTreeSet::from([k.kin(600)]), &spared, link, &mut reads),
            vec![k.kin(603)],
            "{link:?}: neither the trustd nor its child is given"
        );
        let root = Some(k.kin(500));
        let none = BTreeSet::new();
        for pid in [650, 651, 670, 671, 690, 691] {
            assert_eq!(
                bind(&k, pid, root, &none, &spared, link, &mut reads),
                None,
                "{pid} {link:?}"
            );
        }
        let aterm = BTreeSet::from([k.kin(660)]);
        assert_eq!(bind(&k, 661, root, &aterm, &spared, link, &mut reads), None);
        assert_eq!(
            bind(&k, 681, root, &none, &spared, link, &mut reads),
            Some(k.kin(681))
        );
        assert!(
            group_kin(&k, &BTreeSet::from([k.kin(650)]), &spared, link, &mut reads).is_empty(),
            "a spared leader's session gives nothing"
        );
    }
    // At the binary's end, each adopted: 651, 661, 671 and 691 name a spared parent's unique id.
    let k = lay(true);
    assert_eq!(
        adopted_kin(
            &k,
            k.kin(500),
            500,
            &BTreeSet::from([k.kin(660)]),
            &spared,
            &mut reads
        ),
        vec![k.kin(680), k.kin(681)]
    );
}

/// Each arm of the climb from a member of a session a kin made (`spares_no_living_parent`),
/// under both links. A parent that does not read holds where the kernel reads its pid as exited
/// (603) or absent (604), and fails where it reads it as present (601: another user's) or
/// cannot tell (602). A living parent fails where its thread is held (605), its verb names a
/// program launchd keeps by design (606), it is an aterm image (607), it runs from a spared
/// directory (613), or its pid reads as another process once its executable was read (608:
/// gone while its program was read); a `/bin/sh` parent holds (609). A parent past `i32::MAX`
/// fails (610). The leader holds as the 64th parent (611) and not as the 65th (612). NEGATIVE
/// CONTROLS: hold at any parent that does not read, and 601 and 602 are given; fail only at a
/// parent that reads as spared, and 605 and 608 are given; hold once `MAX_HOPS` pass, and 612
/// is given.
#[test]
fn each_arm_of_the_session_climb_gives_or_refuses_its_member() {
    use crate::process::ProcessPresence;
    let spared = [PathBuf::from("/store/trust/27009.noindex")];
    for link in [Link::UniqueId, Link::Birth] {
        let mut k = KinKernel::default();
        k.put(600, 6000, 600, 1, 1);
        k.sessions.insert(600, 600);
        // The living parents, each the leader's child in a group of its own.
        for (pid, uniqueid) in [
            (650, 6500),
            (651, 6510),
            (652, 6520),
            (653, 6530),
            (654, 6540),
            (655, 6550),
        ] {
            k.put(pid, uniqueid, pid.unsigned_abs(), 600, 6000);
        }
        k.held.insert(650);
        k.designed.insert(651);
        k.runs(652, "/Applications/aterm.app/Contents/MacOS/aterm");
        let mut taker = k.lineages[&653];
        taker.identity.birth = Birth(9999);
        taker.uniqueid = 9999;
        taker.parent = 1;
        k.taken.insert(653, taker);
        k.runs(655, "/store/trust/27009.noindex/bin/trustc");
        // Parents that do not read.
        for (pid, presence) in [
            (1001, ProcessPresence::Present),
            (1002, ProcessPresence::Unknown),
            (1003, ProcessPresence::Exited),
            (1004, ProcessPresence::Absent),
        ] {
            k.presences.insert(pid, presence);
        }
        // 63 parents between 611 and the leader, 64 between 612 and it.
        let (above_611, above_611_id) = k.chain(600, 20_000..20_063, 6100);
        let (above_612, above_612_id) = k.chain(600, 30_000..30_064, 6200);
        for (pid, uniqueid, parent, parent_uniqueid) in [
            (601, 7010, 1001, 1),
            (602, 7020, 1002, 1),
            (603, 7030, 1003, 1),
            (604, 7040, 1004, 1),
            (605, 7050, 650, 6500),
            (606, 7060, 651, 6510),
            (607, 7070, 652, 6520),
            (608, 7080, 653, 6530),
            (609, 7090, 654, 6540),
            (610, 7100, 0x8000_0000, 1),
            (611, 7110, above_611, above_611_id),
            (612, 7120, above_612, above_612_id),
            (613, 7130, 655, 6550),
        ] {
            k.put(pid, uniqueid, 600, parent, parent_uniqueid);
            k.sessions.insert(pid, 600);
        }
        let mut reads = 0;
        assert_eq!(
            group_kin(&k, &BTreeSet::from([k.kin(600)]), &spared, link, &mut reads),
            vec![k.kin(603), k.kin(604), k.kin(609), k.kin(611)],
            "{link:?}"
        );
    }
}

/// A leader that runs a spared program by the time its members are read gives none of its
/// session's members: its program is read again, with its lineage after it, once they are
/// (`program_of`). The same session under a leader that runs none gives both members.
/// NEGATIVE CONTROL: read only the leader's lineage again, and 601 and 602 are given.
#[test]
fn a_leader_that_runs_a_spared_program_while_its_members_are_read_gives_none() {
    let lay = |execs: bool| {
        let mut k = KinKernel::default();
        k.put(600, 6000, 600, 1, 1);
        k.put(601, 6010, 600, 600, 6000);
        k.put(602, 6020, 600, 601, 6010);
        for pid in [600, 601, 602] {
            k.sessions.insert(pid, 600);
        }
        if execs {
            k.execs_when_listed
                .insert(600, PathBuf::from("/opt/other/bin/trustd"));
        }
        k
    };
    for link in [Link::UniqueId, Link::Birth] {
        let mut reads = 0;
        let k = lay(false);
        assert_eq!(
            group_kin(&k, &BTreeSet::from([k.kin(600)]), &[], link, &mut reads),
            vec![k.kin(601), k.kin(602)],
            "{link:?}"
        );
        let k = lay(true);
        assert_eq!(
            group_kin(&k, &BTreeSet::from([k.kin(600)]), &[], link, &mut reads),
            Vec::new(),
            "{link:?}: the leader ran trustd while its members were read"
        );
    }
}

/// The climbs of a session's members read each parent they share once: under nine members
/// that share one chain of parents up to the leader, a chain 28 parents longer costs 28 × 5
/// reads more (each parent's step, and its program's thread, executable, verb and lineage
/// again), not that for each member, and every member is given either way. NEGATIVE CONTROL:
/// keep nothing a climb reached (no lookup of `climbed`), and it costs 9 × 28 × 5 more.
#[test]
fn the_climbs_of_a_session_read_each_shared_parent_once() {
    for link in [Link::UniqueId, Link::Birth] {
        let reads_under = |parents: i32| {
            let mut k = KinKernel::default();
            k.put(600, 6000, 600, 1, 1);
            k.sessions.insert(600, 600);
            let (bottom, bottom_id) = k.chain(600, 20_000..20_000 + parents, 6100);
            let members: Vec<i32> = (601..610).collect();
            for (&pid, uniqueid) in members.iter().zip(7000..) {
                k.put(pid, uniqueid, 600, bottom, bottom_id);
                k.sessions.insert(pid, 600);
            }
            let mut reads = 0;
            assert_eq!(
                group_kin(&k, &BTreeSet::from([k.kin(600)]), &[], link, &mut reads),
                members.iter().map(|&pid| k.kin(pid)).collect::<Vec<_>>(),
                "{link:?} under {parents} parents"
            );
            reads
        };
        assert_eq!(reads_under(32) - reads_under(4), 28 * 5, "{link:?}");
    }
}

/// A process a kept parent started after a climb read that parent's program is not given: in
/// the session the leader 600 made, 650's program is read as `/bin/sh` and kept (by 601's
/// climb, or, where 650 is in the leader's group and listed first, by its own try as a member);
/// just after that read 650 runs a `trustd`, the leader's child 602 has exited, and the
/// `trustd` starts a process at pid 602, born 9000, in the leader's group and session. Every
/// member was read before any climb, so pid 602 is the process born 6020, whose program then
/// reads as gone, and the one born 9000 is not given. 601 and, where 650 is in the leader's
/// group, 650 read as no spared program when they were read and are given. Under both links,
/// with 650 in a group of its own or in the leader's. NEGATIVE CONTROL: read each member's
/// lineage in the loop that tries it, and 602 born 9000 is given through 650's kept verdict.
#[test]
fn a_process_a_kept_parent_started_after_its_read_is_not_given() {
    for link in [Link::UniqueId, Link::Birth] {
        for parent_pgid in [650, 600] {
            let lay = |after: bool| {
                let mut k = KinKernel::default();
                k.put(600, 6000, 600, 1, 1);
                k.put(650, 6500, parent_pgid, 600, 6000);
                k.put(601, 6510, 600, 650, 6500);
                if after {
                    k.runs(650, "/opt/other/bin/trustd");
                    k.put(602, 9000, 600, 650, 6500);
                } else {
                    k.put(602, 6020, 600, 600, 6000);
                }
                for pid in [600, 650, 601, 602] {
                    k.sessions.insert(pid, 600);
                }
                k
            };
            let k = Turning {
                before: lay(false),
                after: lay(true),
                at: 650,
                read: std::cell::Cell::new(0),
            };
            let mut reads = 0;
            let leader = k.before.kin(600);
            let got = group_kin(&k, &BTreeSet::from([leader]), &[], link, &mut reads);
            assert_eq!(
                k.read.get(),
                2,
                "{link:?} {parent_pgid}: 650's program was read"
            );
            let given = if parent_pgid == 600 {
                vec![k.before.kin(650), k.before.kin(601)]
            } else {
                vec![k.before.kin(601)]
            };
            assert_eq!(got, given, "{link:?}, 650 in group {parent_pgid}");
        }
    }
}

/// A verdict a climb keeps refuses each member it reaches, as climbing again would: in the
/// session the leader 600 made, 601's climb passes its parent 602 and fails at 602's parent
/// 650, a `trustd` outside the group, so 602, listed after its child 601, is refused with no
/// climb of its own; 603, 650's other child, meets 650's kept refusal and is refused; 604, the
/// leader's child, is given. Under both links. NEGATIVE CONTROLS: keep a refused climb's path
/// as held, and 602 is given; give a member kept refused, and 602 is given; keep a parent whose
/// program reads otherwise as held, and 603 is given; let a climb that meets a kept refusal
/// hold, and 603 is given.
#[test]
fn a_kept_refusal_refuses_each_member_it_reaches() {
    for link in [Link::UniqueId, Link::Birth] {
        let mut k = KinKernel::default();
        k.put(600, 6000, 600, 1, 1);
        k.put(650, 6500, 650, 600, 6000);
        k.runs(650, "/opt/other/bin/trustd");
        k.put(601, 7020, 600, 602, 7010);
        k.put(602, 7010, 600, 650, 6500);
        k.put(603, 7030, 600, 650, 6500);
        k.put(604, 7040, 600, 600, 6000);
        for pid in [600, 650, 601, 602, 603, 604] {
            k.sessions.insert(pid, 600);
        }
        let mut reads = 0;
        assert_eq!(
            group_kin(&k, &BTreeSet::from([k.kin(600)]), &[], link, &mut reads),
            vec![k.kin(604)],
            "{link:?}"
        );
    }
}

/// A climb that reaches a process kept as held `h` hops above it holds at hop `i` only where
/// `i + h` is within `MAX_HOPS`: 601 holds at the leader 64 hops up (63 parents between), 602,
/// its sibling, holds through their parent's kept verdict at 1 + 63 hops, and 603, 601's child,
/// is refused through 601's at 1 + 64. Under both links. NEGATIVE CONTROL: let a kept held
/// verdict drop the hops above it, and 603 is given.
#[test]
fn a_climb_through_a_kept_verdict_holds_only_within_max_hops() {
    for link in [Link::UniqueId, Link::Birth] {
        let mut k = KinKernel::default();
        k.put(600, 6000, 600, 1, 1);
        k.sessions.insert(600, 600);
        let (bottom, bottom_id) = k.chain(600, 20_000..20_063, 6100);
        for (pid, uniqueid, parent, parent_uniqueid) in [
            (601, 7010, bottom, bottom_id),
            (602, 7020, bottom, bottom_id),
            (603, 7030, 601, 7010),
        ] {
            k.put(pid, uniqueid, 600, parent, parent_uniqueid);
            k.sessions.insert(pid, 600);
        }
        let mut reads = 0;
        assert_eq!(
            group_kin(&k, &BTreeSet::from([k.kin(600)]), &[], link, &mut reads),
            vec![k.kin(601), k.kin(602)],
            "{link:?}"
        );
    }
}

/// A kin sidecar reads back whole, and any part of one that is cut short, unknown, or names
/// another run gives none.
#[test]
fn a_kin_sidecar_reads_back_only_whole() {
    let id = RunId {
        pid: 41983,
        birth: Birth(7),
    };
    let path = Path::new("/r/41983-7.kin");
    let kin = Kin {
        pid: 700,
        birth: Birth(9),
        uniqueid: 70,
    };
    let text = format!(
        "{}{}\n",
        kin_header_text(
            "B",
            id,
            Some(16),
            &[PathBuf::from("/s/trust/27009.noindex")]
        ),
        kin_line(&kin, 500)
    );
    let read = parse_kin(path, &text).expect("a whole sidecar");
    assert_eq!(
        read,
        KinRecord {
            boot: "B".into(),
            id,
            tty: Some(16),
            spared: vec![PathBuf::from("/s/trust/27009.noindex")],
            kin: vec![kin],
        }
    );
    assert_eq!(parse_kin(path, text.trim_end()), None, "cut short");
    assert_eq!(parse_kin(Path::new("/r/41983-8.kin"), &text), None);
    assert_eq!(parse_kin(path, &format!("{text}odd x=1\n")), None);
    assert_eq!(
        parse_kin(path, &text.replace("pid=700", "pid=+700")),
        None,
        "a signed pid is no pid"
    );
    assert_eq!(parse_kin(path, &text.replace("v=1", "v=2")), None);
    assert_eq!(
        parse_kin(path, &format!("{}stage=t\n", text.trim_end())),
        None,
        "a field no writer writes"
    );
    let full = (0..=KIN_LINES_MAX).fold(text.clone(), |mut text, _| {
        text.push_str(&kin_line(&kin, 500));
        text.push('\n');
        text
    });
    assert_eq!(
        parse_kin(path, &full),
        None,
        "more lines than a writer writes"
    );
    assert!(
        kin_line(
            &Kin {
                pid: i32::MAX,
                birth: Birth(u64::MAX),
                uniqueid: u64::MAX
            },
            u32::MAX
        )
        .len()
            < KIN_LINE_BYTES
    );
}
