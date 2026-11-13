// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::requests_host::{Made, opened_for_test, set_dir_for_test};
use crate::session_store::{new_store, test_handle};

const NODE: &str = "87be5228c0ffee0087be5228c0ffee00";
/// A drive of [`NODE`] that names no requester session.
const BY: &str = "fleet:87be5228:-";
const PERSON: &str = "s-ffffffffffffffffffff";
/// A hang detector for the requests' threads.
const MUST: Duration = Duration::from_secs(60);

/// This test thread's requests in a folder of its own, and a home beside them, until dropped.
struct Here {
    requests: aterm_tempfile::TempDir,
    home: aterm_tempfile::TempDir,
}

impl Here {
    fn new() -> Self {
        let requests = aterm_tempfile::tempdir().expect("a folder");
        set_dir_for_test(Some(requests.path().to_path_buf()));
        let home = aterm_tempfile::tempdir().expect("a home");
        Self { requests, home }
    }

    fn store(&self) -> aterm_requests::Store {
        aterm_requests::Store::open(self.requests.path()).expect("the store")
    }

    /// The home as a claim records it: the temporary folder's real path (`/var` is a link on
    /// macOS, and a claim records where links lead).
    fn home(&self) -> PathBuf {
        std::fs::canonicalize(self.home.path()).expect("the home's real path")
    }

    fn places(&self) -> Places {
        Places {
            home: Some(self.home()),
            state: Some(self.requests.path().to_path_buf()),
        }
    }

    /// A folder `name` made in the home.
    fn folder(&self, name: &str) -> PathBuf {
        let path = self.home().join(name);
        std::fs::create_dir_all(&path).expect("a folder");
        path
    }
}

impl Drop for Here {
    fn drop(&mut self) {
        set_dir_for_test(None);
        set_measure_for_test(MeasureProbe::INERT);
    }
}

/// The folders whose birth a test's probe was asked, so a test sees which were looked at.
static BORN_ASKED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// A folder's birth as a test names it: the digits its name begins with (`1500-x` was born at
/// 1500 ms), recorded as asked.
fn born_by_name(path: &Path) -> Option<u64> {
    BORN_ASKED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(path.to_path_buf());
    let name = path.file_name()?.to_str()?;
    name.split('-').next()?.parse().ok()
}

const FS: FsProbe = FsProbe {
    list: list_children,
    born: born_by_name,
};

fn span(sid: &str, blk: u64, cwd: Option<&Path>, from_ms: u64, to_ms: Option<u64>) -> BlockSpan {
    BlockSpan {
        sid: sid.to_owned(),
        blk,
        cwd: cwd.map(|cwd| cwd.to_string_lossy().into_owned()),
        input_ms: Some(from_ms.saturating_sub(100)),
        span: Span { from_ms, to_ms },
    }
}

fn hand(here: &Here, at: u64, spans: Vec<BlockSpan>) -> Hand {
    Hand {
        at,
        places: here.places(),
        fs: FS,
        measure: MeasureProbe::INERT,
        spans,
        typed: HashMap::new(),
    }
}

/// A drive's plain shell's request with tab `sid`, opened now: its id and when it opened.
fn request_with(sid: &str) -> (String, u64) {
    let id = opened_for_test(true, NODE, "m114", BY, Made::Shell, sid);
    let dir = crate::requests_host::dir().unwrap().unwrap();
    let opened = aterm_requests::Store::open(&dir)
        .unwrap()
        .get(&id)
        .unwrap()
        .opened_ms;
    (id, opened)
}

fn claims_of(here: &Here, id: &str) -> Vec<aterm_requests::Claim> {
    here.store()
        .get(id)
        .map(|request| {
            request
                .claims
                .iter()
                .map(|held| held.claim.clone())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn the_scan_claims_a_folder_born_in_a_request_block() {
    let here = Here::new();
    let tab = test_handle(301);
    let (id, opened) = request_with(tab.sid.as_str());
    let (from, to) = (opened + 10_000, opened + 20_000);
    let born = here.folder(&format!("{}-trust-ir", from + 1_000));
    // NEGATIVE: born before the span, a dot-folder, a link, a file, a guarded name.
    here.folder(&format!("{}-older", from - BIRTH_SKEW_MS - 1));
    here.folder(&format!(".{}-dot", from + 1_000));
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        &born,
        here.home.path().join(format!("{}-link", from + 1_000)),
    )
    .unwrap();
    std::fs::write(
        here.home.path().join(format!("{}-file", from + 1_000)),
        b"x",
    )
    .unwrap();
    let guarded = here.folder("Documents");
    let mut store = here.store();
    let new = look(
        here.requests.path(),
        &mut store,
        &hand(
            &here,
            to + 1,
            vec![span(tab.sid.as_str(), 1, None, from, Some(to))],
        ),
    )
    .unwrap();
    let path = born.to_str().unwrap().to_owned();
    assert_eq!(new, [(path.clone(), id.clone())]);
    let claims = claims_of(&here, &id);
    assert_eq!(claims.len(), 1, "{claims:?}");
    assert_eq!(
        (
            claims[0].src,
            claims[0].alone,
            claims[0].tab.as_deref(),
            claims[0].born_ms
        ),
        (
            ClaimSrc::Born,
            Some(true),
            Some(tab.sid.as_str()),
            Some(from + 1_000)
        )
    );
    assert!(
        !BORN_ASKED
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(&guarded),
        "a folder macOS guards is never looked at"
    );
    // The next look finds it claimed and writes nothing more.
    let again = look(
        here.requests.path(),
        &mut store,
        &hand(
            &here,
            to + 2,
            vec![span(tab.sid.as_str(), 2, None, from, Some(to))],
        ),
    )
    .unwrap();
    assert!(again.is_empty());
}

#[test]
fn a_birth_while_a_persons_tab_ran_is_claimed_not_alone() {
    let here = Here::new();
    let tab = test_handle(302);
    let (id, opened) = request_with(tab.sid.as_str());
    let from = opened + 10_000;
    here.folder(&format!("{}-shared", from + 500));
    let mut store = here.store();
    look(
        here.requests.path(),
        &mut store,
        &hand(
            &here,
            from + 5_000,
            vec![
                span(tab.sid.as_str(), 1, None, from, Some(from + 1_000)),
                span(PERSON, 9, None, from + 200, None),
            ],
        ),
    )
    .unwrap();
    let claims = claims_of(&here, &id);
    assert_eq!(claims.len(), 1, "{claims:?}");
    assert_eq!(claims[0].alone, Some(false));
}

#[test]
fn a_command_in_a_claimed_folder_from_a_persons_tab_is_touched() {
    let here = Here::new();
    let tab = test_handle(303);
    let (id, opened) = request_with(tab.sid.as_str());
    let from = opened + 10_000;
    let folder = here.folder(&format!("{}-work", from + 500));
    let mut store = here.store();
    look(
        here.requests.path(),
        &mut store,
        &hand(
            &here,
            from + 2_000,
            vec![span(tab.sid.as_str(), 1, None, from, Some(from + 1_000))],
        ),
    )
    .unwrap();
    let claimed_at = store
        .get(&id)
        .unwrap()
        .claim(folder.to_str().unwrap())
        .unwrap()
        .at_ms;
    // NEGATIVE first: the requester's own command there, in its own tab, is no touch.
    let later = claimed_at + 1_000;
    look(
        here.requests.path(),
        &mut store,
        &hand(
            &here,
            later + 2_000,
            vec![span(
                tab.sid.as_str(),
                2,
                Some(&folder),
                later,
                Some(later + 1),
            )],
        ),
    )
    .unwrap();
    assert!(store.get(&id).unwrap().touched.is_empty());
    // A person's tab's command in a folder under it is.
    look(
        here.requests.path(),
        &mut store,
        &hand(
            &here,
            later + 3_000,
            vec![span(
                PERSON,
                5,
                Some(&folder.join("sub")),
                later + 2_000,
                Some(later + 2_500),
            )],
        ),
    )
    .unwrap();
    assert!(
        store
            .get(&id)
            .unwrap()
            .touched
            .contains(folder.to_str().unwrap())
    );
}

#[test]
fn a_person_typing_in_a_request_tab_touches_its_folder() {
    let here = Here::new();
    let tab = test_handle(304);
    let (id, opened) = request_with(tab.sid.as_str());
    let from = opened + 10_000;
    let folder = here.folder(&format!("{}-typed", from + 500));
    let mut store = here.store();
    look(
        here.requests.path(),
        &mut store,
        &hand(
            &here,
            from + 2_000,
            vec![span(tab.sid.as_str(), 1, None, from, Some(from + 1_000))],
        ),
    )
    .unwrap();
    let later = store.get(&id).unwrap().claims.first().unwrap().at_ms + 1_000;
    let mut typed = hand(
        &here,
        later + 2_000,
        vec![span(
            tab.sid.as_str(),
            2,
            Some(&folder),
            later,
            Some(later + 1),
        )],
    );
    typed.typed.insert(tab.sid.as_str().to_owned(), later - 50);
    look(here.requests.path(), &mut store, &typed).unwrap();
    assert!(!store.get(&id).unwrap().touched.is_empty());
}

fn drive(node: &str) -> crate::fabric::DrivingAs {
    crate::fabric::DrivingAs::enter(crate::fabric::Driver {
        node: node.to_owned(),
        host: "m114".to_owned(),
    })
}

fn no_client() -> crate::typed_by::Who {
    crate::typed_by::Who::client_with(None, 1, &|_| None, &|_| None, &|_| None)
}

#[test]
fn claim_from_a_drive_needs_its_own_request() {
    let here = Here::new();
    let store = new_store();
    let tab = test_handle(305);
    let (id, _) = request_with(tab.sid.as_str());
    let folder = here.folder("named");
    let path = aterm_codec::pct::encode(folder.to_str().unwrap());
    let line = format!("claim path={path} req={id}");
    {
        let _driving = drive(NODE);
        let reply = cmd_request_as(&line, Scope::Owner, &store, no_client, &here.places());
        assert!(
            reply.starts_with(&format!("OK claimed req={id} path={path} born=")),
            "{reply}"
        );
        let again = cmd_request_as(&line, Scope::Owner, &store, no_client, &here.places());
        assert_eq!(again, format!("OK already req={id} path={path}\n"));
        let inner = here.folder("named/inner");
        let inner_line = format!(
            "claim path={} req={id}",
            aterm_codec::pct::encode(inner.to_str().unwrap())
        );
        let inside = cmd_request_as(&inner_line, Scope::Owner, &store, no_client, &here.places());
        assert!(inside.starts_with("OK inside "), "{inside}");
        // NEGATIVE: no `req=` from a drive names no request.
        let bare = cmd_request_as(
            &format!("claim path={path}"),
            Scope::Owner,
            &store,
            no_client,
            &here.places(),
        );
        assert!(
            bare.starts_with("ERR request claim: this caller asks for no request"),
            "{bare}"
        );
    }
    // NEGATIVE: the same token from another Mac.
    {
        let _driving = drive("87be5228ffffffffffffffffffffffff");
        let other = here.folder("other");
        let reply = cmd_request_as(
            &format!(
                "claim path={} req={id}",
                aterm_codec::pct::encode(other.to_str().unwrap())
            ),
            Scope::Owner,
            &store,
            no_client,
            &here.places(),
        );
        assert_eq!(reply, "ERR request claim: another requester's request\n");
    }
    // NEGATIVE: a caller in no tab, and a usage error.
    let none = cmd_request_as(&line, Scope::Owner, &store, no_client, &here.places());
    assert!(
        none.starts_with("ERR request claim: this caller asks for no request"),
        "{none}"
    );
    assert_eq!(
        cmd_request_as("show", Scope::Owner, &store, no_client, &here.places()),
        USAGE
    );
    assert_eq!(claims_of(&here, &id).len(), 1);
}

#[cfg(unix)]
#[test]
fn claim_refuses_home_its_parents_a_guarded_folder_and_a_missing_one() {
    let here = Here::new();
    let store = new_store();
    let tab = test_handle(306);
    let (id, _) = request_with(tab.sid.as_str());
    let _driving = drive(NODE);
    let home = here.home();
    let guarded = here.folder("Documents");
    let cases = [
        ("/".to_owned(), "is the root folder"),
        (
            home.display().to_string(),
            "is this Mac's home or a folder holding it",
        ),
        (
            home.parent().unwrap().display().to_string(),
            "is this Mac's home or a folder holding it",
        ),
        (guarded.display().to_string(), GUARDED),
        (home.join("gone").display().to_string(), GONE),
        ("~/x".to_owned(), "names `~`"),
        ("relative".to_owned(), "is not an absolute path"),
        (
            here.requests.path().join("x").display().to_string(),
            "aterm's state home",
        ),
    ];
    for (path, why) in cases {
        let reply = cmd_request_as(
            &format!("claim path={} req={id}", aterm_codec::pct::encode(&path)),
            Scope::Owner,
            &store,
            no_client,
            &here.places(),
        );
        assert!(
            reply.starts_with("ERR request claim path=") && reply.contains(why),
            "{path}: {reply}"
        );
    }
    assert!(claims_of(&here, &id).is_empty());
}

#[cfg(target_os = "macos")]
#[test]
fn claim_of_an_older_folder_answers_born_before() {
    let here = Here::new();
    let store = new_store();
    let tab = test_handle(307);
    let (id, _) = request_with(tab.sid.as_str());
    let _driving = drive(NODE);
    // A folder macOS made long before any request: listed, never deleted.
    let reply = cmd_request_as(
        &format!("claim path=/usr/share req={id}"),
        Scope::Owner,
        &store,
        no_client,
        &Places {
            home: Some(here.home()),
            state: None,
        },
    );
    assert_eq!(
        reply,
        format!("OK claimed req={id} path=/usr/share born=before\n")
    );
    let request = here.store().get(&id).cloned().unwrap();
    let held = request.claim("/usr/share").unwrap();
    assert_eq!(
        aterm_requests::claim::keep_of(&request, &held.claim, None, held.at_ms),
        Some(aterm_requests::Keep::BornBefore)
    );
}

#[test]
fn measure_runs_on_its_own_thread_and_flags_unpushed() {
    let here = Here::new();
    let store = new_store();
    let tab = test_handle(308);
    let (id, _) = request_with(tab.sid.as_str());
    let _driving = drive(NODE);
    set_measure_for_test(MeasureProbe {
        walk: |_, _| Some((5 * 1024 * 1024, true)),
        repos: |_, _| Some(true),
    });
    let pushed = here.folder("unpushed");
    let reply = cmd_request_as(
        &format!(
            "claim path={} req={id}",
            aterm_codec::pct::encode(pushed.to_str().unwrap())
        ),
        Scope::Owner,
        &store,
        no_client,
        &here.places(),
    );
    assert!(reply.starts_with("OK claimed"), "{reply}");
    assert!(flush_within(MUST), "the footprint thread flushed");
    let now = crate::requests_host::now_unix_ms();
    let found = measured(pushed.to_str().unwrap(), now).expect("measured");
    assert_eq!(
        (found.bytes, found.complete, found.unpushed),
        (Some(5 * 1024 * 1024), true, Some(true))
    );
    // NEGATIVE: a repo that could not be read leaves the claim unmeasured, so it is kept.
    set_measure_for_test(MeasureProbe {
        walk: |_, _| Some((1, true)),
        repos: |_, _| None,
    });
    let unread = here.folder("unread");
    cmd_request_as(
        &format!(
            "claim path={} req={id}",
            aterm_codec::pct::encode(unread.to_str().unwrap())
        ),
        Scope::Owner,
        &store,
        no_client,
        &here.places(),
    );
    assert!(flush_within(MUST), "the footprint thread flushed");
    let measured_unread = measured(unread.to_str().unwrap(), now).expect("measured");
    assert_eq!(measured_unread.unpushed, None);
    let request = here.store().get(&id).cloned().unwrap();
    let held = request.claim(unread.to_str().unwrap()).unwrap();
    // A named claim no span accounts for is unattributed first; as if one did, it is unmeasured.
    assert_eq!(
        aterm_requests::claim::keep_of(&request, &held.claim, Some(&measured_unread), now),
        Some(aterm_requests::Keep::Unattributed)
    );
    let alone = aterm_requests::Claim {
        alone: Some(true),
        ..held.claim.clone()
    };
    assert_eq!(
        aterm_requests::claim::keep_of(&request, &alone, Some(&measured_unread), now),
        Some(aterm_requests::Keep::Unmeasured)
    );
}

#[test]
fn spend_sampler_keeps_the_max_written_of_each_process() {
    const MIB: u64 = 1024 * 1024;
    let counters = |cpu_s: u64, written_mib: u64| aterm_sysprobe::jobs::Counters {
        cpu_ns: cpu_s * 1_000_000_000,
        disk_written: Some(written_mib * MIB),
        peak_footprint: Some(written_mib * MIB),
        ..aterm_sysprobe::jobs::Counters::default()
    };
    let mut spend = LiveSpend::default();
    assert_eq!(
        spend.spent(),
        Spent::default(),
        "nothing read is unread, never 0"
    );
    spend.sample(&[(10, counters(1, 100)), (11, counters(2, 50))]);
    // 11 ended between samples: what it wrote stays counted.
    spend.sample(&[(10, counters(3, 150))]);
    assert_eq!(
        spend.spent(),
        Spent {
            cpu_s: Some(3),
            peak_mb: Some(150),
            written_mb: Some(200),
        }
    );
    // NEGATIVE: pid 11 again, its count lower: a new process, counted afresh.
    spend.sample(&[(10, counters(3, 150)), (11, counters(1, 20))]);
    assert_eq!(spend.spent().written_mb, Some(220));
}

/// A claim of the home, a folder holding it or the state home is refused at every spelling
/// macOS resolves to that folder before a look: another case, and the data volume's firmlinks
/// (`/System/Volumes/Data` holds every folder). With no home known, every claim is refused.
/// CONTROL: a folder in the home is claimed.
#[cfg(target_os = "macos")]
#[test]
fn claim_refuses_the_home_and_the_state_home_at_every_spelling() {
    let here = Here::new();
    let store = new_store();
    let tab = test_handle(309);
    let (id, _) = request_with(tab.sid.as_str());
    let _driving = drive(NODE);
    let home = here.home().display().to_string();
    let parent = here.home().parent().unwrap().display().to_string();
    let state = here.requests.path().display().to_string();
    let ask = |path: &str, places: &Places| {
        cmd_request_as(
            &format!("claim path={} req={id}", aterm_codec::pct::encode(path)),
            Scope::Owner,
            &store,
            no_client,
            places,
        )
    };
    for (path, why) in [
        (home.to_uppercase(), HOME),
        (parent.to_uppercase(), HOME),
        (format!("/System/Volumes/Data{home}"), HOME),
        ("/System/Volumes/Data".to_owned(), HOME),
        (format!("{}/x", state.to_uppercase()), STATE),
    ] {
        let reply = ask(&path, &here.places());
        assert!(
            reply.starts_with("ERR request claim path=") && reply.contains(why),
            "{path}: {reply}"
        );
    }
    let folder = here.folder("mine").display().to_string();
    let homeless = Places {
        home: None,
        state: None,
    };
    let reply = ask(&folder, &homeless);
    assert!(reply.contains(NO_HOME), "{reply}");
    assert!(claims_of(&here, &id).is_empty());
    let reply = ask(&folder, &here.places());
    assert!(reply.starts_with("OK claimed"), "CONTROL: {reply}");
}

/// A spawn's root is the folder its links lead to, so a scan never lists through a link, and
/// never the home, the state home or a folder macOS guards, at the end of a link or not.
#[cfg(unix)]
#[test]
fn a_root_is_where_its_links_lead_and_never_a_folder_a_claim_refuses() {
    let here = Here::new();
    let places = here.places();
    let real = here.folder("code");
    let link = here.home().join("code-link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert_eq!(
        root_of(link.to_str().unwrap(), &places).as_deref(),
        real.to_str()
    );
    let guarded = here.folder("Documents");
    let into_guarded = here.home().join("docs");
    std::os::unix::fs::symlink(&guarded, &into_guarded).unwrap();
    let in_state = here.requests.path().join("x");
    std::fs::create_dir_all(&in_state).unwrap();
    for refused in [
        here.home(),
        guarded,
        into_guarded,
        here.requests.path().to_path_buf(),
        in_state,
    ] {
        assert_eq!(
            root_of(refused.to_str().unwrap(), &places),
            None,
            "{}",
            refused.display()
        );
    }
}

/// A birth scan claims no folder in the state home or holding it, nor lists a root there.
/// CONTROL: a folder born beside it at the same moment is claimed.
#[test]
fn the_scan_never_claims_the_state_home_or_a_folder_holding_it() {
    let here = Here::new();
    let tab = test_handle(311);
    let (id, opened) = request_with(tab.sid.as_str());
    let from = opened + 10_000;
    let holder = here.folder(&format!("{}-holder", from + 500));
    let free = here.folder(&format!("{}-free", from + 500));
    let mut store = here.store();
    let mut hand = hand(
        &here,
        from + 5_000,
        vec![span(tab.sid.as_str(), 1, None, from, Some(from + 1_000))],
    );
    hand.places.state = Some(holder.join("aterm"));
    look(here.requests.path(), &mut store, &hand).unwrap();
    let claims: Vec<String> = claims_of(&here, &id)
        .into_iter()
        .map(|claim| claim.path)
        .collect();
    assert_eq!(claims, [free.to_str().unwrap()]);
}

/// A folder born while a request's command still runs waits for that command's end, then is the
/// request's alone; one born under a command that ran past `ALONE_SPAN_MS`, or still runs past
/// it (an agent's session), is the request's with its birth unattributed, which a cleanup keeps.
#[test]
fn a_birth_under_a_running_or_long_command_waits_or_is_unattributed() {
    let here = Here::new();
    let tab = test_handle(312);
    let sid = tab.sid.as_str();
    let (id, opened) = request_with(sid);
    let from = opened + 10_000;
    let quick = here.folder(&format!("{}-quick", from + 500));
    let mut store = here.store();
    let dir = here.requests.path();
    look(
        dir,
        &mut store,
        &hand(&here, from + 5_000, vec![span(sid, 1, None, from, None)]),
    )
    .unwrap();
    assert!(claims_of(&here, &id).is_empty(), "it waits for the end");
    look(
        dir,
        &mut store,
        &hand(
            &here,
            from + 60_000,
            vec![span(sid, 1, None, from, Some(from + 30_000))],
        ),
    )
    .unwrap();
    let alone_of = |path: &Path| {
        claims_of(&here, &id)
            .into_iter()
            .find(|claim| claim.path == path.to_str().unwrap())
            .map(|claim| claim.alone)
    };
    assert_eq!(alone_of(&quick), Some(Some(true)));
    let long_from = from + 100_000;
    let long = here.folder(&format!("{}-long", long_from + 500));
    let long_to = long_from + claim::ALONE_SPAN_MS + 1;
    look(
        dir,
        &mut store,
        &hand(
            &here,
            long_to + 1_000,
            vec![span(sid, 2, None, long_from, Some(long_to))],
        ),
    )
    .unwrap();
    assert_eq!(alone_of(&long), Some(None));
    let open_from = long_to + 10_000;
    let open = here.folder(&format!("{}-open", open_from + 500));
    look(
        dir,
        &mut store,
        &hand(
            &here,
            open_from + claim::ALONE_SPAN_MS + 1,
            vec![span(sid, 3, None, open_from, None)],
        ),
    )
    .unwrap();
    assert_eq!(alone_of(&open), Some(None));
}

/// A running span no longer handed (its tab left, or the read passed its block) is cut at the
/// look that misses it: it still contests a folder born before then, never accounts for one of
/// its own request's, and leaves the ring once its cut is older than the ring's reach.
#[test]
fn a_running_span_no_longer_handed_is_cut_contests_and_expires() {
    let here = Here::new();
    let tab = test_handle(313);
    let sid = tab.sid.as_str();
    let (id, opened) = request_with(sid);
    let from = opened + 10_000;
    let mut store = here.store();
    let dir = here.requests.path();
    look(
        dir,
        &mut store,
        &hand(
            &here,
            from + 1_000,
            vec![
                span(PERSON, 9, None, from, None),
                span(sid, 1, None, from, None),
            ],
        ),
    )
    .unwrap();
    let contested = here.folder(&format!("{}-contested", from + 25_000));
    let orphan = here.folder(&format!("{}-orphan", from + 70_000));
    // The person's span and the request's first are no longer handed: both are cut at 60 s.
    look(
        dir,
        &mut store,
        &hand(
            &here,
            from + 60_000,
            vec![span(sid, 2, None, from + 20_000, Some(from + 30_000))],
        ),
    )
    .unwrap();
    let held = held_spans(dir);
    let person = held.iter().find(|had| had.span.sid == PERSON).unwrap();
    assert_eq!(
        (person.whole, person.span.span.to_ms),
        (false, Some(from + 60_000))
    );
    look(
        dir,
        &mut store,
        &hand(
            &here,
            from + 80_000,
            vec![span(sid, 3, None, from + 65_000, Some(from + 75_000))],
        ),
    )
    .unwrap();
    let claims = claims_of(&here, &id);
    let of = |path: &Path| {
        claims
            .iter()
            .find(|claim| claim.path == path.to_str().unwrap())
            .map(|claim| claim.alone)
    };
    assert_eq!(of(&contested), Some(Some(false)));
    assert_eq!(
        of(&orphan),
        Some(Some(true)),
        "CONTROL: a whole span of its own held it"
    );
    // The request's own cut span accounts for nothing: a folder born in it after its last sight
    // and before the cut, held by no whole span, is not claimed.
    let unseen = here.folder(&format!("{}-unseen", from + 40_000));
    look(
        dir,
        &mut store,
        &hand(
            &here,
            from + 90_000,
            vec![span(sid, 4, None, from + 85_000, Some(from + 86_000))],
        ),
    )
    .unwrap();
    assert!(
        claims_of(&here, &id)
            .iter()
            .all(|claim| claim.path != unseen.to_str().unwrap())
    );
    look(
        dir,
        &mut store,
        &hand(&here, from + 60_000 + SPAN_RING_MS + 1, vec![]),
    )
    .unwrap();
    assert!(
        held_spans(dir).iter().all(|had| had.span.sid != PERSON),
        "a cut span leaves the ring"
    );
}

/// A person's command in a folder after its birth is a touch even when it ran before the look
/// that claimed the folder; and while a claim no person touched stands, the minute's look keeps
/// reading spans ([`crate::requests_host::watching`]).
#[test]
fn a_touch_counts_from_the_birth_and_an_untouched_claim_is_watched() {
    let here = Here::new();
    let tab = test_handle(314);
    let (id, opened) = request_with(tab.sid.as_str());
    let from = opened + 10_000;
    let folder = here.folder(&format!("{}-early", from + 500));
    let mut store = here.store();
    let dir = here.requests.path();
    look(
        dir,
        &mut store,
        &hand(
            &here,
            from + 5_000,
            vec![span(tab.sid.as_str(), 1, None, from, Some(from + 1_000))],
        ),
    )
    .unwrap();
    crate::requests_host::note_claims(dir, &mut store);
    assert!(
        crate::requests_host::watching(),
        "an untouched claim is watched"
    );
    assert!(store.get(&id).unwrap().touched.is_empty());
    // The person's command began after the birth but before the claim was recorded.
    look(
        dir,
        &mut store,
        &hand(
            &here,
            from + 70_000,
            vec![span(
                PERSON,
                5,
                Some(&folder),
                from + 2_000,
                Some(from + 3_000),
            )],
        ),
    )
    .unwrap();
    assert!(
        store
            .get(&id)
            .unwrap()
            .touched
            .contains(folder.to_str().unwrap())
    );
    crate::requests_host::note_claims(dir, &mut store);
    assert!(
        !crate::requests_host::watching(),
        "a touched claim is kept for good: nothing more to watch"
    );
}

/// The repo search answers unknown wherever it did not see the whole folder: a folder past its
/// depth holding a folder, more repos than it reads, a folder it cannot list. CONTROL: the repos
/// of a folder it saw whole, and none in a folder with none.
#[cfg(unix)]
#[test]
fn repos_in_is_unknown_where_it_did_not_see_the_whole_folder() {
    let here = Here::new();
    let home = here.home();
    let mk = |path: &Path| std::fs::create_dir_all(path).unwrap();
    let seen = here.folder("seen");
    mk(&seen.join("a/.git"));
    mk(&seen.join("b/c/.git/objects"));
    mk(&seen.join("plain/x"));
    let mut found = repos_in(&seen, &home).expect("seen whole");
    found.sort();
    assert_eq!(found, [seen.join("a"), seen.join("b/c")]);
    let none = here.folder("none");
    mk(&none.join("x/y"));
    assert_eq!(repos_in(&none, &home), Some(Vec::new()));
    let deep = here.folder("deep");
    mk(&deep.join("1/2/3/4/5"));
    assert_eq!(repos_in(&deep, &home), None, "past its depth");
    let many = here.folder("many");
    for n in 0..=REPOS_MAX {
        mk(&many.join(format!("r{n}/.git")));
    }
    assert_eq!(repos_in(&many, &home), None, "more repos than it reads");
    let shut = here.folder("shut");
    mk(&shut.join("locked/x"));
    use std::os::unix::fs::PermissionsExt as _;
    let locked = shut.join("locked");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let read = repos_in(&shut, &home);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(read, None, "a folder it cannot list");
}

/// A walk that failed measures no size, and one cut short is no whole figure: `claimed_mb` reads
/// unavailable for either, never 0 or a floor passed off as the size. CONTROL: a whole walk.
#[test]
fn a_failed_or_cut_walk_is_no_claimed_size() {
    let here = Here::new();
    let store = new_store();
    let tab = test_handle(315);
    let sid = tab.sid.as_str().to_owned();
    store.write().unwrap().register(tab);
    let (id, _) = request_with(&sid);
    let _driving = drive(NODE);
    let claim_with = |name: &str, probe: MeasureProbe| {
        set_measure_for_test(probe);
        let folder = here.folder(name);
        let reply = cmd_request_as(
            &format!(
                "claim path={} req={id}",
                aterm_codec::pct::encode(folder.to_str().unwrap())
            ),
            Scope::Owner,
            &store,
            no_client,
            &here.places(),
        );
        assert!(reply.starts_with("OK claimed"), "{reply}");
        assert!(flush_within(MUST), "the footprint thread flushed");
        measured(
            folder.to_str().unwrap(),
            crate::requests_host::now_unix_ms(),
        )
        .expect("measured")
    };
    let failed = claim_with(
        "failed",
        MeasureProbe {
            walk: |_, _| None,
            repos: |_, _| Some(false),
        },
    );
    assert_eq!((failed.bytes, failed.complete), (None, false));
    let facts = crate::sys_host::request_facts(&store, &sid).expect("its request");
    assert_eq!(facts.claimed_mb, None);
    let cut = MeasureProbe {
        walk: |_, _| Some((7 * 1024 * 1024, false)),
        repos: |_, _| Some(false),
    };
    let whole = MeasureProbe {
        walk: |_, _| Some((3 * 1024 * 1024, true)),
        repos: |_, _| Some(false),
    };
    // Re-measure the failed one whole, then add one cut short.
    measure(
        crate::requests_host::now_unix_ms(),
        vec![here.home().join("failed").to_str().unwrap().to_owned()],
        Some(here.home()),
        whole,
    );
    assert!(flush_within(MUST));
    let facts = crate::sys_host::request_facts(&store, &sid).expect("its request");
    assert_eq!(facts.claimed_mb, Some(3), "CONTROL: a whole walk");
    claim_with("cut", cut);
    let facts = crate::sys_host::request_facts(&store, &sid).expect("its request");
    assert_eq!(facts.claimed_mb, None);
}
