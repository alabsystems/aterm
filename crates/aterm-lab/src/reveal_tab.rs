// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab reveal-tab`: one-mesh design §11 A17 end to end, on a private windowed instance.
//!
//! The world's home holds a one-Mac Commons (`aterm_db::table::fixture::Lab`), and the real
//! `aterm db alab import --mailbox-toml` seals synthetic mailbox logins (`synthetic-pass-NN`) from
//! a mailbox file in the world into its registry: the one import source that needs no
//! `[everyone]` key, which a fixture Commons has none of. Outside aterm (a launchd job under `script`, so a terminal and
//! no aterm ancestor) `aterm db emails --show-passwords` prints them. In the instance's boot tab a
//! zsh whose rc accepts that command line once, nobody typing over the control socket, opens them
//! in a read-only Editor tab beside it, and the tab gets one line naming that tab. Then `text`,
//! `screen`, `image`, `window`, `video`, `key` and `doc read` of the Editor tab refuse; the tab's
//! own text, blocks and session log hold no value; and closing the Editor tab ends the command and
//! removes its folder. The file the Editor shows holds the values: the positive control.
//!
//! A17b: the window's accessibility tree, read as any process the person granted Accessibility
//! reads it (the AX API, every string attribute of every element and each text element's
//! characters), carries the grid (a `hidden value` cell) and every login, and no value. Where
//! the tree's reader holds no Accessibility grant, those checks are a named skip, never a pass.
//!
//! A17c: the reader is this process ([`TreeReader::Own`]), or with `--ax-helper` the guest
//! helper's `ax-texts` verb ([`TreeReader::Helper`]), which `lab mac nux`'s `reveal-tab` rows use:
//! there this run is the guest's, in a clone of the person base, whose helper holds Accessibility
//! by its identifier. Its `--json` report carries the run's [`Evidence`] ([`evidence_json`],
//! never a value), so the host judges it with [`judge`]. `--plain-publication` turns on the GUI's
//! negative control in the instance (each Editor publishes as though nothing were concealed),
//! which must fail "and no value".
//!
//! Nothing reads or writes this Mac's vault, registry or journal: every process runs in the
//! world's environment, its home the fixture's, and the world goes when the run ends. The report
//! names checks only, never a value.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_db::table::fixture::Lab;
use aterm_json::{Value, json};
use aterm_secrets::commons::roster::Role;

use crate::band::Job;
use crate::instance::{
    CONFIG_OFF, Instance, Launch, Reply, World, resolve_aterm, take_flag, take_value,
    terminal_request,
};
use crate::{Fail, Res};

/// What every synthetic password the fixture seeds starts with (`synthetic-pass-NN`).
const NEEDLE: &str = "synthetic-pass-";

/// Mailbox logins seeded.
const ROWS: usize = 3;

/// The synthetic mailbox file the import reads, in the world's folder.
const MAILBOX_FILE: &str = "ai-mailbox.toml";

/// One-minute hang detector for each wait: an import, the job, the Editor tab, its close.
const HANG: Duration = Duration::from_secs(60);

/// The line the command leaves in its tab, in part (`views::opened_line`).
const OPENED: &str = "read-only Editor tab beside this one";

/// What a read of the private front answers (`PRIVATE_VIEW_REFUSED`), in part.
const REFUSED_VIEW: &str = "refused private view";

/// The instance's window, in cells of aterm's own font. A grid publishes only the columns on
/// its screen (`amacs::editor::grid::project_grid`), and the emails grid's password is the
/// sixteenth of the address table's seventeen columns, each up to 40 cells wide: about 270 cells
/// in all. The Editor tab draws in its own font whatever the terminal's (`--font-px` shrinks the
/// window and not the grid's cells), so the window is wide in cells: at 320 columns of the 12 px
/// default it is about 2,300 points wide, which the tree rows' guest display holds. Measured
/// 2026-11-05 on m111: at aterm's own size in a 1024-point guest the tree carried the logins and
/// no password cell; at 360 columns of a 7 px font, fifteen columns and one row.
const WINDOW: [&str; 2] = ["--columns", "320"];

/// What the run saw, as [`judge`] reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Evidence {
    pub imported: bool,
    /// Outside aterm, the command printed the values; what its terminal and job log said else.
    pub outside_printed: bool,
    pub outside_said: String,
    /// The Editor tab opened on a private document, and its file holds the values.
    pub editor_opened: bool,
    pub file_holds_values: bool,
    /// The first line of each refused read or drive, by verb.
    pub refusals: Vec<(String, String)>,
    /// `doc read` of the private document.
    pub doc_read: String,
    /// The caller's tab: its text names the Editor tab; its text, blocks and log hold no value;
    /// its blocks hold the command and its log a run (so neither check reads nothing).
    pub tab_names_editor: bool,
    pub tab_text_clean: bool,
    pub blocks_hold_command: bool,
    pub blocks_clean: bool,
    pub log_clean: bool,
    /// Closing the Editor tab ended the command and removed its folder.
    pub closed_ends_command: bool,
    pub folder_removed: bool,
    /// The caller's tab, read again after the close, holds no value.
    pub after_close_clean: bool,
    /// The Editor tab's accessibility tree as a process with Accessibility reads it; `None`
    /// where this process holds no Accessibility grant.
    pub ax: Option<AxEvidence>,
}

/// What the window's accessibility tree held while the Editor tab was in front.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AxEvidence {
    /// The grid's nodes were published: a concealed cell's `hidden value` was read.
    pub grid_read: bool,
    /// Every cell not secret that the caller named as on the first screen (a login) was read:
    /// the columns not secret are there.
    pub logins_read: bool,
    /// No revealed value was read.
    pub clean: bool,
}

fn fail(error: impl std::fmt::Display) -> Fail {
    Fail::die(format!("reveal-tab: {error}"))
}

/// Wait up to [`HANG`] for `ready` to answer `Some`.
fn within<T>(what: &str, mut ready: impl FnMut() -> Option<T>) -> Result<T, Fail> {
    let end = Instant::now() + HANG;
    loop {
        if let Some(value) = ready() {
            return Ok(value);
        }
        if Instant::now() >= end {
            return Err(fail(format!("{what}: not within {} s", HANG.as_secs())));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The world's environment with zsh as its shell.
pub(crate) fn zsh_env(world: &World) -> Vec<(String, String)> {
    let mut env = world.env();
    for (key, value) in &mut env {
        if key == "SHELL" {
            "/bin/zsh".clone_into(value);
        }
    }
    env
}

/// A zsh rc that queues `line` and accepts it once, at the first prompt: the command starts as
/// the person's own typing would, with no control client typing it.
pub(crate) fn zshrc(line: &str) -> String {
    format!(
        "typeset -gi __a17_go=1\n\
         __a17_accept() {{ if (( __a17_go )); then __a17_go=0; zle accept-line; fi }}\n\
         zle -N zle-line-init __a17_accept\n\
         print -z -- '{line}'\n"
    )
}

/// The grid scratch folders under the world's temporary root (`private_edit::scratch_root`).
fn scratch_folders(world: &World) -> Vec<PathBuf> {
    let root = aterm_tempfile::quiet_in(&world.home.join("tmp"));
    std::fs::read_dir(root)
        .map(|listing| {
            listing
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .and_then(aterm_types::private_edit::scratch_maker)
                        .is_some()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `text` holds any of `needles`.
fn holds(text: &str, needles: &[String]) -> bool {
    needles.iter().any(|needle| text.contains(needle.as_str()))
}

/// Whether any file directly in `folder` holds any of `needles`.
fn folder_holds(folder: &Path, needles: &[String]) -> bool {
    std::fs::read_dir(folder).is_ok_and(|listing| {
        listing.flatten().any(|entry| {
            entry.file_type().is_ok_and(|t| t.is_file())
                && crate::util::read_regular_file(&entry.path()).is_ok_and(|bytes| {
                    needles.iter().any(|needle| {
                        bytes
                            .windows(needle.len())
                            .any(|window| window == needle.as_bytes())
                    })
                })
        })
    })
}

/// `doc ls`'s private document: its id, else `None`.
fn private_doc(inst: &Instance) -> Option<String> {
    let listed = inst.ctl(&["doc", "ls"]);
    listed
        .stdout
        .lines()
        .find(|line| line.contains(aterm_types::private_edit::DIR_SUFFIX))
        .and_then(|line| {
            line.split_whitespace()
                .find_map(|word| word.strip_prefix("doc="))
                .map(str::to_owned)
        })
}

/// `text` with each line that holds a value withheld and each other cut to 160 bytes: what the
/// report may say of a run.
pub(crate) fn scrub(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(4096));
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if line.contains(NEEDLE) {
            out.push_str("[a line with a value, withheld]");
        } else {
            out.extend(line.chars().take(160));
        }
    }
    out
}

/// A reply's whole text, both streams.
fn said(reply: &Reply) -> String {
    format!("{}{}", reply.stdout, reply.stderr)
}

/// Run `aterm db emails --show-passwords` outside aterm: a launchd job (no aterm ancestor) under
/// `script` (a terminal on both ends), in the world's environment. Whether it printed the values,
/// and what its terminal and the job's log said ([`scrub`]bed).
fn outside(world: &World, aterm: &Path) -> Result<(bool, String), Fail> {
    let dir = world.dir.join("outside");
    aterm_uds::private_file::create_private_dir(&dir).map_err(fail)?;
    let typescript = dir.join("typescript");
    let log = dir.join("job.log");
    // launchd's stdin is no terminal and no file: `script` refuses it (`tcgetattr/ioctl:
    // Operation not supported on socket`) and takes `/dev/null` (ENOTTY), so a shell gives it that
    // and execs, leaving `script` the command's parent and launchd its.
    let line = [
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        "exec /usr/bin/script -q \"$@\" </dev/null".to_owned(),
        "script".to_owned(),
        typescript.to_string_lossy().into_owned(),
        aterm.to_string_lossy().into_owned(),
        "db".to_owned(),
        "emails".to_owned(),
        "--show-passwords".to_owned(),
    ];
    let job = Job::start(&dir, &line, &world.env(), &world.home, &log).map_err(fail)?;
    let ended = within("the outside job", || (!job.running()).then_some(()));
    crate::instance::after_stop(ended, job.remove().map_err(|why| why.to_string()))?;
    let printed = crate::util::read_regular_text(&typescript).unwrap_or_default();
    let logged = crate::util::read_regular_text(&log).unwrap_or_default();
    Ok((
        printed.contains(NEEDLE),
        scrub(&format!("{printed}{logged}")),
    ))
}

/// The GUI's negative control `--plain-publication` turns on: each Editor publishes its document as
/// though nothing were concealed (`aterm-gui`'s `EditorApp::plain_for_negative_control`, which a
/// debug build's Editor takes from `aterm_types::negative_control::on` of this name).
const PLAIN_PUBLICATION: &str = "plain-publication";

/// How a run reads the tree, and whether its instance publishes plainly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Ask {
    pub(crate) reader: TreeReader,
    /// The instance runs under [`PLAIN_PUBLICATION`]: the tree must carry the values.
    pub(crate) plain: bool,
}

/// The instance's own environment: the world's with zsh, and with `plain` the negative control's
/// variable naming [`PLAIN_PUBLICATION`], in a build that has the variable's reader
/// (`aterm_types::negative_control::on`, under `any(test, debug_assertions)`; census
/// `debug-seam` reads this function's own `cfg`, not the debug-only `mod` that mounts the file).
#[cfg(any(test, debug_assertions))]
fn instance_env(world: &World, plain: bool) -> Vec<(String, String)> {
    let mut env = zsh_env(world);
    if plain {
        env.push((
            crate::mesh_churn::NEGATIVE_CONTROL.to_owned(),
            PLAIN_PUBLICATION.to_owned(),
        ));
    }
    env
}

/// The run: the world and its registry, the outside print, then the instance.
///
/// # Errors
/// A step that has to work for the checks to mean anything failed.
pub(crate) fn run(aterm: &Path, ask: &Ask) -> Result<Evidence, Fail> {
    let mut ev = Evidence::default();
    let mut world = World::new(CONFIG_OFF).map_err(fail)?;
    let vaults = aterm_tempfile::Builder::new()
        .prefix("v")
        .tempdir_in(&world.dir)
        .map_err(fail)?;
    world.rehome(&vaults.path().join("m1")).map_err(fail)?;
    if aterm_types::dirs::home_dir().is_some_and(|live| live == world.home) {
        return Err(fail("the world's home is this user's"));
    }
    let lab = Lab::in_home(vaults, &[Role::Writer]);
    if lab.paths[0].home != world.home {
        return Err(fail("the fixture's Mac is not the world's home"));
    }
    aterm_uds::private_file::write_private(&world.dir, MAILBOX_FILE, mailbox_file(ROWS).as_bytes())
        .map_err(fail)?;
    let mailboxes = world.dir.join(MAILBOX_FILE);
    let env = zsh_env(&world);
    let import = aterm_uds::spawnfd::command(aterm)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .args(["db", "alab", "import", "--mailbox-toml"])
        .arg(&mailboxes)
        .arg("--apply")
        .current_dir(&world.home)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .map_err(fail)?;
    ev.imported = import.status.success();
    if !ev.imported {
        return Err(fail(format!(
            "the import failed ({}): {}",
            import.status,
            String::from_utf8_lossy(&import.stderr)
                .lines()
                .next()
                .unwrap_or_default()
        )));
    }
    (ev.outside_printed, ev.outside_said) = outside(&world, aterm)?;

    let command = aterm.to_string_lossy();
    if command.contains('\'') {
        return Err(fail("the aterm path holds a single quote"));
    }
    crate::util::write_fresh(
        &world.home.join(".zshrc"),
        zshrc(&format!("{command} db emails --show-passwords")),
    )
    .map_err(fail)?;
    let mut inst = Instance::start(
        &world,
        Launch {
            aterm: aterm.to_path_buf(),
            headless: false,
            args: WINDOW.map(str::to_owned).to_vec(),
            env: instance_env(&world, ask.plain),
            cwd: world.home.clone(),
            sandbox: None,
            reroute: false,
        },
    )
    .map_err(fail)?;
    let logins: Vec<String> = (1..=ROWS)
        .map(|i| format!("box{i:02}@proton.example"))
        .collect();
    let observed = observe_with(
        &mut inst,
        &world,
        &ask.reader,
        &[NEEDLE.to_owned()],
        &logins,
        &mut ev,
    );
    crate::instance::after_stop(observed, inst.stop())?;
    drop(lab);
    Ok(ev)
}

/// Everything read of the running instance, `needles` being the values its command reveals and
/// `shown` the cells not secret that the grid's first screen shows, which its accessibility tree
/// must still carry; the tree read by this process ([`TreeReader::Own`]).
///
/// # Errors
/// As [`observe_with`].
pub(crate) fn observe(
    inst: &mut Instance,
    world: &World,
    needles: &[String],
    shown: &[String],
    ev: &mut Evidence,
) -> Result<(), Fail> {
    observe_with(inst, world, &TreeReader::Own, needles, shown, ev)
}

/// [`observe`], the tree read by `reader`.
///
/// # Errors
/// The Editor tab did not open, could not be closed, or the tree's reader failed.
pub(crate) fn observe_with(
    inst: &mut Instance,
    world: &World,
    reader: &TreeReader,
    needles: &[String],
    shown: &[String],
    ev: &mut Evidence,
) -> Result<(), Fail> {
    inst.await_socket(HANG)?;
    let doc = within("the Editor tab", || private_doc(inst))?;
    ev.editor_opened = true;
    let folders = scratch_folders(world);
    ev.file_holds_values = folders.iter().any(|folder| folder_holds(folder, needles));
    ev.ax = ax_tree(inst, reader, needles, shown)?;

    for verb in [
        &["text"][..],
        &["screen"],
        &["image"],
        &["window"],
        &["video", "1"],
        &["key", "a"],
        &["pointer", "status"],
        &["invoke", "SelectAll"],
    ] {
        ev.refusals.push((verb.join(" "), inst.ctl(verb).first()));
    }
    ev.doc_read = inst.ctl(&["doc", "read", &doc]).first();

    let tab = said(&terminal_request(|args| inst.ctl(args), &["text"]));
    ev.tab_names_editor = tab.contains(OPENED);
    ev.tab_text_clean = !holds(&tab, needles);
    let blocks = said(&terminal_request(|args| inst.ctl(args), &["blocks"]));
    ev.blocks_hold_command = blocks.contains("--show-passwords");
    ev.blocks_clean = !holds(&blocks, needles);
    // The log keeps a command's program, never its output or arguments: a run is there, and no
    // value, the in-memory content tier included.
    let log = said(&terminal_request(
        |args| inst.ctl(args),
        &["log", "tier=content"],
    ));
    ev.log_clean = log.contains("open r") && !holds(&log, needles);

    let closed = inst.ctl(&["tab", "close", "2"]);
    if closed.code != 0 {
        return Err(fail(format!("closing the Editor tab: {}", closed.first())));
    }
    ev.closed_ends_command = within("the command's end", || {
        let blocks = said(&terminal_request(|args| inst.ctl(args), &["blocks"]));
        (private_doc(inst).is_none() && blocks.contains("exit=0")).then_some(())
    })
    .is_ok();
    ev.folder_removed = within("the folder's removal", || {
        folders.iter().all(|folder| !folder.exists()).then_some(())
    })
    .is_ok();
    let after = [
        said(&terminal_request(|args| inst.ctl(args), &["text"])),
        said(&terminal_request(|args| inst.ctl(args), &["blocks"])),
        said(&terminal_request(
            |args| inst.ctl(args),
            &["log", "tier=content"],
        )),
    ];
    ev.after_close_clean = after.iter().all(|read| !holds(read, needles));
    Ok(())
}

/// What a concealed grid cell reads as (`amacs::editor::HIDDEN_VALUE`).
const HIDDEN_VALUE: &str = "hidden value";

/// Who reads the window's accessibility tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TreeReader {
    /// This process, through the AX API ([`ax`]): macOS judges the grant of the app it runs under.
    Own,
    /// The guest helper at this path, `aterm-lab-guest ax-texts PID` (`guest/main.rs`), which reads
    /// the tree with the same [`ax`] code. It is started disclaimed, so it is its own responsible
    /// process and tccd judges its own grant: a `lab mac nux` person base grants Accessibility to
    /// that path and identifier, and to nothing this run is.
    Helper(PathBuf),
}

/// What the guest helper's `ax-texts` printed (`{"trusted": bool, "texts": [string…]}`, its
/// `tree_report`): the strings it read, `None` inside where it holds no Accessibility grant, and
/// `None` outside for anything else.
fn helper_reading(stdout: &str) -> Option<Option<Vec<String>>> {
    let reading = aterm_json::from_str::<Value>(stdout.trim()).ok()?;
    if !reading.get("trusted")?.as_bool()? {
        return Some(None);
    }
    reading
        .get("texts")?
        .as_array()?
        .iter()
        .map(|text| text.as_str().map(str::to_owned))
        .collect::<Option<Vec<_>>>()
        .map(Some)
}

/// One reading of `pid`'s windows by `reader`: every string they carry, `None` where the reader
/// holds no Accessibility grant.
///
/// # Errors
/// The helper did not start, did not end within [`HANG`], ended in failure, or printed no reading.
fn read_tree(reader: &TreeReader, pid: i32) -> Result<Option<Vec<String>>, Fail> {
    let helper = match reader {
        TreeReader::Own => return Ok(ax::trusted().then(|| ax::texts(pid))),
        TreeReader::Helper(helper) => helper,
    };
    let out = aterm_uds::deadline::output_within(
        aterm_uds::spawnfd::command(helper)
            .arg("ax-texts")
            .arg(pid.to_string())
            .disclaim_responsibility(true),
        HANG,
    )
    .map_err(|e| fail(format!("the tree's reader {}: {e}", helper.display())))?;
    if !out.status.success() {
        return Err(fail(format!(
            "the tree's reader {} ended {}: {}",
            helper.display(),
            out.status,
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .next()
                .unwrap_or_default()
        )));
    }
    helper_reading(&String::from_utf8_lossy(&out.stdout)).ok_or_else(|| {
        fail(format!(
            "the tree's reader {} printed no reading",
            helper.display()
        ))
    })
}

/// The Editor tab's accessibility tree as a process with Accessibility reads it ([`AxEvidence`]),
/// read by `reader` until the grid's nodes are there (an adapter publishes once a reader attaches)
/// or the hang detector ends the wait; `None` where the reader holds no Accessibility grant.
/// `needles` are the revealed values it must not carry, `shown` the cells it must.
fn ax_tree(
    inst: &Instance,
    reader: &TreeReader,
    needles: &[String],
    shown: &[String],
) -> Result<Option<AxEvidence>, Fail> {
    let pid = inst
        .aterm_pid()
        .ok_or_else(|| fail("the instance's aterm has no pid"))?;
    let end = Instant::now() + HANG;
    loop {
        let Some(texts) = read_tree(reader, pid)? else {
            return Ok(None);
        };
        let holds = |needle: &str| texts.iter().any(|text| text.contains(needle));
        let seen = AxEvidence {
            grid_read: holds(HIDDEN_VALUE),
            logins_read: shown.iter().all(|cell| holds(cell)),
            clean: !needles.iter().any(|needle| holds(needle)),
        };
        if (seen.grid_read && seen.logins_read) || !seen.clean || Instant::now() >= end {
            return Ok(Some(seen));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[path = "ax_texts.rs"]
mod ax;

/// Whether a verb's first line is the private-view refusal: `ERR refused private view: …`, or
/// the verb's own `ERR <verb…>: refused private view: …` (`video`, `pointer`).
fn refused_view(first: &str) -> bool {
    first.split_once("ERR ").is_some_and(|(_, why)| {
        why.starts_with(REFUSED_VIEW) || why.contains(&format!(": {REFUSED_VIEW}"))
    })
}

/// A harness mailbox file naming `n` synthetic logins, `box<i>@proton.example` with password
/// `synthetic-pass-<i>` ([`NEEDLE`]), each its own `[[mailboxes]]` entry.
fn mailbox_file(n: usize) -> String {
    (1..=n)
        .map(|i| {
            format!(
                "[[mailboxes]]\nlogin = \"box{i:02}@proton.example\"\n\
                 password = \"{NEEDLE}{i:02}\"\n"
            )
        })
        .collect()
}

/// Each check and whether it held.
#[must_use]
pub fn judge(ev: &Evidence) -> Vec<(String, bool)> {
    let mut checks = vec![
        (
            "the import sealed the synthetic logins".to_owned(),
            ev.imported,
        ),
        (
            "outside aterm the command prints the values".to_owned(),
            ev.outside_printed,
        ),
    ];
    checks.extend(judge_tab(ev));
    checks
}

/// The checks of the Editor tab and the caller's tab ([`observe`]'s), and whether each held.
#[must_use]
pub fn judge_tab(ev: &Evidence) -> Vec<(String, bool)> {
    let mut checks = vec![
        (
            "in a tab the Editor tab opens on a private document".to_owned(),
            ev.editor_opened,
        ),
        (
            "its file holds the values (positive control)".to_owned(),
            ev.file_holds_values,
        ),
    ];
    for (verb, first) in &ev.refusals {
        checks.push((
            format!("`{verb}` of the Editor tab refuses"),
            refused_view(first),
        ));
    }
    checks.extend([
        (
            "`doc read` of its document refuses".to_owned(),
            ev.doc_read.contains("ERR refused private doc"),
        ),
        (
            "the tab's text names the Editor tab".to_owned(),
            ev.tab_names_editor,
        ),
        (
            "the tab's text holds no value".to_owned(),
            ev.tab_text_clean,
        ),
        (
            "the tab's blocks hold the command".to_owned(),
            ev.blocks_hold_command,
        ),
        ("the tab's blocks hold no value".to_owned(), ev.blocks_clean),
        (
            "the session log has the run and holds no value".to_owned(),
            ev.log_clean,
        ),
        (
            "closing the Editor tab ends the command".to_owned(),
            ev.closed_ends_command,
        ),
        ("and removes its folder".to_owned(), ev.folder_removed),
        (
            "after the close the tab holds no value".to_owned(),
            ev.after_close_clean,
        ),
    ]);
    if let Some(ax) = &ev.ax {
        let [grid, logins, clean] = TREE_CHECKS;
        checks.extend([
            (grid.to_owned(), ax.grid_read),
            (logins.to_owned(), ax.logins_read),
            (clean.to_owned(), ax.clean),
        ]);
    }
    checks
}

/// The names of the three checks of the window's accessibility tree, as [`judge_tab`] gives them:
/// it carries the grid ([`AxEvidence::grid_read`]), every login ([`AxEvidence::logins_read`]),
/// and no value ([`AxEvidence::clean`]).
pub(crate) const TREE_CHECKS: [&str; 3] = [
    "the window's accessibility tree carries the grid",
    "and every login",
    "and no value",
];

/// The checks this run could not make, and why.
pub(crate) fn skipped(ev: &Evidence) -> Vec<&'static str> {
    if ev.ax.is_none() {
        vec![
            "the window's accessibility tree: its reader holds no Accessibility grant \
             (System Settings > Privacy & Security > Accessibility, for the app this lab runs \
             under, or in a `lab mac nux` guest the guest helper's)",
        ]
    } else {
        Vec::new()
    }
}

/// `ev` as a report carries it: every field, each text [`scrub`]bed, so no value is in it.
#[must_use]
pub(crate) fn evidence_json(ev: &Evidence) -> Value {
    let refusals: Vec<Value> = ev
        .refusals
        .iter()
        .map(|(verb, first)| json!({"verb": verb.as_str(), "first": scrub(first)}))
        .collect();
    let ax = ev.ax.as_ref().map_or(
        Value::Null,
        |ax| json!({"grid_read": ax.grid_read, "logins_read": ax.logins_read, "clean": ax.clean}),
    );
    json!({
        "imported": ev.imported,
        "outside_printed": ev.outside_printed,
        "outside_said": scrub(&ev.outside_said),
        "editor_opened": ev.editor_opened,
        "file_holds_values": ev.file_holds_values,
        "refusals": refusals,
        "doc_read": scrub(&ev.doc_read),
        "tab_names_editor": ev.tab_names_editor,
        "tab_text_clean": ev.tab_text_clean,
        "blocks_hold_command": ev.blocks_hold_command,
        "blocks_clean": ev.blocks_clean,
        "log_clean": ev.log_clean,
        "closed_ends_command": ev.closed_ends_command,
        "folder_removed": ev.folder_removed,
        "after_close_clean": ev.after_close_clean,
        "ax": ax,
    })
}

/// The [`Evidence`] a report's [`evidence_json`] carries: `None` when any field is missing or of
/// another kind, so a report from another build is never read as a run with nothing found.
#[must_use]
pub(crate) fn evidence_from(v: &Value) -> Option<Evidence> {
    let flag = |key: &str| v.get(key).and_then(Value::as_bool);
    let text = |key: &str| v.get(key).and_then(Value::as_str).map(str::to_owned);
    let refusals = v
        .get("refusals")?
        .as_array()?
        .iter()
        .map(|r| {
            Some((
                r.get("verb")?.as_str()?.to_owned(),
                r.get("first")?.as_str()?.to_owned(),
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    let ax = match v.get("ax")? {
        Value::Null => None,
        ax => {
            let flag = |key: &str| ax.get(key).and_then(Value::as_bool);
            Some(AxEvidence {
                grid_read: flag("grid_read")?,
                logins_read: flag("logins_read")?,
                clean: flag("clean")?,
            })
        }
    };
    Some(Evidence {
        imported: flag("imported")?,
        outside_printed: flag("outside_printed")?,
        outside_said: text("outside_said")?,
        editor_opened: flag("editor_opened")?,
        file_holds_values: flag("file_holds_values")?,
        refusals,
        doc_read: text("doc_read")?,
        tab_names_editor: flag("tab_names_editor")?,
        tab_text_clean: flag("tab_text_clean")?,
        blocks_hold_command: flag("blocks_hold_command")?,
        blocks_clean: flag("blocks_clean")?,
        log_clean: flag("log_clean")?,
        closed_ends_command: flag("closed_ends_command")?,
        folder_removed: flag("folder_removed")?,
        after_close_clean: flag("after_close_clean")?,
        ax,
    })
}

/// `lab reveal-tab [--json] [--ax-helper HELPER] [--plain-publication] [<aterm>]`.
///
/// # Errors
/// Usage, or a step the checks rest on failed.
pub fn cmd(args: &[String]) -> Res {
    let mut args = args.to_vec();
    let as_json = take_flag(&mut args, "--json");
    let plain = take_flag(&mut args, "--plain-publication");
    let reader = match take_value(&mut args, "--ax-helper")? {
        Some(helper) => TreeReader::Helper(PathBuf::from(crate::paths::from_caller(&helper))),
        None => TreeReader::Own,
    };
    if args.len() > 1 {
        return Err(Fail::usage(
            "reveal-tab takes --json, --ax-helper HELPER, --plain-publication and an aterm at most",
        ));
    }
    let aterm = resolve_aterm(args.pop()).map_err(|e| Fail::exit(3, e))?;
    let ev = run(&aterm, &Ask { reader, plain })?;
    let checks = judge(&ev);
    let pass = checks.iter().all(|(_, held)| *held);
    let report: Value = json!({
        "lane": "reveal-tab",
        "pass": pass,
        "plain_publication": plain,
        "checks": checks.iter().map(|(name, held)| json!({"check": name, "pass": held})).collect::<Vec<_>>(),
        "skipped": skipped(&ev).into_iter().map(|why| json!(why)).collect::<Vec<_>>(),
        "evidence": evidence_json(&ev),
    });
    let text = if as_json {
        format!("{}\n", aterm_json::to_string(&report).map_err(fail)?)
    } else {
        let mut text = String::from("reveal-tab: a private windowed instance, synthetic logins\n");
        for (name, held) in &checks {
            text.push_str(&format!(
                "  {}  {name}\n",
                if *held { "PASS" } else { "FAIL" }
            ));
        }
        for why in skipped(&ev) {
            text.push_str(&format!("  SKIP  {why}\n"));
        }
        if !pass {
            for (verb, first) in ev
                .refusals
                .iter()
                .chain([&("doc read".to_owned(), ev.doc_read.clone())])
            {
                text.push_str(&format!("  {verb}: {}\n", scrub(first)));
            }
            if !ev.outside_printed {
                text.push_str(&format!("  outside said:\n{}\n", ev.outside_said));
            }
        }
        text.push_str(if pass { "PASS\n" } else { "FAIL\n" });
        text
    };
    print!("{text}");
    Ok(i32::from(!pass))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passing() -> Evidence {
        Evidence {
            imported: true,
            outside_printed: true,
            outside_said: String::new(),
            editor_opened: true,
            file_holds_values: true,
            refusals: vec![(
                "text".to_owned(),
                "aterm-ctl: ERR refused private view: the front tab shows a private document"
                    .to_owned(),
            )],
            doc_read: "aterm-ctl: ERR refused private doc".to_owned(),
            tab_names_editor: true,
            tab_text_clean: true,
            blocks_hold_command: true,
            blocks_clean: true,
            log_clean: true,
            closed_ends_command: true,
            folder_removed: true,
            after_close_clean: true,
            ax: Some(AxEvidence {
                grid_read: true,
                logins_read: true,
                clean: true,
            }),
        }
    }

    /// Every check holds on a passing run, and each fails on its own: an answer that is not the
    /// refusal, a value in the tab, a refused `doc read` turned served.
    #[test]
    fn each_check_fails_alone() {
        assert!(judge(&passing()).iter().all(|(_, held)| *held));
        let mut served = passing();
        served.refusals[0].1 = "OK 24".to_owned();
        let mut leaked = passing();
        leaked.tab_text_clean = false;
        let mut read = passing();
        read.doc_read = "OK 120 doc=1".to_owned();
        let mut spoken = passing();
        spoken.ax = Some(AxEvidence {
            clean: false,
            ..AxEvidence::default()
        });
        let mut unpublished = passing();
        unpublished.ax = Some(AxEvidence {
            grid_read: false,
            ..passing().ax.unwrap()
        });
        for ev in [served, leaked, read, unpublished] {
            assert_eq!(judge(&ev).iter().filter(|(_, held)| !*held).count(), 1);
        }
        assert_eq!(judge(&spoken).iter().filter(|(_, held)| !*held).count(), 3);
        assert!(skipped(&passing()).is_empty());
    }

    /// Without an Accessibility grant the tree's checks are a named skip, never a pass.
    #[test]
    fn an_unread_tree_is_a_skip_not_a_pass() {
        let mut unread = passing();
        unread.ax = None;
        let checks = judge(&unread);
        assert!(
            checks
                .iter()
                .all(|(name, _)| !name.contains("accessibility"))
        );
        assert_eq!(skipped(&unread).len(), 1);
    }

    /// A report's evidence reads back as the run it was, the tree unread too, and a report missing
    /// a field (another build's) reads as none rather than as a run that found nothing. A text
    /// holding a value is withheld on the way, and a refusal withheld that way no longer reads as
    /// one, so the host fails it as the guest would have.
    #[test]
    fn a_reports_evidence_reads_back_and_carries_no_value() {
        let mut unread = passing();
        unread.ax = None;
        for ev in [passing(), unread] {
            let wire = evidence_json(&ev);
            assert_eq!(evidence_from(&wire), Some(ev.clone()));
            let text = aterm_json::to_string(&wire).unwrap();
            let parsed = aterm_json::from_str::<Value>(&text).unwrap();
            assert_eq!(evidence_from(&parsed), Some(ev));
        }
        let mut wire = evidence_json(&passing());
        if let Value::Object(map) = &mut wire {
            map.remove("log_clean");
        }
        assert_eq!(evidence_from(&wire), None);

        let mut leaked = passing();
        leaked.refusals[0].1 = format!("{NEEDLE}01");
        leaked.outside_said = format!("box01@proton.example {NEEDLE}01");
        let wire = evidence_json(&leaked);
        assert!(!aterm_json::to_string(&wire).unwrap().contains(NEEDLE));
        let back = evidence_from(&wire).unwrap();
        assert!(!refused_view(&back.refusals[0].1));
        assert_eq!(judge(&back).iter().filter(|(_, held)| !*held).count(), 1);
    }

    /// The guest helper's reading is its strings, or no grant; anything else is no reading.
    #[test]
    fn the_helpers_reading_is_its_strings_or_no_grant() {
        assert_eq!(
            helper_reading(
                "{\"trusted\":true,\"texts\":[\"hidden value\",\"box01@proton.example\"]}\n"
            ),
            Some(Some(vec![
                "hidden value".to_owned(),
                "box01@proton.example".to_owned()
            ]))
        );
        assert_eq!(
            helper_reading("{\"trusted\":false,\"texts\":[]}"),
            Some(None)
        );
        for other in [
            "",
            "trusted=yes",
            "{\"texts\":[]}",
            "{\"trusted\":true}",
            "{\"trusted\":true,\"texts\":[1]}",
        ] {
            assert_eq!(helper_reading(other), None, "{other}");
        }
    }

    /// The negative control reaches the instance by name, and only when asked.
    #[test]
    fn the_plain_publication_is_the_instances_alone_and_only_when_asked() {
        let world = World::new(CONFIG_OFF).unwrap();
        let named = |env: &[(String, String)]| {
            env.iter()
                .filter(|(key, _)| key == crate::mesh_churn::NEGATIVE_CONTROL)
                .map(|(_, value)| value.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(named(&instance_env(&world, false)), Vec::<String>::new());
        assert_eq!(named(&instance_env(&world, true)), [PLAIN_PUBLICATION]);
        assert_eq!(named(&zsh_env(&world)), Vec::<String>::new());
        assert!(
            instance_env(&world, true)
                .iter()
                .any(|(key, value)| key == "SHELL" && value == "/bin/zsh")
        );
    }

    /// The tree's three checks are judged under the names a host reads them by.
    #[test]
    fn the_trees_checks_are_named_as_judged() {
        let names: Vec<String> = judge_tab(&passing())
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(names[names.len() - 3..], TREE_CHECKS.map(str::to_owned));
    }

    #[test]
    fn each_verbs_refusal_reads_as_one() {
        for first in [
            "aterm-ctl: ERR refused private view: the front tab shows a private document",
            "aterm-ctl: ERR video: refused private view: the front tab shows a private document",
            "aterm-ctl: ERR pointer command failed: refused private view: the front tab shows a \
             private document",
        ] {
            assert!(refused_view(first), "{first}");
        }
        for first in [
            "OK at=-",
            "aterm-ctl: ERR pointer command failed: no window",
            "aterm-ctl: ERR refused private doc",
            "refused private view",
        ] {
            assert!(!refused_view(first), "{first}");
        }
    }

    #[test]
    fn the_mailbox_file_names_each_synthetic_login_once() {
        let file = mailbox_file(3);
        assert_eq!(file.matches("[[mailboxes]]").count(), 3);
        assert_eq!(file.matches(NEEDLE).count(), 3);
        assert!(
            file.contains("login = \"box03@proton.example\"\npassword = \"synthetic-pass-03\"")
        );
    }

    #[test]
    fn the_rc_accepts_its_line_once() {
        let rc = zshrc("/b/aterm db emails --show-passwords");
        assert!(rc.contains("print -z -- '/b/aterm db emails --show-passwords'"));
        assert!(rc.contains("zle -N zle-line-init __a17_accept"));
        assert!(rc.contains("__a17_go=0; zle accept-line"));
    }
}
