// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::contract;

fn grok_alab() -> &'static Row {
    contract::row_named("grok-alab").expect("rostered")
}

/// THE NEGATIVE CONTROL'S aterm (`ATERM_DEV_NO_CONTRACT3=1`): it serves no `contract=` and
/// refuses a claim naming one as the v2 grammar's usage error; unset, or any other value, it
/// serves `2,3,4` and reads every claim as before; a v2 claim is read as before either way.
#[test]
fn the_seam_answers_as_an_aterm_before_contract_three() {
    use std::ffi::OsStr;
    assert_eq!(served_with(None).as_deref(), Some("2,3,4"));
    assert_eq!(served_with(Some(OsStr::new("0"))).as_deref(), Some("2,3,4"));
    assert_eq!(served_with(Some(OsStr::new(""))).as_deref(), Some("2,3,4"));
    assert_eq!(served_with(Some(OsStr::new("1"))), None);
    let v3 = ["contract=3", "scope=relaunch,upgrade"];
    assert!(refused_before_v3(false, &v3));
    assert!(!refused_before_v3(true, &v3), "the serving aterm reads it");
    assert!(!refused_before_v3(false, &["ttl=5000"]), "a v2 lease");
    assert!(!refused_before_v3(false, &[]), "a v2 claim");
}

/// A layout in a scratch prefix holding `store/grok-alab/<build>/bin/grok-alab`, complete
/// (`<build>.ready` as atpkg writes it) when `ready`.
fn store_with_build(
    root: &Path,
    build: u64,
    ready: bool,
) -> (atpkg::store::Layout, std::path::PathBuf) {
    let layout = atpkg::store::Layout {
        prefix: root.join("pkg"),
    };
    let dir = layout.build_dir("grok-alab", build);
    std::fs::create_dir_all(dir.join("bin")).expect("build dir");
    let exe = dir.join("bin").join("grok-alab");
    std::fs::write(&exe, b"#!/bin/sh\n").expect("exe");
    if ready {
        atpkg::store::mark_build_ready(&dir).expect("ready");
    }
    (layout, exe)
}

/// THE KERNEL RULES are codex-alab's, keyed on the roster: the peer that leads the
/// foreground and runs a build of the row's program proves `grok-alab@<pid>`; any other peer,
/// foreground or image does not, and neither does a holder off the roster or codex-alab's.
#[test]
fn only_the_rostered_peer_that_leads_the_foreground_on_its_build_proves() {
    let facts = |peer, foreground, image| crate::self_claim::Facts {
        peer,
        foreground,
        alab_image: image,
    };
    assert_eq!(
        proves("grok-alab@500", facts(Some(500), 500, true)),
        Some(500)
    );
    assert_eq!(proves("grok-alab@500", facts(Some(500), 500, false)), None);
    assert_eq!(proves("grok-alab@500", facts(Some(501), 500, true)), None);
    assert_eq!(proves("grok-alab@500", facts(Some(500), 499, true)), None);
    assert_eq!(proves("grok-alab@500", facts(None, 500, true)), None);
    assert_eq!(proves("grok-alab@500", facts(Some(500), -1, true)), None);
    assert_eq!(proves("codex-alab@500", facts(Some(500), 500, true)), None);
    assert_eq!(
        proves("kimi-alab@500", facts(Some(500), 500, true)),
        Some(500)
    );
    assert_eq!(
        proves("claude-code-alab@500", facts(Some(500), 500, true)),
        Some(500)
    );
    assert_eq!(
        proves("claude-code-alab@500", facts(Some(500), 500, false)),
        None
    );
    assert_eq!(proves("claude-alab@500", facts(Some(500), 500, true)), None);
    assert_eq!(proves("other-alab@500", facts(Some(500), 500, true)), None);
    let me = std::process::id();
    let proven = prove(
        &format!("grok-alab@{me}"),
        facts(Some(me), i32::try_from(me).expect("pid"), true),
    )
    .expect("this process proves its own claim");
    assert_eq!(proven.pid, me);
    assert!(proven.alive());
}

/// THE IMAGE RULE, under either contract a seed id claims: a complete store build of the row's
/// program proves; the same file in a build that is not ready, in another program's store, or
/// anywhere else does not; a dev link's checkout proves, the link making the seed row a link's
/// (`docs/HARNESS-CONTRACT.md` §2), and its contract 3 claim keeps contract 3's rule there.
#[test]
fn only_a_complete_store_build_or_the_dev_link_of_the_rows_program_proves() {
    use contract::{SCOPED, UNIVERSAL};
    let dir = aterm_tempfile::TempDir::new().expect("dir");
    let root = std::fs::canonicalize(dir.path()).expect("canonical");
    let (layout, exe) = store_with_build(&root, 412, true);
    assert!(image_in(&layout, grok_alab(), &exe), "positive control");
    for contract in [SCOPED, UNIVERSAL] {
        assert!(
            image_for(&layout, grok_alab(), contract, &exe),
            "contract {contract}"
        );
    }

    let (unready, unready_exe) = store_with_build(&root.join("b"), 413, false);
    assert!(!image_in(&unready, grok_alab(), &unready_exe), "not ready");
    assert!(
        !image_for(&unready, grok_alab(), SCOPED, &unready_exe),
        "not ready, contract 3"
    );

    let other = layout.build_dir("grok", 7);
    std::fs::create_dir_all(other.join("bin")).expect("dir");
    let vendor = other.join("bin").join("grok");
    std::fs::write(&vendor, b"x").expect("vendor");
    atpkg::store::mark_build_ready(&other).expect("ready");
    assert!(
        !image_in(&layout, grok_alab(), &vendor),
        "the vendor's store is not the harness's"
    );

    let stray = root.join("stray-grok-alab");
    std::fs::write(&stray, b"x").expect("stray");
    assert!(
        !image_in(&layout, grok_alab(), &stray),
        "a file named like the harness"
    );
    assert!(
        !image_in(&layout, grok_alab(), &root.join("missing")),
        "nothing there"
    );

    let checkout = root.join("checkout");
    let built = checkout.join("target").join("release").join("grok-alab");
    std::fs::create_dir_all(built.parent().expect("dir")).expect("dir");
    std::fs::write(&built, b"x").expect("built");
    assert!(!image_in(&layout, grok_alab(), &built), "not linked yet");
    assert!(
        !image_for(&layout, grok_alab(), SCOPED, &built),
        "not linked yet, contract 3"
    );
    atpkg::link(
        &layout,
        "grok-alab",
        &checkout,
        &[std::path::PathBuf::from("target/release/grok-alab")],
    )
    .expect("dev link");
    assert!(
        image_in(&layout, grok_alab(), &built),
        "the dev link's checkout"
    );
    let linked = aterm_agent::harness::registry::for_layout(&layout);
    let row = linked.entry("grok-alab").expect("the seed row stands").row;
    assert_eq!(row.source, contract::Source::Linked, "a link's row now");
    assert!(
        matches!(
            contract::read_in(
                linked.rows(),
                "grok-alab@4242",
                &["contract=3", "scope=relaunch"]
            ),
            contract::Words::Scoped(contract::Claim { contract: 3, .. })
        ),
        "its contract 3 claim is read"
    );
    for contract in [SCOPED, UNIVERSAL] {
        assert!(
            image_for(&layout, row, contract, &built),
            "the dev link's checkout proves contract {contract}"
        );
        assert!(
            !image_for(&layout, row, contract, &stray),
            "still nothing outside it, contract {contract}"
        );
    }
}

/// claude-code-alab's IMAGE (`docs/DESIGN-harness-repos-2026-10-24.md` §11 D19, D53): its tool is
/// `claude-alab`, and the rule reads the package's store, never the tool's name. A complete
/// build of `claude-code-alab` proves its row; the vendor's own store, another harness's build
/// and its build read under another row do not.
#[test]
fn claude_code_alabs_image_is_its_packages_build_whatever_its_tool_is_named() {
    let row = contract::row_named("claude-code-alab").expect("rostered");
    let dir = aterm_tempfile::TempDir::new().expect("dir");
    let root = std::fs::canonicalize(dir.path()).expect("canonical");
    let layout = atpkg::store::Layout {
        prefix: root.join("pkg"),
    };
    let build = |program: &str, id: u64, tool: &str, ready: bool| {
        let dir = layout.build_dir(program, id);
        std::fs::create_dir_all(dir.join("bin")).expect("build dir");
        let exe = dir.join("bin").join(tool);
        std::fs::write(&exe, b"#!/bin/sh\n").expect("exe");
        if ready {
            atpkg::store::mark_build_ready(&dir).expect("ready");
        }
        exe
    };
    let tool = build("claude-code-alab", 14275, "claude-alab", true);
    assert!(image_in(&layout, row, &tool), "its package's build");
    assert!(
        !image_in(&layout, grok_alab(), &tool),
        "one row's build proves no other row"
    );
    let vendor = build("claude", 2_001_310, "claude", true);
    assert!(
        !image_in(&layout, row, &vendor),
        "Claude Code's own store is not the harness's"
    );
    let other = build("grok-alab", 412, "claude-alab", true);
    assert!(
        !image_in(&layout, row, &other),
        "a tool of its name in another harness's store"
    );
    let unready = build("claude-code-alab", 14276, "claude-alab", false);
    assert!(!image_in(&layout, row, &unready), "a build not ready");
}

/// A fake kernel for [`keep_child`]: each pid's parent, group and birth.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn lineages(rows: &[(i32, u32, u32, u64)]) -> impl Fn(i32) -> Option<Lineage> + '_ {
    move |pid| {
        rows.iter()
            .find(|row| row.0 == pid)
            .map(|&(pid, parent, pgid, birth)| Lineage {
                identity: aterm_uds::process::Identity {
                    pid,
                    birth: Birth(birth),
                    idversion: None,
                },
                uniqueid: birth,
                parent,
                pgid,
                launcher_version: None,
                tty: None,
            })
    }
}

/// HX-06'S KEPT CHILD (`docs/DESIGN-harness-interface-2026-11-12.md` §2.2): aterm keeps a
/// contract 4 claim's `child=` only on its own kernel reads: the child is in the claimant's
/// foreground group, descends from it within [`CHILD_WALK_MAX`] parents, reads as the same
/// process after the walk, and started when the claim says (exactly, where the birth is the
/// wall record in microseconds). NEGATIVE CONTROLS: the claimant itself, a child the kernel does
/// not read, one in another group, a claimant not in front, a stranger's process, a walk past the
/// bound, and a start one microsecond off each keep nothing.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
#[test]
fn a_claims_child_is_kept_only_on_aterms_own_reads() {
    const LEADER: u32 = 4242;
    const BIRTH: u64 = 1_763_000_000_123_456;
    let claimant = crate::self_claim::Proven {
        pid: LEADER,
        start: "fixture".to_string(),
    };
    let word = |pid: u32, start_us: u64| ChildWord { pid, start_us };
    // The leader, a shell under it, the vendor under the shell; a stranger in its own group.
    let rows = [
        (4242, 1, 4242, 1),
        (4300, 4242, 4242, 2),
        (4301, 4300, 4242, BIRTH),
        (4400, 1, 4400, BIRTH),
    ];
    let kernel = lineages(&rows);
    let fg = 4242;
    assert_eq!(
        keep_child(&claimant, fg, word(4301, BIRTH), &kernel),
        Some(KeptChild {
            pid: 4301,
            claimed_us: BIRTH,
            birth: Birth(BIRTH),
        }),
        "two exec layers down"
    );
    assert_eq!(
        keep_child(&claimant, fg, word(4301, BIRTH + 1), &kernel),
        None,
        "start off"
    );
    assert_eq!(
        keep_child(&claimant, fg, word(4242, 1), &kernel),
        None,
        "the claimant"
    );
    assert_eq!(
        keep_child(&claimant, fg, word(4999, BIRTH), &kernel),
        None,
        "unread"
    );
    assert_eq!(
        keep_child(&claimant, fg, word(4400, BIRTH), &kernel),
        None,
        "a stranger"
    );
    assert_eq!(
        keep_child(&claimant, 4400, word(4301, BIRTH), &kernel),
        None,
        "not in front"
    );
    let regrouped = [(4242, 1, 4242, 1), (4301, 4242, 4999, BIRTH)];
    assert_eq!(
        keep_child(&claimant, fg, word(4301, BIRTH), &lineages(&regrouped)),
        None,
        "another group"
    );
    let deep: Vec<(i32, u32, u32, u64)> = std::iter::once((4242, 1, 4242, 1))
        .chain((0..=CHILD_WALK_MAX).map(|n| {
            let pid = 5000 + i32::try_from(n).unwrap();
            let parent = if n == 0 {
                LEADER
            } else {
                4999 + u32::try_from(n).unwrap()
            };
            (pid, parent, 4242, BIRTH)
        }))
        .collect();
    let last = 5000 + u32::try_from(CHILD_WALK_MAX).unwrap();
    assert_eq!(
        keep_child(&claimant, fg, word(last, BIRTH), &lineages(&deep)),
        None,
        "past the walk's bound"
    );
    let near = 5000 + u32::try_from(CHILD_WALK_MAX - 1).unwrap();
    assert!(
        keep_child(&claimant, fg, word(near, BIRTH), &lineages(&deep)).is_some(),
        "at the walk's bound"
    );
}

/// Linux's start check: aterm's birth is clock ticks after boot, and a harness's
/// `btime * 1e6 + starttime * 1e6 / HZ` is kept within one second plus a tick of it.
#[test]
fn a_linux_start_is_kept_within_a_second_and_a_tick() {
    let boot_s = 1_763_000_000;
    let ticks = 12_345;
    let ours = boot_s * 1_000_000 + ticks * 10_000;
    assert!(linux_start_matches(ticks, ours, boot_s, 100));
    assert!(linux_start_matches(ticks, ours + 1_010_000, boot_s, 100));
    assert!(!linux_start_matches(ticks, ours + 1_010_001, boot_s, 100));
    assert!(linux_start_matches(ticks, ours - 1_010_000, boot_s, 100));
    assert!(
        !linux_start_matches(ticks, ours, boot_s, 0),
        "no clock rate"
    );
}

/// HX-06: A LIVE CONTRACT 4 CLAIM IS PROVEN AGAIN WHEN THE REGISTRY CHANGES
/// (`docs/DESIGN-harness-interface-2026-11-12.md` §2.2). Under the registry's rows as they read
/// now, a contract 4 claim whose row is gone, whose row now comes from elsewhere, or whose image
/// the row's rule now refuses is dropped: the claim is released and its revision rises. One whose
/// row still stands and still admits it stays at its revision. NEGATIVE CONTROLS: a contract 3
/// claim is never dropped here (an update must not cut a seed row's live claim), and before
/// HX-06's fix nothing proved a standing claim again, so a removed link's or a yanked build's
/// claim kept its grant until it ended.
#[test]
fn a_live_contract_4_claim_is_proven_again_when_the_registry_changes() {
    use aterm_agent::harness::contract::{Scope, Source, UNIVERSAL};
    let admits = |_: &Row, _: u32| true;
    let refuses = |_: &Row, _: u32| false;
    let install = |ctx: &crate::SessionCtx, contract: u32| {
        let mut scoped = crate::session_timeline::scoped_input_tests::scoped(&[Scope::Relaunch]);
        scoped.claim.contract = contract;
        crate::session_timeline::claim_supervisor_scoped(
            ctx,
            "grok-alab@4242",
            Some(71),
            scoped.clone(),
            crate::metrics::now_us(),
            aterm_slog::Actor::Ctl,
        )
        .expect("the fixture claim");
        scoped
    };
    let standing = |ctx: &crate::SessionCtx| {
        let meta = ctx.meta.lock().unwrap();
        (
            meta.live_scoped(crate::metrics::now_us()).is_some(),
            meta.supervisor_revision,
        )
    };
    // Its row still stands and admits it: it stays, at its revision.
    let ctx = crate::session_store::test_handle_on(7601, -1).ctx;
    let scoped = install(&ctx, UNIVERSAL);
    let rows = std::slice::from_ref(scoped.claim.row);
    let before = standing(&ctx);
    assert_eq!(reprove(&ctx, rows, &admits, crate::metrics::now_us()), None);
    assert_eq!(standing(&ctx), before);
    // Its row is gone, moved to another source, or refuses its image: dropped, revision raised.
    let moved = Row {
        source: if scoped.claim.row.source == Source::Linked {
            Source::Signed
        } else {
            Source::Linked
        },
        ..*scoped.claim.row
    };
    for (n, (rows, admits, why)) in [
        (
            &[][..],
            &admits as &dyn Fn(&Row, u32) -> bool,
            "its registry row is gone",
        ),
        (
            std::slice::from_ref(&moved),
            &admits as &dyn Fn(&Row, u32) -> bool,
            "its registry row now comes from elsewhere",
        ),
        (
            rows,
            &refuses as &dyn Fn(&Row, u32) -> bool,
            "its image no longer passes its row's rule",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let ctx = crate::session_store::test_handle_on(7602 + n as u64, -1).ctx;
        install(&ctx, UNIVERSAL);
        let (_, revision) = standing(&ctx);
        assert_eq!(
            reprove(&ctx, rows, admits, crate::metrics::now_us()),
            Some(why)
        );
        let (stands, moved_to) = standing(&ctx);
        assert!(!stands, "{why}: dropped");
        assert_ne!(moved_to, revision, "{why}: its revision rises");
    }
    // A contract 3 claim is left to its seed row's rule.
    let ctx = crate::session_store::test_handle_on(7606, -1).ctx;
    install(&ctx, 3);
    assert_eq!(reprove(&ctx, &[], &refuses, crate::metrics::now_us()), None);
    assert!(standing(&ctx).0);
}

/// WHAT A SCOPED CLAIM HEARS OF A RESTART (HX-05's review, finding 13): a model restart where it
/// holds `relaunch`, or `accounts` under contract 4; an account move, which names the account's
/// home, only where it was granted `accounts` under contract 4, as its reply says. NEGATIVE
/// CONTROLS: `relaunch` without `accounts`, under either contract, hears no account move (before
/// the fix it heard one, home and all); a contract 3 claim naming `accounts`, a word its grammar
/// refuses, hears neither; `approvals` alone hears nothing.
#[test]
fn only_a_claim_granted_accounts_hears_an_account_move() {
    use aterm_agent::harness::contract::{SCOPED, Scope, UNIVERSAL};
    let hears = |contract: u32, scopes: &[Scope]| {
        let mut scoped = crate::session_timeline::scoped_input_tests::scoped(scopes);
        scoped.claim.contract = contract;
        (scoped.hears_restart(), scoped.hears_account_move())
    };
    let relaunch = [Scope::Relaunch, Scope::Upgrade];
    assert_eq!(hears(SCOPED, &relaunch), (true, false));
    assert_eq!(hears(UNIVERSAL, &relaunch), (true, false));
    assert_eq!(hears(UNIVERSAL, &[Scope::Accounts]), (true, true));
    assert_eq!(
        hears(UNIVERSAL, &[Scope::Relaunch, Scope::Accounts]),
        (true, true)
    );
    assert_eq!(hears(SCOPED, &[Scope::Accounts]), (false, false));
    assert_eq!(hears(UNIVERSAL, &[Scope::Approvals]), (false, false));
}

/// A STANDING CLAIM'S RE-PROOF REUSES ITS CLAIMANT'S IMAGE VERDICT (HX-05's review, finding 1): a
/// pass is kept under what it was judged on, and a re-proof under the same key stands without
/// judging again, so a fenced input or an act verifies no signature and hashes no tree. A moved
/// part of the key judges again: the registry's generation, the executable rewritten in place,
/// another path to the same file, another holder, contract or kernel start. A claim's own proof
/// (no reuse) always judges, and its pass serves the re-proofs after it; a refusal keeps nothing,
/// so the next re-proof judges again; an executable whose identity cannot be read is judged every
/// time. NEGATIVE CONTROL: before the fix every re-proof judged in full, one count per re-proof.
#[cfg(unix)]
#[test]
fn a_re_proof_keeps_the_image_verdict_while_nothing_it_was_judged_on_moved() {
    use aterm_agent::harness::contract::{SCOPED, UNIVERSAL};
    fn who<'a>(holder: &'a str, row: &'a Row, contract: u32, start: &'a str) -> Claimant<'a> {
        Claimant {
            holder,
            row,
            contract,
            pid: 4242,
            start,
        }
    }
    let dir = aterm_tempfile::TempDir::new().expect("dir");
    let root = std::fs::canonicalize(dir.path()).expect("canonical");
    let (_layout, exe) = store_with_build(&root, 412, true);
    let row = grok_alab();
    let (holder, stranger) = (format!("{}@4242", row.id), format!("{}@4243", row.id));
    let kept = KeptImage::default();
    let judged = std::cell::Cell::new(0_usize);
    let pass = |key: Option<ImageKey>, reuse: bool, verdict: bool| {
        kept.passes(key, reuse, || {
            judged.set(judged.get() + 1);
            verdict
        })
    };
    let base = who(&holder, row, UNIVERSAL, "s1");
    let at = |c: Claimant<'_>, exe: &Path, generation: u64| ImageKey::of(&c, exe, generation);

    // The claim's proof judges and keeps its pass; the re-proofs after it judge nothing.
    assert!(pass(at(base, &exe, 1), false, true));
    assert_eq!(judged.get(), 1, "the claim's proof judges");
    for _ in 0..3 {
        assert!(pass(at(base, &exe, 1), true, true));
    }
    assert_eq!(judged.get(), 1, "the re-proofs reuse its pass");

    // Each moved part of the key judges again, and its pass is kept in turn.
    let other = root.join("other-name");
    std::fs::hard_link(&exe, &other).expect("another path to the same file");
    assert!(
        pass(at(base, &exe, 1), false, true),
        "a claim after the link, which moved the change time, keeps its pass"
    );
    let before = judged.get();
    for (n, key) in [
        at(base, &exe, 2),
        at(base, &other, 2),
        at(who(&stranger, row, UNIVERSAL, "s1"), &exe, 2),
        at(who(&holder, row, SCOPED, "s1"), &exe, 2),
        at(who(&holder, row, UNIVERSAL, "s2"), &exe, 2),
    ]
    .into_iter()
    .enumerate()
    {
        assert!(key.is_some(), "probe {n} has a key");
        assert!(pass(key.clone(), true, true));
        assert_eq!(judged.get(), before + 1 + n, "probe {n} judges");
        assert!(pass(key, true, true));
        assert_eq!(judged.get(), before + 1 + n, "probe {n}'s pass is kept");
    }
    let moved = at(base, &exe, 2);
    assert!(pass(moved.clone(), true, true));
    std::fs::write(&exe, b"#!/bin/sh\necho rewritten\n").expect("rewritten in place");
    let rewritten = at(base, &exe, 2);
    assert_ne!(rewritten, moved, "the file's identity moved");
    let before = judged.get();
    assert!(pass(rewritten, true, true));
    assert_eq!(
        judged.get(),
        before + 1,
        "a rewritten executable judges again"
    );

    // A refusal keeps nothing; an unread executable has no key and is judged each time.
    let key = at(base, &exe, 3);
    assert!(!pass(key.clone(), false, false));
    let before = judged.get();
    assert!(pass(key, true, true), "judged again after a refusal");
    assert_eq!(judged.get(), before + 1);
    let missing = at(base, &root.join("missing"), 3);
    assert!(missing.is_none(), "no identity, no key");
    let before = judged.get();
    assert!(pass(missing.clone(), true, true));
    assert!(pass(missing, true, true));
    assert_eq!(judged.get(), before + 2, "judged every time");
}
