// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab reveal-tab`: one-mesh design §11 A17 end to end, on a private windowed instance.
//!
//! The world's folder holds a private grid: a TSV of synthetic logins and their passwords
//! (`synthetic-pass-NN`) in a folder whose name marks it private
//! (`aterm_types::private_edit::DIR_SUFFIX`), written by this run. In the instance's boot tab a
//! zsh whose rc accepts `aterm edit --wait --secret=password +1 <file>` once, nobody typing over
//! the control socket, opens it in an Editor tab beside it, its `password` column concealed. Then
//! `text`, `screen`, `image`, `window`, `video`, `key`, `pointer`, `invoke` and `doc read` of the
//! Editor tab refuse; the tab's own text, blocks and session log hold no value; and closing the
//! Editor tab ends the command. The file the Editor shows holds the values: the positive control.
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
//! world's environment, and the world goes when the run ends. The report names checks only,
//! never a value.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_json::{Value, json};

use crate::instance::{
    CONFIG_OFF, Instance, Launch, Reply, World, resolve_aterm, take_flag, take_value,
    terminal_request,
};
use crate::{Fail, Res};

/// What every synthetic password the grid holds starts with (`synthetic-pass-NN`).
const NEEDLE: &str = "synthetic-pass-";

/// Logins in the grid.
const ROWS: usize = 3;

/// The grid's private folder in the world's folder: its name ends in
/// `aterm_types::private_edit::DIR_SUFFIX`, which makes the document the Editor opens from it a
/// private one.
const GRID_DIR: &str = "a17.private.noindex";

/// The grid's file in [`GRID_DIR`].
const GRID_FILE: &str = "logins.tsv";

/// The grid's concealed column, by header, as `aterm edit --secret=` names it.
const SECRET_COLUMN: &str = "password";

/// One-minute hang detector for each wait: the Editor tab, its close, the tree.
const HANG: Duration = Duration::from_secs(60);

/// What a read of the private front answers (`PRIVATE_VIEW_REFUSED`), in part.
const REFUSED_VIEW: &str = "refused private view";

/// What the run saw, as [`judge`] reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Evidence {
    /// The Editor tab opened on a private document, and the file it shows holds the values.
    pub editor_opened: bool,
    pub file_holds_values: bool,
    /// The first line of each refused read or drive, by verb.
    pub refusals: Vec<(String, String)>,
    /// `doc read` of the private document.
    pub doc_read: String,
    /// The caller's tab: its text and blocks hold the command and no value, and its log a run and
    /// no value (so no clean check reads nothing).
    pub tab_holds_command: bool,
    pub tab_text_clean: bool,
    pub blocks_hold_command: bool,
    pub blocks_clean: bool,
    pub log_clean: bool,
    /// Closing the Editor tab ended the command.
    pub closed_ends_command: bool,
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
fn zsh_env(world: &World) -> Vec<(String, String)> {
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
fn zshrc(line: &str) -> String {
    format!(
        "typeset -gi __a17_go=1\n\
         __a17_accept() {{ if (( __a17_go )); then __a17_go=0; zle accept-line; fi }}\n\
         zle -N zle-line-init __a17_accept\n\
         print -z -- '{line}'\n"
    )
}

/// The command line the boot tab accepts: `aterm` opening `grid` in an Editor tab and waiting
/// for it to close, its [`SECRET_COLUMN`] concealed. `None` where a path holds a quote or white
/// space, which the rc's one quoted word cannot carry.
fn edit_line(aterm: &Path, grid: &Path) -> Option<String> {
    let (aterm, grid) = (aterm.to_str()?, grid.to_str()?);
    let plain =
        |path: &str| !path.contains(['\'', '"', '\\']) && !path.contains(char::is_whitespace);
    (plain(aterm) && plain(grid))
        .then(|| format!("{aterm} edit --wait --secret={SECRET_COLUMN} +1 {grid}"))
}

/// The grid's text: a header row, then `n` synthetic logins, `box<i>@lab.example` with password
/// `synthetic-pass-<i>` ([`NEEDLE`]), tab-separated.
fn grid_file(n: usize) -> String {
    let mut text = format!("login\t{SECRET_COLUMN}\n");
    for i in 1..=n {
        text.push_str(&format!("{}\t{NEEDLE}{i:02}\n", login(i)));
    }
    text
}

/// The `i`th synthetic login.
fn login(i: usize) -> String {
    format!("box{i:02}@lab.example")
}

/// Whether `text` holds any of `needles`.
fn holds(text: &str, needles: &[String]) -> bool {
    needles.iter().any(|needle| text.contains(needle.as_str()))
}

/// `doc ls`'s private document: its id and URI, else `None`.
fn private_doc(inst: &Instance) -> Option<(String, String)> {
    let listed = inst.ctl(&["doc", "ls"]);
    listed
        .stdout
        .lines()
        .find(|line| line.contains(aterm_types::private_edit::DIR_SUFFIX))
        .and_then(|line| {
            let field = |key: &str| {
                line.split_whitespace()
                    .find_map(|word| word.strip_prefix(key))
                    .map(str::to_owned)
            };
            Some((field("doc=")?, field("uri=")?))
        })
}

/// Whether the document at `uri` is `grid`: the URI ends in its folder's and its own name.
fn shows(uri: &str, grid: &Path) -> bool {
    let named = |path: &Path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
    };
    match (grid.parent().and_then(named), named(grid)) {
        (Some(dir), Some(file)) => uri.ends_with(&format!("/{dir}/{file}")),
        _ => false,
    }
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

/// The run: the world and its private grid, then the instance.
///
/// # Errors
/// A step that has to work for the checks to mean anything failed.
pub(crate) fn run(aterm: &Path, ask: &Ask) -> Result<Evidence, Fail> {
    let mut ev = Evidence::default();
    let world = World::new(CONFIG_OFF).map_err(fail)?;
    if aterm_types::dirs::home_dir().is_some_and(|live| live == world.home) {
        return Err(fail("the world's home is this user's"));
    }
    let dir = world.dir.join(GRID_DIR);
    aterm_uds::private_file::create_private_dir(&dir).map_err(fail)?;
    aterm_uds::private_file::write_private(&dir, GRID_FILE, grid_file(ROWS).as_bytes())
        .map_err(fail)?;
    let grid = dir.join(GRID_FILE);
    let line = edit_line(aterm, &grid)
        .ok_or_else(|| fail("the aterm or grid path holds a quote or white space"))?;
    crate::util::write_fresh(&world.home.join(".zshrc"), zshrc(&line)).map_err(fail)?;
    let mut inst = Instance::start(
        &world,
        Launch {
            aterm: aterm.to_path_buf(),
            headless: false,
            args: Vec::new(),
            env: instance_env(&world, ask.plain),
            cwd: world.home.clone(),
            sandbox: None,
            reroute: false,
        },
    )
    .map_err(fail)?;
    let logins: Vec<String> = (1..=ROWS).map(login).collect();
    let observed = observe(
        &mut inst,
        &grid,
        &ask.reader,
        &[NEEDLE.to_owned()],
        &logins,
        &mut ev,
    );
    crate::instance::after_stop(observed, inst.stop())?;
    Ok(ev)
}

/// Everything read of the running instance, whose boot tab opened `grid`: `needles` are the
/// values it holds and `shown` the cells not secret that the grid's first screen shows, which
/// its accessibility tree must still carry; the tree read by `reader`.
///
/// # Errors
/// The Editor tab did not open, could not be closed, or the tree's reader failed.
fn observe(
    inst: &mut Instance,
    grid: &Path,
    reader: &TreeReader,
    needles: &[String],
    shown: &[String],
    ev: &mut Evidence,
) -> Result<(), Fail> {
    inst.await_socket(HANG)?;
    let (doc, uri) = within("the Editor tab", || private_doc(inst))?;
    ev.editor_opened = true;
    ev.file_holds_values = shows(&uri, grid)
        && crate::util::read_regular_text(grid).is_ok_and(|text| holds(&text, needles));
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

    let command = format!("--secret={SECRET_COLUMN}");
    let tab = said(&terminal_request(|args| inst.ctl(args), &["text"]));
    ev.tab_holds_command = tab.contains(&command);
    ev.tab_text_clean = !holds(&tab, needles);
    let blocks = said(&terminal_request(|args| inst.ctl(args), &["blocks"]));
    ev.blocks_hold_command = blocks.contains(&command);
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

/// Each check of the Editor tab and the caller's tab ([`observe`]'s), and whether it held.
#[must_use]
pub fn judge(ev: &Evidence) -> Vec<(String, bool)> {
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
            "the tab's text holds the command".to_owned(),
            ev.tab_holds_command,
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

/// The names of the three checks of the window's accessibility tree, as [`judge`] gives them:
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
        "editor_opened": ev.editor_opened,
        "file_holds_values": ev.file_holds_values,
        "refusals": refusals,
        "doc_read": scrub(&ev.doc_read),
        "tab_holds_command": ev.tab_holds_command,
        "tab_text_clean": ev.tab_text_clean,
        "blocks_hold_command": ev.blocks_hold_command,
        "blocks_clean": ev.blocks_clean,
        "log_clean": ev.log_clean,
        "closed_ends_command": ev.closed_ends_command,
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
        editor_opened: flag("editor_opened")?,
        file_holds_values: flag("file_holds_values")?,
        refusals,
        doc_read: text("doc_read")?,
        tab_holds_command: flag("tab_holds_command")?,
        tab_text_clean: flag("tab_text_clean")?,
        blocks_hold_command: flag("blocks_hold_command")?,
        blocks_clean: flag("blocks_clean")?,
        log_clean: flag("log_clean")?,
        closed_ends_command: flag("closed_ends_command")?,
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
        let mut text = String::from(
            "reveal-tab: a private windowed instance, a private grid of synthetic logins\n",
        );
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
        }
        text.push_str(if pass { "PASS\n" } else { "FAIL\n" });
        text
    };
    print!("{text}");
    Ok(i32::from(!pass))
}

#[cfg(test)]
#[path = "reveal_tab_tests.rs"]
mod tests;
