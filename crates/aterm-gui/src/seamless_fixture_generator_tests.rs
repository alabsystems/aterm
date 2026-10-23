// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Handoff fixture generator for seamless upgrade compatibility tests.

use super::*;

/// Keep the installed build stamp and generator environment out of sibling
/// tests.
fn fx_generator_in_child(output: &str) -> bool {
    if crate::test_alone::in_child() {
        return true;
    }
    let scratch = aterm_tempfile::tempdir().expect("private fixture destination");
    let mut vars = vec![];
    let destination = if let Some(out) = std::env::var_os(output) {
        std::path::PathBuf::from(out)
    } else {
        vars.extend([
            (
                "ATERM_BUILD_GIT_COMMIT",
                std::ffi::OsString::from(crate::build_info::git_commit()),
            ),
            (
                "ATERM_BUILD_GIT_COMMIT_FULL",
                std::ffi::OsString::from(crate::build_info::git_commit_full()),
            ),
            (
                "SOURCE_DATE_EPOCH",
                std::ffi::OsString::from(crate::running_build_number().to_string()),
            ),
        ]);
        scratch.path().to_path_buf()
    };
    vars.push((output, destination.as_os_str().to_owned()));
    let env: Vec<_> = vars.iter().map(|(k, v)| (*k, v.as_os_str())).collect();
    crate::test_alone::run_alone_with(&env);
    assert!(
        std::fs::read_dir(&destination)
            .expect("generator destination exists")
            .next()
            .is_some(),
        "the generator wrote fixtures"
    );
    false
}

const FIXTURE_DIR_PLACEHOLDER: &str = "@HANDOFF_DIR@";
/// The successor these desks hand to: a build a day newer than this release's own, the hop every
/// install of this release makes.
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
/// The exit ledger's last row id each desk's store carries (`SessionHandoff::roster_seq`): seven
/// shells exited before the park.
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

/// The release's own build stamp, installed before anything reads provenance. The shipped binary's
/// stamp comes from `crates/aterm/build.rs` (installed by `main`) and this test binary reads a
/// fixed one, so the generator reads the three pins the release build hands that script
/// (`ATERM_BUILD_GIT_COMMIT`, `ATERM_BUILD_GIT_COMMIT_FULL`, `SOURCE_DATE_EPOCH`) and installs what
/// it stamps from them: a clean tree at that commit.
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

/// The release's compiler: `trustc -vV` of the release's prover (`$TRUST_STAGE2_BIN`, which the
/// cutter builds the release with and hands every child), else the `trustc` on PATH.
fn fx_release_compiler() -> FxCompiler {
    let trustc = std::env::var_os("TRUST_STAGE2_BIN")
        .filter(|bin| !bin.is_empty())
        .map_or_else(
            || std::path::PathBuf::from("trustc"),
            |bin| std::path::PathBuf::from(bin).join("trustc"),
        );
    let out = match aterm_uds::spawnfd::command(&trustc).arg("-vV").output() {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && trustc.is_relative() => {
            aterm_uds::host::could_not_run(format_args!(
                "no {} here: set $TRUST_STAGE2_BIN or put trustc on PATH",
                trustc.display()
            ))
        }
        Err(e) => panic!("the release's compiler: {} -vV: {e}", trustc.display()),
    };
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
    /// Its timeline ([`fx_timeline`]): its session log rides the control
    /// carry and the record, and its epoch the layout leaf.
    timeline: Arc<Mutex<crate::session_timeline::SessionTimeline>>,
    /// The rung this release's ladder carries this session at: `Full` for every honest desk,
    /// `StrippedLinks` for a screen holding a line record over the wire's per-record cap.
    rung: CarryRung,
    /// How many of the lines the `.hist` sidecar carries for import (its first `take`) lose their
    /// links to the export: `0` on every desk but the one for that shape.
    taken_links_stripped: usize,
}

struct FxDesk {
    name: &'static str,
    nonce_seed: u128,
    /// The carry ceiling of the successor's handoff policy this desk is parked under: `Full` for
    /// the release's own checked-in (empty) policy, `Visible` or `Repaint` for a successor whose
    /// signed policy asks for that (`fx_park_policy` lays the file out and reads it either way).
    ceiling: aterm_update_core::handoff_policy::CarryCeiling,
    window: WindowCarry,
    sessions: Vec<FxSession>,
    layout: crate::restore::RestoreManifest,
    connections: Vec<ConnectionCarry>,
    next_turn_id: Option<u64>,
    /// The bridge `fabric attach` armed beyond the configured one, as the
    /// park draws it (`fabric_launch::handoff_attached`): empty on every desk
    /// but `carried-state`.
    fabric_attached: Vec<String>,
    /// The panes kept open after their command exited (`--hold`): in the
    /// layout, never handed, their final screens carried beside the
    /// manifest since v0.100.0.
    held: Vec<FxHeld>,
    /// What the park draws from its store beside the records
    /// (`SessionHandoff::from_store`): nothing on every desk but
    /// `carried-state`.
    drawn: FxDrawn,
}

/// The rows a park draws from the store and the status item rather than
/// from a session (`SessionHandoff::from_store`, `App::fleet_told_rows`):
/// the fleet needs whose notice the owner was told, the bus mirror's closed
/// sessions, and whether a bridge served the mirror.
#[derive(Default)]
struct FxDrawn {
    fleet_told: Vec<String>,
    mirror_dropped: Vec<String>,
    mirror_served: bool,
}

/// The names the kernel gave every desk's machine as the park reads them
/// (`tab_label::machine_names_for_handoff`), which every park writes: the
/// name each desk's shells report in their OSC 7 (`file://devhost/…`), and
/// the name the network gave the machine since they started.
fn fx_machine_names() -> Vec<String> {
    ["devhost", "devhost-2.local"].map(String::from).to_vec()
}

/// One held pane: the pool id its layout leaf names, its engine and turn ledger, how its command
/// ended as the outgoing process collected it (`App::exit_status`), and whether the person had
/// looked at the failure its exit marks the tab for (`StatusObserver::failure_seen`).
struct FxHeld {
    local_id: u64,
    term: Arc<Mutex<Terminal>>,
    turns: Arc<Mutex<TurnLedger>>,
    /// A held pane is a pooled session too: its leaf names its log's epoch.
    timeline: Arc<Mutex<crate::session_timeline::SessionTimeline>>,
    exit: Option<aterm_pty::ChildExit>,
    failure_seen: bool,
    /// A resize that reached the pane after its held capture and before the
    /// history export did (`App::history_sessions` exports after the park):
    /// the fence the capture took no longer names its lines, so its scrollback
    /// is left behind as a failure (`HeldLoss::Failed`).
    resized: Option<(u16, u16)>,
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
        topics: Vec::new(),
        fg_holder: None,
        rekey: false,
        loader: false,
        history: None,
        history_dropped: 0,
        history_withheld: false,
        history_lost: 0,
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
        render_debt: None,
        render_debt_cut: 0,
        journal_key: None,
        task: None,
        status_seen: None,
        autopilot_end: None,
    }
}

/// A pooled session's timeline as its registration leaves it
/// (`SessionStore::register`): `spawned` recorded and its session log's
/// session node opened, the log at this process's next epoch
/// (`SessionTimeline::default`).
fn fx_timeline() -> Arc<Mutex<crate::session_timeline::SessionTimeline>> {
    let mut timeline = crate::session_timeline::SessionTimeline::default();
    timeline.record("spawned", "state=alive".to_string());
    let at = timeline.slog().now();
    timeline.slog_mut().open_session(None, None, at);
    Arc::new(Mutex::new(timeline))
}

fn fx_session(local_id: u64, title: &str, t: Terminal) -> FxSession {
    FxSession {
        record: fx_record(local_id, title),
        term: Arc::new(Mutex::new(t)),
        turns: Arc::new(Mutex::new(TurnLedger::default())),
        timeline: fx_timeline(),
        rung: CarryRung::Full,
        taken_links_stripped: 0,
    }
}

fn fx_leaf(local_id: u64, title: &str, cwd: Option<&str>) -> crate::restore::RestoredSplitTree {
    use crate::restore::{RestoredSplitTree, RestoredView, TerminalLeafRestore};
    RestoredSplitTree::leaf(RestoredView::Terminal(TerminalLeafRestore {
        sid: None,
        lineage: None,
        log_ep: None,
        cwd: cwd.map(str::to_string),
        title: title.to_string(),
        profile: None,
        local_id: Some(local_id),
        user_title: None,
        description: None,
        icon: None,
        role: None,
        attention: None,
        theme: None,
        questions: None,
        autopilot: None,
        land_at: None,
        autopilot_by: None,
        agent: None,
        supervised: None,
        lost: None,
        held: false,
        holds: Vec::new(),
    }))
}

/// EVERY TERMINAL LEAF AS THE HANDOFF'S CAPTURE WRITES IT
/// (`App::view_restore_descriptor_carrying`), off the pooled session it
/// names: its sid, the first session of the restores it continues (its
/// lineage) and the epoch its log is at (written by every capture since the
/// session log's phase 2A and 6A-2), and the operator's
/// metadata in lockstep with the session's record (drawn from `SessionMeta`).
fn fx_capture_leaves(desk: &mut FxDesk) {
    use crate::restore::{RestoredSplitTree, RestoredView};
    fn walk(node: &mut RestoredSplitTree, fill: &dyn Fn(&mut crate::restore::TerminalLeafRestore)) {
        match node {
            RestoredSplitTree::Leaf {
                view: RestoredView::Terminal(leaf),
            } => fill(leaf),
            RestoredSplitTree::Leaf { .. } => {}
            RestoredSplitTree::Split { first, second, .. } => {
                walk(first, fill);
                walk(second, fill);
            }
        }
    }
    let sessions = &desk.sessions;
    let held = &desk.held;
    let fill = |leaf: &mut crate::restore::TerminalLeafRestore| {
        let Some(id) = leaf.local_id else {
            return;
        };
        let (record, timeline) = if let Some(s) = sessions.iter().find(|s| s.record.local_id == id)
        {
            (s.record.clone(), &s.timeline)
        } else if let Some(pane) = held.iter().find(|pane| pane.local_id == id) {
            (fx_record(id, ""), &pane.timeline)
        } else {
            return;
        };
        let (lineage, log_ep) = {
            let timeline = timeline.lock().unwrap();
            (timeline.slog().lineage(), timeline.slog().log().ep())
        };
        leaf.lineage = lineage
            .map(|lineage| lineage.as_str().to_string())
            .filter(|lineage| *lineage != record.sid && crate::restore::carried_sid(lineage));
        leaf.sid = Some(record.sid);
        leaf.log_ep = Some(log_ep);
        leaf.user_title = record.user_title;
        leaf.description = record.description;
        leaf.icon = record.icon;
        leaf.role = record.role;
        leaf.attention = record.attention;
        leaf.questions = record.questions;
        leaf.autopilot = record.autopilot;
        leaf.land_at = record.land_at;
    };
    for window in &mut desk.layout.windows {
        for tab in &mut window.restored_tabs {
            walk(&mut tab.root, &fill);
        }
    }
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

/// A restored tree's legacy mirror (`PaneLayout`), as `PaneTree::to_layout` writes it: the
/// divider's orientation where the restored tree names the children's (children side by side are
/// the restored `Horizontal` and the legacy `Vertical`), and the tab's focused leaf marked.
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

fn fx_window(
    rows: u16,
    cols: u16,
    tabs: Vec<crate::restore::RestoredTab>,
) -> crate::restore::WindowLayout {
    fx_window_in(rows, cols, 0, tabs)
}

/// A window's raw inner size as this release's capture reads it (`WindowState::win_px`, written as
/// `WindowLayout::inner_px` and, for window 0, `WindowCarry::inner_px`): an unzoomed window is
/// exactly the frame of its grid and the `band_rows` message-band rows above it
/// (`App::window_frame_px_for`), at this build's default metrics.
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
        let mut node = &tab.root;
        for branch in &tab.focused_path {
            let RestoredSplitTree::Split { first, second, .. } = node else {
                break;
            };
            node = match branch {
                crate::restore::RestoreBranch::First => first,
                crate::restore::RestoreBranch::Second => second,
            };
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
            let descriptor = match (native.restore_tag.as_str(), &native.route, &native.uri) {
                ("settings", Some(route), _) => NativeTabRestore::Settings {
                    route: route.clone(),
                },
                ("markdown", None, Some(uri)) => NativeTabRestore::Markdown { uri: uri.clone() },
                ("editor", None, Some(uri)) => NativeTabRestore::Editor { uri: uri.clone() },
                ("system-state", None, None) => NativeTabRestore::SystemState,
                other => panic!("a native tab this generator does not spell: {other:?}"),
            };
            if descriptor.joins_mirror(&native_tabs) {
                tab_order.push(TabOrderEntry::Native {
                    index: native_tabs.len(),
                });
                native_tabs.push(descriptor);
            }
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
        inner_px: fx_inner_px(rows, cols, band_rows),
        presence_row: false,
        status_bar_rows: band_rows,
        update_verified_unix_ms: installing.then_some((fx_target_build() + 3_600) * 1_000),
        messages: carried.messages,
        next_message_id: carried.next_message_id,
        font_px_milli: None,
        font_reset_px_milli: None,
        font_size_milli: None,
        font_reset_size_milli: None,
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
        text: TURN_TEXT.into(),
        screen_hash: 2,
        seq: t.content_seq(),
        arch,
        by: crate::turn_ledger::TurnBy::Ctl,
        carried: false,
    });
    FxSession {
        record: fx_record(local_id, "claude"),
        term: Arc::new(Mutex::new(t)),
        turns: Arc::new(Mutex::new(ledger)),
        timeline: fx_timeline(),
        rung: CarryRung::Full,
        taken_links_stripped: 0,
    }
}

/// The park's policy, as an install of this release reads it. The cut copies the successor's
/// `publish/handoff-policy.toml` into its bundle at `BUNDLE_PATH`; the producer's pre-verification
/// reads it there (`handoff_policy::read_from_bundle`, after the codesign check) and keeps what it
/// must follow (`handoff_policy::adopt`), which every park reads by the attempt's key
/// (`App::park_policy`).
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
    /// The sessions whose parser the capture found mid-sequence, the state it was in, and whether
    /// the carry holds that partial state (false for a hooked DCS, which no desk stands for).
    abandoned: Vec<(u64, &'static str, bool)>,
    /// The rung each session was carried at, and how many of its lines lost
    /// their links (`StrippedLinks`).
    rungs: Vec<(u64, CarryRung, usize)>,
    /// The rows the engine owed each carried alternate screen (v0.101.0's
    /// `ParkedScreens::render_debts`), only where not zero.
    render_debts: Vec<(u64, aterm_core::terminal::RenderDebt)>,
    /// The held panes' screens (`App::capture_held_screens`), captured last,
    /// priced from what the handed screens left of the aggregate cells; their
    /// control carries are in `sources`.
    held: Vec<HeldScreen>,
}

/// This release's capture (`App::capture_parked_screens`) over a desk whose time is ample, on the
/// fork lane, under the park's policy `ceiling`: the pool walked in session-id order, its
/// mandatory visible grids reserved first, then each session carried by `carry_for_wire_within`
/// with 256 lines of history (none under a ceiling that carries no scrollback) when the cell and
/// byte budgets admit it after that reserve, its history head and (at a rung that keeps it) its
/// control carry's head taken under the same lock; then the pool committed by
/// `settle_wire_carries`, and last the held panes (`fx_capture_held`).
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
        if rung != CarryRung::Repaint && guard.modes().alternate_screen {
            let debt = guard.render_debt();
            if debt.owed() {
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
        let mut source = handoff_carry::capture_head(
            id,
            &guard,
            &s.term,
            &s.turns,
            Some(&s.timeline),
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

/// This release's held capture (`App::capture_held_screens`): every pooled pane not handed, in id
/// order, its visible screen at the most faithful rung the successor's caps and the policy's
/// ceiling admit, without scrollback, priced from what the handed screens left of the aggregate
/// cells.
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
/// OSC 8 links, and every other line is exact.
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

/// Every line of a v0.98.0 `.hist` sidecar, read off its bytes by the line codec alone:
/// `ATHIST1\n`, the width (`u16` LE), two zero bytes, then frames of a `u32` LE length and a
/// line-codec block.
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

    let (park, adopted) = fx_park_policy(&scratch, desk.ceiling);
    let ceiling = park.carry_ceiling();
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
    let log_draws: Vec<(u64, String, u64)> = desk
        .sessions
        .iter()
        .map(|s| {
            let mut timeline = s.timeline.lock().unwrap();
            (
                s.record.local_id,
                timeline.handoff_log_ids(),
                timeline.handoff_log_seq(),
            )
        })
        .collect();
    let caps = WireCaps::for_target(fx_target_build());
    let capture = fx_capture(&desk.sessions, &desk.held, &fds.entries, caps, ceiling);
    let screens = &capture.screens;
    let mut records: Vec<SessionRecord> = desk.sessions.iter().map(|s| s.record.clone()).collect();
    for (i, record) in records.iter_mut().enumerate() {
        let index = i32::try_from(i).unwrap();
        let holder = if record.title == "zsh" {
            40_000 + index
        } else {
            41_000 + index
        };
        record.fg_holder = Some(holder).filter(|holder| *holder > 0);
        record.carry_render_debt(
            capture
                .render_debts
                .iter()
                .find(|(local_id, _)| *local_id == record.local_id)
                .map(|(_, debt)| *debt),
        );
        let (_, ids, seq) = log_draws
            .iter()
            .find(|(local_id, _, _)| *local_id == record.local_id)
            .expect("every session's log was drawn");
        record.log_ids = Some(ids.clone());
        record.log_seq = *seq;
    }
    let mut connections = desk.connections.clone();
    connections.sort_by(|a, b| (&a.src, &a.dst, &a.op).cmp(&(&b.src, &b.dst, &b.op)));
    connections.dedup();
    assert_eq!(
        connections, desk.connections,
        "{}: the desk's edges are each drawn once",
        desk.name
    );
    let resources = (desk.name == "carried-state").then(|| {
        crate::resource_jobs::live_fixture_carry(&records.first().expect("carried session").sid)
    });
    let mut manifest = SessionHandoff {
        resources,
        schema: SessionHandoff::SCHEMA,
        sessions: records,
        window: None,
        connections,
        next_turn_id: None,
        outgoing_build: Some(crate::running_build_number()),
        held: Vec::new(),
        roster_seq: Some(FX_ROSTER_SEQ),
        fabric_attached: desk.fabric_attached.clone(),
        fleet_told: desk.drawn.fleet_told.clone(),
        mirror_dropped: desk.drawn.mirror_dropped.clone(),
        mirror_served: desk.drawn.mirror_served,
        machine_names: fx_machine_names(),
    };
    let mut layout = desk.layout.clone();
    layout.fold_into_caps();
    let past_caps = desk.layout.windows.len() > crate::restore::MAX_WINDOWS
        || desk
            .layout
            .windows
            .iter()
            .any(|window| window.restored_tabs.len() > crate::restore::MAX_TABS_PER_WINDOW);
    assert_eq!(
        layout.fold.folded(),
        past_caps,
        "{}: the capture folds a desk exactly when it is past the caps",
        desk.name
    );
    if !past_caps {
        assert_eq!(
            layout, desk.layout,
            "{}: a desk within the caps is not folded",
            desk.name
        );
    }
    let folded = layout.clone();
    let committed_layout = crate::app_update_handoff::commit_handoff_layout(&layout)
        .unwrap_or_else(|refusal| {
            panic!("{}: the park commits its layout: {refusal:?}", desk.name)
        });
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
    assert!(
        crate::app_update_handoff::capture_exceeds_policy(Some(ceiling), adopted).is_none(),
        "{}: the capture was taken under the policy the worker reads",
        desk.name
    );
    let nonce = format!("{:032x}", desk.nonce_seed);
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
    let minted = desk.next_turn_id.unwrap_or(0);
    let turn_id_ceiling = minted
        .saturating_add(crate::control::TURN_ID_RESERVE)
        .min(crate::handoff_carry::MAX_TURN_ID);
    let controls =
        handoff_carry::export_for_target(&capture.sources, turn_id_ceiling, fx_target_build());
    manifest.next_turn_id = handoff_carry::manifest_turn_id(turn_id_ceiling);
    for pane in &desk.held {
        if let Some((rows, cols)) = pane.resized {
            pane.term.lock().unwrap().resize(rows, cols);
        }
    }
    let mut results = plan.results_for_test(&dir);
    let held_history = results.take_held(
        &capture
            .held
            .iter()
            .map(|pane| pane.local_id)
            .collect::<Vec<_>>(),
        !crate::handoff_history::exports_held_for_target(
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
        assert_eq!(
            record.history_withheld,
            !ceiling.carries_scrollback() && record.history_dropped > 0,
            "{}: session {}: the record says whether the policy withheld its history",
            desk.name,
            record.local_id
        );
    }
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
    crate::handoff_diet::seal_layout(&layout_path, &layout_wire, None).expect("layout sidecar");
    let layout_wire = std::fs::read_to_string(&layout_path).unwrap();
    let layout = layout_wire_digest(&layout_wire).unwrap();
    assert_eq!(Some(layout), layout_digest(&folded));
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
    let body = std::fs::read_to_string(&manifest_path).unwrap();
    let (_, toml) = body.split_once('\n').unwrap();
    let published = SessionHandoff::from_toml(toml).unwrap();
    assert_eq!(
        published.resources, manifest.resources,
        "resource identities survive the writer"
    );
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
        session_rows.push_str(&format!(
            "absolute_row_counter = {}\nalt_absolute_row_counter = {}\ncolor = {}\nshell_phase = {}\nshell_marks = {}\nshell_completed_seq = {}\n",
            cp.absolute_row_counter,
            cp.alt_absolute_row_counter,
            !cp.color.is_default(),
            cp.shell.phase,
            cp.shell.command_marks.len(),
            cp.shell.completed_seq,
        ));
        session_rows.push_str(&format!("rung = \"{rung}\"\nlinks_stripped = {stripped}\n"));
        session_rows.push_str(&format!(
            "history_links_stripped = {history_links_stripped}\n"
        ));
        session_rows.push_str(&format!(
            "output_state = {}\n",
            !cp.output_state.is_default()
        ));
        if let Some(debt) = &rec.render_debt {
            session_rows.push_str(&format!("render_debt = \"{debt}\"\n"));
        }
        if rec.render_debt_cut != 0 {
            session_rows.push_str(&format!("render_debt_cut = {}\n", rec.render_debt_cut));
        }
        if ceiling != aterm_update_core::handoff_policy::CarryCeiling::Full {
            session_rows.push_str(&format!(
                "repaint = {}\nhistory_dropped = {}\n",
                carry.repaint, rec.history_dropped,
            ));
        }
        session_rows.push('\n');
    }
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
    assert_eq!(
        incoming.layout.as_ref().unwrap().carried_settings_drafts(),
        folded.carried_settings_drafts(),
        "{}: the Settings drafts are placed with their tabs",
        desk.name
    );
    assert_eq!(
        incoming.unplaced_settings_drafts,
        (Vec::new(), 0),
        "{}",
        desk.name
    );
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
        assert_eq!(
            restored.output_state, carried.output_state,
            "{}: session {}'s output state crosses",
            desk.name, adopted.local_id
        );
        assert_eq!(
            adopted.render_debt,
            rec.carried_render_debt(),
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
        let crossed =
            usize::try_from(report.imported + u64::from(checkpoint.history_lines)).unwrap();
        assert_eq!(
            crossed as u64 + dropped,
            parent.len() as u64,
            "{}: session {}'s history is carried or counted, line for line",
            desk.name,
            item.local_id
        );
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
        let source = desk
            .sessions
            .iter()
            .find(|s| s.record.local_id == item.local_id)
            .unwrap()
            .term
            .lock()
            .unwrap();
        let mut successor = successor.lock().unwrap();
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
        let carry = published
            .held
            .iter()
            .find(|carry| carry.local_id == shown.local_id)
            .unwrap();
        if carry.history.is_none() {
            let counted = capture
                .held
                .iter()
                .find(|pane| pane.local_id == shown.local_id)
                .unwrap()
                .history_dropped;
            let withheld = !ceiling.carries_scrollback();
            assert!(counted > 0, "{}: held pane {}", desk.name, shown.local_id);
            assert_eq!(
                (
                    carry.history_dropped,
                    carry.history_withheld,
                    carry.history_failed
                ),
                (counted, withheld, !withheld),
                "{}: held pane {} counts the lines it left behind, and why",
                desk.name,
                shown.local_id
            );
            assert_eq!(
                (
                    shown.history.carry.is_none(),
                    shown.history.dropped,
                    shown.history.withheld,
                    shown.history.held,
                    shown.history.held_pane
                ),
                (true, counted, if withheld { counted } else { 0 }, 0, true),
                "{}: held pane {}'s successor says the loss as its parent did",
                desk.name,
                shown.local_id
            );
            continue;
        }
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
        fabric_attached: Vec::new(),
        drawn: FxDrawn::default(),
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
        timeline: fx_timeline(),
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
        fabric_attached: Vec::new(),
        drawn: FxDrawn::default(),
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
        fabric_attached: Vec::new(),
        drawn: FxDrawn::default(),
        held: Vec::new(),
    }
}

fn fx_twelve_pane_desk() -> FxDesk {
    use crate::restore::{RestoreManifest, SplitKind};
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
        fabric_attached: Vec::new(),
        drawn: FxDrawn::default(),
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

/// The shape v0.92.0 changed: each shell's authorized shell-integration nonce rides the carry
/// (meta key `shell_integration_nonce`), so the successor authorizes the same value and the
/// shells' marks keep counting.
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
        fabric_attached: Vec::new(),
        drawn: FxDrawn::default(),
        held: Vec::new(),
    }
}

/// The shapes v0.93.0 and v0.94.0 changed. The history carry: a shell whose history is deeper
/// than the checkpoint's 256 lines, and one running `less` in 1049, whose checkpoint carries none
/// of its shell's history (the saved primary rides at its visible rows), so the `.hist` sidecar
/// carries all of it.
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
        fabric_attached: Vec::new(),
        drawn: FxDrawn::default(),
        held: Vec::new(),
    }
}

/// A shell under OSC 133/633 integration with `commands` finished commands
/// (prompt, command line, a few lines of output, an exit status), its marks
/// signed with `nonce` when it has one (aterm's own integration) and unsigned
/// otherwise (a shell that sources its own script, with aterm's injection
/// off), ending at a fresh prompt.
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

/// The shape v0.95.0 changed: the meta carries the absolute row counters, the colours an
/// application set (a diff from the configured theme, and the whole XTPUSHCOLORS stack) and the
/// OSC 133/633 state, running command included.
fn fx_colour_and_shell_desk() -> FxDesk {
    use crate::restore::{RestoreManifest, SplitKind};
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
        fabric_attached: Vec::new(),
        drawn: FxDrawn::default(),
        held: Vec::new(),
    }
}

/// A parser left mid-sequence over a quiet PTY, the shape v0.93.0 changed (v0.94.0 and v0.95.0
/// wrote no such desk).
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
        fabric_attached: Vec::new(),
        drawn: FxDrawn::default(),
        held: Vec::new(),
    }
}

/// One row of `n` OSC 8 links labelled `<label><k>`, each to a signed URL of
/// exactly `url_len` bytes: a CI tool's row of log links.
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

/// The shape v0.97.0 changed: a line record over the wire's per-record cap costs that line its
/// OSC 8 links, never the screen.
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
        fabric_attached: Vec::new(),
        drawn: FxDrawn::default(),
        held: Vec::new(),
    }
}

/// The shape v0.98.0 changed: a link-dense line in the history a `.hist` sidecar carries. Exports
/// from v0.94.0 through v0.97.0 wrote such a line as the engine serialized it, and v0.97.0's import
/// refuses the whole sidecar for it (a frame is 1,024 lines, decoded whether or not the line is
/// inside the checkpoint's 256).
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
        fabric_attached: Vec::new(),
        drawn: FxDrawn::default(),
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

/// The shape v0.97.0 added, at `carry = "visible"`: the successor's signed handoff policy asks
/// for the visible screen and its scalars, never the scrollback.
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
        fabric_attached: Vec::new(),
        drawn: FxDrawn::default(),
        held: Vec::new(),
    }
}

/// The shape v0.97.0 added, at `carry = "repaint"`: every session is carried as a blank canonical
/// screen its program redraws, its scalars from `Terminal::carry_meta_abandoning_partial` with no
/// grid projected (the path such a policy is sealed to route around).
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
        fabric_attached: Vec::new(),
        drawn: FxDrawn::default(),
        held: Vec::new(),
    }
}

/// The shape v0.100.0 added: panes kept open by `--hold` after their command exited. They are in
/// the layout but never handed (no process, no PTY), so they have no session record; their final
/// screens ride the manifest's `held` list, each with how its command ended, in `.h<id>.heldgrid`
/// (and, for a screen left in 1049, `.heldalt`) sidecars outside every digest.
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
        fabric_attached: Vec::new(),
        drawn: FxDrawn::default(),
        held: vec![
            FxHeld {
                local_id: 2,
                term: Arc::new(Mutex::new(make)),
                turns: Arc::new(Mutex::new(TurnLedger::default())),
                timeline: fx_timeline(),
                exit: Some(aterm_pty::ChildExit::Code(2)),
                failure_seen: true,
                resized: None,
            },
            FxHeld {
                local_id: 3,
                term: Arc::new(Mutex::new(vim)),
                turns: Arc::new(Mutex::new(TurnLedger::default())),
                timeline: fx_timeline(),
                exit: Some(aterm_pty::ChildExit::Signal(9)),
                failure_seen: false,
                resized: None,
            },
        ],
    }
}

/// The bridge `aterm ctl fabric attach` armed on `carried-state`'s instance,
/// which has no `[fabric] command` configured: what the park draws
/// (`fabric_launch::handoff_attached`) and the successor arms again.
fn fx_attached_bridge() -> Vec<String> {
    ["/home/dev/.local/bin/fabric-bridge", "serve", "--fleet"]
        .map(String::from)
        .to_vec()
}

/// What [`fx_mailbox`] drew: the first session's mailbox and topics, and the
/// store's bus mirror rows.
struct FxMailbox {
    fabric: crate::fabric::FabricCarry,
    topics: Vec<String>,
    mirror_dropped: Vec<String>,
    mirror_served: bool,
}

/// WHAT `carried-state`'S FIRST SESSION'S MAILBOX AND ITS STORE'S BUS MIRROR
/// HOLD AT THE PARK, driven through the bridge plane's handlers as
/// `control::dispatch_bridge_verb` calls them and the session's own verbs, on
/// a store of this build's, and drawn as `SessionHandoff::from_store` draws
/// them (`SessionFabric::handoff_carry` under the one mail budget, the topic
/// set, `StoreMirror::draw_tombstones` and `StoreMirror::served`).
fn fx_mailbox() -> FxMailbox {
    use crate::fabric::{
        CARRY_MAIL_BYTES, RING_CAP, bridge_attached, cmd_deliver, cmd_hello, cmd_inbox_seen,
        cmd_link_after_log_switch, cmd_post, next_bridge_generation, render_topics,
    };
    use crate::session_store::{ExitActor, ExitReason};
    crate::fabric::with_link_reset(|| {
        let store = crate::session_store::new_store();
        let h = crate::session_store::test_handle(0);
        let sid = h.sid.as_str().to_string();
        let ctx = h.ctx.clone();
        store.write().unwrap().register(h);
        let forever = format!(
            "to=@s-peer kind=note dl={} the release notes are in docs/RELEASE.md",
            u64::MAX
        );
        assert_eq!(cmd_post(&ctx, &forever, None), "OK 1\n");
        let generation = next_bridge_generation();
        bridge_attached(generation);
        let hello = cmd_hello("lane=2 refill=in,say log=turns", Some(generation));
        assert!(
            hello.starts_with("OK ") && !hello.starts_with("OK stale=1"),
            "{hello}"
        );
        let _ = crate::fabric::with_current_bridge(Some(generation), || {
            store
                .read()
                .unwrap()
                .mirror()
                .set_served(crate::fabric::bridge_mirrors_logs());
        });
        let link = |log: String| {
            cmd_link_after_log_switch(&store, &format!("log {log}.0"), Some(generation), || {
                crate::session_log_mirror::forget_landed(&store);
            })
        };
        let ask = |off: u64, text: &str| {
            cmd_deliver(
                &store,
                &format!("{sid} off={off} from=s-peer@n-lab kind=ask trust=agent text={text}"),
            )
        };
        assert!(link("aa".repeat(16)).starts_with("OK"));
        assert_eq!(ask(10, "green?"), "OK 1\n");
        assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=1\n");
        assert!(link("bb".repeat(16)).starts_with("OK"));
        assert_eq!(ask(10, "tag?"), "OK 2\n");
        assert_eq!(cmd_inbox_seen(&ctx, "2 handled"), "OK seen=2\n");
        assert_eq!(
            cmd_post(
                &ctx,
                "to=@s-peer kind=answer re=10 dl=600000 via=h-andrew key=build-42 tagged \
                 v0.108.0",
                None,
            ),
            "OK 2\n"
        );
        assert_eq!(ask(u64::MAX, "notes?"), "OK 3\n");
        assert_eq!(cmd_inbox_seen(&ctx, "3 deferred"), "OK seen=3\n");
        assert_eq!(
            ctx.fabric.topic_add("builds", "@12", &ctx.timeline),
            Ok(true)
        );
        for n in 0..=u64::try_from(RING_CAP).unwrap() {
            let reply = cmd_deliver(
                &store,
                &format!(
                    "{sid} off={} from=s-ci@n-runner{n} kind=note trust=agent topic=builds \
                     text=passed",
                    20 + n
                ),
            );
            assert!(reply.starts_with("OK "), "{reply}");
        }
        let closed = crate::session_store::test_handle(9);
        let closed_ctx = closed.ctx.clone();
        store.write().unwrap().register(closed);
        {
            use aterm_slog::{Close, Conf, Kind, Open, Out, RunOut, Src};
            let mut tl = closed_ctx.timeline.lock().unwrap();
            let now = tl.slog().now();
            tl.slog_mut().open_session(None, None, now);
            let log = tl.slog_mut().log_mut_for_tests();
            for _ in 0..3 {
                let c = log
                    .open(Open::new(Kind::Cmd, Src::Shell, Conf::Strong), now)
                    .expect("a command opens");
                log.close(
                    c,
                    Close::new(Out::Run(RunOut::Ok), Src::Shell, Conf::Exact),
                    now,
                )
                .expect("it closes");
            }
        }
        let take = crate::session_log_mirror::cmd_log_take(&store, "", Some(generation));
        assert!(take.starts_with("OK "), "{take}");
        store
            .write()
            .unwrap()
            .deregister_local_as(9, ExitReason::ShellExit, ExitActor::Unknown);
        let mut budget = CARRY_MAIL_BYTES;
        let fabric = ctx.fabric.handoff_carry(&mut budget);
        assert_eq!(fabric.bus_log, Some(format!("{}.0", "bb".repeat(16))));
        assert!(
            fabric.posts.len() == 1
                && fabric.posts.iter().all(|post| post.re.is_some()
                    && post.dl.is_some()
                    && post.via.is_some()
                    && post.key.is_some()),
            "{fabric:?}"
        );
        assert_eq!(fabric.uncarried, [1], "the note past TOML is left behind");
        assert_eq!(
            (
                fabric.receipts.len(),
                fabric.earlier_receipts.len(),
                fabric.receipts_uncarried
            ),
            (1, 1, 1)
        );
        assert!(
            fabric.dropped > 0 && fabric.broadcasts_uncarried > 0 && fabric.broadcasts_refilled > 0,
            "{fabric:?}"
        );
        let topics = render_topics(&ctx.fabric.topics());
        let guard = store.read().unwrap();
        let mirror_dropped = guard.mirror().draw_tombstones();
        assert_eq!(mirror_dropped.len(), 1, "the closed session's tail");
        FxMailbox {
            fabric,
            topics,
            mirror_dropped,
            mirror_served: guard.mirror().served(),
        }
    })
}

/// A WORKER CLAUDE CODE ADOPTED ACROSS AN EARLIER UPDATE ('s
/// U10), at `rows`x`cols`: its ledger came carried with the floor its
/// sender's ring had shed harness records below (`ControlCarry::ledger`),
/// holding the manager's turn 41; since then the window harness pressed
/// `keep going` (turn 42, `STATUS_PRESSED`), and the person scrolled the app
/// back eight rows, which left the bottom of its scrolling region for the
/// differ's `below`.
fn fx_worker(local_id: u64, rows: u16, cols: u16) -> (FxSession, u64) {
    use crate::turn_ledger::{STATUS_PRESSED, TurnBy};
    let mut app = FakeApp::new();
    let mut t = Terminal::new(rows, cols);
    t.set_alt_archive_enabled(true);
    t.set_alt_archive_origin(ORIGIN + local_id);
    t.process(b"$ claude");
    t.process(b"\x1b[?1049h\x1b[?2004h\x1b[>1u");
    t.process(&app.frame(rows, cols));
    let manager_mark = ArchMark::of(&t);
    app.say(&mut t, [format!("\u{276f} {TURN_TEXT}")]);
    app.say(&mut t, numbered(1..=24));
    let manager = TurnRecord {
        id: 41,
        started_ms: 100,
        dur_ms: 2000,
        submitted: true,
        status: "settled",
        text: TURN_TEXT.into(),
        screen_hash: 2,
        seq: t.content_seq(),
        arch: manager_mark,
        by: TurnBy::Ctl,
        carried: false,
    };
    let pressed_mark = ArchMark::of(&t);
    app.say(&mut t, ["\u{276f} keep going".to_string()]);
    app.say(&mut t, numbered(25..=40));
    let pressed = TurnRecord {
        id: 42,
        started_ms: 4_100,
        dur_ms: 0,
        submitted: true,
        status: STATUS_PRESSED,
        text: "keep going".into(),
        screen_hash: 3,
        seq: t.content_seq(),
        arch: pressed_mark,
        by: TurnBy::Harness,
        carried: false,
    };
    let top = app.doc.len() - (usize::from(rows) - 5) - 8;
    t.process(&app.frame_from(top, rows, cols));
    let mut adopted = crate::handoff_carry::ControlCarry {
        turns: vec![manager],
        unheld_below: 0,
        shed_below: 37,
        turn_ids: 0,
        archive: None,
        log: None,
        log_mirror: None,
    };
    let mut ledger = adopted.ledger();
    ledger.push(pressed);
    assert_eq!(ledger.shed_below(), 37, "the carried floor stands");
    let mut session = fx_session(local_id, "claude", t);
    session.turns = Arc::new(Mutex::new(ledger));
    (session, 42)
}

/// A hosted agent's leaf restore (`AgentRestore`), as a cold restore's queue
/// owes it or the host supervises it: Claude Code's (no run, no word) or the
/// lane's under its word.
fn fx_agent(
    pid: u32,
    program: &str,
    session: Option<&str>,
    run: Option<crate::restore::RunRestore>,
    word: &str,
) -> crate::restore::AgentRestore {
    crate::restore::AgentRestore {
        pid,
        start: "Mon Oct 19 09:14:02 2026".to_string(),
        program: format!("/home/dev/.local/bin/{program}"),
        argv: vec![program.to_string()],
        session: session.map(str::to_string),
        cwd: "/home/dev/src/aterm".to_string(),
        version: Some("1.4.0".to_string()),
        home: None,
        login: None,
        run,
        word: word.to_string(),
        cause: None,
        tab: None,
    }
}

/// THE STATE A SESSION, ITS WINDOW AND ITS DESK CARRY BESIDE THE SCREENS
/// (v0.108.0 on): every field the producer writes
/// only for a session someone supervises, escalates, halts, subscribes or
/// tasks, a window someone zoomed, warned or posted to, and a desk with a
/// bridge `fabric attach` armed — the shapes no other desk carries, each
/// spelled by the code that writes it or held to the parser that reads it.
fn fx_carried_state_desk() -> FxDesk {
    use crate::restore::{
        AgentRestore, AgyRestore, AlabRestore, CodexRestore, GeminiRestore, LostRestore,
        MuseRestore, RestoreManifest, RunRestore,
    };
    use crate::session_timeline::{
        AutopilotSetBy, SessionMeta, parse_carried_attention, parse_carried_supervisor,
    };
    let cwd = "/home/dev/src/aterm";
    let release_ms = i64::try_from(crate::running_build_number() * 1_000).unwrap();
    let mut scrolled = fx_shell(ROWS, COLS, 300, cwd, 5);
    scrolled.scroll_display(12);
    assert_eq!(
        scrolled.grid().display_offset(),
        12,
        "the person scrolled back"
    );
    let (worker, minted) = fx_worker(2, ROWS, COLS);
    let mut sessions = vec![
        fx_session(0, "zsh", scrolled),
        fx_session(1, "zsh", fx_shell(ROWS, COLS, 40, cwd, 3)),
        worker,
    ];
    for local_id in 3..=7 {
        let flavour = u8::try_from(local_id).unwrap();
        sessions.push(fx_session(
            local_id,
            "zsh",
            fx_shell(ROWS, COLS, 6, cwd, flavour),
        ));
    }
    let mut meta = SessionMeta::default();
    meta.set("title", Some("release".to_string()));
    meta.set("icon", Some("rocket".to_string()));
    meta.set("attention", Some("look at the failing test".to_string()));
    meta.set_attention_kind(
        "harness",
        Some("waiting on CI".to_string()),
        Some(aterm_types::needs::AttentionKind::Wait),
    )
    .expect("room for an owner");
    assert!(meta.set_autopilot(Some("cruise".to_string()), Some(release_ms + 7_200_000)));
    meta.note_autopilot_set_by(AutopilotSetBy {
        actor: aterm_slog::Actor::Ctl,
        surface: None,
        since_ms: release_ms + 3_600_000,
    });
    let drawn = meta.sanitized();
    let record = &mut sessions[0].record;
    record.user_title = drawn.user_title.clone();
    record.icon = drawn.icon.clone();
    record.attention = drawn.attention.clone();
    record.autopilot = drawn.autopilot.clone();
    record.land_at = drawn.land_at;
    record.attention_owners = meta.handoff_attention_owners();
    record.supervisor = Some("harness 30000".to_string());
    record.claim_known = true;
    let mut holds = crate::fabric::render_holds(&crate::fabric::parse_holds([
        "fleet maintenance",
        "local manual",
    ]))
    .into_iter();
    record.hold = holds.next();
    record.more_holds = holds.collect();
    let mailbox = fx_mailbox();
    record.topics = mailbox.topics;
    record.fabric = mailbox.fabric;
    record.status_seen = crate::status_item::StatusCarry {
        need: None,
        told: false,
        held: Some((
            aterm_types::needs::HoldWord::Local,
            release_ms / 1_000 + 3_600,
        )),
        done: None,
    }
    .row();
    record.task = Some(format!(
        "rev=2 state=working from=ctl since={} result_bytes=0 attention=0",
        release_ms + 3_000_000
    ));
    record.autopilot_end = crate::harness_end_carry::EndCarry {
        end: None,
        worked_ms: 25_000,
    }
    .row();
    {
        let mut timeline = sessions[0].timeline.lock().unwrap();
        timeline
            .slog_mut()
            .seed_lineage(Some(&format!("s-{:020x}", 0x00ff_0000_0007_u64)));
        let now = timeline.slog().now();
        let _ = timeline.slog_mut().mirror_log().bus_copy(1, now);
    }
    sessions[1].record.journal_key = Some(crate::session_store::journal_key_row((48_100, 1)));
    sessions[2].record.frozen_path = true;
    let lost_conversation = "1057c0de-6e66-4f1e-9c0d-8e4f5a6b7c8d";
    let lost_command = format!("claude --resume {lost_conversation}");
    sessions[7].record.attention = Some(format!(
        "Cannot bring the agent back (its conversation could not be resumed); run \
         {lost_command} in this tab"
    ));
    let record = &sessions[0].record;
    assert!(
        crate::status_item::StatusCarry::parse(record.status_seen.as_deref().unwrap())
            .held
            .is_some()
    );
    assert!(
        crate::session_store::parse_journal_key(sessions[1].record.journal_key.as_deref().unwrap())
            .is_some()
    );
    assert!(parse_carried_supervisor(record.supervisor.as_deref().unwrap()).is_some());
    assert!(
        record
            .attention_owners
            .iter()
            .all(|row| parse_carried_attention(row).is_some())
            && !record.attention_owners.is_empty()
    );
    assert_eq!(
        crate::fabric::parse_holds(
            record
                .hold
                .iter()
                .chain(&record.more_holds)
                .map(String::as_str)
        )
        .standing()
        .count(),
        2
    );
    assert_eq!(record.topics, ["builds @12"]);
    assert!(
        crate::harness_end_carry::EndCarry::parse(record.autopilot_end.as_deref().unwrap())
            .is_some()
    );
    assert!(
        crate::task_record::TaskRecord::from_carry_row(
            record.task.as_deref().unwrap(),
            None,
            std::time::Instant::now(),
            u64::try_from(release_ms).unwrap() + 3_600_000,
        )
        .is_some()
    );
    sessions[1].record.parent = Some(sessions[0].record.sid.clone());

    let mut app = crate::App::headless_for_test();
    let reset = app.default_font_size;
    assert!(app.set_font_size_with(reset + 2.0 * crate::FONT_ZOOM_STEP, |_| true));
    let zoom = app.handoff_font_zoom();
    let notice = |line: &str| aterm_messages::wire::NoticeRequest::parse(line).expect(line);
    for _ in 0..2 {
        let reply = app.take_notice(notice(
            "post build sev=warn Disk almost full -- /home is 92% used",
        ));
        assert!(reply.starts_with("OK"), "{reply}");
    }
    let row = |severity, title: &str| {
        aterm_messages::Message::new(aterm_messages::tags::SYSTEM, severity, title)
    };
    let slow = app.post_message(row(aterm_messages::Severity::Warn, "Indexing is slow"));
    assert!(app.restate_message(
        slow,
        crate::messages_host::restatement_of(&row(
            aterm_messages::Severity::Error,
            "Indexing stopped"
        )),
    ));
    assert!(app.note_update_health(
        aterm_update::health_failing_title("pipeline"),
        &format!(
            "20 failed checks in a row since {}: release manifests exist but cannot be \
             downloaded.",
            aterm_types::rfc3339::format_rfc3339(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("after the epoch")
                    .as_secs()
                    - 3_600
            )
        ),
    ));
    let reply = app.take_notice(notice(
        "progress cargo pct=42 Building aterm -- 412/980 crates",
    ));
    assert!(reply.starts_with("OK"), "{reply}");
    app.post_message(crate::message_reporters::crash_message(
        &crate::logging::CrashEvidence {
            path: std::path::PathBuf::from(
                "/home/dev/Library/Logs/aterm/crash-1760865242.log.seen",
            ),
            head: vec!["thread 'main' panicked at crates/aterm-gui/src/app.rs:4100:9".to_string()],
        },
    ));
    app.post_message(
        crate::message_reporters::integration_lost(&[crate::message_reporters::LostTab {
            place: "in window 2, tab 2".to_string(),
            sid: sessions[2].record.sid.clone(),
            heal: crate::integration_notice::Heal::NewTab,
        }])
        .expect("one tab"),
    );
    app.post_message(crate::message_reporters::resume_ask(
        &sessions[2].record.sid,
        &crate::message_reporters::UpgradeSpot {
            place: "in window 2, tab 2".to_string(),
            nav: Some(aterm_messages::Intent::ShowTab { tab: 2, window: 2 }),
        },
        &aterm_agent::supervise::policy::exception::Exception::of_notice(
            &aterm_agent::supervise::policy::notice::LOGIN_SIGN_IN,
            None,
            None,
            "the agent working",
        ),
    ));
    let parked = std::time::Instant::now()
        + aterm_messages::PROGRESS_GRACE
        + std::time::Duration::from_millis(10);
    app.settle_messages(parked);
    app.sync_message_band_rows(parked);
    let band_rows = app.message_band_rows;
    let carried = app.carried_messages();
    assert!(
        carried.messages.iter().any(|m| m.wire && m.repeats == 2),
        "the wire's notice, posted twice: {:?}",
        carried.messages
    );
    assert!(
        carried
            .messages
            .iter()
            .any(|m| m.fill_permille.is_some() && !m.stats.is_empty()),
        "the metered build: {:?}",
        carried.messages
    );
    assert!(
        carried.messages.iter().any(|m| m.finished.is_some()),
        "the integration row's own words: {:?}",
        carried.messages
    );
    assert!(
        carried.messages.iter().any(|m| m.retrospective),
        "the crash's record: {:?}",
        carried.messages
    );
    assert!(
        carried.messages.iter().any(|m| m.posted_severity.is_some()),
        "a row marked away from how it was posted: {:?}",
        carried.messages
    );
    assert!(
        carried.messages.iter().any(|m| m.short_title.is_some()),
        "the lost login's short title: {:?}",
        carried.messages
    );
    assert!(!carried.update_health_said.is_empty(), "the warning said");
    let (rows, cols) = (ROWS, COLS * 2 + 1);
    let size = app.frame_px(rows.saturating_add(band_rows), cols);
    let inner_px = Some([size.width, size.height]).filter(|[w, h]| *w > 0 && *h > 0);
    let window = WindowCarry {
        rows,
        cols,
        outer_x: Some(96),
        outer_y: Some(48),
        inner_px,
        presence_row: false,
        status_bar_rows: band_rows,
        messages: carried.messages,
        next_message_id: carried.next_message_id,
        update_verified_unix_ms: None,
        font_px_milli: zoom.device.map(|(px, _)| px),
        font_reset_px_milli: zoom.device.map(|(_, reset)| reset),
        font_size_milli: zoom.size.map(|(size, _)| size),
        font_reset_size_milli: zoom.size.map(|(_, reset)| reset),
        update_health_said: carried.update_health_said,
        messages_captured_unix_ms: carried.captured_unix_ms,
    };

    let thread = "019a4c2e-7d10-7c3a-9e51-3f2b6d8a0c41";
    let leaf_of =
        |local_id: u64, title: &str, fill: &dyn Fn(&mut crate::restore::TerminalLeafRestore)| {
            let mut node = fx_leaf(local_id, title, Some(cwd));
            if let crate::restore::RestoredSplitTree::Leaf {
                view: crate::restore::RestoredView::Terminal(leaf),
            } = &mut node
            {
                fill(leaf);
            }
            node
        };
    let owed = leaf_of(1, "zsh", &|leaf| {
        leaf.agent = Some(Box::new(AgentRestore {
            pid: 48_213,
            start: "Mon Oct 19 09:14:02 2026".to_string(),
            program: "/home/dev/.local/bin/codex-alab".to_string(),
            argv: vec!["codex-alab".to_string()],
            session: Some(thread.to_string()),
            cwd: cwd.to_string(),
            version: Some("0.157.0".to_string()),
            home: None,
            login: None,
            run: Some(RunRestore::Codex(CodexRestore {
                home: "/home/dev/.codex-work".to_string(),
                home_env: Some("/home/dev/.codex-work".to_string()),
                embedded: true,
                started_ms: 0,
                alab: Some(AlabRestore {
                    argv: vec![
                        "/home/dev/.local/bin/codex-alab".to_string(),
                        "resume".to_string(),
                        thread.to_string(),
                    ],
                    session: thread.to_string(),
                    codex_home: Some("/home/dev/.codex-work".to_string()),
                    cwd: Some(cwd.to_string()),
                }),
                carried: None,
            })),
            word: "codex".to_string(),
            cause: None,
            tab: None,
        }));
    });
    let muse = leaf_of(3, "zsh", &|leaf| {
        leaf.agent = Some(Box::new(fx_agent(
            48_301,
            "muse",
            None,
            Some(RunRestore::Muse(MuseRestore {
                session: Some("5b1e7a52-3c0d-4f8e-9a61-0d2c4b7e9f13".to_string()),
                workspace: Some(cwd.to_string()),
                data: "/home/dev/.local/share/muse".to_string(),
                env: vec!["XDG_DATA_HOME=/home/dev/.local/share".to_string()],
            })),
            "muse",
        )));
    });
    let agy = leaf_of(4, "zsh", &|leaf| {
        leaf.agent = Some(Box::new(fx_agent(
            48_402,
            "agy",
            None,
            Some(RunRestore::Agy(AgyRestore {
                conversation: Some("8f3c2a1e-6b4d-4e7f-9c2a-1d5e8b3f7a60".to_string()),
                config: "/home/dev/agy/.gemini/antigravity".to_string(),
                env: vec!["HOME=/home/dev/agy".to_string()],
            })),
            "agy",
        )));
    });
    let gemini = leaf_of(5, "zsh", &|leaf| {
        leaf.agent = Some(Box::new(fx_agent(
            48_503,
            "gemini",
            None,
            Some(RunRestore::Gemini(GeminiRestore {
                session: Some("c2d9e4f1-7a3b-4c5d-8e6f-0a1b2c3d4e5f".to_string()),
                home: "/home/dev/.gemini".to_string(),
                chats: Some("/home/dev/.gemini/tmp/7f3a9c/chats".to_string()),
                env: vec!["GEMINI_CLI_HOME=/home/dev".to_string()],
                started_ms: u64::try_from(release_ms).unwrap() + 1_800_000,
            })),
            "gemini",
        )));
    });
    let asked = leaf_of(6, "zsh", &|leaf| {
        let mut agent = fx_agent(
            48_604,
            "claude",
            Some("a7e3c1d9-2b4f-4a6e-8c0d-3f5b7d9e1a24"),
            None,
            "",
        );
        agent.version = Some("2.1.302".to_string());
        agent.home = Some("/home/dev/.claude-work".to_string());
        agent.login = Some(String::new());
        agent.cause = Some("asked".to_string());
        agent.tab = Some(format!("s-{:020x}", 0x00ff_0000_0006_u64));
        leaf.agent = Some(Box::new(agent));
    });
    let worker_sid = sessions[2].record.sid.clone();
    let worker = leaf_of(2, "claude", &|leaf| {
        let mut agent = fx_agent(
            51_200,
            "claude",
            Some("e4b8f2a6-1c3d-4e5f-a7b9-c0d1e2f3a4b5"),
            None,
            "",
        );
        agent.version = Some("2.1.302".to_string());
        agent.home = Some("/home/dev/.claude-work".to_string());
        agent.login = Some(String::new());
        agent.tab = Some(worker_sid.clone());
        leaf.supervised = Some(Box::new(agent));
    });
    let lost = leaf_of(7, "zsh", &|leaf| {
        leaf.lost = Some(Box::new(LostRestore {
            session: lost_conversation.to_string(),
            pid: 4_242,
            marked: true,
            reason: "its conversation could not be resumed".to_string(),
            restarted: true,
            command: Some(lost_command.clone()),
        }));
    });
    let first = leaf_of(0, "zsh", &|leaf| leaf.theme = Some("Nord".to_string()));
    let pair = |a, b| fx_tab(fx_split(crate::restore::SplitKind::Vertical, a, b));
    let mut front = fx_window_in(
        rows,
        cols,
        band_rows,
        vec![pair(first, owed), pair(muse, agy), pair(gemini, asked)],
    );
    front.inner_px = inner_px;
    let co_view = leaf_of(0, "zsh", &|leaf| leaf.theme = Some("Nord".to_string()));
    let mut behind = fx_window(
        ROWS,
        COLS,
        vec![fx_tab(co_view), fx_tab(worker), fx_tab(lost)],
    );
    if behind.show.z_order.is_some() {
        behind.show.z_order = Some(1);
        behind.show.key = Some(false);
    }
    let drawn_rows = FxDrawn {
        fleet_told: vec![crate::fleet_glance::told_row(
            crate::fleet_glance::herald_key("mac-2", &format!("s-{:020x}", 0x00ee_0000_0001_u64)),
            aterm_types::needs::NeedWord::Ask,
        )],
        mirror_dropped: mailbox.mirror_dropped,
        mirror_served: mailbox.mirror_served,
    };
    assert_eq!(
        crate::fleet_glance::parse_told(&drawn_rows.fleet_told).len(),
        1,
        "the told need reads back"
    );
    let mut desk = FxDesk {
        name: "carried-state",
        nonce_seed: 0x0100_0014,
        ceiling: aterm_update_core::handoff_policy::CarryCeiling::Full,
        window,
        sessions,
        layout: RestoreManifest::new(vec![front, behind]),
        connections: Vec::new(),
        next_turn_id: Some(minted),
        fabric_attached: fx_attached_bridge(),
        drawn: drawn_rows,
        held: Vec::new(),
    };
    for window in &mut desk.layout.windows {
        for tab in &mut window.restored_tabs {
            if let crate::restore::RestoredSplitTree::Split { first, .. } = &mut tab.root
                && let crate::restore::RestoredSplitTree::Leaf {
                    view: crate::restore::RestoredView::Terminal(leaf),
                } = first.as_mut()
                && leaf.local_id == Some(0)
            {
                leaf.autopilot_by = drawn.autopilot_set_by.map(|set_by| set_by.carried());
            }
        }
    }
    desk
}

/// A HELD PANE'S SCROLLBACK LEFT BEHIND, AND WHY (v0.108.0 on's
/// U10): a shell beside a `make` that failed and was kept open, with its build
/// log in its scrollback.
fn fx_held_history_desk(failed: bool) -> FxDesk {
    use crate::restore::{RestoreManifest, SplitKind};
    let cwd = "/home/dev/src/aterm";
    let mut make = fx_shell(ROWS, COLS, 90, cwd, 2);
    make.process(b"make check\r\n");
    for i in 0..40 {
        make.process(
            format!("   Compiling crate-{i} v0.1.0 ({cwd}/crates/crate-{i})\r\n").as_bytes(),
        );
    }
    make.process(
        b"\x1b[1;31merror\x1b[0m: could not compile `crate-39`\r\nmake: *** [check] Error 101\r\n",
    );
    let (ceiling, name, nonce_seed) = if failed {
        (
            aterm_update_core::handoff_policy::CarryCeiling::Full,
            "held-failed",
            0x0100_0016,
        )
    } else {
        (
            aterm_update_core::handoff_policy::CarryCeiling::Visible,
            "held-withheld",
            0x0100_0017,
        )
    };
    let mut shell = fx_session(0, "zsh", fx_shell(ROWS, COLS, 60, cwd, 5));
    if !failed {
        shell.rung = CarryRung::VisibleOnly;
    }
    FxDesk {
        name,
        nonce_seed,
        ceiling,
        window: fx_window_carry(ROWS, COLS * 2 + 1, false),
        sessions: vec![shell],
        layout: RestoreManifest::new(vec![fx_window(
            ROWS,
            COLS * 2 + 1,
            vec![fx_tab(fx_split(
                SplitKind::Vertical,
                fx_leaf(0, "zsh", Some(cwd)),
                fx_leaf(1, "make", Some(cwd)),
            ))],
        )]),
        connections: Vec::new(),
        next_turn_id: None,
        fabric_attached: Vec::new(),
        drawn: FxDrawn::default(),
        held: vec![FxHeld {
            local_id: 1,
            term: Arc::new(Mutex::new(make)),
            turns: Arc::new(Mutex::new(TurnLedger::default())),
            timeline: fx_timeline(),
            exit: Some(aterm_pty::ChildExit::Code(2)),
            failure_seen: false,
            resized: failed.then_some((ROWS, COLS - 12)),
        }],
    }
}

/// One drawer's card on the main screen (an inline agent's answer), three
/// rows on its own background.
fn fx_card(n: u8) -> Vec<u8> {
    let row = format!("⏺ answer {n} — {}", "the drawer's own rows ".repeat(2));
    format!("\x1b[48;5;{}m{row}\r\n{row}\r\n{row}\x1b[0m\r\n", 236 + n).into_bytes()
}

/// A shell running an inline agent (Claude Code drawing on the main screen)
/// that the person stopped with Ctrl-Z and resumed in a second one: the
/// stopped drawer's mark sealed and kept, the running one's open
/// (`Terminal::note_holder_cut`, as the host reports each foreground change).
fn fx_two_drawers(flavour: u8) -> Terminal {
    use aterm_core::terminal::{CutKind, HolderCut};
    let mut t = fx_shell(ROWS, COLS, 30, "/home/dev/src/aterm", flavour);
    t.process(b"claude\r\n");
    t.note_holder_cut(HolderCut {
        to: Some(0x00c1_a0de_0001),
        kind: CutKind::Group,
        from_alive: false,
    });
    t.process(&fx_card(0));
    t.note_holder_cut(HolderCut {
        to: None,
        kind: CutKind::Group,
        from_alive: true,
    });
    t.process(
        b"\r\n[1]+  Stopped                 claude\r\ndev@devhost aterm % claude --continue\r\n",
    );
    t.note_holder_cut(HolderCut {
        to: Some(0x00c1_a0de_0002),
        kind: CutKind::Group,
        from_alive: false,
    });
    t.process(&fx_card(1));
    t
}

/// THE ENGINE STATE A CARRIED SCREEN HOLDS BEYOND ITS CELLS (v0.108.0 on),
/// one session for each shape no other desk carries, each its own tab.
fn fx_engine_state_desk() -> FxDesk {
    use crate::restore::RestoreManifest;
    let cwd = "/home/dev/src/aterm";
    let mut shells: Vec<(&str, Terminal, &str)> = Vec::new();

    let mut ssh = fx_shell(ROWS, COLS, 40, "/home/dev", 2);
    ssh.process(b"ssh build-host\r\n\x1b[1;32mdev@build-host\x1b[0m:~$ ls\r\n\x1b[38:2::120:200:3");
    assert_eq!(ssh.partial_sequence_state(), Some("CsiParam"));
    shells.push(("ssh", ssh, "/home/dev"));

    let mut vim = fx_editor(ROWS, COLS, 4);
    vim.process(b"\x1b[2 ");
    assert_eq!(vim.partial_sequence_state(), Some("CsiIntermediate"));
    shells.push(("vim", vim, "/home/dev/src"));

    let mut imgcat = fx_shell(ROWS, COLS, 40, cwd, 5);
    imgcat.process(b"imgcat plot.png\r\n\x1b]1337;File=name=cGxvdC5wbmc=;size=6144;inline=1:");
    imgcat.process("iVBORw0KGgo".repeat(500).as_bytes());
    assert_eq!(imgcat.partial_sequence_state(), Some("OscString"));
    assert!(imgcat.partial_sequence_swallows_picture());
    shells.push(("imgcat", imgcat, cwd));

    let mut cat = fx_shell(ROWS, COLS, 40, cwd, 6);
    cat.process("cat README.ja.md\r\n日本語の説明\r\n日本".as_bytes());
    cat.process(&"語".as_bytes()[..2]);
    shells.push(("cat", cat, cwd));

    let mut screen = fx_shell(ROWS, COLS, 40, "/home/dev", 7);
    screen.process("screen -r build\r\n\x1b_build ✓ deploy ".as_bytes());
    screen.process(&"✓".as_bytes()[..2]);
    assert_eq!(screen.partial_sequence_state(), Some("SosPmApcString"));
    shells.push(("screen", screen, "/home/dev"));

    let mut tmux = fx_shell(ROWS, COLS, 40, cwd, 8);
    tmux.process(b"tmux attach -t work\r\n\x1bPtmux;\x1b\x1b_Gf=100,a=T,m=1;iVBORw0KGgo");
    assert!(tmux.partial_sequence_state().is_some());
    shells.push(("tmux", tmux, cwd));

    let mut sixel = fx_shell(ROWS, COLS, 40, cwd, 0);
    sixel.process(b"img2sixel plot.png\r\n\x1bP0;1;8");
    assert_eq!(sixel.partial_sequence_state(), Some("DcsParam"));
    shells.push(("img2sixel", sixel, cwd));

    let mut mc = fx_shell(ROWS, COLS, 40, "/home/dev", 1);
    mc.set_allow_palette_reconfigure(true);
    mc.process(
        b"mc\r\n\x1b]4;1;rgb:cc/24/1d\x07\x1b]4;4;rgb:45/85/88\x07\x1b]10;rgb:eb/db/b2\x07\
          \x1b]11;rgb:28/28/28\x07\x1b]12;rgb:fe/80/19\x07\x1b]17;rgb:50/49/45\x07\
          \x1b]19;rgb:fb/f1/c7\x07\x1b]30001\x07\x1b]19;rgb:ff/ff/ff\x07",
    );
    mc.process(b"\x1b-A\x1b.F\x1b/L");
    mc.process(b"\x1b]1;\x07\x1b]2;mc [dev@devhost]:~\x07\x1b[22;0t\x1b]9;4;1;42\x07");
    mc.process(b"\x1b[?1000h\x1b[?1000s");
    mc.process(b"\x1b[?1049h\x1b[>1u\x1b[58:2::255:128:0m\x1b[5;7H\x1b7\x1b[?1049l");
    mc.process(b"\x1b[58:2::255:128:0m\x1b[1\"q\x1b[#{");
    mc.process(b"\x1b]8;id=mc-help;https://example.invalid/mc/help\x1b\\help ");
    mc.process("\u{1F469}\u{200D}".as_bytes());
    shells.push(("mc", mc, "/home/dev"));

    let mut htop = fx_shell(ROWS, COLS, 40, cwd, 3);
    htop.process(b"htop\r\n\x1b[?1049h\x1b[H\x1b[2J");
    for r in 1..=ROWS {
        let row = format!(
            "  {:>5} dev   20   0  412M  38M S  0.{r}  0.4  0:0{}.12 /home/dev/.local/bin/aterm --sock /tmp/s{r}",
            4100 + u32::from(r),
            r % 10
        );
        htop.process(format!("\x1b[{r};1H{row:-<99}").as_bytes());
    }
    htop.resize(ROWS, COLS - 20);
    assert!(htop.render_debt().owed(), "htop has not redrawn since");
    shells.push(("htop", htop, cwd));

    let mut short = fx_shell(ROWS, COLS, 18, "/home/dev", 4);
    short.resize(ROWS - 12, COLS);
    shells.push(("zsh", short, "/home/dev"));

    let mut vt52 = fx_shell(ROWS, COLS, 40, "/home/dev", 5);
    vt52.process(b"./vt52-menu\r\n\x1b[?2l\x1bY%");
    shells.push(("vt52-menu", vt52, "/home/dev"));

    let rev_nonce = [0xa1; 32];
    let rev_id = fx_hex(&rev_nonce);
    let mut rev = fx_signed_shell(ROWS, COLS, rev_nonce, cwd, 3);
    rev.process(
        format!(
            "cd ../notes\r\n\x1b]133;C;id={rev_id}\x07\x1b]133;D;0;id={rev_id}\x07\
             \x1b]633;P;AtermIntegration=0123456789abcdef;id={rev_id}\x07\
             \x1b]633;P;AtermCwd=file://devhost/home/dev/src/notes;id={rev_id}\x07\
             \x1b]7;file://devhost/home/dev/src/notes\x07\
             \x1b]133;A;id={rev_id}\x07dev@devhost notes % \x1b]133;B;id={rev_id}\x07"
        )
        .as_bytes(),
    );
    shells.push(("zsh", rev, "/home/dev/src/notes"));

    let mut job = fx_signed_shell(ROWS, COLS, [0xa2; 32], cwd, 4);
    job.process(b"\x1b]7;file://devhost/home/dev/tmp\x07");
    job.set_foreground_job(true);
    job.process(b"\x1b]7;file://devhost/home/dev/src/aterm/crates\x07");
    shells.push(("zsh", job, cwd));

    let stale_nonce = [0xa3; 32];
    let mut stale = fx_shell(ROWS, COLS, 30, cwd, 6);
    stale.set_require_shell_integration_nonce(true);
    stale.authorize_shell_integration_on_first_mark(stale_nonce);
    stale.process(format!("\x1b]133;D;0;id={}\x07", fx_hex(&stale_nonce)).as_bytes());
    shells.push(("zsh", stale, cwd));

    let (mut cleared, sign) = fx_marked_shell(ROWS, COLS, Some([0xa4; 32]), false, b"", 6, cwd, 3);
    cleared.process(
        format!(
            "clear\r\n\x1b]133;C{sign}\x07\x1b[H\x1b[2J\x1b]133;D;0{sign}\x07\
             \x1b]133;A{sign}\x07dev@devhost aterm % \x1b]133;B{sign}\x07\x1b]133;D;0{sign}\x07"
        )
        .as_bytes(),
    );
    shells.push(("zsh", cleared, cwd));

    shells.push(("claude", fx_two_drawers(2), cwd));
    let mut restored = Terminal::from_checkpoint(&fx_two_drawers(5).checkpoint());
    restored.process(b"\r\n\x1b[2mresuming\x1b[0m\r\n");
    shells.push(("claude", restored, cwd));

    let mut sessions = Vec::new();
    let mut tabs = Vec::new();
    for (id, (title, term, at)) in (0_u64..).zip(shells) {
        tabs.push(fx_tab(fx_leaf(id, title, Some(at))));
        sessions.push(fx_session(id, title, term));
    }
    FxDesk {
        name: "engine-state",
        nonce_seed: 0x0100_0015,
        ceiling: aterm_update_core::handoff_policy::CarryCeiling::Full,
        window: fx_window_carry(ROWS, COLS, false),
        sessions,
        layout: RestoreManifest::new(vec![fx_window(ROWS, COLS, tabs)]),
        connections: Vec::new(),
        next_turn_id: None,
        fabric_attached: Vec::new(),
        drawn: FxDrawn::default(),
        held: Vec::new(),
    }
}

/// A DESK PAST THE LAYOUT'S WINDOW CAP (v0.108.0 on): a
/// shell in front, and behind it a document in each of [`MAX_WINDOWS`] windows
/// — the config open in the Editor with two selections, notes read in Markdown,
/// one with a passage selected — so the capture folds the last window's tab
/// into the one before it (`RestoreManifest::fold_into_caps`),
/// and the layout says what it folded and which pane was in front.
fn fx_fold_desk() -> FxDesk {
    use crate::restore::{
        NativeLeafRestore, RestoreManifest, RestoreSelection, RestoredSplitTree, RestoredView,
    };
    let cwd = "/home/dev/src/aterm";
    let sessions = vec![fx_session(0, "zsh", fx_shell(ROWS, COLS, 12, cwd, 6))];
    let mut windows = vec![fx_window(
        ROWS,
        COLS,
        vec![fx_tab(fx_leaf(0, "zsh", Some(cwd)))],
    )];
    for n in 1..=crate::restore::MAX_WINDOWS {
        let view = if n == 1 {
            let mut editor = NativeLeafRestore::document(
                "editor",
                "file:///home/dev/.config/aterm/aterm.toml".to_string(),
            );
            editor.config_editor = true;
            editor.editor_selections = vec![
                RestoreSelection {
                    anchor: 120,
                    head: 134,
                },
                RestoreSelection {
                    anchor: 402,
                    head: 402,
                },
            ];
            editor.primary_selection = 1;
            editor.viewport_anchor = 6;
            editor.durable_seq = 3;
            editor
        } else {
            let mut notes = NativeLeafRestore::document(
                "markdown",
                format!("file:///home/dev/notes/week-{n:02}.md"),
            );
            notes.source_anchor = 64 * n;
            notes.viewport_anchor = n;
            if n == 2 {
                notes.selection = Some(RestoreSelection {
                    anchor: 210,
                    head: 288,
                });
            }
            notes
        };
        let mut window = fx_window(
            ROWS,
            COLS,
            vec![fx_tab(RestoredSplitTree::leaf(RestoredView::Native(view)))],
        );
        if window.show.z_order.is_some() {
            window.show.z_order = Some(u32::try_from(n).unwrap());
            window.show.key = Some(false);
        }
        windows.push(window);
    }
    FxDesk {
        name: "fold",
        nonce_seed: 0x0100_0018,
        ceiling: aterm_update_core::handoff_policy::CarryCeiling::Full,
        window: fx_window_carry(ROWS, COLS, false),
        sessions,
        layout: RestoreManifest::new(windows),
        connections: Vec::new(),
        next_turn_id: None,
        fabric_attached: Vec::new(),
        held: Vec::new(),
        drawn: FxDrawn::default(),
    }
}

/// EVERY VARIANT A CARRIED ENUM TAKES THAT NO OTHER DESK DOES (v0.108.0 on),
/// each as a program or the host sets it.
fn fx_variants_desk() -> FxDesk {
    use crate::restore::{
        PlaceholderLeafRestore, RestoreBranch, RestoreManifest, RestoredSplitTree, RestoredTab,
        RestoredView, SplitKind,
    };
    let cwd = "/home/dev/src/aterm";

    let mut nrcs = fx_shell(ROWS, COLS, 6, cwd, 1);
    nrcs.set_color_scheme(aterm_types::Appearance::Light);
    nrcs.process(b"./nrcs-demo\r\n");
    nrcs.process(b"\x1b(R\x1b)Q\x1b*K\x1b+Y\x1bn\x1b~\x1b[?1049h");
    nrcs.process(b"\x1b(E\x1b)Z\x1b*H\x1b+=\x1bo\x1b|\x1b7\x1b[?1049l");
    nrcs.process(b"\x1b(0\x1b)A\x1b*4\x1b+C\x0e");
    nrcs.process(b"\x1b[2 q\x1b[?9h\x1b[?1005h\x1b]9;4;0\x07\x1b[1 k\x1b[8l\x1b[61\"p\x1bN");
    assert_eq!(nrcs.mouse_mode(), aterm_core::terminal::MouseMode::X10);

    let mut latin = fx_shell(ROWS, COLS, 6, cwd, 2);
    let mut config = aterm_core::config::TerminalConfig::default();
    config.bidi.mode = aterm_core::config::BiDiMode::Disabled;
    latin.apply_config(&config);
    latin.process(b"less --charset=latin2 notes.txt\r\n");
    latin.process(b"\x1b(<\x1b-B\x1b.M\x1b[3 q\x1b[?1002h\x1b[?1006h\x1b]9;4;2;75\x07");
    latin.process(b"\x1b[2 k\x1b[62\"p\x1bO");
    assert_eq!(
        latin.mouse_mode(),
        aterm_core::terminal::MouseMode::ButtonEvent
    );

    let mut vt320 = fx_shell(ROWS, COLS, 6, cwd, 3);
    vt320.process(b"./mouse-demo --any\r\n");
    vt320.process(b"\x1b[4 q\x1b[?1003h\x1b[?1015h\x1b]9;4;3\x07\x1b[63\"p");
    assert_eq!(
        vt320.mouse_mode(),
        aterm_core::terminal::MouseMode::AnyEvent
    );

    let mut vt520 = fx_shell(ROWS, COLS, 6, cwd, 4);
    vt520.process(b"./mouse-demo --pixels\r\n");
    vt520.process(b"\x1b[6 q\x1b[?1000h\x1b[?1016h\x1b]9;4;4;10\x07\x1b[65\"p");
    assert_eq!(
        vt520.mouse_encoding(),
        aterm_core::terminal::MouseEncoding::SgrPixel
    );

    let shells = [
        ("nrcs-demo", nrcs),
        ("less", latin),
        ("mouse-demo", vt320),
        ("mouse-demo", vt520),
    ];
    let mut sessions = Vec::new();
    let mut tabs = Vec::new();
    for (id, (title, term)) in (0_u64..).zip(shells) {
        let leaf = fx_leaf(id, title, Some(cwd));
        tabs.push(if id == 3 {
            let retired =
                RestoredSplitTree::leaf(RestoredView::Placeholder(PlaceholderLeafRestore {
                    restore_tag: "terminal".to_string(),
                    reason: crate::update_words::PANE_NOT_CARRIED.to_string(),
                    metadata: format!("folder: {cwd}\ntitle: cargo test"),
                    unhanded: None,
                }));
            RestoredTab {
                root: fx_split(SplitKind::Vertical, retired, leaf),
                focused_path: vec![RestoreBranch::Second],
                zoomed: false,
            }
        } else {
            fx_tab(leaf)
        });
        sessions.push(fx_session(id, title, term));
    }
    let mut app = crate::App::headless_for_test();
    assert!(app.open_system_state_tab_in_window(crate::WindowId(0)));
    let mut captured = app.capture_handoff_layout();
    let system_state = captured.windows[0]
        .restored_tabs
        .pop()
        .expect("the opened System State tab is captured");
    assert!(matches!(
        &system_state.root,
        RestoredSplitTree::Leaf {
            view: RestoredView::Native(native),
        } if native.restore_tag == "system-state"
    ));
    tabs.push(system_state);
    FxDesk {
        name: "variants",
        nonce_seed: 0x0100_0019,
        ceiling: aterm_update_core::handoff_policy::CarryCeiling::Full,
        window: fx_window_carry(ROWS, COLS, false),
        sessions,
        layout: RestoreManifest::new(vec![fx_window(ROWS, COLS, tabs)]),
        connections: Vec::new(),
        next_turn_id: None,
        fabric_attached: Vec::new(),
        held: Vec::new(),
        drawn: FxDrawn::default(),
    }
}

#[test]
fn generate_handoff_fixtures() {
    if !fx_generator_in_child("ATERM_HANDOFF_FIXTURE_OUT") {
        return;
    }
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
        fx_carried_state_desk(),
        fx_engine_state_desk(),
        fx_held_history_desk(true),
        fx_held_history_desk(false),
        fx_fold_desk(),
        fx_variants_desk(),
    ] {
        let mut desk = desk;
        fx_capture_leaves(&mut desk);
        fx_write(&desk, &scratch);
    }
    let _ = std::fs::remove_dir_all(&scratch);
    fx_record_fields();
    let out = std::path::PathBuf::from(
        std::env::var_os("ATERM_HANDOFF_FIXTURE_OUT")
            .expect("set ATERM_HANDOFF_FIXTURE_OUT to tests/fixtures/handoff/v<release>"),
    );
    crate::seamless::fixture_tests::assert_written_desks_adopt(&out);
    fx_record_shipped_state(&out);
}

/// Where the release cut keeps [`fx_record_shipped_state`]'s answers file (`aterm-release`'s
/// `fixtures.rs`).
const SHIPPED_STATE_OUT: &str = "ATERM_SHIPPED_STATE_OUT";

/// THE SHIPPED-STATE ANSWERS THESE DESKS ADD: the serde corpus's sweep of every shipped
/// producer's bytes (`serde_corpus_tests::state_written_by_shipped_producers_answers_as_recorded`)
/// as it reads once the desks in `out` are checked in as this release's, which refuses unless
/// this release's models read back every manifest, layout and meta they hold.
fn fx_record_shipped_state(out: &std::path::Path) {
    let tag = format!("v{}", crate::build_info::version());
    let answers = crate::serde_corpus_tests::shipped_state_with(&tag, out);
    if let Some(to) = std::env::var_os(SHIPPED_STATE_OUT) {
        std::fs::write(&to, answers)
            .unwrap_or_else(|error| panic!("write {}: {error}", to.to_string_lossy()));
    }
}

/// THE CARRIED FIELDS NO HANDOFF OFF MACOS WRITES: a window's show is
/// captured on macOS alone (`window_show::captured_show`), so a session
/// elsewhere carries `WindowShow::UNKNOWN`, and so does its desk
/// (`fx_window_in`).
const NO_HANDOFF_WRITES_OFF_MACOS: &[(&str, &str)] = &[
    ("layout", "WindowLayout.show"),
    ("layout", "WindowShow.fullscreen"),
    ("layout", "WindowShow.key"),
    ("layout", "WindowShow.minimized"),
    ("layout", "WindowShow.z_order"),
];

/// THE CARRIED FIELDS AND VARIANTS NO HANDOFF WRITES, so no desk can: each one
/// the update's producer never sets (written by another writer of its struct,
/// retired, or set by nothing in this build).
const NO_HANDOFF_WRITES: &[(&str, &str)] = &[
    ("control", "Wire.turn_ids"),
    ("manifest", "WindowCarry.presence_row"),
    ("layout", "WindowLayout.presence_row"),
    ("layout", "WindowLayout.maximized"),
    ("layout", "TerminalLeafRestore.held"),
    ("layout", "TerminalLeafRestore.holds"),
    ("layout", "TerminalLeafRestore.profile"),
    ("layout", "CodexRestore.carried"),
    ("layout", "NativeLeafRestore.metadata"),
    ("meta", "ColorRepr.stack"),
    ("meta", "CursorStyle::Hidden"),
    ("meta", "CursorStyle::HollowBlock"),
    ("meta", "CursorStyle::Bolt"),
    ("meta", "ParagraphDirection::AutoRtl"),
    ("meta", "VtLevel::VT240"),
    ("meta", "VtLevel::VT330"),
    ("meta", "VtLevel::VT340"),
    ("meta", "VtLevel::VT510"),
    ("meta", "VtLevel::VT525"),
];

/// THE FIELDS AND VARIANTS THESE DESKS WRITE (`fixture_cover`): recorded
/// beside them as `fields.toml`, and held strictly.
fn fx_record_fields() {
    use crate::seamless::fixture_cover::{DeskFields, FIELDS_FILE, newest_recorded};
    let out = std::path::PathBuf::from(
        std::env::var_os("ATERM_HANDOFF_FIXTURE_OUT")
            .expect("set ATERM_HANDOFF_FIXTURE_OUT to tests/fixtures/handoff/v<release>"),
    );
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/handoff");
    let baseline = newest_recorded(std::path::Path::new(root), &out);
    let fields = DeskFields::of_release(&out);
    std::fs::write(out.join(FIELDS_FILE), fields.to_toml()).expect("write fields.toml");
    let off_macos: &[(&str, &str)] = if cfg!(target_os = "macos") {
        &[]
    } else {
        NO_HANDOFF_WRITES_OFF_MACOS
    };
    let unhanded: Vec<(&str, &str)> = NO_HANDOFF_WRITES.iter().chain(off_macos).copied().collect();
    let uncovered = fields.uncovered(baseline.as_ref().map(|(_, recorded)| recorded), &unhanded);
    assert!(
        uncovered.is_empty(),
        "carried fields and variants no desk here writes{}; give each a value in \
         a desk, as a real session would carry it, or name one no handoff writes \
         in NO_HANDOFF_WRITES with why (tests/fixtures/handoff/README.md):\n  {}",
        baseline
            .as_ref()
            .map_or_else(String::new, |(release, _)| format!(
                " (held since {release})"
            )),
        uncovered.join("\n  ")
    );
}

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

    /// Write `$ATERM_RENDEZVOUS_FIXTURE_OUT/rendezvous.toml` with this release's claim, grant,
    /// descriptor order, launch environment and device-term proof over the fixed inputs.
    #[test]
    fn generate_rendezvous_fixture() {
        if !super::fx_generator_in_child("ATERM_RENDEZVOUS_FIXTURE_OUT") {
            return;
        }
        let _env = super::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        super::fx_install_release_stamp();
        let out = std::path::PathBuf::from(
            std::env::var_os("ATERM_RENDEZVOUS_FIXTURE_OUT")
                .expect("set ATERM_RENDEZVOUS_FIXTURE_OUT to tests/fixtures/handoff/v<release>"),
        );
        let scratch = crate::test_scratch::short_dir("the claim's socket");
        let capture = capture_transport(scratch.path());
        drop(scratch);
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
