// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE IMAGE HANDOFF (ATERM-KI-1112-145): each judgment and recheck runs as the verifier built
//! from the tree it judges, the landing handing itself to that build where it is another image.
//!
//! A tree's verifier here is its contract: the landing-owned flags its parser takes, the lines
//! of the tree's `verify/contract`, and the build recipe its code builds with (a `recipe=` line;
//! none is the first). Building a tree's verifier gives one file per contract and builder's
//! recipe, so two trees of one contract build the same image by one recipe and trees of two
//! contracts do not; an image builds by its own code's recipe. An image composes a judgment's
//! words with the real [`judgment_command`] less each landing-owned flag its own contract lacks;
//! the tree it judges refuses a flag its contract lacks as the KI's child did, exiting 2 before
//! its events existed. The real engine, handoff encoding, file, decoding and resume run; only
//! the exec is a call. Each test sits beside the control without the hop (`lie`: every build
//! reads as the running image, the behavior before this change), which reproduces the KI's
//! line.

use super::markdown::{DOC, edit_line, markdown_world, rechecked, rival_does};
use super::*;

/// The flags the landing gives a judgment's child, besides `--base`/`--events` and a round's
/// `--judged`/`--shared`.
const FLAGS: [&str; 4] = ["--input-drift", "--admit", "--timeout", "--test-cores"];
/// Where a tree states its verifier's contract.
const CONTRACT: &str = "verify/contract";
/// A contract's line naming the build recipe its verifier's code builds with (where its
/// intermediates go, its flags), as `recipe=<name>`.
const RECIPE: &str = "recipe=";
/// An image's line naming the recipe another than the first built it by, which changes its bytes.
const BUILT_BY: &str = "built-by=";
/// What every refusal of a resumed landing says the person does.
const FIX: &str = "Fix: run `targo --unverified verify land` again";

fn contract(flags: &[&str]) -> Vec<String> {
    flags.iter().map(|flag| (*flag).to_owned()).collect()
}

/// A contract without `flag`.
fn without(flag: &str) -> Vec<String> {
    FLAGS
        .iter()
        .filter(|kept| **kept != flag)
        .map(|kept| (*kept).to_owned())
        .collect()
}

fn contract_text(contract: &[String]) -> String {
    contract.iter().map(|flag| format!("{flag}\n")).collect()
}

/// The contract of the checkout at `root`, as it stands.
fn contract_at(root: &Path) -> Vec<String> {
    std::fs::read_to_string(root.join(CONTRACT))
        .expect("the tree states its contract")
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The verifier built from a tree of `contract`.
fn image_of(scratch: &Path, contract: &[String]) -> Image {
    let images = scratch.join("images");
    std::fs::create_dir_all(&images).expect("images");
    let path = images.join(format!("verifier{}", contract.join(",")));
    std::fs::write(&path, contract.join("\n")).expect("the image");
    let digest = crate::test_index::digest(&path).expect("its digest");
    Image { path, digest }
}

/// The recipe the code of a tree or an image of `contract` builds with: its `recipe=` line's,
/// else the first (empty).
fn recipe(contract: &[String]) -> String {
    contract
        .iter()
        .find_map(|line| line.strip_prefix(RECIPE))
        .unwrap_or_default()
        .to_owned()
}

/// The verifier an image of recipe `by` builds from a tree of `contract`: the tree's contract,
/// marked with the recipe where it is not the first.
fn build(scratch: &Path, contract: &[String], by: &str) -> Image {
    let mut lines = contract.to_vec();
    if !by.is_empty() {
        lines.push(format!("{BUILT_BY}{by}"));
    }
    image_of(scratch, &lines)
}

/// The contract the image at `path` was built with, and the recipe that built it.
fn contract_of(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .expect("the image")
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The words an image of contract `composer` gives a judgment's child: the real
/// [`judgment_command`]'s, after the tether's prefix, less each landing-owned flag (and its
/// value) that contract lacks.
fn words(
    composer: &[String],
    root: &Path,
    verifier: &Path,
    base: &str,
    round: Option<Round<'_>>,
    resources: &LandResources,
) -> Vec<String> {
    let cmd = judgment_command(
        verifier,
        root,
        base,
        round,
        Path::new("/events"),
        resources,
        &crate::EnvSnapshot::default(),
    )
    .expect("the command");
    let all: Vec<String> = cmd
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let mut words = all[if cfg!(unix) { 4 } else { 0 }..].iter();
    let mut kept = Vec::new();
    while let Some(word) = words.next() {
        if FLAGS.contains(&word.as_str()) && !composer.contains(word) {
            if word != crate::cli::INPUT_DRIFT {
                words.next();
            }
            continue;
        }
        kept.push(word.clone());
    }
    kept
}

/// How the verifier of a tree of `contract` takes `words`: a landing-owned flag outside its
/// contract is an unknown flag, and everything else must parse.
fn takes(contract: &[String], words: &[String]) -> Result<(), String> {
    if let Some(flag) = words
        .iter()
        .find(|word| FLAGS.contains(&word.as_str()) && !contract.contains(word))
    {
        return Err(format!("verify: unknown flag `{flag}`"));
    }
    crate::cli::parse(words.iter().cloned())
        .map(drop)
        .map_err(|usage| usage.message())
}

/// A child that refused its words: it exited 2 before it opened its events.
fn refused() -> Judged {
    Judged::from_events("", exit::USAGE, 0)
}

/// What one world's landings did, across every image.
#[derive(Default)]
struct Log {
    said: Vec<String>,
    handoffs: Vec<Handoff>,
    /// Each branch read: the contract of the image that read it, and its base.
    branches: Vec<(Vec<String>, String)>,
    /// Each child: the contract of the image that composed its words, and the words.
    runs: Vec<(Vec<String>, Vec<String>)>,
    /// Each refusal a tree's verifier gave.
    refusals: Vec<String>,
}

/// What a case changes in the checkout between the hop and the resume.
#[derive(Clone, Copy)]
enum Prep {
    None,
    /// The checkout gains a file no commit holds.
    Dirty,
    /// Origin's push URL names another repository than its fetch URL.
    SplitPush,
}

/// How a landing image starts: from the person's words, or from a handoff.
enum Entry {
    Start(Vec<String>),
    Resume(Box<Handoff>),
}

/// A world's landing across its images.
struct Landing<'w> {
    world: &'w World,
    scratch: &'w Path,
    /// The behavior before this change: every build reads as the running image.
    lie: bool,
    /// Each image a handoff resumed builds yet another image of its tree.
    unstable: bool,
    /// Each judgment's start, by its count across images: a run in its place, or `None` to run.
    on_judge: &'w dyn Fn(usize, Poll<'_>) -> Option<Ran>,
    log: &'w RefCell<Log>,
}

impl Landing<'_> {
    /// Land as the image of `contract` from `entry`: its exit.
    fn run(&self, contract: Vec<String>, entry: Entry) -> i32 {
        let own_image = image_of(self.scratch, &contract);
        let by = recipe(&contract);
        let resumed = matches!(entry, Entry::Resume(_));
        let land_words = match &entry {
            Entry::Start(words) => words.clone(),
            Entry::Resume(handoff) => handoff.words.clone(),
        };
        let resources = options(&land_words).expect("this image takes the landing's words");
        let child = |root: &Path, verifier: &Path, base: &str, round: Option<Round<'_>>| {
            let tree = contract_at(root);
            assert_eq!(
                verifier,
                build(self.scratch, &tree, &by).path,
                "a child runs as its tree's build"
            );
            let words = words(&contract, root, verifier, base, round, &resources);
            self.log
                .borrow_mut()
                .runs
                .push((contract.clone(), words.clone()));
            takes(&tree, &words)
        };
        let mut seams = Seams {
            git: &git_cmd,
            verifier: &mut |root: &Path| {
                let mut tree = contract_at(root);
                if self.unstable && resumed {
                    tree.push(format!("unstable{}", self.log.borrow().handoffs.len()));
                }
                let mut image = build(self.scratch, &tree, &by);
                if self.lie {
                    image.digest.clone_from(&own_image.digest);
                }
                Ok(image)
            },
            judge: &mut |root: &Path, verifier: &Path, base: &str, poll: Poll<'_>| {
                let taken = child(root, verifier, base, None);
                let n = self.log.borrow().runs.len() - 1;
                if let Some(ran) = (self.on_judge)(n, poll) {
                    return ran;
                }
                match taken {
                    Ok(()) => Ran::Ended(green()),
                    Err(why) => {
                        self.log.borrow_mut().refusals.push(why);
                        Ran::Ended(refused())
                    }
                }
            },
            branch: &mut |_: &Path, base: &str| {
                self.log
                    .borrow_mut()
                    .branches
                    .push((contract.clone(), base.to_owned()));
                crates(&["a"])
            },
            lane: &mut |_: &Path, _: &str| Lane::Pass,
            // A shared path a crate may own reaches every crate here: only the worlds' shared
            // Markdown is answered by a round.
            reach: &|_: &Path, _: &str, paths: Option<&[String]>| match paths {
                None => reaches(&["a"]),
                Some(_) => Reach::Everything(WHOLE.into()),
            },
            recheck: &mut |root: &Path, verifier: &Path, base: &str, delta: &Delta| match child(
                root,
                verifier,
                base,
                Some(Round {
                    judged: &delta.judged,
                    shared: Path::new("/shared"),
                    mechanical: delta.mechanical,
                }),
            ) {
                Ok(()) => rechecked(),
                Err(why) => {
                    self.log.borrow_mut().refusals.push(why);
                    refused()
                }
            },
            hand_over: &mut |image: &Image, file: &Path, root: &Path| {
                assert_eq!(
                    root,
                    self.world.work.as_path(),
                    "handed over in the checkout"
                );
                let handoff = Handoff::take(file).expect("the handoff reads");
                self.log.borrow_mut().handoffs.push(handoff.clone());
                self.run(contract_of(&image.path), Entry::Resume(Box::new(handoff)))
            },
            say: &mut |line: &str| self.log.borrow_mut().said.push(line.to_owned()),
            remote: &crate::remote::run,
            pause: &no_retry,
        };
        let own = Own {
            words: &land_words,
            image: &own_image.digest,
        };
        match entry {
            Entry::Start(_) => land(&self.world.work, &own, &mut seams),
            Entry::Resume(handoff) => resume(&self.world.work, &own, *handoff, &mut seams),
        }
    }
}

fn person(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

/// The KI's line: the starting image's words, refused by the tree it judges.
const KI_LINE: &str = "verify land: COULD NOT RUN — the run's record is not whole (its \
                       run-finished event is missing or names a check no line holds), the run \
                       exited 2 did not run; nothing was pushed";

/// Seed main with the verifier `contract`, and start the work branch from it.
fn seed_contract(world: &World, contract: &[String]) -> String {
    let seeded = rival_moves(world, CONTRACT, &contract_text(contract));
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
    seeded
}

/// THE KI'S EXIT TEST: main moves into the branch's file during the first judgment and, in the
/// same commit, retires `--input-drift` from its verifier. The interrupted judgment's re-judge
/// judges a tree whose verifier no longer takes a flag the starting image passes: the landing
/// hands itself, once, to that tree's verifier with the receipt so far (one interrupted
/// judgment), the branch's crates (read by the starting image over M1) and the person's
/// `--test-cores 4`, and that image composes the words it takes itself and lands, its receipt
/// as the harness reads it naming the interrupt and the judgment of the merge. NEGATIVE
/// CONTROL: without the hop, the starting image's words are refused and the landing ends with
/// the KI's line, pushing nothing.
#[test]
fn a_rejudge_tree_whose_verifier_drops_a_flag_the_starting_run_passes_lands_as_that_tree() {
    let v1 = contract(&FLAGS);
    let v2 = without(crate::cli::INPUT_DRIFT);
    for lie in [false, true] {
        let world = world();
        let m1 = seed_contract(&world, &v1);
        let branch_head = edit_line(&world.work, "shared.txt", 8, "branch");
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let log = RefCell::new(Log::default());
        let moved = RefCell::new(String::new());
        let on_judge = |n: usize, poll: Poll<'_>| {
            if n != 0 {
                return None;
            }
            let first = log.borrow().runs[0].1.clone();
            assert!(
                first.iter().any(|word| word == crate::cli::INPUT_DRIFT),
                "{first:?}"
            );
            assert!(
                first.windows(2).any(|pair| pair == ["--test-cores", "4"]),
                "{first:?}"
            );
            *moved.borrow_mut() = rival_does(&world, |rival| {
                write(rival, CONTRACT, &contract_text(&v2));
                drop(edit_line(rival, "shared.txt", 1, "main"));
            });
            assert!(
                poll(&mut || false),
                "a shared non-Markdown path dooms the run"
            );
            Some(Ran::Interrupted { secs: 0 })
        };
        let landing = Landing {
            world: &world,
            scratch: scratch.path(),
            lie,
            unstable: false,
            on_judge: &on_judge,
            log: &log,
        };
        let code = landing.run(v1.clone(), Entry::Start(person(&["--test-cores", "4"])));
        let log = log.into_inner();
        let moved = moved.into_inner();
        let said = &log.said;
        if lie {
            assert_eq!(code, exit::COULD_NOT_RUN, "{said:#?}");
            assert!(said.iter().any(|line| line == KI_LINE), "{said:#?}");
            assert_eq!(log.refusals, ["verify: unknown flag `--input-drift`"]);
            assert!(log.handoffs.is_empty());
            assert_eq!(origin_main(&world), moved, "nothing was pushed");
            continue;
        }
        assert_eq!(code, exit::PASS, "{said:#?}");
        let [handoff] = log.handoffs.as_slice() else {
            panic!("one handoff: {:#?}", log.handoffs);
        };
        assert_eq!(handoff.step, aterm_land::Step::Qualify);
        assert_eq!(handoff.base, moved);
        assert_eq!(
            git(&world.work, &["rev-parse", &format!("{}^1", handoff.at)]),
            branch_head
        );
        assert_eq!(
            git(&world.work, &["rev-parse", &format!("{}^2", handoff.at)]),
            moved
        );
        assert_eq!((handoff.composes, &handoff.cone), (0, &m1));
        assert_eq!(handoff.branch, Some(crates(&["a"])));
        assert_eq!(handoff.words, ["--test-cores", "4"]);
        assert_eq!(
            (
                handoff.stated.interrupted.len(),
                handoff.stated.checked.len()
            ),
            (1, 0)
        );
        assert_eq!(
            log.branches,
            [(v1.clone(), m1)],
            "read once, by the starting image"
        );
        let [(_, first), (composer, second)] = log.runs.as_slice() else {
            panic!("two children: {:#?}", log.runs);
        };
        assert!(first.iter().any(|word| word == crate::cli::INPUT_DRIFT));
        assert_eq!(
            composer, &v2,
            "the re-judge's words are its tree's own build's"
        );
        assert!(
            !second.iter().any(|word| word == crate::cli::INPUT_DRIFT),
            "{second:?}"
        );
        assert!(
            second.windows(2).any(|pair| pair == ["--test-cores", "4"]),
            "{second:?}"
        );
        assert!(log.refusals.is_empty(), "{:?}", log.refusals);
        for part in [
            "verify land: INTERRUPTED the judgment",
            "the landing goes on as",
            "verify land: resumed as",
            "verify land: LANDED",
        ] {
            assert_eq!(
                said.iter().filter(|line| line.contains(part)).count(),
                1,
                "{part}: {said:#?}"
            );
        }
        let landing = origin_main(&world);
        assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), landing);
        let receipt = stated(&world, &landing).expect("the harness reads the receipt");
        assert_eq!(receipt.interrupted[0].tip, moved);
        assert_eq!(receipt.checked.len(), 1);
        assert_eq!(receipt.checked[0].base, moved);
        assert_eq!(
            receipt.checked[0].tree,
            git(&world.work, &["rev-parse", &format!("{landing}^{{tree}}")])
        );
        assert_eq!(receipt.rejudges(), 1);
    }
}

/// Main retired a flag before the landing started, and the landing merges it itself: its first
/// judgment already judges a tree whose verifier is another image, so the landing hands itself
/// over before any judgment, with an empty receipt and no branch crates, and the receiver reads
/// the branch's crates over M1 as itself, judges and lands. NEGATIVE CONTROL: without the hop
/// the first judgment ends with the KI's line.
#[test]
fn a_first_judgment_of_a_merge_the_landing_made_runs_as_the_merged_tree() {
    let v1 = contract(&FLAGS);
    let v2 = without(crate::cli::INPUT_DRIFT);
    for lie in [false, true] {
        let world = world();
        seed_contract(&world, &v1);
        commit(&world.work, "a/x.txt", "a2\n");
        let m1 = rival_moves(&world, CONTRACT, &contract_text(&v2));
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let log = RefCell::new(Log::default());
        let landing = Landing {
            world: &world,
            scratch: scratch.path(),
            lie,
            unstable: false,
            on_judge: &|_, _| None,
            log: &log,
        };
        let code = landing.run(v1.clone(), Entry::Start(Vec::new()));
        let log = log.into_inner();
        let said = &log.said;
        if lie {
            assert_eq!(code, exit::COULD_NOT_RUN, "{said:#?}");
            assert!(said.iter().any(|line| line == KI_LINE), "{said:#?}");
            assert_eq!(log.branches, [(v1.clone(), m1.clone())]);
            assert_eq!(origin_main(&world), m1, "nothing was pushed");
            continue;
        }
        assert_eq!(code, exit::PASS, "{said:#?}");
        let [handoff] = log.handoffs.as_slice() else {
            panic!("one handoff: {:#?}", log.handoffs);
        };
        assert_eq!(handoff.step, aterm_land::Step::Qualify);
        assert_eq!(handoff.base, m1);
        assert_eq!((&handoff.cone, handoff.branch.as_ref()), (&m1, None));
        assert_eq!(handoff.stated, Stated::default());
        assert_eq!(
            log.branches,
            [(v2.clone(), m1)],
            "read by the image that judges"
        );
        let [(composer, run)] = log.runs.as_slice() else {
            panic!("one child: {:#?}", log.runs);
        };
        assert_eq!(composer, &v2);
        assert!(!run.iter().any(|word| word == crate::cli::INPUT_DRIFT));
        assert!(
            said.iter()
                .any(|line| line.starts_with("verify land: LANDED")),
            "{said:#?}"
        );
    }
}

/// A recheck hands over too: main moves only the shared Markdown during the judgment and, in a
/// path the branch never touched, retires `--admit` from its verifier. The pass is merged with
/// that main and its recheck is handed to the merge's own verifier with the composition spent
/// (and no deadline: none spans a judgment's push); that image rechecks with the words it takes
/// and lands the merge, stated as rechecked onto that main. NEGATIVE CONTROL: without the hop
/// the starting image sends `--admit wait`, which the merge's verifier refuses: nothing is
/// pushed.
#[test]
fn a_recheck_hands_over_with_its_composition_count() {
    let v1 = contract(&FLAGS);
    let v2 = without("--admit");
    for lie in [false, true] {
        let seed = contract_text(&v1);
        let world = markdown_world(&[(CONTRACT, Some(seed.as_str()), false)]);
        commit(&world.work, "a/x.txt", "a2\n");
        edit_line(&world.work, DOC, 1, "one");
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let log = RefCell::new(Log::default());
        let moved = RefCell::new(String::new());
        let on_judge = |n: usize, _: Poll<'_>| {
            if n == 0 {
                *moved.borrow_mut() = rival_does(&world, |rival| {
                    write(rival, CONTRACT, &contract_text(&v2));
                    drop(edit_line(rival, DOC, 8, "eight"));
                });
            }
            None
        };
        let landing = Landing {
            world: &world,
            scratch: scratch.path(),
            lie,
            unstable: false,
            on_judge: &on_judge,
            log: &log,
        };
        let code = landing.run(v1.clone(), Entry::Start(Vec::new()));
        let log = log.into_inner();
        let moved = moved.into_inner();
        let said = &log.said;
        let recheck = &log.runs.last().expect("a recheck ran").1;
        assert!(
            recheck.windows(2).any(|pair| pair[0] == "--judged")
                && recheck
                    .windows(2)
                    .any(|pair| pair == ["--shared", "/shared"]),
            "the last run is the round: {recheck:?}"
        );
        if lie {
            assert_eq!(code, exit::COULD_NOT_RUN, "{said:#?}");
            assert!(recheck.windows(2).any(|pair| pair == ["--admit", "wait"]));
            assert_eq!(log.refusals, ["verify: unknown flag `--admit`"]);
            assert!(log.handoffs.is_empty());
            assert_eq!(origin_main(&world), moved, "nothing was pushed");
            continue;
        }
        assert_eq!(code, exit::PASS, "{said:#?}");
        let [handoff] = log.handoffs.as_slice() else {
            panic!("one handoff: {:#?}", log.handoffs);
        };
        assert_eq!(handoff.step, aterm_land::Step::Recheck);
        assert_eq!(handoff.base, moved);
        assert_eq!(handoff.composes, 1, "the recheck's composition is spent");
        assert_eq!(handoff.stated.checked.len(), 1);
        assert_eq!(log.runs.len(), 2, "{:#?}", log.runs);
        assert_eq!(
            log.runs[1].0, v2,
            "the recheck's words are the merge's own build's"
        );
        assert!(!recheck.iter().any(|word| word == "--admit"), "{recheck:?}");
        assert!(log.refusals.is_empty(), "{:?}", log.refusals);
        let landing = origin_main(&world);
        let receipt = stated(&world, &landing).expect("stated");
        assert_eq!(receipt.rechecked.len(), 1);
        assert_eq!(receipt.rechecked[0].onto, moved);
        assert!(
            said.iter()
                .any(|line| line.starts_with("verify land: LANDED")
                    && line.contains(&format!("rechecked onto {moved}"))),
            "{said:#?}"
        );
    }
}

/// A step hands on at most [`HOPS`] times: where each receiver's own build of the tree is yet
/// another image, the landing could not run after the second handoff, naming the rerun, with
/// nothing judged or pushed. NEGATIVE CONTROL: a receiver whose build is itself lands after one.
#[test]
fn an_image_whose_own_build_is_not_itself_could_not_run_instead_of_looping() {
    let v1 = contract(&FLAGS);
    let v2 = without(crate::cli::INPUT_DRIFT);
    for unstable in [true, false] {
        let world = world();
        seed_contract(&world, &v1);
        commit(&world.work, "a/x.txt", "a2\n");
        let m1 = rival_moves(&world, CONTRACT, &contract_text(&v2));
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let log = RefCell::new(Log::default());
        let landing = Landing {
            world: &world,
            scratch: scratch.path(),
            lie: false,
            unstable,
            on_judge: &|_, _| None,
            log: &log,
        };
        let code = landing.run(v1.clone(), Entry::Start(Vec::new()));
        let log = log.into_inner();
        let said = &log.said;
        let hops: Vec<usize> = log.handoffs.iter().map(|handoff| handoff.hop).collect();
        if unstable {
            assert_eq!(hops, (1..=HOPS).collect::<Vec<_>>(), "{said:#?}");
            assert_eq!(code, exit::COULD_NOT_RUN, "{said:#?}");
            assert!(
                said.iter()
                    .any(|line| line.starts_with("verify land: COULD NOT RUN")
                        && line.contains("hands it on again")
                        && line.contains(FIX)),
                "{said:#?}"
            );
            assert!(log.runs.is_empty() && log.branches.is_empty());
            assert_eq!(origin_main(&world), m1, "nothing was pushed");
        } else {
            assert_eq!(hops, [1], "{said:#?}");
            assert_eq!(code, exit::PASS, "{said:#?}");
        }
    }
}

/// THE RECIPE SKEW: a merge that changes how the verifier is built (where its intermediates
/// go, its flags) hands its step on twice. The starting image builds the merged tree by its own
/// recipe; that image, running the merged tree's code, builds the same tree by the tree's
/// recipe into another image and hands the step on once more, its second hop, with the same
/// receipt; that image's own build is itself, and it reads the branch's crates, judges and
/// lands. NEGATIVE CONTROL: a merge that keeps the recipe lands after one hop.
#[test]
fn a_merge_that_changes_the_verifiers_build_recipe_hands_its_step_on_twice_and_lands() {
    let v1 = contract(&FLAGS);
    for changed in [true, false] {
        let mut v2 = without(crate::cli::INPUT_DRIFT);
        if changed {
            v2.push(format!("{RECIPE}2"));
        }
        let world = world();
        seed_contract(&world, &v1);
        commit(&world.work, "a/x.txt", "a2\n");
        let m1 = rival_moves(&world, CONTRACT, &contract_text(&v2));
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let log = RefCell::new(Log::default());
        let landing = Landing {
            world: &world,
            scratch: scratch.path(),
            lie: false,
            unstable: false,
            on_judge: &|_, _| None,
            log: &log,
        };
        let code = landing.run(v1.clone(), Entry::Start(Vec::new()));
        let log = log.into_inner();
        let said = &log.said;
        assert_eq!(code, exit::PASS, "{said:#?}");
        let hops: Vec<usize> = log.handoffs.iter().map(|handoff| handoff.hop).collect();
        assert_eq!(
            hops,
            if changed { vec![1, 2] } else { vec![1] },
            "{said:#?}"
        );
        let first = &log.handoffs[0];
        for handoff in &log.handoffs {
            assert_eq!(
                (handoff.step, &handoff.at, &handoff.base, &handoff.stated),
                (
                    aterm_land::Step::Qualify,
                    &first.at,
                    &m1,
                    &Stated::default()
                )
            );
        }
        let mut judging = v2.clone();
        if changed {
            judging.push(format!("{BUILT_BY}2"));
        }
        assert_eq!(
            log.branches,
            [(judging.clone(), m1)],
            "read by the image that judges"
        );
        let [(composer, run)] = log.runs.as_slice() else {
            panic!("one child: {:#?}", log.runs);
        };
        assert_eq!(composer, &judging);
        assert!(!run.iter().any(|word| word == crate::cli::INPUT_DRIFT));
        assert!(
            said.iter()
                .any(|line| line.starts_with("verify land: LANDED")),
            "{said:#?}"
        );
    }
}

/// A recheck hands on as a judgment does: where the merge it rechecks also changes the
/// verifier's build recipe, the receiver's own build of the merge is yet another image, and it
/// hands the recheck on as its second hop with the same composition count and receipt; that
/// image rechecks and lands, stated as rechecked onto that main. NEGATIVE CONTROL: a merge that
/// keeps the recipe rechecks after one hop.
#[test]
fn a_recheck_whose_merge_changes_the_build_recipe_hands_on_twice_with_its_count() {
    let v1 = contract(&FLAGS);
    for changed in [true, false] {
        let mut v2 = without("--admit");
        if changed {
            v2.push(format!("{RECIPE}2"));
        }
        let seed = contract_text(&v1);
        let world = markdown_world(&[(CONTRACT, Some(seed.as_str()), false)]);
        commit(&world.work, "a/x.txt", "a2\n");
        edit_line(&world.work, DOC, 1, "one");
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let log = RefCell::new(Log::default());
        let moved = RefCell::new(String::new());
        let on_judge = |n: usize, _: Poll<'_>| {
            if n == 0 {
                *moved.borrow_mut() = rival_does(&world, |rival| {
                    write(rival, CONTRACT, &contract_text(&v2));
                    drop(edit_line(rival, DOC, 8, "eight"));
                });
            }
            None
        };
        let landing = Landing {
            world: &world,
            scratch: scratch.path(),
            lie: false,
            unstable: false,
            on_judge: &on_judge,
            log: &log,
        };
        let code = landing.run(v1.clone(), Entry::Start(Vec::new()));
        let log = log.into_inner();
        let moved = moved.into_inner();
        let said = &log.said;
        assert_eq!(code, exit::PASS, "{said:#?}");
        let hops: Vec<_> = log
            .handoffs
            .iter()
            .map(|handoff| (handoff.step, handoff.hop, handoff.composes))
            .collect();
        let recheck = |hop| (aterm_land::Step::Recheck, hop, 1);
        assert_eq!(
            hops,
            if changed {
                vec![recheck(1), recheck(2)]
            } else {
                vec![recheck(1)]
            },
            "{said:#?}"
        );
        let first = &log.handoffs[0];
        for handoff in &log.handoffs {
            assert_eq!(
                (&handoff.at, &handoff.base, &handoff.stated),
                (&first.at, &moved, &first.stated)
            );
        }
        let receipt = stated(&world, &origin_main(&world)).expect("stated");
        assert_eq!(receipt.rechecked.len(), 1);
        assert_eq!(receipt.rechecked[0].onto, moved);
    }
}

/// A LATER STEP HANDS ON AS THE FIRST DID: a step that runs where it was handed leaves the next
/// step at hop 0. The main the landing merges before its first judgment retires `--input-drift`
/// and changes the verifier's build recipe, so that judgment hands on twice and runs and passes
/// at hop [`HOPS`]; main then moves into the branch's file and, in the same commit, retires
/// `--admit` too. The re-judge's own verifier is another image again, and that step hands over
/// as its own first hop, with the pass made at hop 2, the branch's crates (read by the image
/// that ran it); its receiver, whose own build is itself, judges and lands, stating both
/// judgments. A step that kept the hop count of the one before would refuse to
/// hand on "after 2 handoffs of this step": COULD NOT RUN, nothing pushed.
#[test]
fn a_rejudge_after_a_step_that_ran_where_it_was_handed_hands_on_from_its_first_hop() {
    let v1 = contract(&FLAGS);
    let mut v2 = without(crate::cli::INPUT_DRIFT);
    v2.push(format!("{RECIPE}2"));
    let v3: Vec<String> = v2
        .iter()
        .filter(|line| *line != "--admit")
        .cloned()
        .collect();
    let world = world();
    seed_contract(&world, &v1);
    edit_line(&world.work, "shared.txt", 8, "branch");
    let m1 = rival_moves(&world, CONTRACT, &contract_text(&v2));
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let log = RefCell::new(Log::default());
    let moved = RefCell::new(String::new());
    let on_judge = |n: usize, _: Poll<'_>| {
        if n == 0 {
            *moved.borrow_mut() = rival_does(&world, |rival| {
                write(rival, CONTRACT, &contract_text(&v3));
                drop(edit_line(rival, "shared.txt", 1, "main"));
            });
        }
        None
    };
    let landing = Landing {
        world: &world,
        scratch: scratch.path(),
        lie: false,
        unstable: false,
        on_judge: &on_judge,
        log: &log,
    };
    let code = landing.run(v1, Entry::Start(Vec::new()));
    let log = log.into_inner();
    let moved = moved.into_inner();
    let said = &log.said;
    assert_eq!(code, exit::PASS, "{said:#?}");
    let hops: Vec<usize> = log.handoffs.iter().map(|handoff| handoff.hop).collect();
    assert_eq!(hops, [1, 2, 1], "{said:#?}");
    let [first, second, rejudge] = log.handoffs.as_slice() else {
        panic!("three handoffs: {:#?}", log.handoffs);
    };
    for handoff in [first, second] {
        assert_eq!(
            (handoff.step, &handoff.at, &handoff.base, &handoff.stated),
            (
                aterm_land::Step::Qualify,
                &first.at,
                &m1,
                &Stated::default()
            )
        );
    }
    assert_eq!(
        (rejudge.step, &rejudge.base, rejudge.composes, &rejudge.cone),
        (aterm_land::Step::Qualify, &moved, 0, &m1)
    );
    assert_eq!(
        git(&world.work, &["rev-parse", &format!("{}^1", rejudge.at)]),
        first.at
    );
    assert_eq!(
        git(&world.work, &["rev-parse", &format!("{}^2", rejudge.at)]),
        moved
    );
    assert_eq!(rejudge.branch, Some(crates(&["a"])));
    assert_eq!(
        (
            rejudge.stated.checked.len(),
            rejudge.stated.interrupted.len()
        ),
        (1, 0)
    );
    assert_eq!(rejudge.stated.checked[0].base, m1);
    let built_by_2 = |contract: &[String]| {
        let mut image = contract.to_vec();
        image.push(format!("{BUILT_BY}2"));
        image
    };
    assert_eq!(
        log.branches,
        [(built_by_2(&v2), m1.clone())],
        "read once, by the image that ran the first judgment"
    );
    let composers: Vec<&Vec<String>> = log.runs.iter().map(|(composer, _)| composer).collect();
    assert_eq!(composers, [&built_by_2(&v2), &built_by_2(&v3)]);
    assert!(log.refusals.is_empty(), "{:?}", log.refusals);
    let landing = origin_main(&world);
    assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), landing);
    let receipt = stated(&world, &landing).expect("the harness reads the receipt");
    assert_eq!(receipt.checked.len(), 2, "{said:#?}");
    assert_eq!(
        (
            receipt.checked[0].base.as_str(),
            receipt.checked[1].base.as_str()
        ),
        (m1.as_str(), moved.as_str())
    );
    assert_eq!(receipt.rejudges(), 1);
}

/// `land --resume` reads and removes only its checkout's own handoff file: a person's file
/// elsewhere, one holding a handoff included, is refused and left as it was. NEGATIVE CONTROL:
/// the same bytes in the git directory's own handoff file are taken, refused by name for their
/// schema, and removed.
#[test]
fn a_resume_of_a_file_that_is_not_the_checkouts_own_handoff_leaves_it_as_it_was() {
    let world = world();
    let head = git(&world.work, &["rev-parse", "HEAD"]);
    let sample = Handoff {
        root: world.work.clone(),
        image: "0".repeat(64),
        words: Vec::new(),
        step: aterm_land::Step::Qualify,
        hop: 1,
        at: head.clone(),
        base: head.clone(),
        composes: 0,
        cone: head,
        branch: None,
        stated: Stated::default(),
    };
    let bytes = String::from_utf8(sample.encode().expect("it encodes"))
        .expect("UTF-8")
        .replace(r#""schema":2"#, r#""schema":3"#);
    let notes = world.work.join("notes.txt");
    let own =
        PathBuf::from(git(&world.work, &["rev-parse", "--absolute-git-dir"])).join(handoff::FILE);
    for file in [&notes, &own] {
        std::fs::write(file, &bytes).expect("the file");
        let words = [handoff::RESUME.to_owned(), file.display().to_string()];
        assert_eq!(
            run(&words, &world.work),
            exit::COULD_NOT_RUN,
            "{}",
            file.display()
        );
        if file == &notes {
            assert_eq!(std::fs::read_to_string(file).ok(), Some(bytes.clone()));
        } else {
            assert!(!file.exists(), "the checkout's own handoff is taken");
        }
    }
}

/// `land --resume` parses the person's `land` words again as this build: a handoff of words
/// its options do not take (a flag the sending build took and this one does not) could not
/// run, naming those words and the rerun, before the image the handoff names is compared or
/// anything is judged or pushed. The checkout's own handoff file is taken, read once whether or
/// not it is honoured, so it is gone. A receiver that went on under its defaults instead would
/// refuse this handoff for its image, which is not this build's, and say nothing of the words.
#[test]
fn a_handoff_of_words_this_build_does_not_take_could_not_run_naming_them() {
    let world = world();
    let head = git(&world.work, &["rev-parse", "HEAD"]);
    let before = origin_main(&world);
    let words = person(&["--checks", "fmt"]);
    assert_eq!(
        options(&words),
        None,
        "this build's `land` takes no `--checks`"
    );
    let own =
        PathBuf::from(git(&world.work, &["rev-parse", "--absolute-git-dir"])).join(handoff::FILE);
    let handed = Handoff {
        root: world.work.clone(),
        image: "0".repeat(64),
        words,
        step: aterm_land::Step::Qualify,
        hop: 1,
        at: head.clone(),
        base: head.clone(),
        composes: 0,
        cone: head.clone(),
        branch: None,
        stated: Stated::default(),
    };
    handoff::write(&own, &handed).expect("the handoff is written");
    let mut said = Vec::new();
    let code = resume_cli(&own, &world.work, &mut |line| said.push(line.to_owned()));
    assert_eq!(code, exit::COULD_NOT_RUN, "{said:#?}");
    let refusal = format!(
        "verify land: COULD NOT RUN — this tree's verify land does not take the landing's words \
         `--checks fmt`; {RERUN}"
    );
    assert_eq!(said, [refusal], "the words' refusal, and nothing after it");
    assert!(said[0].contains(FIX), "{said:#?}");
    assert!(
        !own.exists(),
        "the checkout's own handoff is taken, whether or not it is honoured"
    );
    assert_eq!(origin_main(&world), before, "nothing was pushed");
    assert_eq!(
        git(&world.work, &["rev-parse", "HEAD"]),
        head,
        "nothing was merged"
    );
}

/// A handoff the receiver cannot honour could not run, naming why and the rerun, before
/// anything is judged or pushed: one handed to another image, for another checkout, over a base
/// or a cone its commit does not hold, from a push source the landing refuses, past this
/// image's composition bound, or whose checkout left its commit (HEAD moved, or the
/// checkout gained changes) between the hop and the resume. NEGATIVE CONTROL: the same handoff,
/// honoured, lands.
#[test]
fn a_handoff_the_receiver_cannot_honour_could_not_run() {
    let v1 = contract(&FLAGS);
    let world = world();
    let m0 = seed_contract(&world, &v1);
    let at = commit(&world.work, "a/x.txt", "a2\n");
    let elsewhere = rival_moves(&world, "b/y.txt", "b2\n");
    git(&world.work, &["fetch", "-q", "origin"]);
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let honoured = Handoff {
        root: world.work.clone(),
        image: image_of(scratch.path(), &v1).digest,
        words: Vec::new(),
        step: aterm_land::Step::Qualify,
        hop: 1,
        at: at.clone(),
        base: m0.clone(),
        composes: 0,
        cone: m0.clone(),
        branch: None,
        stated: Stated::default(),
    };
    let passed = aterm_types::land_trailer::Checked {
        tree: git(&world.work, &["rev-parse", "HEAD^{tree}"]),
        rows: vec![aterm_types::land_trailer::Row {
            check: "test".into(),
            word: "ok".into(),
        }],
        base: m0.clone(),
        secs: 1,
    };
    // Each case: what its refusal names, the handoff, and what changes in the checkout between
    // the hop and the resume.
    let cases = [
        (
            "the landing was handed to image 000000000000".to_owned(),
            Handoff {
                image: "0".repeat(64),
                ..honoured.clone()
            },
            Prep::None,
        ),
        (
            "the landing was handed over for the checkout".to_owned(),
            Handoff {
                root: world.rival.clone(),
                ..honoured.clone()
            },
            Prep::None,
        ),
        (
            format!("the handoff's base {elsewhere} is not in its commit {at}"),
            Handoff {
                base: elsewhere.clone(),
                ..honoured.clone()
            },
            Prep::None,
        ),
        (
            format!("the handoff's cone {elsewhere} is not in its commit {at}"),
            Handoff {
                cone: elsewhere.clone(),
                ..honoured.clone()
            },
            Prep::None,
        ),
        (
            "remote fetch and push URLs must address the same repository".to_owned(),
            honoured.clone(),
            Prep::SplitPush,
        ),
        (
            "landing race count exceeds its original bound".to_owned(),
            Handoff {
                step: aterm_land::Step::Recheck,
                composes: COMPOSES + 1,
                branch: Some(crates(&["a"])),
                stated: Stated {
                    checked: vec![passed],
                    ..Stated::default()
                },
                ..honoured.clone()
            },
            Prep::None,
        ),
        (
            "the checkout gained changes no commit holds".to_owned(),
            honoured.clone(),
            Prep::Dirty,
        ),
    ];
    for (why, handoff, prep) in cases {
        let why = why.as_str();
        match prep {
            Prep::None => {}
            Prep::Dirty => write(&world.work, "untracked.txt", "uncommitted\n"),
            Prep::SplitPush => drop(git(
                &world.work,
                &["remote", "set-url", "--push", "origin", path(&world.rival)],
            )),
        }
        let log = RefCell::new(Log::default());
        let landing = Landing {
            world: &world,
            scratch: scratch.path(),
            lie: false,
            unstable: false,
            on_judge: &|_, _| None,
            log: &log,
        };
        let code = landing.run(v1.clone(), Entry::Resume(Box::new(handoff)));
        let log = log.into_inner();
        assert_eq!(code, exit::COULD_NOT_RUN, "{why}: {:#?}", log.said);
        assert!(
            log.said
                .iter()
                .any(|line| line.starts_with("verify land: COULD NOT RUN")
                    && line.contains(why)
                    && line.contains(FIX)),
            "{why}: {:#?}",
            log.said
        );
        assert!(log.runs.is_empty() && log.branches.is_empty(), "{why}");
        assert_eq!(origin_main(&world), elsewhere, "{why}: nothing was pushed");
        match prep {
            Prep::None => {}
            Prep::Dirty => {
                std::fs::remove_file(world.work.join("untracked.txt")).expect("it was written")
            }
            Prep::SplitPush => drop(git(
                &world.work,
                &["config", "--unset", "remote.origin.pushurl"],
            )),
        }
    }
    // HEAD moved from the handoff's commit.
    let later = commit(&world.work, "a/x.txt", "a3\n");
    let log = RefCell::new(Log::default());
    let landing = Landing {
        world: &world,
        scratch: scratch.path(),
        lie: false,
        unstable: false,
        on_judge: &|_, _| None,
        log: &log,
    };
    let code = landing.run(v1.clone(), Entry::Resume(Box::new(honoured.clone())));
    let said = log.into_inner().said;
    assert_eq!(code, exit::COULD_NOT_RUN, "{said:#?}");
    let line =
        format!("verify land: COULD NOT RUN — HEAD moved from {at} to {later}; {RERUN_MOVED}");
    assert!(said.contains(&line) && line.contains(FIX), "{said:#?}");
    // NEGATIVE CONTROL: at its commit, the same handoff lands.
    git(&world.work, &["reset", "-q", "--hard", &at]);
    let log = RefCell::new(Log::default());
    let landing = Landing {
        world: &world,
        scratch: scratch.path(),
        lie: false,
        unstable: false,
        on_judge: &|_, _| None,
        log: &log,
    };
    let code = landing.run(v1, Entry::Resume(Box::new(honoured)));
    let said = log.into_inner().said;
    assert_eq!(code, exit::PASS, "{said:#?}");
}

/// The receipt of one green judgment of `tree` over the cone since `base`.
fn green_judgment(tree: &str, base: &str) -> Stated {
    Stated {
        checked: vec![Checked {
            tree: tree.to_owned(),
            rows: green()
                .checks
                .iter()
                .map(|row| Row {
                    check: row.check.clone(),
                    word: row.word.clone(),
                })
                .collect(),
            base: base.to_owned(),
            secs: green().secs,
        }],
        ..Stated::default()
    }
}

/// Keep `stated` as the passed judgment of `at` over the cone since `merged` ([`Git::keep`]),
/// as a landing whose push did not land leaves it.
fn keep(world: &World, at: &str, merged: &str, stated: &Stated) {
    let git = Git {
        root: &world.work,
        make: &git_cmd,
    };
    git.keep(at, merged, stated).expect("kept");
}

/// Whether `world`'s checkout still keeps a judgment ([`KEPT`]).
fn keeps(world: &World) -> bool {
    let mut cmd = git_cmd(&world.work);
    cmd.args(["rev-parse", "-q", "--verify", KEPT]);
    aterm_uds::deadline::output_within(&mut cmd, aterm_spec::wait::MUST_HAPPEN)
        .expect("git rev-parse")
        .status
        .success()
}

/// THE KEPT JUDGMENT AND THE HANDOFF AGREE: a step handed to an image whose checkout keeps a
/// passed judgment of that step's own revision ([`KEPT`]: its commit, holding the kept tree,
/// over the main commit the kept judgment merged) is not run again: the receiver goes to the
/// push with the kept receipt, as the next run would, lands it and forgets the record.
/// NEGATIVE CONTROLS: with no record, and with a record of the same commit over another main
/// commit, the receiver runs the step and lands its own judgment.
#[test]
fn a_handed_step_whose_revision_the_kept_judgment_passed_is_not_run_again() {
    let v1 = contract(&FLAGS);
    for record in ["same", "none", "other-base"] {
        let world = world();
        let before = origin_main(&world);
        let m0 = seed_contract(&world, &v1);
        let at = commit(&world.work, "a/x.txt", "a2\n");
        let tree = git(&world.work, &["rev-parse", "HEAD^{tree}"]);
        let kept = match record {
            "same" => Some(green_judgment(&tree, &m0)),
            "other-base" => Some(green_judgment(&tree, &before)),
            _ => None,
        };
        if let Some(kept) = &kept {
            keep(&world, &at, &kept.checked[0].base, kept);
        }
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let handoff = Handoff {
            root: world.work.clone(),
            image: image_of(scratch.path(), &v1).digest,
            words: Vec::new(),
            step: aterm_land::Step::Qualify,
            hop: 1,
            at: at.clone(),
            base: m0.clone(),
            composes: 0,
            cone: m0.clone(),
            branch: None,
            stated: Stated::default(),
        };
        let log = RefCell::new(Log::default());
        let landing = Landing {
            world: &world,
            scratch: scratch.path(),
            lie: false,
            unstable: false,
            on_judge: &|_, _| None,
            log: &log,
        };
        let code = landing.run(v1.clone(), Entry::Resume(Box::new(handoff)));
        let log = log.into_inner();
        let said = &log.said;
        assert_eq!(code, exit::PASS, "{record}: {said:#?}");
        let resumed = said
            .iter()
            .any(|line| line.starts_with("verify land: resuming at the push"));
        let receipt = stated(&world, &origin_main(&world)).expect("stated");
        if record == "same" {
            assert!(resumed, "{said:#?}");
            assert!(log.runs.is_empty() && log.branches.is_empty(), "{said:#?}");
            assert_eq!(Some(receipt), kept, "the kept judgment, as it was made");
        } else {
            assert!(!resumed, "{record}: {said:#?}");
            assert_eq!(log.runs.len(), 1, "{record}: {said:#?}");
            assert_eq!(log.branches, [(v1.clone(), m0.clone())], "{record}");
            assert_eq!(receipt.checked.len(), 1, "{record}");
            assert_eq!(
                (
                    receipt.checked[0].tree.as_str(),
                    receipt.checked[0].base.as_str()
                ),
                (tree.as_str(), m0.as_str()),
                "{record}"
            );
        }
        assert!(!keeps(&world), "{record}: a landed judgment is forgotten");
    }
}

/// A LANDING RESUMED FROM ITS KEPT JUDGMENT JUDGES A MAIN THAT DOES NOT COMPOSE AS THE MERGED
/// TREE: after the judgment, main moved into the branch's file and, in the same commit, retired
/// `--input-drift` from its verifier. The rerun resumes at the push with the kept receipt, finds
/// that main does not compose, merges it and hands the re-judge to the merge's verifier: the
/// handoff carries the kept receipt (the earlier run's two interrupted judgments and its pass),
/// no branch crates (the resumed landing judged nothing) and the cone they are read over (the main commit the
/// kept judgment merged). The receiver reads the crates over that cone as itself, judges and
/// lands, stating every judgment. NEGATIVE CONTROL: without the hop the re-judge refuses the
/// starting image's words and the landing ends with the KI's line, pushing nothing.
#[test]
fn a_resumed_landing_whose_moved_main_does_not_compose_is_judged_again_as_the_merged_tree() {
    let v1 = contract(&FLAGS);
    let v2 = without(crate::cli::INPUT_DRIFT);
    for lie in [false, true] {
        let world = world();
        let before = origin_main(&world);
        let mid = rival_moves(&world, "b/y.txt", "b2\n");
        let m0 = seed_contract(&world, &v1);
        let at = edit_line(&world.work, "shared.txt", 8, "branch");
        let tree = git(&world.work, &["rev-parse", "HEAD^{tree}"]);
        let kept = Stated {
            interrupted: vec![
                Interrupted {
                    tree: tree.clone(),
                    base: before,
                    tip: mid.clone(),
                    secs: 3,
                },
                Interrupted {
                    tree: tree.clone(),
                    base: mid,
                    tip: m0.clone(),
                    secs: 2,
                },
            ],
            ..green_judgment(&tree, &m0)
        };
        keep(&world, &at, &m0, &kept);
        let moved = rival_does(&world, |rival| {
            write(rival, CONTRACT, &contract_text(&v2));
            drop(edit_line(rival, "shared.txt", 1, "main"));
        });
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let log = RefCell::new(Log::default());
        let landing = Landing {
            world: &world,
            scratch: scratch.path(),
            lie,
            unstable: false,
            on_judge: &|_, _| None,
            log: &log,
        };
        let code = landing.run(v1.clone(), Entry::Start(Vec::new()));
        let log = log.into_inner();
        let said = &log.said;
        assert_eq!(
            said.iter()
                .filter(|line| line.starts_with("verify land: resuming at the push"))
                .count(),
            1,
            "{said:#?}"
        );
        if lie {
            assert_eq!(code, exit::COULD_NOT_RUN, "{said:#?}");
            assert!(said.iter().any(|line| line == KI_LINE), "{said:#?}");
            assert_eq!(log.refusals, ["verify: unknown flag `--input-drift`"]);
            assert!(log.handoffs.is_empty());
            assert_eq!(origin_main(&world), moved, "nothing was pushed");
            continue;
        }
        assert_eq!(code, exit::PASS, "{said:#?}");
        let [handoff] = log.handoffs.as_slice() else {
            panic!("one handoff: {:#?}", log.handoffs);
        };
        assert_eq!(
            (handoff.step, &handoff.base, handoff.composes),
            (aterm_land::Step::Qualify, &moved, 0)
        );
        assert_eq!(
            (&handoff.cone, handoff.branch.as_ref(), &handoff.stated),
            (&m0, None, &kept)
        );
        assert_eq!(
            log.branches,
            [(v2.clone(), m0.clone())],
            "read by the judging image"
        );
        let [(composer, run)] = log.runs.as_slice() else {
            panic!("one child: {:#?}", log.runs);
        };
        assert_eq!(composer, &v2);
        assert!(!run.iter().any(|word| word == crate::cli::INPUT_DRIFT));
        let landing = origin_main(&world);
        let receipt = stated(&world, &landing).expect("stated");
        assert_eq!(receipt.interrupted, kept.interrupted, "{said:#?}");
        assert_eq!(receipt.checked.len(), 2, "{said:#?}");
        assert_eq!(receipt.checked[0], kept.checked[0]);
        assert_eq!(receipt.checked[1].base, moved);
        assert_eq!(receipt.composed_onto, None);
        assert!(!keeps(&world), "a landed judgment is forgotten");
    }
}

/// A STEP THE KEPT JUDGMENT STOOD FOR RAN NOWHERE HERE, SO THE NEXT STARTS AT HOP 0: a judgment
/// handed over as its last hop ([`HOPS`]) to an image whose checkout keeps a passed judgment of
/// that revision goes to the push with the kept receipt. Main has moved into the branch's file
/// and, in the same commit, retired `--input-drift` from its verifier, so the re-judge's own
/// verifier is another image, and that step hands over as its own first hop, with the kept
/// receipt, no branch crates (nothing was judged here) and the cone the kept judgment merged;
/// its receiver judges and lands, stating both judgments. A step that kept the hop
/// count it was handed would refuse to hand on "after 2 handoffs of this step": COULD NOT RUN,
/// nothing pushed.
#[test]
fn a_rejudge_after_a_handed_step_the_kept_judgment_stood_for_hands_on_from_its_first_hop() {
    let v1 = contract(&FLAGS);
    let v2 = without(crate::cli::INPUT_DRIFT);
    let world = world();
    let m0 = seed_contract(&world, &v1);
    let at = edit_line(&world.work, "shared.txt", 8, "branch");
    let tree = git(&world.work, &["rev-parse", "HEAD^{tree}"]);
    let kept = green_judgment(&tree, &m0);
    keep(&world, &at, &m0, &kept);
    let moved = rival_does(&world, |rival| {
        write(rival, CONTRACT, &contract_text(&v2));
        drop(edit_line(rival, "shared.txt", 1, "main"));
    });
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let handed = Handoff {
        root: world.work.clone(),
        image: image_of(scratch.path(), &v1).digest,
        words: Vec::new(),
        step: aterm_land::Step::Qualify,
        hop: HOPS,
        at: at.clone(),
        base: m0.clone(),
        composes: 0,
        cone: m0.clone(),
        branch: None,
        stated: Stated::default(),
    };
    let log = RefCell::new(Log::default());
    let landing = Landing {
        world: &world,
        scratch: scratch.path(),
        lie: false,
        unstable: false,
        on_judge: &|_, _| None,
        log: &log,
    };
    let code = landing.run(v1, Entry::Resume(Box::new(handed)));
    let log = log.into_inner();
    let said = &log.said;
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(
        said.iter()
            .filter(|line| line.starts_with("verify land: resuming at the push"))
            .count(),
        1,
        "{said:#?}"
    );
    let [rejudge] = log.handoffs.as_slice() else {
        panic!("one handoff: {:#?}", log.handoffs);
    };
    assert_eq!(
        (rejudge.step, rejudge.hop, &rejudge.base, rejudge.composes),
        (aterm_land::Step::Qualify, 1, &moved, 0)
    );
    assert_eq!(
        (&rejudge.cone, rejudge.branch.as_ref(), &rejudge.stated),
        (&m0, None, &kept)
    );
    assert_eq!(
        git(&world.work, &["rev-parse", &format!("{}^1", rejudge.at)]),
        at
    );
    assert_eq!(
        git(&world.work, &["rev-parse", &format!("{}^2", rejudge.at)]),
        moved
    );
    assert_eq!(
        log.branches,
        [(v2.clone(), m0.clone())],
        "read by the judging image"
    );
    let [(composer, _)] = log.runs.as_slice() else {
        panic!("one child: {:#?}", log.runs);
    };
    assert_eq!(composer, &v2);
    assert!(log.refusals.is_empty(), "{:?}", log.refusals);
    let receipt = stated(&world, &origin_main(&world)).expect("stated");
    assert_eq!(receipt.checked.len(), 2, "{said:#?}");
    assert_eq!(receipt.checked[0], kept.checked[0]);
    assert_eq!(receipt.checked[1].base, moved);
    assert!(!keeps(&world), "a landed judgment is forgotten");
}
