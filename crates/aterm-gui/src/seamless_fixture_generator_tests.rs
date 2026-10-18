// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HANDOFF FIXTURE GENERATOR: what THIS release's producer writes, for every
//! desk in `REQUIRED_DESKS` and for the rendezvous transport, recorded so every
//! later release's guard (`seamless_fixture_tests.rs`) can run it through its
//! consumer (`tests/fixtures/handoff/README.md`).
//!
//! It is compiled into every `aterm-gui` library test build, beside the carry
//! tests whose helpers it reuses (`FakeApp`, `old_session`, `ENV_LOCK`,
//! `pipe_pair`, `restore_env`, `this_build`, `child_proof_from`), and calls the
//! release's own policy reader, capture ladder, history carry, writer and
//! consumer, and the guard's own rendezvous capture. An API change that breaks
//! it fails to compile, and each release's tree carries the generator that
//! matches its producer. Its two entry tests are `#[ignore]`d: they write a
//! release's bytes and run only by name.
//!
//! `targo --unverified ship release` runs them in the cut tree once the
//! release's identity is reserved, before anything is claimed, into the
//! release cache, and lands what they wrote on main after the cut
//! (`aterm-release`'s `fixtures.rs`). By hand (a release cut by an older
//! cutter; the README's fallback):
//!
//! ```text
//! ATERM_BUILD_GIT_COMMIT=<the tag's 12-digit commit> \
//! ATERM_BUILD_GIT_COMMIT_FULL=<the tag's 40-digit commit> \
//! SOURCE_DATE_EPOCH=<the release's build number in RELEASES.ledger> \
//! ATERM_HANDOFF_FIXTURE_OUT=<dir>/vX.Y.0 ATERM_RENDEZVOUS_FIXTURE_OUT=<dir>/vX.Y.0 \
//!   targo --unverified test -p aterm-gui --lib -- generate_ --ignored --test-threads 1
//! ```
//!
//! EVERY RELEASE INPUT IS DERIVED, never spelled. The three pins are the ones
//! the release build hands `crates/aterm/build.rs` (`aterm-release`'s
//! `buildplan`): the release commit the tag names and the ledger's build
//! number. [`fx_install_release_stamp`] installs the stamp the shipped binary
//! carries from them, so `producer_version`, `producer_commit` and each
//! manifest's `outgoing_build` are the release's. The version is this tree's
//! (`[workspace.package] version`), the compiler that of the release's prover
//! (`$TRUST_STAGE2_BIN/trustc -vV`, else `trustc -vV` on PATH), the successor
//! a build a day newer than the release (`fx_target_build`), at the next MINOR
//! (`fx_successor_version`), and the policy desks' `[[ranges]]` rescue names
//! the release's own build. Nothing here is edited per release: a release that
//! changes its producer's shape changes this code with it, adds a desk for the
//! shape to `REQUIRED_DESKS` with itself as the floor, and says what changed in
//! the README's section for it.
//!
//! What v0.102.0's generator (the one that wrote `v0.102.0/`, adapted by hand
//! at the tag) changed from v0.101.0's is in the README's v0.102.0 section;
//! this is that generator, with its per-release inputs derived.

use super::*;

const FIXTURE_DIR_PLACEHOLDER: &str = "@HANDOFF_DIR@";
/// The successor these desks hand to: a build a day NEWER than this release's
/// own (`SOURCE_DATE_EPOCH`, installed by [`fx_install_release_stamp`]), the
/// hop every install of this release makes. It decides the wire caps
/// (`WireCaps::for_target`: the parser and the output-state carries), that the
/// history is exported at all, the held panes' too
/// (`handoff_history::exports_for_target`), and the words the control carry
/// spells its records in (`StatusWords::for_target`).
fn fx_target_build() -> u64 {
    crate::running_build_number() + FX_SUCCESSOR_AFTER_SECS
}
/// How long after this release its successor was built (a day).
const FX_SUCCESSOR_AFTER_SECS: u64 = 86_400;
const FIXTURE_TARGET_COMMIT: &str = "fedcba987654";
/// The successor's version, as the update row names it
/// (`update_words::installing`): the next MINOR after this release's.
fn fx_successor_version() -> String {
    let version = crate::build_info::version();
    let mut parts = version.split('.').map(|part| part.parse::<u64>().ok());
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(Some(major)), Some(Some(minor)), Some(Some(_)), None) => {
            format!("{major}.{}.0", minor + 1)
        }
        _ => panic!("this release's version {version:?} is MAJOR.MINOR.PATCH"),
    }
}
/// The exit ledger's last row id each desk's store carries
/// (`SessionHandoff::roster_seq`, v0.100.0's): seven shells exited before
/// the park.
const FX_ROSTER_SEQ: u64 = 7;

fn fx_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `epoch` as the RFC3339 UTC timestamp `date -u +%Y-%m-%dT%H:%M:%SZ` prints
/// (days to the civil date by Howard Hinnant's `civil_from_days`).
fn fx_rfc3339(epoch: u64) -> String {
    let (days, secs) = (epoch / 86_400, epoch % 86_400);
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3_600,
        secs / 60 % 60,
        secs % 60
    )
}

/// THE RELEASE'S OWN BUILD STAMP, installed before anything reads provenance.
/// Provenance is the BINARY's (`crates/aterm/build.rs` emits it and the
/// binary's `main` installs it), and this library's own test binary reads a
/// fixed test stamp (build 3000000000, commit `7e57b1d0c0de`): the pins never
/// reach it at compile time. So the generator reads the three the release
/// build hands that build script (`ATERM_BUILD_GIT_COMMIT`,
/// `ATERM_BUILD_GIT_COMMIT_FULL`, `SOURCE_DATE_EPOCH`: the release commit the
/// tag names and the ledger's build number) at run time, and installs exactly
/// what the build script stamps from them: a clean tree at that commit, no
/// commits past it. The first install wins, so this runs first, and the
/// assertion at its end proves this process reads it.
///
/// The rest of `aterm_types::version::BuildStamp` is the shipped binary's too:
/// the version is this tree's `[workspace.package] version`; the compiler is the
/// release's prover ([`fx_release_compiler`]); and the profile, verification
/// state and release marker are the release lane's (`targo --unverified ship
/// release`: the `release` profile, the `--unverified` lane's `off`, the
/// cutter's `ATERM_RELEASE_BUILD`). None of the compiler, profile or
/// verification fields reaches a fixture byte.
fn fx_install_release_stamp() {
    let var = |name: &str| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| panic!("set {name} (tests/fixtures/handoff/README.md)"))
    };
    let commit = var("ATERM_BUILD_GIT_COMMIT");
    let full = var("ATERM_BUILD_GIT_COMMIT_FULL");
    let build = var("SOURCE_DATE_EPOCH");
    assert!(
        commit.len() == 12
            && full.len() == 40
            && full.starts_with(&commit)
            && full.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "the release commit's 12-digit and 40-digit spellings: {commit} {full}"
    );
    let epoch = build.parse::<u64>().expect("the release's build number");
    let compiler = fx_release_compiler();
    let leak = |text: String| -> &'static str { Box::leak(text.into_boxed_str()) };
    aterm_types::version::install(aterm_types::version::BuildStamp {
        app_version: env!("CARGO_PKG_VERSION"),
        git_commit: leak(commit.clone()),
        git_commit_full: leak(full),
        git_dirty: "false",
        build_time: leak(fx_rfc3339(epoch)),
        build_number: leak(build),
        dev_commits: "0",
        compiler_version_line: leak(compiler.version_line),
        compiler_commit: leak(compiler.commit),
        compiler_host: leak(compiler.host),
        compiler_trust_version: leak(compiler.trust_version),
        compiler_flavor: "t",
        build_profile: "release",
        trust_verify: "off",
        release_build: true,
    });
    assert_eq!(
        (
            crate::running_build_number(),
            crate::build_info::git_commit(),
            crate::build_info::version(),
        ),
        (epoch, commit.as_str(), env!("CARGO_PKG_VERSION")),
        "the release's stamp is the one this process reads"
    );
}

/// The `-vV` fields of the compiler that built the release.
struct FxCompiler {
    version_line: String,
    commit: String,
    host: String,
    trust_version: String,
}

/// THE RELEASE'S COMPILER: `trustc -vV` of the release's prover, the
/// `$TRUST_STAGE2_BIN` the cutter builds the release with and hands every
/// child it runs (the toolchain's discovery order puts it first), else the
/// `trustc` on PATH. Read as `crates/aterm/build.rs` reads it (its
/// `parse_rustc_vv`), and refused unless it is Trust: a release is built by
/// nothing else.
fn fx_release_compiler() -> FxCompiler {
    let trustc = std::env::var_os("TRUST_STAGE2_BIN")
        .filter(|bin| !bin.is_empty())
        .map_or_else(
            || std::path::PathBuf::from("trustc"),
            |bin| std::path::PathBuf::from(bin).join("trustc"),
        );
    let out = aterm_uds::spawnfd::command(&trustc)
        .arg("-vV")
        .output()
        .unwrap_or_else(|e| panic!("the release's compiler: {} -vV: {e}", trustc.display()));
    assert!(
        out.status.success(),
        "the release's compiler: {} -vV exited {}",
        trustc.display(),
        out.status
    );
    let vv = String::from_utf8(out.stdout).expect("trustc -vV prints UTF-8");
    let field = |key: &str| {
        vv.lines()
            .find_map(|line| line.strip_prefix(key))
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| panic!("{} -vV names no `{key}`: {vv}", trustc.display()))
    };
    FxCompiler {
        version_line: vv
            .lines()
            .next()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .unwrap_or_else(|| panic!("{} -vV printed nothing", trustc.display()))
            .to_string(),
        commit: field("commit-hash:"),
        host: field("host:"),
        trust_version: field("trust:"),
    }
}

struct FxSession {
    record: SessionRecord,
    term: Arc<Mutex<Terminal>>,
    turns: Arc<Mutex<TurnLedger>>,
    /// The rung this release's ladder carries this session at: `Full` for every
    /// honest desk, `StrippedLinks` for a screen holding a line record over
    /// the wire's per-record cap. The capture asserts it, and that the
    /// self-check (`settle_wire_carries`) lowers nothing.
    rung: CarryRung,
    /// How many of the lines the session's `.hist` sidecar carries for the
    /// successor to import (its first `take`) lose their links to v0.98.0's
    /// export: `0` for every desk but the one that stands for that shape.
    /// The writer asserts it.
    taken_links_stripped: usize,
}

struct FxDesk {
    name: &'static str,
    nonce_seed: u128,
    /// The carry ceiling of the successor's handoff policy this desk is
    /// parked under: `Full` for the release's own checked-in (empty) policy,
    /// `Visible` or `Repaint` for a successor whose signed policy asks for
    /// that (`fx_park_policy` lays the file out and reads it either way).
    ceiling: aterm_update_core::handoff_policy::CarryCeiling,
    window: WindowCarry,
    sessions: Vec<FxSession>,
    layout: crate::restore::RestoreManifest,
    connections: Vec<ConnectionCarry>,
    next_turn_id: Option<u64>,
    /// The panes kept open after their command exited (`--hold`): in the
    /// layout, never handed, their final screens carried beside the
    /// manifest since v0.100.0 (round five, item 19). Empty on every desk
    /// but `held-pane`.
    held: Vec<FxHeld>,
}

/// One held pane: the pool id its layout leaf names, its engine and turn
/// ledger, how its command ended as the outgoing process collected it
/// (`App::exit_status`), and (since v0.101.0, round eight's U34) whether the
/// person had looked at the failure its exit marks the tab for
/// (`StatusObserver::failure_seen`).
struct FxHeld {
    local_id: u64,
    term: Arc<Mutex<Terminal>>,
    turns: Arc<Mutex<TurnLedger>>,
    exit: Option<aterm_pty::ChildExit>,
    failure_seen: bool,
}

fn fx_record(local_id: u64, title: &str) -> SessionRecord {
    SessionRecord {
        local_id,
        sid: format!("s-{:020x}", 0x0100_0000_0000_u64 + local_id * 0x1_0101),
        parent: None,
        state: "alive".to_string(),
        title: title.to_string(),
        screen: None,
        user_title: None,
        description: None,
        icon: None,
        role: None,
        attention: None,
        questions: None,
        autopilot: None,
        land_at: None,
        control: None,
        frozen_path: false,
        identity: None,
        topics: Vec::new(),
        // Stamped by the park capture (`fx_write`, as `stamp_fg_holders`).
        fg_holder: None,
        rekey: false,
        loader: false,
        // Stamped by the history carry's join (`stamp_manifest`). Since
        // v0.98.0 the join also says whether a successor's policy withheld the
        // carry (`history_withheld`); under the shipped empty policy it never
        // does, and `false` is not written.
        history: None,
        history_dropped: 0,
        history_withheld: false,
        history_lost: 0,
        // Since v0.100.0 (round four, item 9; rounds five and seven of the
        // update audit), as `SessionStore::handoff_manifest` fills them for a
        // session nobody halted, supervised from outside or escalated:
        // no halt, no other supervisor's lease and so a claim the producer
        // vouches for, no keyed escalation, an empty outbox, and the
        // timeline's next id plus the overlap's reserve
        // (`SessionTimeline::handoff_id`). `viewport_from_bottom` is stamped
        // with the history carry, from the park's head.
        hold: None,
        more_holds: Vec::new(),
        supervisor: None,
        claim_known: true,
        attention_owners: Vec::new(),
        viewport_from_bottom: None,
        fabric: crate::fabric::FabricCarry::default(),
        timeline_id: 24 + local_id * 3 + crate::session_timeline::HANDOFF_ID_RESERVE,
        log_ids: None,
        log_seq: 0,
        // Since v0.101.0, stamped by the park (`fx_write`, as
        // `stamp_render_debts` and `stamp_journal_keys`): the rows the engine
        // owed a carried alternate screen, and the crash journal key of a
        // shell whose journal image was not this process's own. Every desk's
        // process wrote its own image, so none carries a key.
        render_debt: None,
        journal_key: None,
    }
}

fn fx_session(local_id: u64, title: &str, t: Terminal) -> FxSession {
    FxSession {
        record: fx_record(local_id, title),
        term: Arc::new(Mutex::new(t)),
        turns: Arc::new(Mutex::new(TurnLedger::default())),
        rung: CarryRung::Full,
        taken_links_stripped: 0,
    }
}

fn fx_leaf(local_id: u64, title: &str, cwd: Option<&str>) -> crate::restore::RestoredSplitTree {
    use crate::restore::{RestoredSplitTree, RestoredView, TerminalLeafRestore};
    RestoredSplitTree::leaf(RestoredView::Terminal(TerminalLeafRestore {
        sid: None,
        lineage: None,
        cwd: cwd.map(str::to_string),
        title: title.to_string(),
        profile: None,
        local_id: Some(local_id),
        user_title: None,
        description: None,
        icon: None,
        role: None,
        attention: None,
        identity: None,
        theme: None,
        questions: None,
        autopilot: None,
        land_at: None,
        // Since v0.95.0: filled only by the crash journal and a system
        // quit's layout, never by the handoff's (`capture_restore_manifest`).
        agent: None,
        supervised: None,
        // Since v0.100.0: a pane kept open after its command exited.
        held: false,
        holds: Vec::new(),
    }))
}

fn fx_split(
    axis: crate::restore::SplitKind,
    first: crate::restore::RestoredSplitTree,
    second: crate::restore::RestoredSplitTree,
) -> crate::restore::RestoredSplitTree {
    crate::restore::RestoredSplitTree::Split {
        axis,
        ratio: 0.5,
        first: Box::new(first),
        second: Box::new(second),
    }
}

/// A tab focused on its first leaf, as a live tab always names one.
fn fx_tab(root: crate::restore::RestoredSplitTree) -> crate::restore::RestoredTab {
    let mut focused_path = Vec::new();
    let mut node = &root;
    while let crate::restore::RestoredSplitTree::Split { first, .. } = node {
        focused_path.push(crate::restore::RestoreBranch::First);
        node = first;
    }
    crate::restore::RestoredTab {
        root,
        focused_path,
        zoomed: false,
    }
}

/// A restored tree's LEGACY MIRROR (`PaneLayout`), as `PaneTree::to_layout`
/// writes it from the same pane tree: the divider's orientation where the
/// restored tree names the children's (`SplitAxis::Horizontal`, children side
/// by side, is the restored `Horizontal` and the legacy `Vertical`), and the
/// tab's focused leaf marked.
fn fx_pane_mirror(
    node: &crate::restore::RestoredSplitTree,
    focused: Option<u64>,
) -> Option<crate::restore::PaneLayout> {
    use crate::restore::{PaneLayout, RestoredSplitTree, RestoredView, SplitKind};
    Some(match node {
        RestoredSplitTree::Leaf {
            view: RestoredView::Terminal(leaf),
        } => PaneLayout::Leaf {
            cwd: leaf.cwd.clone(),
            title: leaf.title.clone(),
            focused: leaf.local_id.is_some() && leaf.local_id == focused,
            local_id: leaf.local_id,
        },
        RestoredSplitTree::Leaf { .. } => return None,
        RestoredSplitTree::Split {
            axis,
            ratio,
            first,
            second,
        } => PaneLayout::Split {
            dir: match axis {
                SplitKind::Horizontal => SplitKind::Vertical,
                SplitKind::Vertical => SplitKind::Horizontal,
            },
            ratio: *ratio,
            first: Box::new(fx_pane_mirror(first, focused)?),
            second: Box::new(fx_pane_mirror(second, focused)?),
        },
    })
}

// Every desk's layout window, built by hand as `App::capture_manifest`
// writes one for a handoff (v0.100.0's generator on; through v0.98.0 it held
// `restored_tabs` only): the canonical `restored_tabs`, and beside them the legacy mirrors
// that capture fills from the same tabs — `tabs`, one `PaneLayout` per
// all-terminal tab; `native_tabs`, one descriptor per single-view native tab
// (`App::native_restore_descriptor`); and `tab_order` interleaving the two —
// and, on macOS, the window's `show` state (`window_show::captured_show`): a
// window on screen, neither full screen nor minimized, frontmost and key —
// and since v0.101.0 its raw inner size (`fx_inner_px`). A desk with a second
// window lowers that one's place in the stack.
fn fx_window(
    rows: u16,
    cols: u16,
    tabs: Vec<crate::restore::RestoredTab>,
) -> crate::restore::WindowLayout {
    fx_window_in(rows, cols, 0, tabs)
}

/// A window's raw inner size as this release's capture reads it off the window
/// (`WindowState::win_px`, written as `WindowLayout::inner_px` and, for window
/// 0, `WindowCarry::inner_px`): an unzoomed window is exactly the frame of its
/// grid and the chrome rows above it (`App::window_frame_px_for`), here the
/// message band's `band_rows`, at this build's default metrics.
fn fx_inner_px(rows: u16, cols: u16, band_rows: u16) -> Option<[u32; 2]> {
    let app = crate::App::headless_for_test();
    let size = app.frame_px(rows.saturating_add(band_rows), cols);
    Some([size.width, size.height]).filter(|[width, height]| *width > 0 && *height > 0)
}

/// [`fx_window`] for a window whose message band holds `band_rows` rows.
fn fx_window_in(
    rows: u16,
    cols: u16,
    band_rows: u16,
    tabs: Vec<crate::restore::RestoredTab>,
) -> crate::restore::WindowLayout {
    use crate::restore::{
        NativeTabRestore, RestoredSplitTree, RestoredView, TabOrderEntry, WindowShow,
    };
    let mut terminal_tabs = Vec::new();
    let mut native_tabs = Vec::new();
    let mut tab_order = Vec::new();
    for tab in &tabs {
        // The focused leaf: `fx_tab` focuses each tab's first leaf.
        let mut node = &tab.root;
        while let RestoredSplitTree::Split { first, .. } = node {
            node = first;
        }
        let focused = match node {
            RestoredSplitTree::Leaf {
                view: RestoredView::Terminal(leaf),
            } => leaf.local_id,
            _ => None,
        };
        if let Some(mirror) = fx_pane_mirror(&tab.root, focused) {
            tab_order.push(TabOrderEntry::Terminal {
                index: terminal_tabs.len(),
            });
            terminal_tabs.push(mirror);
        } else if let RestoredSplitTree::Leaf {
            view: RestoredView::Native(native),
        } = &tab.root
        {
            let descriptor = match (native.restore_tag.as_str(), &native.route) {
                ("settings", Some(route)) => NativeTabRestore::Settings {
                    route: route.clone(),
                },
                other => panic!("a native tab this generator does not spell: {other:?}"),
            };
            tab_order.push(TabOrderEntry::Native {
                index: native_tabs.len(),
            });
            native_tabs.push(descriptor);
        }
    }
    let show = if cfg!(target_os = "macos") {
        WindowShow {
            fullscreen: Some(false),
            minimized: Some(false),
            z_order: Some(0),
            key: Some(true),
        }
    } else {
        WindowShow::UNKNOWN
    };
    crate::restore::WindowLayout {
        rows,
        cols,
        active_tab: 0,
        outer_x: Some(96),
        outer_y: Some(48),
        maximized: None,
        // Since v0.101.0 (P6 finding 4): the size the successor recreates
        // the window at, and whether a presence row stood under its chrome
        // (none on these desks, which is not written).
        inner_px: fx_inner_px(rows, cols, band_rows),
        presence_row: false,
        show,
        tabs: terminal_tabs,
        native_tabs,
        tab_order,
        active_item: Some(0),
        restored_tabs: tabs,
    }
}

fn fx_window_carry(rows: u16, cols: u16, installing: bool) -> WindowCarry {
    // The unified messages: the update's flow row as the real App posts it
    // and hands it over, and (since v0.101.0) when they were captured
    // (`App::carried_messages`, which every park calls).
    let mut app = crate::App::headless_for_test();
    if installing {
        app.post_message(crate::update_words::installing(&fx_successor_version()));
    }
    let carried = app.carried_messages();
    let band_rows = u16::from(installing);
    WindowCarry {
        rows,
        cols,
        outer_x: Some(96),
        outer_y: Some(48),
        // Since v0.101.0 (`App::handoff_window_carry`): window 0's raw inner
        // size, and no presence row.
        inner_px: fx_inner_px(rows, cols, band_rows),
        presence_row: false,
        status_bar_rows: band_rows,
        // Verified an hour after the successor was built.
        update_verified_unix_ms: installing.then_some((fx_target_build() + 3_600) * 1_000),
        messages: carried.messages,
        next_message_id: carried.next_message_id,
        // Since v0.100.0 (`App::handoff_window_carry`): the font zoom, only
        // when the person zoomed (`App::handoff_font_zoom`), and what the
        // parent's health warnings said (`carried_messages`).
        font_px_milli: None,
        font_reset_px_milli: None,
        update_health_said: carried.update_health_said,
        messages_captured_unix_ms: carried.captured_unix_ms,
    }
}

/// A shell with `lines` of styled history, a cwd and a title.
fn fx_shell(rows: u16, cols: u16, lines: usize, cwd: &str, flavour: u8) -> Terminal {
    let mut t = Terminal::new(rows, cols);
    t.set_alt_archive_enabled(true);
    t.set_alt_archive_origin(0x5eed_0100 + u64::from(flavour));
    t.process(format!("\x1b]7;file://devhost{cwd}\x07\x1b]0;zsh\x07").as_bytes());
    for i in 0..lines {
        let row = match (i + usize::from(flavour)) % 9 {
            0 => format!("\x1b[1;38;5;202mdev@devhost\x1b[0m {cwd} % make test-{i}\r\n"),
            1 => {
                format!("\x1b[32mok\x1b[0m  crate::module::case_{i:04} \x1b[2m({i} ms)\x1b[0m\r\n")
            }
            2 => format!(
                "\x1b[38;2;120;200;255mtruecolor\x1b[0m \x1b[4:3m\x1b[58;5;196mcurly\x1b[0m line {i}\r\n"
            ),
            3 => format!("日本語テキスト {i} 🎉 wide\r\n"),
            4 => format!("cafe\u{0301} na\u{0308}ive {i}\r\n"),
            5 => format!(
                "\x1b]8;id=l{i};https://example.invalid/doc/{i}\x1b\\link {i}\x1b]8;;\x1b\\ tail\r\n"
            ),
            6 => format!(
                "\x1b[7mreverse\x1b[27m \x1b[3mitalic\x1b[23m \x1b[9mstrike\x1b[29m {i}\r\n"
            ),
            7 => format!("{}\r\n", "-".repeat(usize::from(cols).min(40 + i % 30))),
            _ => format!(
                "drwxr-xr-x  12 dev  staff   384 Sep 23 10:{:02} dir{i}\r\n",
                i % 60
            ),
        };
        t.process(row.as_bytes());
    }
    t.process(format!("\x1b[1;38;5;202mdev@devhost\x1b[0m {cwd} % ").as_bytes());
    t
}

/// A full-screen editor in 1049, shaped like vim.
fn fx_editor(rows: u16, cols: u16, flavour: u8) -> Terminal {
    let mut t = fx_shell(rows, cols, 40, "/home/dev/src", flavour);
    t.process(b"vim notes.md\r\n\x1b[?1049h\x1b[?1h\x1b=\x1b[H\x1b[2J");
    for r in 1..rows {
        t.process(format!("\x1b[{r};1H\x1b[34m~\x1b[0m").as_bytes());
    }
    t.process(
        format!("\x1b[1;1H# notes {flavour}\x1b[{rows};1H\x1b[7m notes.md \x1b[0m\x1b[2;1H")
            .as_bytes(),
    );
    t
}

/// Claude Code at `rows`x`cols`: 1049, a transcript, and a turn in the ledger.
fn fx_claude(local_id: u64, rows: u16, cols: u16, before: usize) -> FxSession {
    let mut app = FakeApp::new();
    let mut t = Terminal::new(rows, cols);
    t.set_alt_archive_enabled(true);
    t.set_alt_archive_origin(ORIGIN + local_id);
    for i in 0..before {
        t.process(format!("$ command {i}\r\n").as_bytes());
    }
    t.process(b"$ claude");
    t.process(b"\x1b[?1049h\x1b[?2004h\x1b[>1u");
    t.process(&app.frame(rows, cols));
    let mut ledger = TurnLedger::default();
    let arch = ArchMark::of(&t);
    app.say(&mut t, [format!("\u{276f} {TURN_TEXT}")]);
    app.say(&mut t, numbered(1..=24));
    ledger.push(TurnRecord {
        id: 41,
        started_ms: 100,
        dur_ms: 2000,
        submitted: true,
        status: "settled",
        text: TURN_TEXT.to_string(),
        screen_hash: 2,
        seq: t.content_seq(),
        arch,
        carried: false,
    });
    FxSession {
        record: fx_record(local_id, "claude"),
        term: Arc::new(Mutex::new(t)),
        turns: Arc::new(Mutex::new(ledger)),
        rung: CarryRung::Full,
        taken_links_stripped: 0,
    }
}

/// THE PARK'S POLICY, AS AN INSTALL OF THIS RELEASE READS IT (plan P0-5,
/// 75f60b08c, eb807651f). The successor's `publish/handoff-policy.toml` is
/// copied byte for byte into its bundle as `BUNDLE_PATH` by the cut; the
/// producer's pre-verification reads it from there
/// (`handoff_policy::read_from_bundle`, after the codesign check) and keeps
/// what it must follow (`handoff_policy::adopt`), which every park then reads
/// by the attempt's key (`App::park_policy`). Releases ship the file EMPTY
/// (`schema = 1`), so there is nothing to follow and the park captures at
/// the `Full` ceiling: the ladder as it ships. This lays the checked-in file
/// out as the cut does and runs that reader, rather than assuming its answer.
///
/// THE POLICY DESKS (`want` below `Full`): the successor is a release whose
/// cut sealed a rescue asking this producer for that ceiling. Since
/// `[[ranges]]` (5afd825d6) a cut seals one only as a range naming the
/// producers it is for, with its reason, and the top-level keys as the hull
/// every build before `[[ranges]]` follows alone (`PolicyFile::parse_for_cut`,
/// which this asserts); this release reads the range. The same reader and `adopt`
/// run over that file, so the ceiling the park captures under is the
/// producer's own reading of it.
fn fx_park_policy(
    scratch: &std::path::Path,
    want: aterm_update_core::handoff_policy::CarryCeiling,
) -> (
    crate::app_update_handoff::ParkPolicy,
    Option<aterm_update_core::handoff_policy::HandoffPolicy>,
) {
    use aterm_update_core::handoff_policy::{self as policy, CarryCeiling, PolicyFile, PolicyRead};
    let build = crate::running_build_number();
    let file = if want == CarryCeiling::Full {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(policy::SOURCE_PATH),
        )
        .expect("the release's checked-in handoff policy")
    } else {
        format!(
            "schema = 1\napplies_to_producers = [{build}, {build}]\ncarry = \"{carry}\"\n\n\
             [[ranges]]\nproducers = [{build}, {build}]\ncarry = \"{carry}\"\n\
             reason = \"the successor asks build {build} for its {carry} screens\"\n",
            carry = want.as_str()
        )
        .into_bytes()
    };
    // What the cutter seals: a file it would refuse is no release's policy.
    if let Err(why) = PolicyFile::parse_for_cut(std::str::from_utf8(&file).unwrap()) {
        panic!("a cut seals this successor's policy: {why}");
    }
    let app_root = scratch.join("successor.app");
    let in_bundle = app_root.join(policy::BUNDLE_PATH);
    std::fs::create_dir_all(in_bundle.parent().unwrap()).unwrap();
    std::fs::write(&in_bundle, &file).unwrap();
    let read = policy::read_from_bundle(&app_root);
    let PolicyRead::Parsed(parsed) = &read else {
        panic!("the successor's policy file reads: {read:?}");
    };
    let mine = parsed.for_build(build);
    let adopted = policy::adopt(
        &read,
        build,
        &format!("fixture successor build {}", fx_target_build()),
    );
    if want == CarryCeiling::Full {
        assert!(
            !parsed.legacy.asks_anything() && mine.is_none_or(|policy| !policy.asks_anything()),
            "releases ship the handoff policy empty: {parsed:?}"
        );
        assert_eq!(adopted, None, "an empty policy is no policy to follow");
    } else {
        assert!(mine.is_some(), "the rescue names build {build}");
        assert_eq!(
            adopted, mine,
            "the producer follows the rescue its successor's policy names it in, asking for \
             {want:?}"
        );
    }
    let park = crate::app_update_handoff::ParkPolicy {
        policy: adopted,
        known: true,
        // The pre-verification that read the policy also read the
        // candidate's `Info.plist`; a successor newer than v0.100.0 declares
        // the chunked rendezvous grant. Only the launched lane reads it.
        grant_chunks: true,
    };
    assert_eq!(
        park.carry_ceiling(),
        want,
        "the park captures under the ceiling the successor's policy names"
    );
    assert!(!park.relaxes_land_gate());
    (park, adopted)
}

/// What this release's park capture hands the worker for one desk.
struct FxCapture {
    screens: Vec<(u64, TerminalCheckpoint)>,
    repaint: Vec<u64>,
    sources: Vec<crate::handoff_carry::CarrySource>,
    heads: Vec<crate::handoff_history::HistoryHead>,
    screen_digest: [u8; 32],
    /// The sessions whose parser the capture found mid-sequence, the state it
    /// was in, and whether the carry holds that partial state (v0.100.0's
    /// parser carry) or leaves it out as CAN would (a hooked DCS, which no
    /// desk here stands for).
    abandoned: Vec<(u64, &'static str, bool)>,
    /// The rung each session was carried at, and how many of its lines lost
    /// their links (`StrippedLinks`).
    rungs: Vec<(u64, CarryRung, usize)>,
    /// The rows the engine owed each carried alternate screen (v0.101.0's
    /// `ParkedScreens::render_debts`), only where not zero.
    render_debts: Vec<(u64, (i32, i32))>,
    /// The held panes' screens (`App::capture_held_screens`), captured last,
    /// priced from what the handed screens left of the aggregate cells; their
    /// control carries are in `sources`.
    held: Vec<HeldScreen>,
}

/// This release's capture (`App::capture_parked_screens`) over a desk whose time
/// is ample, on the fork lane, under the park's policy `ceiling`: the pool walked in
/// SESSION-ID order (eb807651f), its mandatory visible grids reserved first,
/// then each session carried by `carry_for_wire_within` with 256 lines of
/// history (none under a ceiling that carries no scrollback) when the cell
/// and byte budgets admit it after that reserve, its history head and (at a
/// rung that keeps it) its control carry's head taken under the same lock;
/// then the pool committed by `settle_wire_carries`, and last the held panes
/// (`fx_capture_held`). A parser left inside a sequence no carry holds, or one
/// the carry would only swallow that is not a picture, is carried only over a
/// quiet PTY (`handoff_masters_have_activity` on the session's own entry in
/// `live`); over queued output this release's park misses instead
/// (`CaptureFailure::MidSequence`), which no desk here stands for. The repaint
/// set is the sessions whose rung `needs_repaint`.
fn fx_capture(
    sessions: &[FxSession],
    held: &[FxHeld],
    live: &[(u64, i32, i32)],
    caps: WireCaps,
    ceiling: aterm_update_core::handoff_policy::CarryCeiling,
) -> FxCapture {
    let mut order: Vec<&FxSession> = sessions.iter().collect();
    order.sort_unstable_by_key(|s| s.record.local_id);
    let mut cells_reserve = 0_u64;
    let mut bytes_reserve = 0_u64;
    for s in &order {
        let guard = s.term.lock().unwrap();
        let (rows, cols) = (guard.rows(), guard.cols());
        cells_reserve = cells_reserve.saturating_add(mandatory_checkpoint_cells(rows, cols));
        bytes_reserve = bytes_reserve
            .saturating_add(checkpoint_capture_budget_bytes(rows, cols, 0).unwrap_or(u64::MAX));
    }
    let mut wire = Vec::new();
    let mut sources = Vec::new();
    let mut heads = Vec::new();
    let mut abandoned = Vec::new();
    let mut rungs = Vec::new();
    let mut render_debts = Vec::new();
    let mut capture_cells = 0_u64;
    let mut capture_budget = 0_u64;
    let mut history_latched_off = false;
    for s in &order {
        let id = s.record.local_id;
        let guard = s.term.lock().unwrap();
        // THE PARSER AND THE OUTPUT STATE RIDE THE CHECKPOINT. A partial
        // sequence rides it and the successor continues it (v0.100.0's
        // parser carry), and since v0.101.0 so does the engine's in-flight
        // output state (`CheckpointMeta::output_state`, round eight's finding
        // 502: the REP character, an open OSC 8 link, a VT52 address in
        // progress, the XTPUSHSGR stack...), so to a successor that
        // `carries_output_state` neither an open link nor a VT52 address is a
        // hazard. A miss over queued output is left for a sequence no carry
        // holds (a hooked DCS) and for one the carry would only swallow
        // (`partial_sequence_swallowed`: an over-cap OSC) that is not a
        // PICTURE (`partial_sequence_swallows_picture`:
        // a kitty-graphics APC, an OSC 1337 image or a sixel string, whose
        // stream's next frame replaces the one dropped). That is
        // `capture_parked_screens` on the fork lane, which has no free
        // mid-sequence re-park (`soft_miss_may_repark` is false). The park
        // carries those only over a quiet PTY.
        assert!(
            caps.carries_parser() && caps.carries_output_state(),
            "the hop these desks stand for carries the parser and the output state"
        );
        let hazard = guard.partial_sequence_uncarried().or_else(|| {
            guard
                .partial_sequence_swallowed()
                .filter(|_| !guard.partial_sequence_swallows_picture())
        });
        if let Some(state) = hazard {
            let entry = live
                .iter()
                .find(|(local_id, _, _)| *local_id == id)
                .expect("every session has a PTY");
            assert!(
                !crate::app_update_handoff::handoff_masters_have_activity(std::slice::from_ref(
                    entry
                )),
                "session {id}: this release carries {state} only over a quiet PTY"
            );
        }
        if let Some(state) = guard.partial_sequence_state() {
            abandoned.push((id, state, guard.partial_sequence_uncarried().is_none()));
        }
        let (rows, cols) = (guard.rows(), guard.cols());
        let own_cells = mandatory_checkpoint_cells(rows, cols);
        let own_bytes = checkpoint_capture_budget_bytes(rows, cols, 0).unwrap_or(u64::MAX);
        cells_reserve = cells_reserve.saturating_sub(own_cells);
        bytes_reserve = bytes_reserve.saturating_sub(own_bytes);
        let carry = if ceiling.carries_scrollback() {
            max_handoff_history_lines()
        } else {
            0
        };
        let history_cells = u64::from(cols).saturating_mul(u64::from(carry));
        let history_bytes = checkpoint_capture_budget_bytes(rows, cols, carry)
            .map_or(u64::MAX, |carried| carried.saturating_sub(own_bytes));
        let fits_cells = crate::app_update_handoff::optional_carry_fits(
            capture_cells,
            own_cells,
            history_cells,
            cells_reserve,
            max_handoff_aggregate_grid_cells(),
        );
        let fits_bytes = crate::app_update_handoff::optional_carry_fits(
            capture_budget,
            own_bytes,
            history_bytes,
            bytes_reserve,
            crate::app_update_handoff::MAX_HANDOFF_CAPTURE_BUDGET_BYTES,
        );
        let history = if !history_latched_off && fits_cells && fits_bytes {
            carry
        } else {
            0
        };
        assert!(
            checkpoint_capture_budget_bytes(rows, cols, 0)
                .and_then(|bytes| capture_budget.checked_add(bytes))
                .is_some_and(
                    |total| total <= crate::app_update_handoff::MAX_HANDOFF_CAPTURE_BUDGET_BYTES
                ),
            "session {id}: inside the decode-authority budget"
        );
        let (checkpoint, rung, cause) =
            carry_for_wire_within(&guard, id, history, &mut capture_cells, caps, ceiling);
        assert_eq!(
            rung, s.rung,
            "session {id}: carried at the rung this desk stands for ({cause:?})"
        );
        let mut stripped = 0;
        if rung == CarryRung::StrippedLinks {
            // Only the links of the line records over the wire's cap are
            // gone, at the whole depth the budget asked for: every scalar,
            // every other line and every stripped line's text, attributes and
            // wrap flag are the engine's own exact carry at that depth.
            let exact = guard
                .checkpoint_carry(history as usize)
                .expect("a link-dense desk is at Ground");
            assert_eq!(
                checkpoint.history_lines, exact.history_lines,
                "session {id}: the whole carried scrollback crosses"
            );
            stripped = super::ladder_tests::assert_only_over_cap_links_dropped(
                &format!("session {id}"),
                &exact,
                &checkpoint,
            );
            assert!(stripped > 0, "session {id}: a line lost its links");
        }
        rungs.push((id, rung, stripped));
        if history != 0 && rung == CarryRung::VisibleOnly {
            history_latched_off = true;
        }
        // THE RENDER DEBT (v0.101.0, the render stream's R08), under the lock
        // the checkpoint was taken under: what the engine owes the alternate
        // screen of a session whose screen is carried (not blank on purpose).
        if rung != CarryRung::Repaint && guard.modes().alternate_screen {
            let debt = guard.render_debt();
            if debt != (0, 0) {
                render_debts.push((id, debt));
            }
        }
        capture_budget = checkpoint_capture_budget_bytes(
            checkpoint.rows,
            checkpoint.cols,
            checkpoint.history_lines,
        )
        .map_or(u64::MAX, |bytes| capture_budget.saturating_add(bytes));
        heads.push(crate::handoff_history::capture_head(id, &guard));
        // Since v0.100.0 every session's control carry travels (round seven,
        // finding 24): a rung that does not keep the screen it describes goes
        // without the DIFFER'S STATE only (`CarryRung::keeps_differ_state`).
        let mut source = handoff_carry::capture_head(
            id,
            &guard,
            &s.term,
            &s.turns,
            None,
            rung.keeps_differ_state(),
        );
        if !rung.keeps_differ_state() {
            source.without_differ();
        }
        sources.push(source);
        drop(guard);
        wire.push(WireCarry {
            local_id: id,
            checkpoint,
            rung,
        });
    }
    let screen_digest =
        settle_wire_carries(&mut wire, caps).expect("this release's park commits this desk");
    for carry in &wire {
        let (_, rung, _) = rungs
            .iter()
            .find(|(id, _, _)| *id == carry.local_id)
            .unwrap();
        assert_eq!(
            carry.rung, *rung,
            "session {}: the self-check lowers nothing",
            carry.local_id
        );
    }
    sources.retain_mut(|source: &mut crate::handoff_carry::CarrySource| {
        let Some(carry) = wire
            .iter()
            .find(|carry| carry.local_id == source.local_id())
        else {
            return false;
        };
        if !carry.rung.keeps_differ_state() {
            source.without_differ();
        }
        true
    });
    let mut screens = Vec::new();
    let mut repaint = Vec::new();
    for carry in wire {
        if carry.rung.needs_repaint() {
            repaint.push(carry.local_id);
        }
        screens.push((carry.local_id, carry.checkpoint));
    }
    assert!(
        repaint.is_empty() || ceiling == aterm_update_core::handoff_policy::CarryCeiling::Repaint,
        "only a successor's `carry = \"repaint\"` policy repaints a session here"
    );
    // THE HELD PANES, last (`capture_parked_screens` hands them what the
    // handed screens left of the aggregate cells, and their control carries
    // join the handed sessions').
    let held = fx_capture_held(held, &mut capture_cells, caps, ceiling, &mut sources);
    FxCapture {
        screens,
        repaint,
        sources,
        heads,
        screen_digest,
        abandoned,
        rungs,
        render_debts,
        held,
    }
}

/// This release's held capture (`App::capture_held_screens`, round five's item
/// 19): every pooled pane NOT handed, in id order, its visible screen at the
/// most faithful rung the successor's caps and the policy's ceiling admit,
/// without scrollback, priced from what the handed screens left of the
/// aggregate cells. Since v0.101.0 each also carries its session id (round
/// eight's finding 21), its scrollback's line count and fence, which its
/// export is joined against when the manifest is written (finding 39),
/// whether the person had seen its failure (U34), and, pushed onto `carries`
/// under the same lock, its control carry's head without the differ's state
/// (finding 730). A pane whose screen would go blank for a repaint no program
/// can answer is left out, as is one whose meta `held_screen_meta` refuses or
/// one past the held caps; no desk here stands for any of them, so the
/// capture asserts every held pane is kept.
fn fx_capture_held(
    held: &[FxHeld],
    capture_cells: &mut u64,
    caps: WireCaps,
    ceiling: aterm_update_core::handoff_policy::CarryCeiling,
    carries: &mut Vec<crate::handoff_carry::CarrySource>,
) -> Vec<HeldScreen> {
    let mut order: Vec<&FxHeld> = held.iter().collect();
    order.sort_unstable_by_key(|pane| pane.local_id);
    let mut screens = Vec::new();
    let mut held_bytes = 0_u64;
    for pane in order {
        assert!(screens.len() < MAX_HELD_PANES);
        let terminal = pane.term.lock().unwrap();
        let mut cells = *capture_cells;
        let (checkpoint, rung, cause) =
            carry_for_wire_within(&terminal, pane.local_id, 0, &mut cells, caps, ceiling);
        let history_dropped = u64::try_from(terminal.main_grid().scrollback_lines()).unwrap();
        let history_head = Some(terminal.history_fence());
        let mut control = handoff_carry::capture_head(
            pane.local_id,
            &terminal,
            &pane.term,
            &pane.turns,
            None,
            false,
        );
        control.without_differ();
        drop(terminal);
        assert!(
            !rung.needs_repaint(),
            "held pane {}: carried, not blanked ({cause:?})",
            pane.local_id
        );
        if let Err(why) = crate::seamless::held_screen_meta(&checkpoint) {
            panic!("held pane {}: carried ({why})", pane.local_id);
        }
        let screen = HeldScreen {
            local_id: pane.local_id,
            sid: aterm_session::SessionId::new(fx_record(pane.local_id, "").sid),
            checkpoint,
            exit: pane.exit,
            history_dropped,
            history_head,
            failure_seen: pane.failure_seen,
        };
        let bytes = screen.grid_bytes();
        assert!(
            held_bytes.saturating_add(bytes) <= MAX_HELD_AGGREGATE_GRID_BYTES,
            "held pane {}: inside the held budget",
            pane.local_id
        );
        held_bytes = held_bytes.saturating_add(bytes);
        *capture_cells = cells;
        screens.push(screen);
        carries.push(control);
    }
    screens
}

/// Every line of `t`'s history-holding grid (the saved primary under an
/// alternate screen), as text.
fn fx_history_text(t: &Terminal) -> Vec<String> {
    let grid = t.main_grid();
    (0..grid.scrollback_lines())
        .map(|i| {
            grid.get_history_line(i)
                .map(|l| l.to_string().trim_end().to_string())
                .unwrap_or_default()
        })
        .collect()
}

/// Every line of `t`'s history-holding grid (the saved primary under an
/// alternate screen), oldest first.
fn fx_history_lines(t: &Terminal) -> Vec<aterm_core::scrollback::Line> {
    let grid = t.main_grid();
    (0..grid.scrollback_lines())
        .map(|i| {
            grid.get_history_line(i)
                .expect("a history line reads back")
                .into_owned()
        })
        .collect()
}

/// `lines` as v0.98.0 carries them at `cols`: a line whose record is over the
/// wire's per-record cap (`line_record_cap`, the arithmetic of both the screen
/// carry's link-stripping rung and the history export's `line_caps`) loses its
/// OSC 8 links, and every other line is exact. Returns the lines and how many
/// lost their links.
fn fx_carried_lines(
    lines: &[aterm_core::scrollback::Line],
    cols: u16,
) -> (Vec<aterm_core::scrollback::Line>, usize) {
    let cap = line_record_cap(cols);
    let mut stripped = 0;
    let carried = lines
        .iter()
        .map(|line| {
            let mut line = line.clone();
            if line.has_hyperlinks() && line.serialize().len() > cap {
                line.clear_hyperlinks();
                stripped += 1;
            }
            line
        })
        .collect();
    (carried, stripped)
}

/// `got` is `want` line for line: as text first (so a failure names the lines
/// that differ), then as each line's own record, every cell's style, link and
/// width.
fn fx_assert_same_lines(
    got: &[aterm_core::scrollback::Line],
    want: &[aterm_core::scrollback::Line],
    what: &str,
) {
    let text = |lines: &[aterm_core::scrollback::Line]| {
        lines
            .iter()
            .map(|line| line.to_string().trim_end().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(text(got), text(want), "{what}");
    for (index, (got, want)) in got.iter().zip(want).enumerate() {
        assert!(
            got.serialize() == want.serialize(),
            "{what}: line {index}, cell for cell"
        );
    }
}

/// Every line of a v0.98.0 `.hist` sidecar, read off its bytes by the line
/// codec alone: `ATHIST1\n`, the width (`u16` LE), two zero bytes, then frames
/// of a `u32` LE length and a line-codec block. Each frame must decode under
/// the STRICT per-line bounds the import reads it under (`cols * 256` content
/// bytes, `line_record_cap(cols)` a record): v0.98.0's export drops an over-cap
/// line's links before it writes, so no record in it is over the cap.
fn fx_sidecar_lines(label: &str, bytes: &[u8]) -> (u16, Vec<aterm_core::scrollback::Line>) {
    let (head, mut frames) = bytes.split_at_checked(12).expect("a sidecar header");
    assert_eq!(&head[..8], b"ATHIST1\n", "{label}: a history sidecar");
    let cols = u16::from_le_bytes([head[8], head[9]]);
    assert_eq!(&head[10..], [0, 0], "{label}: the reserved header bytes");
    let mut lines = Vec::new();
    while let Some((size, rest)) = frames.split_first_chunk::<4>() {
        let (frame, rest) = rest
            .split_at_checked(u32::from_le_bytes(*size) as usize)
            .expect("a whole frame");
        let decoded = aterm_core::scrollback::deserialize_lines_strict(
            frame,
            crate::handoff_history::CHUNK_LINES,
            usize::from(cols),
            usize::from(cols) * 256,
            line_record_cap(cols),
        )
        .unwrap_or_else(|| panic!("{label}: every frame decodes under the strict caps"));
        lines.extend(decoded);
        frames = rest;
    }
    assert!(frames.is_empty(), "{label}: nothing after the last frame");
    (cols, lines)
}

/// The text of absolute row `abs` of `t`'s history-holding grid (the saved
/// primary under an alternate screen), history or visible; `None` once the
/// row is evicted or past the screen.
fn fx_text_at(t: &Terminal, abs: u64) -> Option<String> {
    let grid = t.main_grid();
    let oldest = grid.oldest_absolute_row();
    let top = oldest + u64::try_from(grid.scrollback_lines()).unwrap();
    if abs < oldest {
        return None;
    }
    if abs < top {
        let index = usize::try_from(abs - oldest).unwrap();
        return grid
            .get_history_line(index)
            .map(|l| l.to_string().trim_end().to_string());
    }
    let row = u16::try_from(abs - top).ok()?;
    grid.row_text(row).map(|text| text.trim_end().to_string())
}

fn fx_write(desk: &FxDesk, scratch_root: &std::path::Path) {
    let scratch = scratch_root.join(desk.name);
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    aterm_log::env::set("XDG_RUNTIME_DIR", &scratch);
    aterm_log::env::set("HOME", &scratch);
    let dir = crate::control_auth::socket_dir().expect("control dir");
    std::fs::create_dir_all(&dir).unwrap();

    // THE SUCCESSOR'S HANDOFF POLICY, read as every park of this release reads it
    // (`App::park_policy`): the ceiling this park captures under.
    let (park, adopted) = fx_park_policy(&scratch, desk.ceiling);
    let ceiling = park.carry_ceiling();
    // Real PTYs, before the capture: this release's capture polls a mid-sequence
    // session's master, and the self-check below adopts them with this
    // release's own consumer, which refuses a master that is not a tty.
    let mut masters = Vec::new();
    let mut slaves = Vec::new();
    for _ in &desk.sessions {
        let (mut master, mut slave) = (0i32, 0i32);
        // SAFETY: valid out-params; openpty fills them on success.
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, 0, "openpty");
        masters.push(master);
        slaves.push(slave);
    }
    let fds = HandoffFds {
        entries: desk
            .sessions
            .iter()
            .enumerate()
            .map(|(i, s)| {
                (
                    s.record.local_id,
                    masters[i],
                    40_000 + i32::try_from(i).unwrap(),
                )
            })
            .collect(),
    };
    let caps = WireCaps::for_target(fx_target_build());
    let capture = fx_capture(&desk.sessions, &desk.held, &fds.entries, caps, ceiling);
    let screens = &capture.screens;
    // THE FOREGROUND HOLDER the park reads for every session and stamps on
    // its record (`stamp_fg_holders`): the shell itself at a prompt, else the
    // job it runs.
    let mut records: Vec<SessionRecord> = desk.sessions.iter().map(|s| s.record.clone()).collect();
    for (i, record) in records.iter_mut().enumerate() {
        let index = i32::try_from(i).unwrap();
        let holder = if record.title == "zsh" {
            40_000 + index
        } else {
            41_000 + index
        };
        record.fg_holder = Some(holder).filter(|holder| *holder > 0);
        // THE RENDER DEBT (v0.101.0, `stamp_render_debts`): `"<shift>,<pushed>"`
        // for a session whose carried alternate screen the engine owed rows,
        // and nothing for any other. No journal key (`stamp_journal_keys`):
        // this process's journal holds its own image.
        record.render_debt = capture
            .render_debts
            .iter()
            .find(|(local_id, _)| *local_id == record.local_id)
            .map(|(_, (shift, pushed))| crate::session_store::render_debt_row(*shift, *pushed));
        record.journal_key = None;
    }
    // Each `(src, dst, op)` once, sorted, as `SessionHandoff::from_store` has
    // drawn them since v0.101.0 (round eight's U23).
    let mut connections = desk.connections.clone();
    connections.sort_by(|a, b| (&a.src, &a.dst, &a.op).cmp(&(&b.src, &b.dst, &b.op)));
    connections.dedup();
    assert_eq!(
        connections, desk.connections,
        "{}: the desk's edges are each drawn once",
        desk.name
    );
    let mut manifest = SessionHandoff {
        schema: SessionHandoff::SCHEMA,
        sessions: records,
        window: None,
        connections,
        // The worker writes the turn-id count below (`manifest_turn_id`).
        next_turn_id: None,
        outgoing_build: Some(crate::running_build_number()),
        // Since v0.100.0 (`SessionHandoff::from_store`): the held panes are
        // named by the writer (`write_outgoing_with_held`); the exit
        // ledger's last row id, so the successor counts on above it; and no
        // bridge `fabric attach` armed beyond the configured one.
        held: Vec::new(),
        roster_seq: Some(FX_ROSTER_SEQ),
        fabric_attached: Vec::new(),
        mirror_dropped: Vec::new(),
        mirror_served: false,
    };
    // THE LAYOUT, captured after the screens and committed at the park
    // (`App::capture_handoff_layout`, then `commit_handoff_layout`). Since
    // v0.101.0 the capture folds a desk past the layout's caps into what one
    // layout holds (`RestoreManifest::fold_into_caps`, round eight's D5); a
    // desk within them is left exactly as it was.
    let mut layout = desk.layout.clone();
    layout.fold_into_caps();
    assert_eq!(
        layout, desk.layout,
        "{}: a desk within the caps is not folded",
        desk.name
    );
    let committed_layout = crate::app_update_handoff::commit_handoff_layout(&layout)
        .unwrap_or_else(|refusal| {
            panic!("{}: the park commits its layout: {refusal:?}", desk.name)
        });
    // The fork lane's history plan, under the ceiling the park was taken
    // under (`HistoryPlan::deferred_under`): every pooled session, exported
    // by the worker once the capture is done (`App::history_sessions`, which
    // exports only to a successor at least as new as this build). Since
    // v0.101.0 the held panes' scrollback is exported too, after the handed
    // sessions' (round eight's finding 39).
    assert!(
        crate::handoff_history::exports_for_target(
            crate::running_build_number(),
            fx_target_build()
        ),
        "this release exports history to its successor"
    );
    let plan = crate::handoff_history::HistoryPlan::deferred_under(ceiling, || {
        let mut sessions = crate::handoff_history::ExportSessions::from(
            desk.sessions
                .iter()
                .map(|s| (s.record.local_id, Arc::clone(&s.term)))
                .collect::<Vec<_>>(),
        );
        sessions.held = desk
            .held
            .iter()
            .map(|pane| (pane.local_id, Arc::clone(&pane.term)))
            .collect();
        sessions.handed.sort_unstable_by_key(|(id, _)| *id);
        sessions.held.sort_unstable_by_key(|(id, _)| *id);
        Some(sessions)
    });
    if ceiling.carries_scrollback() {
        assert!(
            matches!(plan, crate::handoff_history::HistoryPlan::Deferred(_)),
            "{}: under the Full ceiling the fork lane exports every history ({plan:?})",
            desk.name
        );
    } else {
        assert!(
            matches!(plan, crate::handoff_history::HistoryPlan::Withheld),
            "{}: under a ceiling that carries no scrollback the plan withholds it ({plan:?})",
            desk.name
        );
    }
    // The worker's first step on the fork lane: it verifies the candidate,
    // reads its policy and refuses a capture taken above it
    // (`capture_exceeds_policy`). The park read the same policy, so the
    // capture is never above it.
    assert!(
        crate::app_update_handoff::capture_exceeds_policy(Some(ceiling), adopted).is_none(),
        "{}: the capture was taken under the policy the worker reads",
        desk.name
    );
    let nonce = format!("{:032x}", desk.nonce_seed);
    // The worker, as `prepare_outgoing_artifacts` runs it on the fork lane:
    // the layout's round trip (it refuses a layout that is empty or does not
    // parse back to itself, or whose wire is not the committed digest), the
    // control carry's export, then the history carry's (exported now with
    // every reader parked), the held panes' exports taken out of it, and the
    // handed sessions' join, then the writer, then the layout sidecar.
    let layout_wire = layout
        .to_toml()
        .ok()
        .filter(|wire| {
            !layout.is_empty()
                && crate::restore::RestoreManifest::from_toml(wire).as_ref() == Some(&layout)
        })
        .unwrap_or_else(|| panic!("{}: the worker persists this layout", desk.name));
    assert_eq!(
        layout_wire_digest(&layout_wire),
        Some(committed_layout),
        "{}: the layout's wire is the committed one",
        desk.name
    );
    // THE TURN-ID COUNT (round seven's finding 22, round eight's 40 and 714):
    // the ids this process minted (the desk's ledger) plus the reserve for
    // the turns it still serves until its `_exit` (`turn_ids_for_handoff`,
    // spelled out so the count is this desk's process's and not the test
    // process's); the export spells its records in the successor's words.
    let minted = desk.next_turn_id.unwrap_or(0);
    let turn_id_ceiling = minted
        .saturating_add(crate::control::TURN_ID_RESERVE)
        .min(crate::handoff_carry::MAX_TURN_ID);
    let controls = handoff_carry::export_for(
        &capture.sources,
        turn_id_ceiling,
        handoff_carry::StatusWords::for_target(fx_target_build()),
    );
    manifest.next_turn_id = handoff_carry::manifest_turn_id(turn_id_ceiling);
    let mut results = plan.results(&dir);
    let held_history = results.take_held(
        &capture
            .held
            .iter()
            .map(|pane| pane.local_id)
            .collect::<Vec<_>>(),
        !crate::handoff_history::exports_for_target(
            crate::running_build_number(),
            fx_target_build(),
        ),
    );
    let verdicts = crate::handoff_history::stamp_manifest(
        &mut manifest,
        screens,
        &capture.heads,
        results,
        &dir,
        &nonce,
    );
    assert_eq!(verdicts.len(), desk.sessions.len(), "{}", desk.name);
    for (local_id, joined) in &verdicts {
        if ceiling.carries_scrollback() {
            assert_eq!(
                (joined.dropped, joined.fallback),
                (0, None),
                "{}: session {local_id}'s whole history crosses",
                desk.name
            );
            continue;
        }
        // THE POLICY WITHHELD THE HISTORY: no sidecar, and every line the
        // park saw beyond the checkpoint's is counted, as the policy's.
        let parent_lines = fx_history_text(
            &desk
                .sessions
                .iter()
                .find(|s| s.record.local_id == *local_id)
                .unwrap()
                .term
                .lock()
                .unwrap(),
        )
        .len() as u64;
        let carried = u64::from(
            screens
                .iter()
                .find(|(id, _)| id == local_id)
                .unwrap()
                .1
                .history_lines,
        );
        assert_eq!(
            joined.take, 0,
            "{}: session {local_id} names no sidecar",
            desk.name
        );
        assert_eq!(
            joined.dropped,
            parent_lines - carried,
            "{}: session {local_id}'s history left behind is counted",
            desk.name
        );
        assert!(
            joined.dropped == 0
                || joined.fallback == Some(crate::handoff_history::Fallback::Withheld),
            "{}: session {local_id}'s loss is said as the policy's",
            desk.name
        );
    }
    for record in &manifest.sessions {
        // Since v0.98.0 the record says a withheld carry as the successor's
        // request (`history_withheld`): never under the empty policy, and for
        // every session that leaves lines behind under one that withholds.
        assert_eq!(
            record.history_withheld,
            !ceiling.carries_scrollback() && record.history_dropped > 0,
            "{}: session {}: the record says whether the policy withheld its history",
            desk.name,
            record.local_id
        );
    }
    // The held panes' screens, captured last at the park, written by the
    // same writer beside the manifest (`write_outgoing_with_held`) with their
    // scrollback's exports (v0.101.0) and their control carries.
    let held = &capture.held;
    let outgoing = write_outgoing_with_held(
        &manifest,
        &fds,
        screens,
        &capture.repaint,
        Some(desk.window.clone()),
        &controls,
        held,
        held_history,
        &nonce,
    )
    .expect("this release's writer publishes this desk");
    assert_eq!(
        outgoing.screen_digest, capture.screen_digest,
        "{}: the written bytes are the committed ones",
        desk.name
    );
    assert!(
        outgoing.connections_left_out.is_empty(),
        "{}: the manifest carries every connection",
        desk.name
    );
    let manifest_path = std::path::PathBuf::from(&outgoing.manifest_path);
    let layout_path = manifest_path.with_extension("layout.toml");
    // The layout sidecar, sealed at the park: the fork lane wrote none before
    // it (`handoff_diet::seal_layout` with no speculative layout).
    crate::handoff_diet::seal_layout(&layout_path, &layout_wire, None).expect("layout sidecar");
    let layout_wire = std::fs::read_to_string(&layout_path).unwrap();
    let layout = layout_wire_digest(&layout_wire).unwrap();
    assert_eq!(Some(layout), layout_digest(&desk.layout));
    assert_eq!(layout, committed_layout);
    let proof = adoption_proof(
        &nonce,
        fx_target_build(),
        FIXTURE_TARGET_COMMIT,
        &layout,
        &outgoing.screen_digest,
        &fds.entries,
    )
    .unwrap();

    let out = std::path::PathBuf::from(
        std::env::var_os("ATERM_HANDOFF_FIXTURE_OUT")
            .expect("set ATERM_HANDOFF_FIXTURE_OUT to tests/fixtures/handoff/v<release>"),
    )
    .join(desk.name);
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).unwrap();
    let stem = manifest_path
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let manifest_name = manifest_path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let dir_text = dir.to_string_lossy().into_owned();
    let mut copied = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(&format!("{stem}.")) {
            continue;
        }
        let keep = name == manifest_name
            || name.ends_with(".layout.toml")
            || name.ends_with(".grid")
            || name.ends_with(".altgrid")
            || name.ends_with(".ctl")
            || name.ends_with(".hist")
            || name.ends_with(".heldgrid")
            || name.ends_with(".heldalt");
        assert!(
            keep,
            "{}: an attempt file this generator does not know: {name}",
            desk.name
        );
        let bytes = std::fs::read(entry.path()).unwrap();
        if name == manifest_name {
            let text = String::from_utf8(bytes).unwrap();
            let paths = text.matches(&dir_text).count();
            let grids = screens.len()
                + screens
                    .iter()
                    .filter(|(_, cp)| cp.alt_grid.is_some())
                    .count()
                + held.len()
                + held
                    .iter()
                    .filter(|pane| pane.checkpoint.alt_grid.is_some())
                    .count();
            assert_eq!(paths, grids, "only the sidecar paths name the private dir");
            std::fs::write(
                out.join(&name),
                text.replace(&dir_text, FIXTURE_DIR_PLACEHOLDER),
            )
            .unwrap();
        } else {
            std::fs::write(out.join(&name), bytes).unwrap();
        }
        copied.push(name);
    }
    copied.sort();
    // Each session's meta JSON, extracted for review (the manifest holds the
    // hashed copy).
    let body = std::fs::read_to_string(&manifest_path).unwrap();
    let (_, toml) = body.split_once('\n').unwrap();
    let published = SessionHandoff::from_toml(toml).unwrap();
    let mut session_rows = String::new();
    for rec in &published.sessions {
        let carry = rec.screen.as_ref().unwrap();
        let (_, rung, stripped) = *capture
            .rungs
            .iter()
            .find(|(id, _, _)| *id == rec.local_id)
            .unwrap();
        assert_eq!(
            carry.repaint,
            rung.needs_repaint(),
            "{}: session {} is flagged for a repaint exactly when its rung needs one",
            desk.name,
            rec.local_id
        );
        std::fs::write(
            out.join(format!("s{}.meta.json", rec.local_id)),
            carry.meta.as_bytes(),
        )
        .unwrap();
        let cp = &screens
            .iter()
            .find(|(id, _)| *id == rec.local_id)
            .unwrap()
            .1;
        let take = rec
            .history
            .as_deref()
            .and_then(crate::handoff_history::parse_stamp)
            .map_or(0, |(_, _, take)| take);
        assert_eq!(
            take > 0,
            copied.contains(&format!("{stem}.s{}.hist", rec.local_id)),
            "{}: session {} names its history sidecar exactly when it has one",
            desk.name,
            rec.local_id
        );
        // v0.98.0: the history export drops the links of every over-cap line
        // before it writes the frame. The sidecar is the parent's whole
        // history as v0.98.0 carries it: every frame within the strict caps,
        // only the over-cap lines' links gone, every other line exact. The
        // row records how many of its lines crossed without their links.
        let history_links_stripped = if take > 0 {
            let hist = format!("{stem}.s{}.hist", rec.local_id);
            let (cols, lines) = fx_sidecar_lines(
                &format!("{}: {hist}", desk.name),
                &std::fs::read(out.join(&hist)).unwrap(),
            );
            assert_eq!(
                cols, cp.cols,
                "{}: {hist} is wrapped at the grid's width",
                desk.name
            );
            let session = desk
                .sessions
                .iter()
                .find(|s| s.record.local_id == rec.local_id)
                .unwrap();
            let parent_lines = fx_history_lines(&session.term.lock().unwrap());
            let (want, stripped) = fx_carried_lines(&parent_lines, cols);
            let take = usize::try_from(take).unwrap();
            assert!(lines.len() >= take, "{}: {hist} holds its take", desk.name);
            assert_eq!(
                fx_carried_lines(&parent_lines[..take], cols).1,
                session.taken_links_stripped,
                "{}: session {}: the lines the successor imports that lose their links",
                desk.name,
                rec.local_id
            );
            fx_assert_same_lines(
                &lines,
                &want,
                &format!(
                    "{}: session {}'s sidecar is its parent's whole history, over-cap links dropped",
                    desk.name, rec.local_id
                ),
            );
            stripped
        } else {
            0
        };
        session_rows.push_str(&format!(
            "[[session]]\nlocal_id = {}\nrows = {}\ncols = {}\nhistory_lines = {}\nalt_grid = {}\ncontrol = {}\nrequire_shell_integration_nonce = {}\n",
            rec.local_id,
            cp.rows,
            cp.cols,
            cp.history_lines,
            cp.alt_grid.is_some(),
            rec.control.is_some(),
            cp.modes.require_shell_integration_nonce,
        ));
        if let Some(nonce) = cp.shell_integration_nonce {
            session_rows.push_str(&format!(
                "shell_integration_nonce = \"{}\"\n",
                nonce.to_hex()
            ));
        }
        session_rows.push_str(&format!(
            "history_take = {take}\nhistory_lost = {}\nfg_holder = {}\n",
            rec.history_lost,
            rec.fg_holder.unwrap_or(0),
        ));
        // v0.95.0's meta additions: the numbering the successor continues,
        // and the colour and shell-integration state it carries.
        session_rows.push_str(&format!(
            "absolute_row_counter = {}\nalt_absolute_row_counter = {}\ncolor = {}\nshell_phase = {}\nshell_marks = {}\nshell_completed_seq = {}\n",
            cp.absolute_row_counter,
            cp.alt_absolute_row_counter,
            !cp.color.is_default(),
            cp.shell.phase,
            cp.shell.command_marks.len(),
            cp.shell.completed_seq,
        ));
        // v0.97.0: the rung the ladder carried the session at (`full`, or
        // `stripped-links` for a screen holding a line record over the wire's
        // cap, or `visible-only` / `repaint` under a policy desk's ceiling),
        // and how many of its lines lost their links.
        session_rows.push_str(&format!("rung = \"{rung}\"\nlinks_stripped = {stripped}\n"));
        // v0.98.0: how many lines of the session's `.hist` sidecar the export
        // wrote with their OSC 8 links dropped (`0` without a sidecar).
        session_rows.push_str(&format!(
            "history_links_stripped = {history_links_stripped}\n"
        ));
        // v0.101.0: whether the meta carries the engine's in-flight output
        // state (`output_state`), and the render debt the record carries.
        session_rows.push_str(&format!(
            "output_state = {}\n",
            !cp.output_state.is_default()
        ));
        if let Some(debt) = &rec.render_debt {
            session_rows.push_str(&format!("render_debt = \"{debt}\"\n"));
        }
        // The policy desks (the v0.97.0 policy generator's rows): whether the
        // successor must make the session's program redraw, and the lines of
        // its history this hop leaves behind, counted. The guard reads both;
        // a row without them repaints nothing and drops nothing, which is
        // what every `Full` desk's rows say by leaving them out.
        if ceiling != aterm_update_core::handoff_policy::CarryCeiling::Full {
            session_rows.push_str(&format!(
                "repaint = {}\nhistory_dropped = {}\n",
                carry.repaint, rec.history_dropped,
            ));
        }
        session_rows.push('\n');
    }
    // v0.100.0: each held pane the writer named, its screen's geometry and
    // how its command ended; since v0.101.0 its sid, its scrollback's carry
    // (the lines its `.h<id>.hist` sidecar carries, and those left behind),
    // whether its control carry crossed and whether the person had seen its
    // failure.
    assert_eq!(
        published
            .held
            .iter()
            .map(|pane| pane.local_id)
            .collect::<Vec<_>>(),
        held.iter().map(|pane| pane.local_id).collect::<Vec<_>>(),
        "{}: the writer names every held pane the capture kept",
        desk.name
    );
    for pane in held {
        let cp = &pane.checkpoint;
        let carry = published
            .held
            .iter()
            .find(|carry| carry.local_id == pane.local_id)
            .unwrap();
        let take = carry
            .history
            .as_deref()
            .and_then(crate::handoff_history::parse_stamp)
            .map_or(0, |(_, _, take)| take);
        assert_eq!(
            take > 0,
            copied.contains(&format!("{stem}.h{}.hist", pane.local_id)),
            "{}: held pane {} names its history sidecar exactly when it has one",
            desk.name,
            pane.local_id
        );
        assert_eq!(
            carry.control.is_some(),
            copied.contains(&format!("{stem}.h{}.ctl", pane.local_id)),
            "{}: held pane {} names its control sidecar exactly when it has one",
            desk.name,
            pane.local_id
        );
        session_rows.push_str(&format!(
            "[[held]]\nlocal_id = {}\nrows = {}\ncols = {}\nalt_grid = {}\nsid = \"{}\"\n\
             history_take = {take}\nhistory_dropped = {}\ncontrol = {}\nfailure_seen = {}\n",
            pane.local_id,
            cp.rows,
            cp.cols,
            cp.alt_grid.is_some(),
            pane.sid.as_str(),
            carry.history_dropped,
            carry.control.is_some(),
            carry.failure_seen,
        ));
        match pane.exit {
            Some(aterm_pty::ChildExit::Code(code)) => {
                session_rows.push_str(&format!("exit_code = {code}\n"));
            }
            Some(aterm_pty::ChildExit::Signal(signal)) => {
                session_rows.push_str(&format!("exit_signal = {signal}\n"));
            }
            None => {}
        }
        session_rows.push('\n');
    }
    let identities = fds
        .entries
        .iter()
        .map(|(id, fd, pid)| format!("[{id}, {fd}, {pid}]"))
        .collect::<Vec<_>>()
        .join(", ");
    let parent = format!(
        "# What the v{version} producer committed to for this desk. Written by the\n\
         # release's own cut (seamless_fixture_generator_tests.rs); see ../../README.md.\n\
         producer_version = \"{version}\"\n\
         producer_commit = \"{commit}\"\n\
         outgoing_build = {build}\n\
         park_carry_ceiling = \"{ceiling}\"\n\
         manifest = \"{manifest_name}\"\n\
         nonce = \"{nonce}\"\n\
         dir_placeholder = \"{FIXTURE_DIR_PLACEHOLDER}\"\n\
         screen_digest = \"{screen}\"\n\
         layout_digest = \"{layout}\"\n\
         proof_target_build = {target_build}\n\
         proof_target_commit = \"{FIXTURE_TARGET_COMMIT}\"\n\
         proof_identities = [{identities}]\n\
         proof_count = {count}\n\
         proof_digest = \"{digest}\"\n\
         ready_wire = \"{ready}\"\n\
         commit_wire = \"{commit_wire}\"\n\
         files = [{files}]\n\n\
         {session_rows}",
        version = crate::build_info::version(),
        commit = crate::build_info::git_commit(),
        build = crate::running_build_number(),
        target_build = fx_target_build(),
        ceiling = ceiling.as_str(),
        screen = fx_hex(&outgoing.screen_digest),
        layout = fx_hex(&layout),
        count = proof.count,
        digest = fx_hex(&proof.digest),
        ready = fx_hex(&proof.to_wire()),
        commit_wire = fx_hex(&proof.to_commit_wire()),
        files = copied
            .iter()
            .map(|n| format!("\"{n}\""))
            .collect::<Vec<_>>()
            .join(", "),
    );
    std::fs::write(out.join("parent.toml"), parent).unwrap();

    // SELF-CHECK: this release's OWN consumer adopts its own desk exactly —
    // every session, nothing repainted, the layout placed, the same digests,
    // the proof the parent expects — and the history import after Commit
    // gives each session its parent's whole history. So the fixture is an
    // honest desk and not one the release itself would have refused.
    let (ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, commit_write) = pipe_pair("commit");
    // SAFETY: `getppid` is a side-effect-free libc getter.
    let parent_pid = unsafe { libc::getppid() };
    aterm_log::env::set(ENV_MANIFEST, &manifest_path);
    aterm_log::env::set(ENV_NONCE, &nonce);
    aterm_log::env::set(ENV_FDS, fds.encode());
    aterm_log::env::set(ENV_LAYOUT, &layout_path);
    aterm_log::env::set(ENV_READY_FD, ready_write.to_string());
    aterm_log::env::set(ENV_COMMIT_FD, commit_read.to_string());
    aterm_log::env::set(ENV_PARENT_PID, parent_pid.to_string());
    match read_process_birth(parent_pid) {
        Some(birth) => aterm_log::env::set(ENV_PARENT_BIRTH, birth.to_wire()),
        None => aterm_log::env::unset(ENV_PARENT_BIRTH),
    }
    aterm_log::env::set(ENV_TARGET, this_build().unwrap());
    let mut incoming = take_incoming();
    assert_eq!(incoming.adopted.len(), desk.sessions.len(), "{}", desk.name);
    assert_eq!(
        incoming.repainted_tabs(),
        capture.repaint.len(),
        "{}",
        desk.name
    );
    assert!(
        incoming.layout.is_some(),
        "{}: the layout places every session",
        desk.name
    );
    // Since v0.98.0: a Settings leaf's unsaved drafts ride the layout, and
    // this release's own consumer places every one of them with its tab.
    assert_eq!(
        incoming.layout.as_ref().unwrap().carried_settings_drafts(),
        desk.layout.carried_settings_drafts(),
        "{}: the Settings drafts are placed with their tabs",
        desk.name
    );
    assert_eq!(
        incoming.unplaced_settings_drafts,
        (Vec::new(), 0),
        "{}",
        desk.name
    );
    // This release's own consumer shows every held pane: its final screen at its
    // geometry, without scrollback or a single link, how its command ended,
    // and since v0.101.0 its sid, whether the person had seen its failure, its
    // control carry and its scrollback's sidecar, taken before the proof.
    assert_eq!(
        incoming
            .held
            .iter()
            .map(|pane| pane.local_id)
            .collect::<Vec<_>>(),
        held.iter().map(|pane| pane.local_id).collect::<Vec<_>>(),
        "{}: every held pane is shown",
        desk.name
    );
    for (shown, sent) in incoming.held.iter().zip(held) {
        let (cp, was) = (&shown.checkpoint, &sent.checkpoint);
        assert_eq!(
            (cp.rows, cp.cols, cp.history_lines, cp.alt_grid.is_some()),
            (was.rows, was.cols, 0, was.alt_grid.is_some()),
            "{}: held pane {}'s geometry",
            desk.name,
            shown.local_id
        );
        assert_eq!(
            shown.exit, sent.exit,
            "{}: held pane {}'s exit",
            desk.name, shown.local_id
        );
        assert_eq!(
            shown.sid.as_ref(),
            Some(&sent.sid),
            "{}: held pane {}'s sid",
            desk.name,
            shown.local_id
        );
        assert_eq!(
            shown.failure_seen, sent.failure_seen,
            "{}: held pane {}'s seen failure",
            desk.name, shown.local_id
        );
        let carry = published
            .held
            .iter()
            .find(|carry| carry.local_id == shown.local_id)
            .unwrap();
        assert_eq!(
            shown.control.is_some(),
            carry.control.is_some(),
            "{}: held pane {}'s control carry crosses",
            desk.name,
            shown.local_id
        );
        assert_eq!(
            shown
                .history
                .carry
                .as_ref()
                .map(crate::handoff_history::HistoryCarry::take),
            carry
                .history
                .as_deref()
                .and_then(crate::handoff_history::parse_stamp)
                .map(|(_, _, take)| take),
            "{}: held pane {}'s history sidecar is taken",
            desk.name,
            shown.local_id
        );
        for grid in [Some(&cp.grid), cp.alt_grid.as_ref()].into_iter().flatten() {
            assert!(
                !grid.windows(b"https://".len()).any(|w| w == b"https://"),
                "{}: held pane {} shows no link",
                desk.name,
                shown.local_id
            );
        }
    }
    assert_eq!(
        incoming.screen_digest,
        Some(outgoing.screen_digest),
        "{}",
        desk.name
    );
    assert_eq!(incoming.layout_digest, Some(layout), "{}", desk.name);
    for adopted in &incoming.adopted {
        let rec = published
            .sessions
            .iter()
            .find(|rec| rec.local_id == adopted.local_id)
            .unwrap();
        assert!(adopted.checkpoint.is_some(), "{}", desk.name);
        let carried = &screens
            .iter()
            .find(|(id, _)| *id == adopted.local_id)
            .unwrap()
            .1;
        let restored = adopted.checkpoint.as_ref().unwrap();
        assert_eq!(restored.grid, carried.grid, "{}", desk.name);
        // Both grids: a link-dense line on the saved primary is stripped in
        // the inactive grid's bytes.
        assert_eq!(restored.alt_grid, carried.alt_grid, "{}", desk.name);
        assert_eq!(
            restored.shell_integration_nonce, carried.shell_integration_nonce,
            "{}",
            desk.name
        );
        assert_eq!(
            restored.modes.require_shell_integration_nonce,
            carried.modes.require_shell_integration_nonce,
            "{}",
            desk.name
        );
        // v0.95.0's meta: the numbering, the application's colours and the
        // shell-integration state cross the wire intact.
        assert_eq!(
            (
                restored.absolute_row_counter,
                restored.alt_absolute_row_counter
            ),
            (
                carried.absolute_row_counter,
                carried.alt_absolute_row_counter
            ),
            "{}: session {}'s absolute numbering crosses",
            desk.name,
            adopted.local_id
        );
        assert_eq!(
            restored.color, carried.color,
            "{}: session {}'s colours cross",
            desk.name, adopted.local_id
        );
        assert_eq!(
            restored.shell, carried.shell,
            "{}: session {}'s shell state crosses",
            desk.name, adopted.local_id
        );
        assert_eq!(
            adopted.control.is_some(),
            rec.control.is_some(),
            "{}",
            desk.name
        );
        // v0.101.0's meta and record additions: the in-flight output state
        // crosses the meta intact, and the render debt the record carries is
        // the one the successor seeds.
        assert_eq!(
            restored.output_state, carried.output_state,
            "{}: session {}'s output state crosses",
            desk.name, adopted.local_id
        );
        assert_eq!(
            adopted.render_debt,
            rec.render_debt
                .as_deref()
                .and_then(crate::session_store::parse_render_debt),
            "{}: session {}'s render debt crosses",
            desk.name,
            adopted.local_id
        );
        assert_eq!(
            adopted.history.carry.is_some(),
            rec.history.is_some(),
            "{}: session {}'s history sidecar is taken",
            desk.name,
            adopted.local_id
        );
        // The repaint set, the withheld history's count and (since v0.98.0)
        // whether the policy withheld it cross.
        assert_eq!(
            adopted.repaint,
            capture.repaint.contains(&adopted.local_id),
            "{}: session {} is repainted exactly when its rung needs it",
            desk.name,
            adopted.local_id
        );
        assert_eq!(
            (adopted.history.dropped, adopted.history.lost),
            (rec.history_dropped, rec.history_lost),
            "{}: session {}'s history left behind is counted",
            desk.name,
            adopted.local_id
        );
        assert_eq!(
            adopted.history.withheld,
            if rec.history_withheld {
                rec.history_dropped
            } else {
                0
            },
            "{}: session {}'s withheld lines are said as the policy's",
            desk.name,
            adopted.local_id
        );
    }
    // A stalled sequence rides the COPY (v0.100.0's parser carry): the
    // carried checkpoint holds the partial state for the successor to
    // continue. One no carry can hold would be left out as CAN would (the
    // checkpoint says Ground). Either way the live parser still holds the
    // partial sequence, so a rolled-back handoff resumes byte-exactly.
    for (id, state, held) in &capture.abandoned {
        let carried = &screens
            .iter()
            .find(|(local_id, _)| local_id == id)
            .unwrap()
            .1;
        if *held {
            assert!(
                !carried.parser_ground && carried.parser.is_some(),
                "{}: session {id}'s partial {state} rides the carry",
                desk.name
            );
        } else {
            assert!(
                carried.parser_ground,
                "{}: session {id} was carried out of {state} as if CAN had ended it",
                desk.name
            );
        }
        let live = desk
            .sessions
            .iter()
            .find(|s| s.record.local_id == *id)
            .unwrap()
            .term
            .lock()
            .unwrap()
            .partial_sequence_state();
        assert_eq!(
            live,
            Some(*state),
            "{}: session {id}'s live parser is untouched",
            desk.name
        );
    }
    // The held panes, kept for their import after Commit.
    let held_shown = std::mem::take(&mut incoming.held);
    let ((child, ready, _), adopted) =
        child_proof_from(incoming).unwrap_or_else(|| panic!("{}: the child proves", desk.name));
    let expected = adoption_proof(
        &nonce,
        crate::build_info::build_number()
            .parse::<u64>()
            .unwrap_or(0),
        crate::build_info::git_commit(),
        &layout,
        &outgoing.screen_digest,
        &fds.entries,
    )
    .unwrap();
    assert_eq!(child, expected, "{}: the proof is the parent's", desk.name);
    // The ready signal owns its write end now, and closes it.
    drop(ready);
    for mut item in adopted {
        let parent = fx_history_text(
            &desk
                .sessions
                .iter()
                .find(|s| s.record.local_id == item.local_id)
                .unwrap()
                .term
                .lock()
                .unwrap(),
        );
        let checkpoint = item.checkpoint.take().unwrap();
        let mut history = std::mem::take(&mut item.history);
        let successor = Arc::new(Mutex::new(Terminal::new(checkpoint.rows, checkpoint.cols)));
        crate::spawn::hydrate_adopted_engine(
            &successor,
            Some(&checkpoint),
            None,
            None,
            None,
            item.local_id,
            &mut history,
        );
        let report = crate::handoff_history::run_imports(vec![crate::handoff_history::ImportJob {
            session: item.local_id,
            term: Arc::clone(&successor),
            history,
        }])
        .remove(0);
        assert_eq!(
            report.failed, None,
            "{}: session {}",
            desk.name, item.local_id
        );
        let record = published
            .sessions
            .iter()
            .find(|rec| rec.local_id == item.local_id)
            .unwrap();
        let dropped = record.history_dropped;
        assert_eq!(
            report.lost(),
            dropped,
            "{}: session {}",
            desk.name,
            item.local_id
        );
        assert_eq!(
            report.withheld(),
            if record.history_withheld { dropped } else { 0 },
            "{}: session {}: the lines left behind are said as the policy's",
            desk.name,
            item.local_id
        );
        // Every line the policy let cross, and no other: the parent's whole
        // history under a `full` ceiling, its newest lines the checkpoint
        // carried (none) under one that withholds the scrollback.
        let crossed =
            usize::try_from(report.imported + u64::from(checkpoint.history_lines)).unwrap();
        assert_eq!(
            crossed as u64 + dropped,
            parent.len() as u64,
            "{}: session {}'s history is carried or counted, line for line",
            desk.name,
            item.local_id
        );
        // v0.98.0's export wrote every record within the cap, so the import's
        // leniency for older producers' over-cap links repairs nothing here.
        assert_eq!(
            report.links_dropped, 0,
            "{}: session {}: the import finds no over-cap record to repair",
            desk.name, item.local_id
        );
        assert_eq!(
            fx_history_text(&successor.lock().unwrap()),
            parent[parent.len() - crossed..],
            "{}: session {}'s history crosses as far as the policy lets it",
            desk.name,
            item.local_id
        );
        // …cell for cell: the parent's lines, with only the over-cap lines'
        // links dropped (by the screen carry's rung for the lines the
        // checkpoint carries, by the export for the sidecar's) and every other
        // line, every other link included, exact.
        {
            let source = desk
                .sessions
                .iter()
                .find(|s| s.record.local_id == item.local_id)
                .unwrap()
                .term
                .lock()
                .unwrap();
            let lines = fx_history_lines(&source);
            let (want, _) = fx_carried_lines(&lines[lines.len() - crossed..], checkpoint.cols);
            fx_assert_same_lines(
                &fx_history_lines(&successor.lock().unwrap()),
                &want,
                &format!(
                    "{}: session {}'s history after the import is its parent's, over-cap links dropped",
                    desk.name, item.local_id
                ),
            );
        }
        // Since v0.95.0: after the import, every shell mark the restore
        // installed still names its line (the successor continues the
        // parent's numbering); the shell state and the application's colours
        // are the parent's; and a command running across the handoff
        // completes on the successor.
        let source = desk
            .sessions
            .iter()
            .find(|s| s.record.local_id == item.local_id)
            .unwrap()
            .term
            .lock()
            .unwrap();
        let mut successor = successor.lock().unwrap();
        // The restored parser continues the partial sequence the carry held
        // (v0.100.0's parser carry), and is at Ground otherwise.
        let continues = if checkpoint.parser.is_some() {
            source.partial_sequence_state()
        } else {
            None
        };
        assert_eq!(
            successor.partial_sequence_state(),
            continues,
            "{}: session {}'s restored parser continues what the carry held",
            desk.name,
            item.local_id
        );
        assert_eq!(
            successor.command_marks().len(),
            checkpoint.shell.command_marks.len(),
            "{}: session {}'s carried marks are installed",
            desk.name,
            item.local_id
        );
        // A repainted screen is blank until its program redraws, so its
        // marks name rows the successor has not drawn yet. A carry keeps a
        // mark while any row it names is carried (804646426f), so a row older
        // than the successor's oldest one (a prompt above a visible-only
        // carry's screen) reads Evicted there, as in the live terminal.
        let redraws = capture.repaint.contains(&item.local_id);
        let oldest = successor.main_grid().oldest_absolute_row();
        for mark in successor.command_marks().iter().filter(|_| !redraws) {
            for row in [Some(mark.prompt_start_row), mark.output_end_row]
                .into_iter()
                .flatten()
                .filter(|row| *row >= oldest)
            {
                let text = fx_text_at(&successor, row);
                assert!(
                    text.is_some(),
                    "{}: session {}'s mark row {row} is readable",
                    desk.name,
                    item.local_id
                );
                assert_eq!(
                    text,
                    fx_text_at(&source, row),
                    "{}: session {}'s mark at row {row} names its line",
                    desk.name,
                    item.local_id
                );
            }
        }
        assert_eq!(
            successor.shell_state(),
            source.shell_state(),
            "{}",
            desk.name
        );
        assert_eq!(
            successor.completed_command_seq(),
            source.completed_command_seq(),
            "{}",
            desk.name
        );
        for index in 0..=u8::MAX {
            assert_eq!(
                successor.palette_color(index),
                source.palette_color(index),
                "{}: session {}'s palette entry {index}",
                desk.name,
                item.local_id
            );
        }
        assert_eq!(
            (
                successor.default_foreground(),
                successor.default_background(),
                successor.cursor_color()
            ),
            (
                source.default_foreground(),
                source.default_background(),
                source.cursor_color()
            ),
            "{}: session {}'s dynamic colours",
            desk.name,
            item.local_id
        );
        if checkpoint.shell.phase == 3 {
            let sign = checkpoint
                .shell_integration_nonce
                .map_or_else(String::new, |nonce| format!(";id={}", nonce.to_hex()));
            if successor.is_alternate_screen() {
                successor.process(b"\x1b[?1049l");
            }
            successor.process(format!("\x1b]133;D;0{sign}\x07").as_bytes());
            assert_eq!(
                successor.completed_command_seq(),
                source.completed_command_seq() + 1,
                "{}: session {}'s running command completes",
                desk.name,
                item.local_id
            );
        }
    }
    // A HELD PANE'S SCROLLBACK CROSSES AS AN ADOPTED SESSION'S DOES (v0.101.0,
    // round eight's finding 39): restored as this release's `App::restore_held_leaf`
    // restores it (`held_pane_session`, then `held_pane_history`: the import's
    // keys reserved under the engine's first lock) and imported by the same
    // worker after Commit, every line of the parent's history in front of the
    // held screen, cell for cell. Since v0.102.0 the import drops every link
    // of a held pane's lines, as its screen's are (`import`'s `held_pane`):
    // nothing runs behind the pane, so a link there could only open a URL.
    for shown in held_shown {
        let sent = desk
            .held
            .iter()
            .find(|pane| pane.local_id == shown.local_id)
            .unwrap();
        let parent_lines = fx_history_lines(&sent.term.lock().unwrap());
        let cols = shown.checkpoint.cols;
        let session = crate::held_pane_session(
            shown.local_id,
            shown.sid.clone().expect("a held pane keeps its sid"),
            &shown.checkpoint,
            None,
        );
        let history = crate::held_pane_history(&session.term, shown.history).unwrap_or_else(|| {
            panic!(
                "{}: held pane {} owes the import",
                desk.name, shown.local_id
            )
        });
        let report = crate::handoff_history::run_imports(vec![crate::handoff_history::ImportJob {
            session: shown.local_id,
            term: Arc::clone(&session.term),
            history,
        }])
        .remove(0);
        assert_eq!(
            report.failed, None,
            "{}: held pane {}",
            desk.name, shown.local_id
        );
        assert_eq!(
            (report.imported, report.links_dropped, report.held_pane),
            (parent_lines.len() as u64, 0, true),
            "{}: held pane {} imports its parent's whole history",
            desk.name,
            shown.local_id
        );
        let (mut want, _) = fx_carried_lines(&parent_lines, cols);
        for line in &mut want {
            line.clear_hyperlinks();
        }
        let got = fx_history_lines(&session.term.lock().unwrap());
        fx_assert_same_lines(
            &got,
            &want,
            &format!(
                "{}: held pane {}'s history after the import is its parent's, every link dropped",
                desk.name, shown.local_id
            ),
        );
        assert!(
            !got.iter().any(aterm_core::scrollback::Line::has_hyperlinks),
            "{}: held pane {}'s scrollback shows no link",
            desk.name,
            shown.local_id
        );
    }
    for fd in slaves
        .into_iter()
        .chain([ready_read, commit_read, commit_write])
    {
        aterm_pty::close_fd(fd);
    }
    let _ = std::fs::remove_dir_all(&scratch);
}

fn fx_incident_desk() -> FxDesk {
    use crate::restore::RestoreManifest;
    // Session 0: Claude Code entered 1049 from a prompt on the LAST row of a
    // 56-row grid, then the message band's row took one row away (the
    // 2026-09-22 incident). Through v0.101.0 the DECSC slot stayed on row 55
    // of the 55-row grid; since v0.102.0 the shrink demotes the saved
    // primary's top row into history and the slot rides up with its text
    // (`Grid::lend_saved_cursor`, xterm's `AdjustSavedCursor`): row 54, after
    // `$ claude`.
    let mut claude = fx_claude(0, 56, 149, 60);
    {
        let mut t = claude.term.lock().unwrap();
        t.resize(55, 149);
        let frame = FakeApp::new().frame(55, 149);
        t.process(&frame);
        let cp = t.checkpoint_carry(256).expect("Ground");
        let meta = CheckpointMeta::from_checkpoint(&cp);
        assert_eq!(meta.rows, 55);
        assert_eq!(
            meta.saved_cursor_main
                .map(|saved| (saved.cursor_row, saved.cursor_col)),
            Some((54, 8)),
            "the incident's slot rode the shrink up with its prompt"
        );
    }
    claude.record.title = "claude".to_string();
    let shell = fx_session(1, "zsh", fx_shell(55, 149, 400, "/home/dev/src/aterm", 1));
    FxDesk {
        name: "incident-55x149",
        nonce_seed: 0x0100_0001,
        ceiling: aterm_update_core::handoff_policy::CarryCeiling::Full,
        window: fx_window_carry(55, 149, true),
        sessions: vec![claude, shell],
        layout: RestoreManifest::new(vec![fx_window_in(
            55,
            149,
            1,
            vec![
                fx_tab(fx_leaf(0, "claude", Some("/home/dev/src/aterm"))),
                fx_tab(fx_leaf(1, "zsh", Some("/home/dev/src/aterm"))),
            ],
        )]),
        connections: Vec::new(),
        next_turn_id: Some(41),
        held: Vec::new(),
    }
}

fn fx_claude_desk() -> FxDesk {
    use crate::restore::{RestoreManifest, SplitKind};
    let mut app = FakeApp::new();
    let (term, turns) = old_session(&mut app, 60);
    let claude = FxSession {
        record: fx_record(0, "claude"),
        term,
        turns,
        rung: CarryRung::Full,
        taken_links_stripped: 0,
    };
    let shell = fx_session(
        1,
        "zsh",
        fx_shell(ROWS, COLS, 120, "/home/dev/src/aterm", 2),
    );
    FxDesk {
        name: "claude-code-1049",
        nonce_seed: 0x0100_0002,
        ceiling: aterm_update_core::handoff_policy::CarryCeiling::Full,
        window: fx_window_carry(ROWS, COLS * 2 + 1, false),
        sessions: vec![claude, shell],
        layout: RestoreManifest::new(vec![fx_window(
            ROWS,
            COLS * 2 + 1,
            vec![fx_tab(fx_split(
                SplitKind::Vertical,
                fx_leaf(0, "claude", Some("/home/dev/src/aterm")),
                fx_leaf(1, "zsh", Some("/home/dev/src/aterm")),
            ))],
        )]),
        connections: vec![ConnectionCarry {
            src: fx_record(1, "").sid,
            dst: fx_record(0, "").sid,
            op: "read-screen".to_string(),
        }],
        next_turn_id: Some(42),
        held: Vec::new(),
    }
}

fn fx_history_desk() -> FxDesk {
    use crate::restore::{RestoreManifest, SplitKind};
    let mut h0 = fx_shell(40, 120, 1200, "/home/dev/src/aterm", 3);
    h0.process(b"\x1b[?2004h\x1b[>1u\x1b[5 q");
    let mut h1 = fx_shell(40, 119, 900, "/home/dev/logs", 4);
    h1.process(b"\x1b[3g\x1b[1;9H\x1bH\x1b[1;17H\x1bH\x1b[40;1H\x1b[5;35r\x1b[35;1H");
    let h2 = fx_shell(24, 80, 10, "/home/dev", 5);
    let mut sessions = vec![
        fx_session(0, "zsh", h0),
        fx_session(1, "tail", h1),
        fx_session(2, "zsh", h2),
    ];
    sessions[1].record.user_title = Some("logs".to_string());
    FxDesk {
        name: "history",
        nonce_seed: 0x0100_0003,
        ceiling: aterm_update_core::handoff_policy::CarryCeiling::Full,
        window: fx_window_carry(40, 240, false),
        sessions,
        layout: RestoreManifest::new(vec![fx_window(
            40,
            240,
            vec![
                fx_tab(fx_split(
                    SplitKind::Vertical,
                    fx_leaf(0, "zsh", Some("/home/dev/src/aterm")),
                    fx_leaf(1, "tail", Some("/home/dev/logs")),
                )),
                fx_tab(fx_leaf(2, "zsh", Some("/home/dev"))),
            ],
        )]),
        connections: Vec::new(),
        next_turn_id: None,
        held: Vec::new(),
    }
}

fn fx_twelve_pane_desk() -> FxDesk {
    use crate::restore::{RestoreManifest, SplitKind};
    // Window A (46x168): a 2x2 tab, a three-column tab, one full tab.
    // Window B (40x140): a three-pane tab, one full tab.
    let geometry: [(u16, u16); 12] = [
        (22, 83),
        (23, 83),
        (22, 84),
        (23, 84),
        (46, 55),
        (46, 56),
        (46, 55),
        (46, 168),
        (40, 70),
        (19, 69),
        (20, 69),
        (40, 140),
    ];
    let mut sessions = Vec::new();
    for (i, (rows, cols)) in geometry.into_iter().enumerate() {
        let id = u64::try_from(i).unwrap();
        let flavour = u8::try_from(i).unwrap();
        let session = match i {
            2 | 9 => fx_session(id, "vim", fx_editor(rows, cols, flavour)),
            5 => fx_claude(id, rows, cols, 30),
            _ => fx_session(
                id,
                "zsh",
                fx_shell(rows, cols, 60 + i * 37, "/home/dev/src/aterm", flavour),
            ),
        };
        sessions.push(session);
    }
    sessions[3].record.user_title = Some("build".to_string());
    sessions[3].record.role = Some("worker".to_string());
    sessions[7].record.description = Some("release notes".to_string());
    let v = SplitKind::Vertical;
    let h = SplitKind::Horizontal;
    let leaf = |id: u64, title: &str| fx_leaf(id, title, Some("/home/dev/src/aterm"));
    let window_a = fx_window(
        46,
        168,
        vec![
            fx_tab(fx_split(
                v,
                fx_split(h, leaf(0, "zsh"), leaf(1, "zsh")),
                fx_split(h, leaf(2, "vim"), leaf(3, "zsh")),
            )),
            fx_tab(fx_split(
                v,
                leaf(4, "zsh"),
                fx_split(v, leaf(5, "claude"), leaf(6, "zsh")),
            )),
            fx_tab(leaf(7, "zsh")),
        ],
    );
    let mut window_b = fx_window(
        40,
        140,
        vec![
            fx_tab(fx_split(
                v,
                leaf(8, "zsh"),
                fx_split(h, leaf(9, "vim"), leaf(10, "zsh")),
            )),
            fx_tab(leaf(11, "zsh")),
        ],
    );
    // The second window is behind the first, and not key.
    if window_b.show.z_order.is_some() {
        window_b.show.z_order = Some(1);
        window_b.show.key = Some(false);
    }
    let connections = ["read-screen", "signal", "write-input"]
        .into_iter()
        .map(|op| ConnectionCarry {
            src: fx_record(3, "").sid,
            dst: fx_record(5, "").sid,
            op: op.to_string(),
        })
        .collect();
    FxDesk {
        name: "twelve-panes",
        nonce_seed: 0x0100_0004,
        ceiling: aterm_update_core::handoff_policy::CarryCeiling::Full,
        window: fx_window_carry(46, 168, false),
        sessions,
        layout: RestoreManifest::new(vec![window_a, window_b]),
        connections,
        next_turn_id: Some(7),
        held: Vec::new(),
    }
}

/// A shell signing its OSC 133/633 marks with its tab's nonce: what `spawn`
/// installs for a shell with integration, and what the carry sends.
fn fx_signed_shell(rows: u16, cols: u16, nonce: [u8; 32], cwd: &str, flavour: u8) -> Terminal {
    let mut t = Terminal::new(rows, cols);
    t.authorize_shell_integration(nonce);
    t.set_require_shell_integration_nonce(true);
    let id = fx_hex(&nonce);
    t.process(format!("\x1b]7;file://devhost{cwd}\x07\x1b]0;zsh\x07").as_bytes());
    for i in 0..(20 + usize::from(flavour) * 7) {
        t.process(
            format!(
                "\x1b]133;A;id={id}\x07dev@devhost {cwd} % \x1b]633;E;make test-{i};id={id}\x07\
                 \x1b]133;B;id={id}\x07make test-{i}\r\n\x1b]133;C;id={id}\x07\
                 \x1b[32mok\x1b[0m case_{i:04}\r\n\x1b]133;D;{};id={id}\x07",
                i % 3
            )
            .as_bytes(),
        );
    }
    t.process(
        format!("\x1b]133;A;id={id}\x07dev@devhost {cwd} % \x1b]133;B;id={id}\x07").as_bytes(),
    );
    t
}

/// The shape v0.92.0 changed: each shell's authorized shell-integration nonce
/// rides the carry (meta key `shell_integration_nonce`), so the successor
/// authorizes the same value and the shells' marks keep counting. Two shells
/// with different nonces (the nonce is per tab), one of them in 1049 with an
/// editor, split side by side. Shells spawned with integration by v0.94.0
/// also read its re-key file and its body pointer (`rekey`, `loader`).
fn fx_shell_integration_desk() -> FxDesk {
    use crate::restore::{RestoreManifest, SplitKind};
    let a = fx_signed_shell(30, 100, [0x94; 32], "/home/dev/src/aterm", 1);
    let mut b = fx_signed_shell(30, 99, [0x49; 32], "/home/dev/notes", 2);
    b.process(b"vim notes.md\r\n\x1b[?1049h\x1b[H\x1b[2J# notes\x1b[30;1H\x1b[7m notes.md \x1b[0m\x1b[2;1H");
    let mut sessions = vec![fx_session(0, "zsh", a), fx_session(1, "vim", b)];
    for session in &mut sessions {
        session.record.rekey = true;
        session.record.loader = true;
    }
    FxDesk {
        name: "shell-integration",
        nonce_seed: 0x0100_0005,
        ceiling: aterm_update_core::handoff_policy::CarryCeiling::Full,
        window: fx_window_carry(30, 200, false),
        sessions,
        layout: RestoreManifest::new(vec![fx_window(
            30,
            200,
            vec![fx_tab(fx_split(
                SplitKind::Vertical,
                fx_leaf(0, "zsh", Some("/home/dev/src/aterm")),
                fx_leaf(1, "vim", Some("/home/dev/notes")),
            ))],
        )]),
        connections: Vec::new(),
        next_turn_id: None,
        held: Vec::new(),
    }
}

/// THE SHAPES v0.93.0/v0.94.0 CHANGED. The history carry: a shell whose
/// history is deeper than the checkpoint's 256 lines, and one running `less`
/// in 1049, whose checkpoint carries none of its shell's history (the saved
/// primary rides at its visible rows), so the `.hist` sidecar carries all of
/// it. The records carry what v0.93.0/v0.94.0 added beside it: a running
/// `history_lost` from an earlier handoff, the foreground holder, the
/// integration's `rekey` and `loader`, and the `questions` word (on the
/// record and, in lockstep, on its layout leaf).
fn fx_history_carry_desk() -> FxDesk {
    use crate::restore::{RestoreManifest, RestoredSplitTree, RestoredView, SplitKind};
    let deep = fx_shell(40, 120, 1500, "/home/dev/src/aterm", 6);
    let mut paged = fx_shell(40, 119, 1000, "/home/dev/logs", 7);
    paged.process(b"less build.log\r\n\x1b[?1049h\x1b[?1h\x1b=\x1b[H\x1b[2J");
    for r in 1..40 {
        paged.process(format!("\x1b[{r};1Hbuild.log line {r:03}").as_bytes());
    }
    paged.process(b"\x1b[40;1H\x1b[7mbuild.log lines 1-39\x1b[0m");
    let mut sessions = vec![fx_session(0, "zsh", deep), fx_session(1, "less", paged)];
    for session in &mut sessions {
        session.record.rekey = true;
        session.record.loader = true;
    }
    sessions[0].record.history_lost = 12;
    sessions[1].record.questions = Some("recommended".to_string());
    let mut pager = fx_leaf(1, "less", Some("/home/dev/logs"));
    if let RestoredSplitTree::Leaf {
        view: RestoredView::Terminal(leaf),
    } = &mut pager
    {
        leaf.questions = Some("recommended".to_string());
    } else {
        unreachable!("fx_leaf builds a terminal leaf");
    }
    FxDesk {
        name: "history-carry",
        nonce_seed: 0x0100_0006,
        ceiling: aterm_update_core::handoff_policy::CarryCeiling::Full,
        window: fx_window_carry(40, 240, false),
        sessions,
        layout: RestoreManifest::new(vec![fx_window(
            40,
            240,
            vec![fx_tab(fx_split(
                SplitKind::Vertical,
                fx_leaf(0, "zsh", Some("/home/dev/src/aterm")),
                pager,
            ))],
        )]),
        connections: Vec::new(),
        next_turn_id: None,
        held: Vec::new(),
    }
}

/// A shell under OSC 133/633 integration with `commands` finished commands
/// (prompt, command line, a few lines of output, an exit status), its marks
/// signed with `nonce` when it has one (aterm's own integration) and unsigned
/// otherwise (a shell that sources its own script, with aterm's injection
/// off), ending at a fresh prompt. `theme` is what its rc file writes at
/// start, and `palette_opt_in` the host's `allow_palette_reconfigure`.
#[allow(clippy::too_many_arguments, reason = "a throwaway fixture builder")]
fn fx_marked_shell(
    rows: u16,
    cols: u16,
    nonce: Option<[u8; 32]>,
    palette_opt_in: bool,
    theme: &[u8],
    commands: usize,
    cwd: &str,
    flavour: u8,
) -> (Terminal, String) {
    let mut t = Terminal::new(rows, cols);
    t.set_alt_archive_enabled(true);
    t.set_alt_archive_origin(0x5eed_0200 + u64::from(flavour));
    t.set_allow_palette_reconfigure(palette_opt_in);
    let sign = match nonce {
        Some(nonce) => {
            t.authorize_shell_integration(nonce);
            t.set_require_shell_integration_nonce(true);
            format!(";id={}", fx_hex(&nonce))
        }
        None => String::new(),
    };
    t.process(format!("\x1b]7;file://devhost{cwd}\x07\x1b]0;zsh\x07").as_bytes());
    t.process(theme);
    for i in 0..commands {
        let command = format!("cargo test -p crate{i}");
        t.process(
            format!(
                "\x1b]133;A{sign}\x07\x1b[1;38;5;202mdev@devhost\x1b[0m {cwd} % \
                 \x1b]633;E;{command}{sign}\x07\x1b]133;B{sign}\x07{command}\r\n\
                 \x1b]133;C{sign}\x07"
            )
            .as_bytes(),
        );
        for j in 0..(1 + (i + usize::from(flavour)) % 6) {
            t.process(
                format!(
                    "\x1b[32mok\x1b[0m  crate{i}::case_{j:02} \x1b[2m({} ms)\x1b[0m\r\n",
                    i * 7 + j
                )
                .as_bytes(),
            );
        }
        let code = if i % 5 == 4 { 101 } else { 0 };
        t.process(format!("\x1b]133;D;{code}{sign}\x07").as_bytes());
    }
    t.process(
        format!(
            "\x1b]133;A{sign}\x07\x1b[1;38;5;202mdev@devhost\x1b[0m {cwd} % \x1b]133;B{sign}\x07"
        )
        .as_bytes(),
    );
    (t, sign)
}

/// THE SHAPE v0.95.0 CHANGED: the meta carries the absolute row counters,
/// the colours an application set (a diff from the configured theme, and the
/// XTPUSHCOLORS stack whole) and the OSC 133/633 state, running command
/// included. Session 0 is a shell with aterm's signed integration, a base16
/// theme (OSC 4, which its owner opted into with `allow_palette_reconfigure`,
/// and OSC 10/11/12), 150 finished commands (so its history is deeper than
/// the checkpoint carries and names a `.hist` sidecar) and a `cargo build`
/// still running. Session 1 is a shell whose own script sends unsigned marks,
/// running vim in 1049, which pushed the colours (OSC 30001) and set its
/// background, cursor and selection colours: its checkpoint carries none of
/// the shell's history, and the marks it carries sit on the saved primary.
fn fx_colour_and_shell_desk() -> FxDesk {
    use crate::restore::{RestoreManifest, SplitKind};
    // base16-shell's theme, as its rc hook writes it at shell start
    // (ST-terminated): sixteen palette entries, then foreground, background
    // and cursor.
    let base16 = [
        "28/2c/34", "e0/6c/75", "98/c3/79", "e5/c0/7b", "61/af/ef", "c6/78/dd", "56/b6/c2",
        "ab/b2/bf", "54/58/62", "e0/6c/75", "98/c3/79", "e5/c0/7b", "61/af/ef", "c6/78/dd",
        "56/b6/c2", "c8/cc/d4",
    ];
    let mut theme = String::new();
    for (index, rgb) in base16.iter().enumerate() {
        theme.push_str(&format!("\x1b]4;{index};rgb:{rgb}\x1b\\"));
    }
    theme
        .push_str("\x1b]10;rgb:ab/b2/bf\x1b\\\x1b]11;rgb:28/2c/34\x1b\\\x1b]12;rgb:52/8b/ff\x1b\\");
    let (mut build, sign) = fx_marked_shell(
        42,
        120,
        Some([0x95; 32]),
        true,
        theme.as_bytes(),
        150,
        "/home/dev/src/aterm",
        1,
    );
    build.process(
        format!(
            "\x1b]633;E;cargo build --release{sign}\x07cargo build --release\r\n\
             \x1b]133;C{sign}\x07"
        )
        .as_bytes(),
    );
    for k in 0..12 {
        build.process(
            format!(
                "\x1b[1;32m   Compiling\x1b[0m crate{k} v0.1.0 (/home/dev/src/aterm/crates/crate{k})\r\n"
            )
            .as_bytes(),
        );
    }
    build.process(b"\x1b[1;36m    Building\x1b[0m [=====>        ] 12/40: crate12");

    let (mut edit, _) = fx_marked_shell(42, 119, None, false, b"", 40, "/home/dev/notes", 2);
    edit.process(
        b"\x1b]633;E;vim notes.md\x07vim notes.md\r\n\x1b]133;C\x07\
          \x1b[?1049h\x1b[?1h\x1b=\x1b]30001\x07\
          \x1b]11;rgb:1d/20/21\x07\x1b]12;rgb:fa/bd/2f\x07\x1b]17;rgb:50/49/45\x07\
          \x1b[H\x1b[2J",
    );
    for r in 2..42 {
        edit.process(format!("\x1b[{r};1H\x1b[34m~\x1b[0m").as_bytes());
    }
    edit.process(b"\x1b[1;1H# notes\x1b[42;1H\x1b[7m notes.md [+] \x1b[0m\x1b[1;8H");

    let mut sessions = vec![fx_session(0, "cargo", build), fx_session(1, "vim", edit)];
    sessions[0].record.rekey = true;
    sessions[0].record.loader = true;
    FxDesk {
        name: "colour-and-shell",
        nonce_seed: 0x0100_0007,
        ceiling: aterm_update_core::handoff_policy::CarryCeiling::Full,
        window: fx_window_carry(42, 240, false),
        sessions,
        layout: RestoreManifest::new(vec![fx_window(
            42,
            240,
            vec![fx_tab(fx_split(
                SplitKind::Vertical,
                fx_leaf(0, "cargo", Some("/home/dev/src/aterm")),
                fx_leaf(1, "vim", Some("/home/dev/notes")),
            ))],
        )]),
        connections: Vec::new(),
        next_turn_id: None,
        held: Vec::new(),
    }
}

/// A parser left mid-sequence over a quiet PTY, the shape v0.93.0 changed.
/// v0.94.0's and v0.95.0's generators asserted every parser was at Ground and
/// so carried no such desk; v0.97.0's carries it again. Through v0.92.0
/// such a session refused every in-session update ("a terminal parser was
/// mid-sequence"). v0.97.0 carries it with the partial sequence left out, as
/// CAN would (`carry_for_wire` projects
/// `Terminal::checkpoint_carry_abandoning_partial`), so its meta says Ground
/// over a live parser that was not; over queued output its park misses
/// instead. Three stalls a real desk holds: a shell that printed an
/// unterminated OSC title (`printf '\e]0;building'`); an ssh session stalled
/// halfway through an SGR; and an editor in 1049 whose kitty-graphics upload
/// stopped inside its APC payload. v0.93.0's desk, and with v0.94.0's history
/// carry the title shell's 300 lines and the editor's saved primary each name
/// a `.hist` sidecar.
fn fx_stalled_sequence_desk() -> FxDesk {
    use crate::restore::{RestoreManifest, SplitKind};
    let mut title = fx_shell(30, 100, 300, "/home/dev/src/aterm", 8);
    title.process(b"printf '\\e]0;building'\r\n\x1b]0;building");
    assert_eq!(title.partial_sequence_state(), Some("OscString"));
    let mut ssh = fx_shell(30, 99, 120, "/home/dev", 0);
    ssh.process(b"ssh build-host\r\n\x1b[1;32mdev@build-host\x1b[0m:~$ \x1b[38;5;");
    assert_eq!(ssh.partial_sequence_state(), Some("CsiParam"));
    let mut image = fx_editor(30, 200, 3);
    image.process(b"\x1b[5;10H\x1b_Gf=100,a=T,i=7,m=1;iVBORw0KGgoAAAANSUhEUgAA");
    assert_eq!(image.partial_sequence_state(), Some("SosPmApcString"));
    let sessions = vec![
        fx_session(0, "zsh", title),
        fx_session(1, "ssh", ssh),
        fx_session(2, "vim", image),
    ];
    FxDesk {
        name: "stalled-sequence",
        nonce_seed: 0x0100_0008,
        ceiling: aterm_update_core::handoff_policy::CarryCeiling::Full,
        window: fx_window_carry(30, 200, false),
        sessions,
        layout: RestoreManifest::new(vec![fx_window(
            30,
            200,
            vec![
                fx_tab(fx_split(
                    SplitKind::Vertical,
                    fx_leaf(0, "zsh", Some("/home/dev/src/aterm")),
                    fx_leaf(1, "ssh", Some("/home/dev")),
                )),
                fx_tab(fx_leaf(2, "vim", Some("/home/dev/src"))),
            ],
        )]),
        connections: Vec::new(),
        next_turn_id: None,
        held: Vec::new(),
    }
}

/// One row of `n` OSC 8 links labelled `<label><k>`, each to a signed URL of
/// exactly `url_len` bytes: a CI tool's row of log links. The engine keeps a
/// URL of up to 8 KiB, and a line record carries each URL once per span, so a
/// dozen 8 KiB links make a record of about 96 KiB, past the wire's
/// per-record cap (`16 KiB + cols * 512`) at any width under 160 columns; two
/// stay under it.
fn fx_link_row(n: usize, url_len: usize, host: &str, label: &str) -> String {
    const SIGNATURE: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut row = String::new();
    for k in 0..n {
        let mut url = format!("https://{host}/runs/4242/jobs/{k}/logs?X-Signature=");
        let mut state = 0x9e37_79b9_u32.wrapping_add(u32::try_from(k).unwrap());
        while url.len() < url_len {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            url.push(char::from(SIGNATURE[(state >> 26) as usize]));
        }
        assert_eq!(url.len(), url_len);
        row.push_str(&format!(
            "\x1b]8;id={label}{k};{url}\x1b\\{label}{k}\x1b]8;;\x1b\\ "
        ));
    }
    row
}

/// THE SHAPE v0.97.0 CHANGED: a line record over the wire's per-record cap
/// costs that line its OSC 8 links, never the screen (b2275989d, plan P2-4;
/// the strip's work bounded by 510affa38). Through v0.95.0 such a screen went
/// to the Repaint rung, a blank tab until its program redrew, and such a line
/// only in the scrollback cost the whole scrollback (and, through the
/// capture's history latch, every later tab's). v0.97.0 carries both at
/// `CarryRung::StrippedLinks`: every character, attribute, wrap flag and
/// scalar exact, only the over-cap lines' link destinations dropped, the
/// scrollback kept, no repaint and the control carry kept.
///
/// Session 0 is a shell that listed two CI runs' signed log links: one row
/// of a dozen 8 KiB links scrolled into its history, another on screen, and
/// a row of two links under the cap, which keeps them. Its history is inside
/// the 256 lines its checkpoint carries, so it names no sidecar. Session 1 is
/// a shell with 600 lines of history that printed a row of links and then
/// opened vim in 1049: the link-dense row is on the SAVED PRIMARY (the
/// inactive grid), which the carry strips, and the `.hist` sidecar carries
/// the saved primary's whole history beside it.
fn fx_link_dense_desk() -> FxDesk {
    use crate::restore::{RestoreManifest, SplitKind};
    let prompt = |cwd: &str| format!("\x1b[1;38;5;202mdev@devhost\x1b[0m {cwd} % ");
    let mut runs = fx_shell(32, 110, 100, "/home/dev/src/aterm", 5);
    runs.process(b"gh run view 4242 --log-links\r\n");
    runs.process(
        "\x1b[1mRun 4242\x1b[0m  ci.yml  main  \x1b[32m\u{2713} success\x1b[0m\r\n".as_bytes(),
    );
    runs.process(fx_link_row(12, 8192, "ci.example.test", "job").as_bytes());
    runs.process(b"\r\n");
    for step in 0..40 {
        runs.process(
            format!("  \x1b[32m\u{2713}\x1b[0m step {step:02}: cargo test -p crate{step}\r\n")
                .as_bytes(),
        );
    }
    runs.process(
        format!(
            "{}gh run view 4243 --log-links\r\n",
            prompt("/home/dev/src/aterm")
        )
        .as_bytes(),
    );
    runs.process("\x1b[1mRun 4243\x1b[0m  ci.yml  main  \x1b[31mX failure\x1b[0m\r\n".as_bytes());
    runs.process(fx_link_row(12, 8192, "ci.example.test", "run").as_bytes());
    runs.process(b"\r\n");
    runs.process(fx_link_row(2, 8192, "docs.example.test", "doc").as_bytes());
    runs.process(b"\r\n");
    runs.process(prompt("/home/dev/src/aterm").as_bytes());

    let mut notes = fx_shell(32, 109, 600, "/home/dev/notes", 6);
    notes.process(
        b"curl -s https://ci.example.test/api/runs/4242 | jq -r '.jobs[].log' | linkify\r\n",
    );
    notes.process(fx_link_row(12, 8192, "ci.example.test", "log").as_bytes());
    notes.process(b"\r\n");
    notes.process(format!("{}vim ci.md\r\n", prompt("/home/dev/notes")).as_bytes());
    notes.process(b"\x1b[?1049h\x1b[?1h\x1b=\x1b[H\x1b[2J");
    for r in 4..32 {
        notes.process(format!("\x1b[{r};1H\x1b[34m~\x1b[0m").as_bytes());
    }
    notes.process(
        b"\x1b[1;1H# CI runs\x1b[2;1H- 4242: green\x1b[3;1H- 4243: \x1b[31mred\x1b[0m\
          \x1b[32;1H\x1b[7m ci.md [+] \x1b[0m\x1b[3;9H",
    );

    let mut sessions = vec![fx_session(0, "zsh", runs), fx_session(1, "vim", notes)];
    for session in &mut sessions {
        session.rung = CarryRung::StrippedLinks;
    }
    FxDesk {
        name: "link-dense",
        nonce_seed: 0x0100_0009,
        ceiling: aterm_update_core::handoff_policy::CarryCeiling::Full,
        window: fx_window_carry(32, 220, false),
        sessions,
        layout: RestoreManifest::new(vec![fx_window(
            32,
            220,
            vec![fx_tab(fx_split(
                SplitKind::Vertical,
                fx_leaf(0, "zsh", Some("/home/dev/src/aterm")),
                fx_leaf(1, "vim", Some("/home/dev/notes")),
            ))],
        )]),
        connections: Vec::new(),
        next_turn_id: None,
        held: Vec::new(),
    }
}

/// THE SHAPE v0.98.0 CHANGED: a link-dense line in the history a `.hist`
/// sidecar carries (190bc4d50, 8defd246f). Every export from v0.94.0 through
/// v0.97.0 wrote such a line as the engine serialized it, and v0.97.0's import
/// refused the whole sidecar for it ("the sidecar arrived with a frame that
/// does not decode"): the tab lost every line of history its sidecar carried,
/// whether the line was older than the checkpoint's 256 lines or inside them
/// (a frame is 1,024 lines, and it is decoded either way). v0.98.0's export
/// drops the links of every line whose record is over the cap before it writes
/// the frame, so its sidecars decode strictly; the line crosses with its text,
/// attributes and wrap flag exact, and every other line keeps its links.
///
/// Session 0 is a shell that listed two CI runs' signed log links, 400 lines
/// apart: the first row of a dozen 8 KiB links is older than the 256 lines its
/// checkpoint carries, so it is in the part of the sidecar the successor takes;
/// the second is inside them, so the screen carry strips it
/// (`CarryRung::StrippedLinks`, one line) and the sidecar holds it stripped
/// too, past its take. A row of two links stays under the cap and keeps them
/// everywhere. Session 1 is a shell with 500 lines of history that printed a
/// row of a dozen links, 80 more lines, and then opened vim in 1049: its
/// checkpoint carries none of the saved primary's history and the saved
/// primary's screen holds no dense row, so it is carried at `Full`, and its
/// sidecar carries the dense row stripped among the rest. Sessions 0 and 1
/// cross with 2 and 1 sidecar lines stripped.
///
/// The window's second tab is Settings, holding an unsaved font-family draft:
/// v0.98.0's handoff layout carries Settings drafts on the leaf (37294ce2f,
/// plan P2-2).
fn fx_link_dense_history_desk() -> FxDesk {
    use crate::restore::{
        NativeLeafRestore, RestoreManifest, RestoredSplitTree, RestoredView, SettingsDraftRestore,
        SplitKind,
    };
    let prompt = |cwd: &str| format!("\x1b[1;38;5;202mdev@devhost\x1b[0m {cwd} % ");
    let mut runs = fx_shell(32, 110, 300, "/home/dev/src/aterm", 7);
    runs.process(b"gh run view 4250 --log-links\r\n");
    runs.process(
        "\x1b[1mRun 4250\x1b[0m  ci.yml  main  \x1b[32m\u{2713} success\x1b[0m\r\n".as_bytes(),
    );
    runs.process(fx_link_row(12, 8192, "ci.example.test", "job").as_bytes());
    runs.process(b"\r\n");
    for step in 0..400 {
        runs.process(
            format!(
                "  \x1b[32m\u{2713}\x1b[0m step {step:03}: cargo test -p crate{} -- case_{step:03} \x1b[2m({} ms)\x1b[0m\r\n",
                step % 40,
                step * 3 % 97
            )
            .as_bytes(),
        );
    }
    runs.process(
        format!(
            "{}gh run view 4251 --log-links\r\n",
            prompt("/home/dev/src/aterm")
        )
        .as_bytes(),
    );
    runs.process("\x1b[1mRun 4251\x1b[0m  ci.yml  main  \x1b[31mX failure\x1b[0m\r\n".as_bytes());
    runs.process(fx_link_row(12, 8192, "ci.example.test", "run").as_bytes());
    runs.process(b"\r\n");
    runs.process(fx_link_row(2, 8192, "docs.example.test", "doc").as_bytes());
    runs.process(b"\r\n");
    for line in 0..60 {
        runs.process(
            format!(
                "\x1b[31merror\x1b[0m[E0308]: mismatched types in crate{} at line {line}\r\n",
                line % 7
            )
            .as_bytes(),
        );
    }
    runs.process(prompt("/home/dev/src/aterm").as_bytes());

    let mut notes = fx_shell(32, 109, 500, "/home/dev/notes", 8);
    notes.process(
        b"curl -s https://ci.example.test/api/runs/4251 | jq -r '.jobs[].log' | linkify\r\n",
    );
    notes.process(fx_link_row(12, 8192, "ci.example.test", "log").as_bytes());
    notes.process(b"\r\n");
    for line in 0..80 {
        notes.process(
            format!(
                "- job {line:02}: \x1b[32mpassed\x1b[0m in {}s\r\n",
                30 + line % 50
            )
            .as_bytes(),
        );
    }
    notes.process(format!("{}vim ci.md\r\n", prompt("/home/dev/notes")).as_bytes());
    notes.process(b"\x1b[?1049h\x1b[?1h\x1b=\x1b[H\x1b[2J");
    for r in 4..32 {
        notes.process(format!("\x1b[{r};1H\x1b[34m~\x1b[0m").as_bytes());
    }
    notes.process(
        b"\x1b[1;1H# CI runs\x1b[2;1H- 4250: green\x1b[3;1H- 4251: \x1b[31mred\x1b[0m\
          \x1b[32;1H\x1b[7m ci.md [+] \x1b[0m\x1b[3;9H",
    );

    let mut sessions = vec![fx_session(0, "zsh", runs), fx_session(1, "vim", notes)];
    sessions[0].rung = CarryRung::StrippedLinks;
    for session in &mut sessions {
        session.taken_links_stripped = 1;
    }

    // A HAND-BUILT Settings leaf, spelled as `App::capture_handoff_layout`
    // spells one (`NativeLeafRestore::settings` with its route, then the one
    // unsaved text draft the view carries). The layout around it is hand-built
    // too (`fx_window`), with the legacy mirrors a real `App::capture_manifest`
    // fills: the Settings tab is `native_tabs`' one descriptor, second in
    // `tab_order`.
    let draft = SettingsDraftRestore {
        key: "font_family".to_string(),
        text: "Iosevka Term".to_string(),
    };
    assert!(crate::restore::settings_draft_is_carriable(
        &draft.key,
        &draft.text
    ));
    let mut settings = NativeLeafRestore::settings("/text-fonts".to_string());
    settings.settings_drafts = vec![draft];
    FxDesk {
        name: "link-dense-history",
        nonce_seed: 0x0100_0010,
        ceiling: aterm_update_core::handoff_policy::CarryCeiling::Full,
        window: fx_window_carry(32, 220, false),
        sessions,
        layout: RestoreManifest::new(vec![fx_window(
            32,
            220,
            vec![
                fx_tab(fx_split(
                    SplitKind::Vertical,
                    fx_leaf(0, "zsh", Some("/home/dev/src/aterm")),
                    fx_leaf(1, "vim", Some("/home/dev/notes")),
                )),
                fx_tab(RestoredSplitTree::leaf(RestoredView::Native(settings))),
            ],
        )]),
        connections: Vec::new(),
        next_turn_id: None,
        held: Vec::new(),
    }
}

/// A shell under aterm's signed integration with `commands` finished commands
/// and a `cargo build` still running (OSC 133 phase 3), so the successor must
/// complete a command the parent started.
fn fx_building_shell(
    rows: u16,
    cols: u16,
    nonce: [u8; 32],
    commands: usize,
    flavour: u8,
) -> Terminal {
    let (mut build, sign) = fx_marked_shell(
        rows,
        cols,
        Some(nonce),
        false,
        b"",
        commands,
        "/home/dev/src/aterm",
        flavour,
    );
    build.process(
        format!(
            "\x1b]633;E;cargo build --release{sign}\x07cargo build --release\r\n\
             \x1b]133;C{sign}\x07"
        )
        .as_bytes(),
    );
    for k in 0..6 {
        build.process(
            format!(
                "\x1b[1;32m   Compiling\x1b[0m crate{k} v0.1.0 (/home/dev/src/aterm/crates/crate{k})\r\n"
            )
            .as_bytes(),
        );
    }
    build.process(b"\x1b[1;36m    Building\x1b[0m [==>           ] 6/40: crate6");
    build
}

/// THE SHAPE v0.97.0 ADDED, at `carry = "visible"` (ported from
/// `generator-v0.97.0-policy.rs.txt`): the successor's signed handoff policy
/// asks for the visible screen and its scalars, never the scrollback. Every
/// session is carried at `VisibleOnly`, no `.hist` sidecar is named, and the
/// join counts every line the park saw as left behind, as the policy's
/// (`history_dropped`, `history_lost` on top of the running loss from an
/// earlier handoff, and since v0.98.0 `history_withheld`). The control carry
/// still crosses: the screen it describes is exact. Session 0 is a shell with
/// 1,500 lines of history, session 1 a shell under aterm's signed integration
/// with a command still running (its marks sit on the visible rows the carry
/// keeps), session 2 Claude Code in 1049 with a turn in its ledger.
fn fx_policy_visible_desk() -> FxDesk {
    use crate::restore::{RestoreManifest, SplitKind};
    let deep = fx_shell(42, 120, 1500, "/home/dev/src/aterm", 6);
    let building = fx_building_shell(42, 119, [0x97; 32], 60, 3);
    let mut sessions = vec![
        fx_session(0, "zsh", deep),
        fx_session(1, "cargo", building),
        fx_claude(2, 42, 240, 30),
    ];
    for session in &mut sessions {
        session.rung = CarryRung::VisibleOnly;
    }
    sessions[0].record.history_lost = 12;
    sessions[1].record.rekey = true;
    sessions[1].record.loader = true;
    FxDesk {
        name: "policy-visible",
        nonce_seed: 0x0100_0011,
        ceiling: aterm_update_core::handoff_policy::CarryCeiling::Visible,
        window: fx_window_carry(42, 240, true),
        sessions,
        layout: RestoreManifest::new(vec![fx_window_in(
            42,
            240,
            1,
            vec![
                fx_tab(fx_split(
                    SplitKind::Vertical,
                    fx_leaf(0, "zsh", Some("/home/dev/src/aterm")),
                    fx_leaf(1, "cargo", Some("/home/dev/src/aterm")),
                )),
                fx_tab(fx_leaf(2, "claude", Some("/home/dev/src/aterm"))),
            ],
        )]),
        connections: Vec::new(),
        next_turn_id: Some(41),
        held: Vec::new(),
    }
}

/// THE SHAPE v0.97.0 ADDED, at `carry = "repaint"` (ported from
/// `generator-v0.97.0-policy.rs.txt`): every session is carried as a blank
/// canonical screen its program redraws, whose scalars come from
/// `Terminal::carry_meta_abandoning_partial` with NO grid projected (the path
/// such a policy is sealed to route around). Every session is in the repaint
/// set, no control carry crosses (the screen it would describe was not
/// carried), and no history does: every line is counted, and said as the
/// policy's. Session 0 is a shell with 1,500 lines of history, session 1 vim
/// in 1049 (so its blank carry has an inactive grid), session 2 a shell
/// stalled inside an unterminated OSC title (its meta abandons the partial
/// sequence, as CAN would), session 3 Claude Code in 1049 with a turn in its
/// ledger.
fn fx_policy_repaint_desk() -> FxDesk {
    use crate::restore::{RestoreManifest, SplitKind};
    let deep = fx_shell(40, 120, 1500, "/home/dev/src/aterm", 6);
    let editor = fx_editor(40, 119, 4);
    let mut stalled = fx_shell(40, 120, 200, "/home/dev/src", 8);
    stalled.process(b"printf '\\e]0;deploying'\r\n\x1b]0;deploying");
    assert_eq!(stalled.partial_sequence_state(), Some("OscString"));
    let mut sessions = vec![
        fx_session(0, "zsh", deep),
        fx_session(1, "vim", editor),
        fx_session(2, "zsh", stalled),
        fx_claude(3, 40, 119, 20),
    ];
    for session in &mut sessions {
        session.rung = CarryRung::Repaint;
    }
    sessions[1].record.user_title = Some("notes".to_string());
    FxDesk {
        name: "policy-repaint",
        nonce_seed: 0x0100_0012,
        ceiling: aterm_update_core::handoff_policy::CarryCeiling::Repaint,
        window: fx_window_carry(40, 240, false),
        sessions,
        layout: RestoreManifest::new(vec![fx_window(
            40,
            240,
            vec![
                fx_tab(fx_split(
                    SplitKind::Vertical,
                    fx_leaf(0, "zsh", Some("/home/dev/src/aterm")),
                    fx_leaf(1, "vim", Some("/home/dev/src/aterm")),
                )),
                fx_tab(fx_split(
                    SplitKind::Vertical,
                    fx_leaf(2, "zsh", Some("/home/dev/src")),
                    fx_leaf(3, "claude", Some("/home/dev/src/aterm")),
                )),
            ],
        )]),
        connections: Vec::new(),
        next_turn_id: Some(41),
        held: Vec::new(),
    }
}

/// v0.100.0's new shape: panes kept open by `--hold` after their command
/// exited (round five, item 19; round six of the update audit, findings 21,
/// 46 and 49). They are in the layout but never handed (no process, no PTY),
/// so they have no session record; their final screens ride the manifest's
/// `held` list, each with how its command ended, in `.h<id>.heldgrid` (and,
/// for a screen left in 1049, `.heldalt`) sidecars outside every digest. The
/// first tab is a live shell beside `make`, which failed with status 2 after
/// printing a link to its log; the second is a live shell above vim, killed
/// by SIGKILL in 1049. The successor shows each held screen read-only, with
/// no link to click; one that skips the list keeps its placeholder.
fn fx_held_pane_desk() -> FxDesk {
    use crate::restore::{RestoreManifest, SplitKind};
    let mut make = fx_shell(ROWS, COLS, 30, "/home/dev/src/aterm", 4);
    make.process(b"make test\r\n");
    for i in 0..6 {
        make.process(
            format!("   Compiling crate-{i} v0.1.0 (/home/dev/src/aterm/crates/crate-{i})\r\n")
                .as_bytes(),
        );
    }
    make.process(
        b"see \x1b]8;;https://ci.example.invalid/run/4521/log\x1b\\the log\x1b]8;;\x1b\\ for details\r\n",
    );
    make.process(b"\x1b[1;31merror\x1b[0m: test failed, to rerun pass `-p crate-3 --lib`\r\n");
    make.process(b"make: *** [test] Error 2\r\n");
    let vim = fx_editor(ROWS, COLS, 6);
    FxDesk {
        name: "held-pane",
        nonce_seed: 0x0100_0013,
        ceiling: aterm_update_core::handoff_policy::CarryCeiling::Full,
        window: fx_window_carry(ROWS, COLS * 2 + 1, false),
        sessions: vec![
            fx_session(
                0,
                "zsh",
                fx_shell(ROWS, COLS, 200, "/home/dev/src/aterm", 1),
            ),
            fx_session(1, "zsh", fx_shell(ROWS, COLS, 80, "/home/dev", 7)),
        ],
        layout: RestoreManifest::new(vec![fx_window(
            ROWS,
            COLS * 2 + 1,
            vec![
                fx_tab(fx_split(
                    SplitKind::Vertical,
                    fx_leaf(0, "zsh", Some("/home/dev/src/aterm")),
                    fx_leaf(2, "make", Some("/home/dev/src/aterm")),
                )),
                fx_tab(fx_split(
                    SplitKind::Horizontal,
                    fx_leaf(1, "zsh", Some("/home/dev")),
                    fx_leaf(3, "vim", Some("/home/dev/src")),
                )),
            ],
        )]),
        connections: Vec::new(),
        next_turn_id: None,
        // Since v0.101.0 each held pane's scrollback and control carry cross
        // too, and whether the person had seen its failure: `make` sits in the
        // tab in front, beside the focused shell, so its failure was seen; vim
        // was killed in the tab behind it.
        held: vec![
            FxHeld {
                local_id: 2,
                term: Arc::new(Mutex::new(make)),
                turns: Arc::new(Mutex::new(TurnLedger::default())),
                exit: Some(aterm_pty::ChildExit::Code(2)),
                failure_seen: true,
            },
            FxHeld {
                local_id: 3,
                term: Arc::new(Mutex::new(vim)),
                turns: Arc::new(Mutex::new(TurnLedger::default())),
                exit: Some(aterm_pty::ChildExit::Signal(9)),
                failure_seen: false,
            },
        ],
    }
}

#[test]
#[ignore = "writes release fixtures; run by name with --ignored"]
fn generate_handoff_fixtures() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = restore_env();
    fx_install_release_stamp();
    let scratch =
        std::env::temp_dir().join(format!("aterm-handoff-fixture-gen-{}", std::process::id()));
    for desk in [
        fx_incident_desk(),
        fx_claude_desk(),
        fx_history_desk(),
        fx_twelve_pane_desk(),
        fx_shell_integration_desk(),
        fx_history_carry_desk(),
        fx_colour_and_shell_desk(),
        fx_stalled_sequence_desk(),
        fx_link_dense_desk(),
        fx_link_dense_history_desk(),
        fx_policy_visible_desk(),
        fx_policy_repaint_desk(),
        fx_held_pane_desk(),
    ] {
        fx_write(&desk, &scratch);
    }
    let _ = std::fs::remove_dir_all(&scratch);
}

// ============================================ THE RENDEZVOUS TRANSPORT CAPTURE
//
// Item 11 of the fifth update-robustness round. It writes
// `$ATERM_RENDEZVOUS_FIXTURE_OUT/rendezvous.toml`: the claim frame this
// release's dialer writes for a fixed secret; the grant header and body its
// parent sends for a fixed nonce and sessions [(0,4000),(1,4001),(7,4002)],
// and the order its descriptors arrive in, observed by a real receiver over a
// real bound `Rendezvous` with openpty masters and two pipes; the launch
// environment's names and value shapes; its own `ATERM_SEAMLESS_TARGET` and
// `ATERM_HANDOFF_PARENT_BIRTH` values; and the adoption proof over the
// recorded `st_rdev` terms. The capture is the guard's own
// (`fixture_tests::capture_transport`), over the guard's fixed inputs, so the
// record and the guard's reading of this build can never drift apart. The
// guard is `seamless::fixture_tests::every_shipped_producers_rendezvous_grants_this_successor`.

#[cfg(target_os = "macos")]
mod rendezvous_generator {
    use crate::seamless::fixture_tests::{
        RZ_NONCE, RZ_SECRET, RZ_SESSIONS, capture_transport, hex_of, open_pty_pair,
    };

    /// `value` as a TOML basic string.
    fn toml_string(value: &str) -> String {
        let mut out = String::from("\"");
        for ch in value.chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                ch if ch.is_control() => out.push_str(&format!("\\u{:04X}", u32::from(ch))),
                ch => out.push(ch),
            }
        }
        out.push('"');
        out
    }

    /// The literal a match arm of this release's own `app_update_handoff.rs`
    /// gives the proof term for `lane` (`HandoffLane::OutOfBand => "device"`):
    /// the parent writes that literal into the successor's environment, and no
    /// function returns it.
    fn proof_term_of(lane: &str) -> String {
        let source = include_str!("app_update_handoff.rs");
        let arm = format!("HandoffLane::{lane} => \"");
        let start = source
            .find(&arm)
            .unwrap_or_else(|| panic!("the release names the {lane} proof term"))
            + arm.len();
        let len = source[start..].find('"').expect("a closed literal");
        source[start..start + len].to_string()
    }

    /// WRITE `$ATERM_RENDEZVOUS_FIXTURE_OUT/rendezvous.toml` with THIS
    /// release's claim, grant, descriptor order, launch environment and
    /// device-term proof over the fixed inputs.
    #[test]
    #[ignore = "writes a release fixture; run by name with --ignored"]
    fn generate_rendezvous_fixture() {
        let _env = super::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        super::fx_install_release_stamp();
        let out = std::path::PathBuf::from(
            std::env::var_os("ATERM_RENDEZVOUS_FIXTURE_OUT")
                .expect("set ATERM_RENDEZVOUS_FIXTURE_OUT to tests/fixtures/handoff/v<release>"),
        );
        let scratch = std::env::temp_dir().join(format!("aterm-rz-gen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).expect("scratch");
        let capture = capture_transport(&scratch);
        let _ = std::fs::remove_dir_all(&scratch);
        for slot in &capture.descriptor_order {
            assert!(
                !slot.starts_with("unknown"),
                "every descriptor was identified: {:?}",
                capture.descriptor_order
            );
        }

        let build = crate::running_build_number();
        let commit = crate::build_info::git_commit();
        assert!(
            commit.len() == 12 && commit.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "ATERM_BUILD_GIT_COMMIT set to the release's 12-digit commit, not {commit:?}"
        );
        let target_value = crate::seamless::encode_target_identity(build, commit);
        let parent_birth_value = crate::seamless::outgoing_parent_env()
            .into_iter()
            .find(|(key, _)| *key == crate::seamless::ENV_PARENT_BIRTH.name())
            .and_then(|(_, value)| value.into_string().ok())
            .expect("the kernel's birth record for this process");

        let ptys = RZ_SESSIONS
            .iter()
            .map(|_| open_pty_pair())
            .collect::<Vec<_>>();
        let identities = RZ_SESSIONS
            .iter()
            .zip(&ptys)
            .map(|((local_id, pid), (master, _))| (*local_id, *master, *pid))
            .collect::<Vec<_>>();
        let terms = crate::handoff_rendezvous::proof_identities_in_device_terms(&identities)
            .expect("every master answers fstat");
        for (master, slave) in ptys {
            aterm_pty::close_fd(master);
            aterm_pty::close_fd(slave);
        }
        let layout = [0x11u8; 32];
        let screen = [0x22u8; 32];
        let proof =
            crate::seamless::adoption_proof(RZ_NONCE, build, commit, &layout, &screen, &terms)
                .expect("a proof over the fixed inputs");

        let mut toml = String::new();
        toml.push_str(
            "# What this release's rendezvous transport does over fixed inputs. Written by\n\
             # `generate_rendezvous_fixture` (the release's own cut); see ../README.md,\n\
             # \"The rendezvous transport\".\n",
        );
        let mut line = |key: &str, value: String| toml.push_str(&format!("{key} = {value}\n"));
        line(
            "producer_version",
            toml_string(crate::build_info::version()),
        );
        line("producer_commit", toml_string(commit));
        line("claim_input", toml_string(RZ_SECRET));
        line(
            "claim_frame_hex",
            toml_string(&hex_of(&capture.claim_frame)),
        );
        line("grant_nonce", toml_string(RZ_NONCE));
        line(
            "grant_sessions",
            format!(
                "[{}]",
                RZ_SESSIONS
                    .iter()
                    .map(|(local_id, pid)| format!("[{local_id}, {pid}]"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
        line(
            "grant_header_hex",
            toml_string(&hex_of(&capture.grant_header)),
        );
        line("grant_body", toml_string(&capture.grant_body));
        line(
            "descriptor_order",
            format!(
                "[{}]",
                capture
                    .descriptor_order
                    .iter()
                    .map(|slot| toml_string(slot))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
        line(
            "env_rendezvous",
            toml_string(crate::handoff_rendezvous::ENV_RENDEZVOUS.name()),
        );
        line(
            "rendezvous_file_shape",
            toml_string(&capture.rendezvous_file_shape),
        );
        line(
            "env_claim",
            toml_string(crate::handoff_rendezvous::ENV_CLAIM.name()),
        );
        line("claim_hex_len", capture.claim_hex_len.to_string());
        line(
            "env_proof_term",
            toml_string(crate::handoff_rendezvous::ENV_PROOF_TERM.name()),
        );
        line(
            "proof_term_launched",
            toml_string(&proof_term_of("OutOfBand")),
        );
        line("proof_term_fork", toml_string(&proof_term_of("Fork")));
        line(
            "env_target",
            toml_string(crate::seamless::ENV_TARGET.name()),
        );
        line("target_value", toml_string(&target_value));
        line(
            "env_parent_birth",
            toml_string(crate::seamless::ENV_PARENT_BIRTH.name()),
        );
        line("parent_birth_value", toml_string(&parent_birth_value));
        line("proof_layout_digest", toml_string(&hex_of(&layout)));
        line("proof_screen_digest", toml_string(&hex_of(&screen)));
        line(
            "proof_terms",
            format!(
                "[{}]",
                terms
                    .iter()
                    .map(|(local_id, rdev, pid)| format!("[{local_id}, {rdev}, {pid}]"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
        line("proof_count", proof.count.to_string());
        line("proof_digest", toml_string(&hex_of(&proof.digest)));
        std::fs::create_dir_all(&out).expect("the release's fixture dir");
        std::fs::write(out.join("rendezvous.toml"), toml).expect("write rendezvous.toml");
    }
}
