// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_vitals::spotlight::{Reading, Share, State, Volume};

const HOME: &str = "/Users//t";

/// The scope this Mac's fixtures read: a home with a `trust` checkout and a Library cache, the
/// volume's `/opt` and the build cache.
fn listing(receipt: &[&str]) -> ScopeListing {
    let home = PathBuf::from(HOME);
    ScopeListing {
        receipt: Ok(receipt.iter().map(PathBuf::from).collect()),
        refused: BTreeSet::new(),
        wanted: Ok(["/Users//t/trust", "/Users//t/Library/Caches", "/opt"]
            .into_iter()
            .map(PathBuf::from)
            .collect()),
        cache: Some(aterm_toolchain::build_cache::cache_root(&home)),
        yours: BTreeSet::new(),
        pause: None,
        home: Some(home),
    }
}

/// A receipt that covers the scope: every folder kept out is on it.
pub(crate) fn covered() -> ScopeReading {
    let listing = listing(&["/Users//t/trust", "/Users//t/Library/Caches", "/opt"]);
    let cache = listing.cache.clone().expect("a cache");
    ScopeReading {
        listing,
        found: BTreeMap::from([(cache, Found::Nothing)]),
    }
}

/// A receipt that misses $HOME/trust, which the probe found indexed.
pub(crate) fn missing_trust() -> ScopeReading {
    let listing = listing(&["/Users//t/Library/Caches", "/opt"]);
    let cache = listing.cache.clone().expect("a cache");
    ScopeReading {
        listing,
        found: BTreeMap::from([
            (PathBuf::from("/Users//t/trust"), Found::Item),
            (cache, Found::Nothing),
        ]),
    }
}

/// A split reading of the family with `volumes`.
pub(crate) fn family(volumes: Vec<Volume>) -> Reading {
    let share = |signing_id: &'static str, pid: u32, live: u32, mb: u64, total_s: u64| Share {
        signing_id,
        lead_pid: pid,
        cpu_mc: 10,
        read_bps: 0,
        footprint: Some(mb << 20),
        live,
        cpu_total_ns: total_s * 1_000_000_000,
    };
    let shares = vec![
        share("com.apple.mds_stores", 91920, 1, 1200, 875),
        share("com.apple.mds", 412, 1, 131, 259),
        share("com.apple.mdworker_shared", 7001, 9, 181, 50),
    ];
    let footprint = aterm_vitals::model::MemoryCharge {
        bytes: shares.iter().filter_map(|s| s.footprint).sum(),
        members: 3,
        unread: 0,
    };
    Reading {
        at_unix_ms: 1_763_300_000_000,
        window_ms: 2000,
        cpu_mc: 30,
        read_bps: 0,
        footprint,
        shares,
        volumes: Some(volumes),
        volumes_age_ms: Some(42_000),
        store: None,
        store_age_ms: None,
        store_stale_ms: None,
        state: State::Idle,
    }
}

/// The data volume and the system volume, reading `data`.
pub(crate) fn volumes(data: Indexing) -> Vec<Volume> {
    vec![
        Volume {
            path: "/".into(),
            indexing: Indexing::Enabled,
        },
        Volume {
            path: spotlight_scope::DATA_VOLUME.into(),
            indexing: data,
        },
    ]
}

/// A reading of a Mac that splits by coalition, its family read with `volumes`.
pub(crate) fn snapshot(volumes: Vec<Volume>) -> aterm_vitals::Snapshot {
    let mut snap = crate::stats_deck::tests::snapshot();
    snap.coal_lane = aterm_vitals::explain::CoalLane::On;
    snap.spotlight = Some(family(volumes));
    snap
}

/// The page's Spotlight document at a two-column width, drawn.
pub(crate) fn laid(
    snap: &aterm_vitals::Snapshot,
    scope: &ScopeReading,
) -> crate::stats_lens::PageLaid {
    let roles = crate::settings::Roles::from_theme(aterm_render::Theme::default());
    let input = crate::stats_deck::DeckInput {
        network: None,
        network_place: None,
        network_target: crate::network_deck::Target::Overview,
        disk_span: crate::stats_lens::DiskSpan::Day,
        cpu_policy_pending: false,
        lab: None,
        fleet: None,
        snap: Some(snap),
        prev_snap: None,
        tab_panes: Vec::new(),
        tab_label: String::new(),
        tab_count: 0,
        inks: crate::stats_deck::cached_inks(roles.surface, roles.text_primary, roles.accent),
        dwc: false,
        marks: Vec::new(),
        plate: None,
        form: crate::stats_plan::DeckForm::Full,
        contrast: false,
        locale: crate::stats_deck::host_locale(Some(snap)),
        live: crate::stats_live::DeckLive::STILL,
    };
    crate::stats_lens::lay_page(
        1152.0,
        &input,
        crate::stats_lens::PageDoc::Spotlight(Some(scope)),
        crate::stats_paint::Bar::Global,
        2.0,
        26.0,
        &crate::deck_face::deck_metrics(),
        true,
    )
}

/// Each What is indexed row's drawn cells, top down: its name, its Now box and its Wanted box.
fn table(laid: &crate::stats_lens::PageLaid) -> Vec<Vec<crate::stats_lens::TextBox>> {
    let at = laid
        .wells
        .iter()
        .position(|(name, _)| name == "What is indexed")
        .expect("the section is laid");
    let p = laid.placed[at].place;
    let mut lines: BTreeMap<i64, Vec<crate::stats_lens::TextBox>> = BTreeMap::new();
    for b in &laid.boxes {
        let inside = b.r[0] >= p.x - 0.01 && b.r[1] >= p.y && b.r[3] <= p.y + p.h + 0.01;
        if inside && b.r[2] <= p.x + p.w + 0.01 {
            lines
                .entry((b.r[3] * 100.0).round() as i64)
                .or_default()
                .push(b.clone());
        }
    }
    lines
        .into_values()
        .map(|mut line| {
            line.sort_by(|a, b| a.r[0].total_cmp(&b.r[0]));
            line
        })
        .filter(|line| line.len() == 3)
        .collect()
}

/// What is indexed holds a row a part of the wanted scope, then everything else on the data
/// volume and the build output, each with its Now, its Wanted and its words, through the page
/// document's words: a receipt that covers the scope reads as wanted everywhere; one that misses
/// $HOME/trust, which Spotlight was found to index, marks everything else on the data volume Indexed,
/// in words naming $HOME/trust; a data volume read off keeps out even what is wanted indexed.
/// CONTROL: of the drawn Now cells only that $HOME/trust row's takes the warning ink, and none does
/// where the receipt covers the scope, so the mark follows the reading, not the row.
#[test]
fn the_spotlight_document_holds_what_is_indexed_against_the_wanted_scope() {
    let on = Indexing::Enabled;
    let names = [
        "Applications",
        "Documents",
        "Desktop",
        "Synced drives",
        "Everything else on the data volume",
        "Build output and Rust docs",
    ];
    let wanted = [
        Now::Indexed,
        Now::Indexed,
        Now::Indexed,
        Now::Indexed,
        Now::KeptOut,
        Now::KeptOut,
    ];
    let check = |scope: &ScopeReading, data: Indexing, now: [Now; 6]| {
        let rows = what_is_indexed(Some(scope), Some(&data));
        let got: Vec<(&str, Now, Now)> = rows
            .iter()
            .map(|r| (r.name.as_str(), r.now, r.wanted))
            .collect();
        let expected: Vec<(&str, Now, Now)> = names
            .iter()
            .zip(now)
            .zip(wanted)
            .map(|((name, now), wanted)| (*name, now, wanted))
            .collect();
        assert_eq!(got, expected);
        let snap = snapshot(volumes(data));
        let laid = laid(&snap, scope);
        let (_, words) = laid
            .wells
            .iter()
            .find(|(name, _)| name == "What is indexed")
            .expect("the document says what is indexed");
        for row in &rows {
            assert!(words.contains(&row.said()), "{:?} in {words}", row.said());
            assert_eq!(row.said().contains("differs from wanted"), row.differs());
        }
        (rows, laid)
    };

    let (rows, covered_laid) = check(&covered(), on.clone(), wanted);
    assert!(rows.iter().all(|r| !r.differs()), "{rows:?}");

    let (rows, trust_laid) = check(
        &missing_trust(),
        on,
        [
            Now::Indexed,
            Now::Indexed,
            Now::Indexed,
            Now::Indexed,
            Now::Indexed,
            Now::KeptOut,
        ],
    );
    let differs: Vec<&ScopeRow> = rows.iter().filter(|r| r.differs()).collect();
    assert_eq!(differs.len(), 1, "{rows:?}");
    assert_eq!(differs[0].name, names[4]);
    assert!(differs[0].words.contains("$HOME/trust"), "{}", differs[0].words);

    let (rows, _) = check(
        &covered(),
        Indexing::Disabled,
        [
            Now::KeptOut,
            Now::KeptOut,
            Now::KeptOut,
            Now::KeptOut,
            Now::KeptOut,
            Now::KeptOut,
        ],
    );
    assert_eq!(
        rows.iter().filter(|r| r.differs()).count(),
        4,
        "what is wanted indexed differs while the volume is off: {rows:?}"
    );

    let roles = crate::settings::Roles::from_theme(aterm_render::Theme::default());
    let inks = crate::stats_deck::cached_inks(roles.surface, roles.text_primary, roles.accent);
    let warned = |laid: &crate::stats_lens::PageLaid| -> Vec<String> {
        let drawn = table(laid);
        assert_eq!(drawn.len(), names.len(), "a row each: {drawn:?}");
        drawn
            .iter()
            .filter(|line| line[1].ink == inks.warn_text)
            .map(|line| line[0].text.clone())
            .collect()
    };
    assert_eq!(warned(&trust_laid), vec![names[4].to_owned()]);
    assert!(
        table(&trust_laid)
            .iter()
            .filter(|line| !line[0].text.starts_with("Everything"))
            .all(|line| line[1].ink != inks.warn_text),
        "CONTROL: the other rows keep their ink"
    );
    assert!(
        warned(&covered_laid).is_empty(),
        "CONTROL: a covered scope marks no row"
    );
}

thread_local! {
    /// The folders the rotation port was asked of on this thread, in order.
    static ASKED: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Fifty folders the receipt does not name, and the build cache.
const MANY: usize = 50;

fn many() -> ScopeListing {
    let home = PathBuf::from(HOME);
    ScopeListing {
        receipt: Ok(BTreeSet::new()),
        refused: BTreeSet::new(),
        wanted: Ok((0..MANY).map(|i| home.join(format!("f{i:02}"))).collect()),
        cache: Some(aterm_toolchain::build_cache::cache_root(&home)),
        yours: BTreeSet::new(),
        pause: None,
        home: Some(home),
    }
}

/// The probe asks every folder in turn however many there are: with 51 folders to ask at four a
/// tick, a tick a minute asks each of them, the build cache in the first tick, within
/// ceil(51 / 4) ticks, though the first folders' answers age out at the tenth tick; and the
/// ticks after that ask the folders asked longest ago, never the same few again and again.
/// CONTROL: picking the first due folders in target order (the folders whose answers aged out
/// counting as due as those never asked) leaves folders never asked after as many ticks.
#[test]
fn the_probe_asks_every_folder_in_turn_however_many() {
    use std::time::{Duration, Instant};
    let slot = Arc::new(ScopeSlot::new(ScopePort {
        list: many,
        found: |path| {
            ASKED.with(|asked| asked.borrow_mut().push(path.to_path_buf()));
            Found::Nothing
        },
    }));
    let listing = many();
    let targets: Vec<PathBuf> = listing.targets().into_iter().map(|(p, _)| p).collect();
    assert_eq!(targets.len(), MANY + 1);
    let ticks = (MANY + 1).div_ceil(PROBES_A_TICK);
    let t0 = Instant::now();
    let at = |tick: usize| t0 + Duration::from_secs(61 * tick as u64);
    ASKED.with(|asked| asked.borrow_mut().clear());
    for tick in 0..ticks {
        assert!(slot.service(true, at(tick)), "tick {tick} ran");
        if tick == 0 {
            let first = ASKED.with(|asked| asked.borrow().clone());
            assert_eq!(first.first(), listing.cache.as_ref(), "the cache first");
        }
    }
    let asked: BTreeSet<PathBuf> = ASKED.with(|asked| asked.borrow().iter().cloned().collect());
    let never: Vec<&PathBuf> = targets.iter().filter(|p| !asked.contains(*p)).collect();
    assert!(
        never.is_empty(),
        "never asked within {ticks} ticks: {never:?}"
    );
    // The next ticks ask the oldest answers first: no folder twice before every other's turn.
    ASKED.with(|asked| asked.borrow_mut().clear());
    for tick in ticks..ticks + MANY / PROBES_A_TICK {
        slot.service(true, at(tick));
    }
    let again = ASKED.with(|asked| asked.borrow().clone());
    let distinct: BTreeSet<&PathBuf> = again.iter().collect();
    assert_eq!(
        distinct.len(),
        again.len(),
        "no folder asked twice: {again:?}"
    );

    // CONTROL: the order the first due folders were picked in before.
    let mut answers: BTreeMap<PathBuf, Instant> = BTreeMap::new();
    for tick in 0..ticks {
        let now = at(tick);
        let picked: Vec<PathBuf> = targets
            .iter()
            .filter(|p| {
                answers
                    .get(*p)
                    .is_none_or(|a| now.saturating_duration_since(*a) >= ANSWER_KEPT)
            })
            .take(PROBES_A_TICK)
            .cloned()
            .collect();
        for p in picked {
            answers.insert(p, now);
        }
    }
    assert!(
        answers.len() < targets.len(),
        "CONTROL: target order leaves folders never asked"
    );
}

/// A folder of `listing` by its name under the home.
fn at_home(name: &str) -> PathBuf {
    PathBuf::from(HOME).join(name)
}

/// What is indexed says each folder as the probes left it: before the first listing every row
/// reads Not read (no claim of what is indexed or why); a folder whose probe timed out is
/// named as not answering, never as not asked; every folder Spotlight finds items in is named,
/// with "setup could not keep it out" only for the folder setup was refused, and a folder the
/// receipt names that Spotlight finds items in reads as taken back; a receipt covering the
/// scope reads as a record of what setup excluded.
#[test]
fn what_is_indexed_says_each_folder_as_the_probes_left_it() {
    let on = Indexing::Enabled;
    let before = what_is_indexed(None, Some(&on));
    assert!(
        before
            .iter()
            .all(|r| r.now == Now::Unread && r.words == "not read yet"),
        "{before:?}"
    );
    assert!(
        what_is_indexed(None, Some(&Indexing::Disabled))
            .iter()
            .all(|r| r.now == Now::KeptOut),
        "CONTROL: a volume read off keeps everything out before any listing"
    );

    let else_row = |scope: &ScopeReading| {
        what_is_indexed(Some(scope), Some(&on))
            .into_iter()
            .find(|r| r.name.starts_with("Everything else"))
            .expect("the row")
    };
    let mut listing = listing(&["/opt"]);
    let trust = at_home("trust");
    let caches = at_home("Library/Caches");

    // $HOME/trust timed out, ~/Library/Caches not asked yet.
    let timed = ScopeReading {
        listing: listing.clone(),
        found: BTreeMap::from([(trust.clone(), Found::Unread)]),
    };
    let row = else_row(&timed);
    assert_eq!(row.now, Now::Unread);
    assert!(
        row.words.contains("did not answer within 5 s for $HOME/trust"),
        "{}",
        row.words
    );
    assert!(
        row.words
            .contains("~/Library/Caches is not kept out by setup and not asked of Spotlight yet"),
        "{}",
        row.words
    );
    assert!(!row.words.contains("$HOME/trust is not"), "{}", row.words);

    // Both found indexed, only $HOME/trust refused: each named, the refusal of $HOME/trust alone.
    listing.refused = BTreeSet::from([trust.clone()]);
    let found = ScopeReading {
        listing: listing.clone(),
        found: BTreeMap::from([(trust.clone(), Found::Item), (caches.clone(), Found::Item)]),
    };
    let row = else_row(&found);
    assert_eq!(row.now, Now::Indexed);
    assert!(
        row.words
            .contains("Spotlight finds items in ~/Library/Caches and $HOME/trust"),
        "{}",
        row.words
    );
    assert!(
        row.words.contains("setup could not keep $HOME/trust out"),
        "{}",
        row.words
    );
    assert!(
        !row.words.contains("keep ~/Library/Caches"),
        "{}",
        row.words
    );

    // The receipt names /opt, which Spotlight finds items in: taken back since.
    let taken = ScopeReading {
        listing: listing.clone(),
        found: BTreeMap::from([
            (trust.clone(), Found::Nothing),
            (caches.clone(), Found::Nothing),
            (PathBuf::from("/opt"), Found::Item),
        ]),
    };
    let row = else_row(&taken);
    assert_eq!(row.now, Now::Indexed);
    assert!(
        row.words
            .contains("setup's receipt says it kept /opt out, so it was taken back since"),
        "{}",
        row.words
    );

    // A covering receipt is a record, and says how many Spotlight was asked of.
    let row = else_row(&covered());
    assert_eq!(row.now, Now::KeptOut);
    assert!(
        row.words
            .contains("setup excluded all 3 of its folders (its receipt)")
            && row.words.contains("not asked of them yet"),
        "{}",
        row.words
    );
}
