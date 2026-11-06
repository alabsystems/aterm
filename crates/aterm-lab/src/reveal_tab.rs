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
//! this process holds no Accessibility grant, those checks are a named skip, never a pass.
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
    CONFIG_OFF, Instance, Launch, Reply, World, resolve_aterm, take_flag, terminal_request,
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
fn scrub(text: &str) -> String {
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

/// The run: the world and its registry, the outside print, then the instance.
///
/// # Errors
/// A step that has to work for the checks to mean anything failed.
pub fn run(aterm: &Path) -> Result<Evidence, Fail> {
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
            args: Vec::new(),
            env,
            cwd: world.home.clone(),
            sandbox: None,
            reroute: false,
        },
    )
    .map_err(fail)?;
    let logins: Vec<String> = (1..=ROWS)
        .map(|i| format!("box{i:02}@proton.example"))
        .collect();
    let observed = observe(&mut inst, &world, &[NEEDLE.to_owned()], &logins, &mut ev);
    crate::instance::after_stop(observed, inst.stop())?;
    drop(lab);
    Ok(ev)
}

/// Everything read of the running instance, `needles` being the values its command reveals and
/// `shown` the cells not secret that the grid's first screen shows, which its accessibility tree
/// must still carry.
///
/// # Errors
/// The Editor tab did not open, or could not be closed.
pub(crate) fn observe(
    inst: &mut Instance,
    world: &World,
    needles: &[String],
    shown: &[String],
    ev: &mut Evidence,
) -> Result<(), Fail> {
    inst.await_socket(HANG)?;
    let doc = within("the Editor tab", || private_doc(inst))?;
    ev.editor_opened = true;
    let folders = scratch_folders(world);
    ev.file_holds_values = folders.iter().any(|folder| folder_holds(folder, needles));
    ev.ax = ax_tree(inst, needles, shown)?;

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

/// The Editor tab's accessibility tree as a process with Accessibility reads it ([`AxEvidence`]),
/// read until the grid's nodes are there (an adapter publishes once a reader attaches) or the
/// hang detector ends the wait; `None` where this process holds no Accessibility grant.
/// `needles` are the revealed values it must not carry, `shown` the cells it must.
fn ax_tree(
    inst: &Instance,
    needles: &[String],
    shown: &[String],
) -> Result<Option<AxEvidence>, Fail> {
    if !ax::trusted() {
        return Ok(None);
    }
    let pid = inst
        .aterm_pid()
        .ok_or_else(|| fail("the instance's aterm has no pid"))?;
    let end = Instant::now() + HANG;
    loop {
        let texts = ax::texts(pid);
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

/// Every string a running app's windows carry, read through the Accessibility API as any
/// process the person granted Accessibility reads them.
#[expect(
    unsafe_code,
    reason = "the Accessibility API: reading the tree another process can read"
)]
mod ax {
    use std::ffi::c_void;

    type CfRef = *const c_void;

    #[repr(C)]
    struct CfRange {
        location: isize,
        length: isize,
    }

    /// How deep a window's tree is read.
    const DEPTH: usize = 32;
    /// The most elements read in a pass.
    const MOST: usize = 50_000;
    /// The most characters read of one text element.
    const CHARACTERS: i64 = 1 << 16;
    /// `kAXValueCFRangeType`.
    const RANGE: u32 = 4;
    /// `kCFNumberSInt64Type`.
    const SINT64: isize = 4;
    const UTF8: u32 = 0x0800_0100;
    /// Each element's string attributes read.
    const STRINGS: [&str; 7] = [
        "AXTitle",
        "AXDescription",
        "AXValue",
        "AXHelp",
        "AXSelectedText",
        "AXPlaceholderValue",
        "AXRoleDescription",
    ];

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: CfRef);
        fn CFGetTypeID(cf: CfRef) -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFArrayGetTypeID() -> usize;
        fn CFNumberGetTypeID() -> usize;
        fn CFArrayGetCount(array: CfRef) -> isize;
        fn CFArrayGetValueAtIndex(array: CfRef, index: isize) -> CfRef;
        fn CFStringGetLength(string: CfRef) -> isize;
        fn CFStringGetCharacters(string: CfRef, range: CfRange, buffer: *mut u16);
        fn CFNumberGetValue(number: CfRef, kind: isize, out: *mut c_void) -> u8;
        fn CFStringCreateWithBytes(
            alloc: CfRef,
            bytes: *const u8,
            len: isize,
            encoding: u32,
            external: u8,
        ) -> CfRef;
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn AXUIElementCreateApplication(pid: i32) -> CfRef;
        fn AXUIElementCopyAttributeValue(el: CfRef, attr: CfRef, out: *mut CfRef) -> i32;
        fn AXUIElementCopyParameterizedAttributeValue(
            el: CfRef,
            attr: CfRef,
            param: CfRef,
            out: *mut CfRef,
        ) -> i32;
        fn AXUIElementSetMessagingTimeout(el: CfRef, secs: f32) -> i32;
        fn AXValueCreate(kind: u32, value: *const c_void) -> CfRef;
    }

    /// A CF object this code owns one reference to (null: none).
    struct Owned(CfRef);

    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: non-null, and this value owns one reference to it.
                unsafe { CFRelease(self.0) }
            }
        }
    }

    /// Whether this process (its responsible app) holds Accessibility, with no prompt.
    pub(super) fn trusted() -> bool {
        // SAFETY: a plain query.
        unsafe { AXIsProcessTrusted() }
    }

    fn cf_string(text: &str) -> Owned {
        // SAFETY: the bytes and length of a live UTF-8 string; the result is owned.
        Owned(unsafe {
            CFStringCreateWithBytes(
                std::ptr::null(),
                text.as_ptr(),
                isize::try_from(text.len()).unwrap_or(0),
                UTF8,
                0,
            )
        })
    }

    fn text(cf: CfRef) -> Option<String> {
        if cf.is_null() {
            return None;
        }
        // SAFETY: `CFGetTypeID` reads any live CF object; a non-string is refused before it is
        // read as one, and the buffer holds the string's whole length.
        unsafe {
            if CFGetTypeID(cf) != CFStringGetTypeID() {
                return None;
            }
            let length = CFStringGetLength(cf);
            let mut units = vec![0u16; usize::try_from(length).ok()?];
            CFStringGetCharacters(
                cf,
                CfRange {
                    location: 0,
                    length,
                },
                units.as_mut_ptr(),
            );
            Some(String::from_utf16_lossy(&units))
        }
    }

    fn number(cf: CfRef) -> Option<i64> {
        if cf.is_null() {
            return None;
        }
        let mut out = 0i64;
        // SAFETY: a live CF object, read as a number only when it is one, into an i64.
        unsafe {
            (CFGetTypeID(cf) == CFNumberGetTypeID()
                && CFNumberGetValue(cf, SINT64, (&raw mut out).cast()) != 0)
                .then_some(out)
        }
    }

    fn items(array: CfRef) -> Vec<CfRef> {
        if array.is_null() {
            return Vec::new();
        }
        // SAFETY: a live CF object, read as an array only when it is one; its items are
        // borrowed while the array lives.
        unsafe {
            if CFGetTypeID(array) != CFArrayGetTypeID() {
                return Vec::new();
            }
            (0..CFArrayGetCount(array))
                .map(|index| CFArrayGetValueAtIndex(array, index))
                .collect()
        }
    }

    fn attr(el: CfRef, name: &str) -> Owned {
        let key = cf_string(name);
        let mut out: CfRef = std::ptr::null();
        // SAFETY: an AX element, an attribute name and an out pointer; the value is owned.
        let err = unsafe { AXUIElementCopyAttributeValue(el, key.0, &raw mut out) };
        Owned(if err == 0 { out } else { std::ptr::null() })
    }

    /// A text element's first [`CHARACTERS`] characters (`AXStringForRange`), as a reader asks
    /// for the text it does not hold as a value.
    fn characters(el: CfRef) -> Option<String> {
        let count = number(attr(el, "AXNumberOfCharacters").0)?.min(CHARACTERS);
        if count <= 0 {
            return None;
        }
        let range = CfRange {
            location: 0,
            length: isize::try_from(count).ok()?,
        };
        // SAFETY: a CFRange of the kind named, copied into an owned AXValue.
        let param = Owned(unsafe { AXValueCreate(RANGE, (&raw const range).cast()) });
        if param.0.is_null() {
            return None;
        }
        let key = cf_string("AXStringForRange");
        let mut out: CfRef = std::ptr::null();
        // SAFETY: an AX element, an attribute name, its parameter and an out pointer; the value
        // is owned.
        let err =
            unsafe { AXUIElementCopyParameterizedAttributeValue(el, key.0, param.0, &raw mut out) };
        let out = Owned(if err == 0 { out } else { std::ptr::null() });
        text(out.0)
    }

    /// Every string `pid`'s windows' elements carry, depth first.
    pub(super) fn texts(pid: i32) -> Vec<String> {
        // SAFETY: an app element for a pid, owned.
        let app = Owned(unsafe { AXUIElementCreateApplication(pid) });
        if app.0.is_null() {
            return Vec::new();
        }
        // SAFETY: the element this code created; a busy app answers within the timeout or not.
        unsafe { AXUIElementSetMessagingTimeout(app.0, 2.0) };
        let windows = attr(app.0, "AXWindows");
        let mut out = Vec::new();
        let mut read = 0usize;
        for window in items(windows.0) {
            walk(window, 0, &mut read, &mut out);
        }
        out
    }

    fn walk(el: CfRef, depth: usize, read: &mut usize, out: &mut Vec<String>) {
        if depth > DEPTH || *read >= MOST {
            return;
        }
        *read += 1;
        for name in STRINGS {
            out.extend(text(attr(el, name).0).filter(|text| !text.is_empty()));
        }
        out.extend(characters(el));
        let children = attr(el, "AXChildren");
        for child in items(children.0) {
            walk(child, depth + 1, read, out);
        }
    }
}

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
        checks.extend([
            (
                "the window's accessibility tree carries the grid".to_owned(),
                ax.grid_read,
            ),
            ("and every login".to_owned(), ax.logins_read),
            ("and no value".to_owned(), ax.clean),
        ]);
    }
    checks
}

/// The checks this run could not make, and why.
pub(crate) fn skipped(ev: &Evidence) -> Vec<&'static str> {
    if ev.ax.is_none() {
        vec![
            "the window's accessibility tree: this process holds no Accessibility grant \
             (System Settings > Privacy & Security > Accessibility, for the app it runs under)",
        ]
    } else {
        Vec::new()
    }
}

/// `lab reveal-tab [--json] [<aterm>]`.
///
/// # Errors
/// Usage, or a step the checks rest on failed.
pub fn cmd(args: &[String]) -> Res {
    let mut args = args.to_vec();
    let as_json = take_flag(&mut args, "--json");
    if args.len() > 1 {
        return Err(Fail::usage("reveal-tab takes --json and an aterm at most"));
    }
    let aterm = resolve_aterm(args.pop()).map_err(|e| Fail::exit(3, e))?;
    let ev = run(&aterm)?;
    let checks = judge(&ev);
    let pass = checks.iter().all(|(_, held)| *held);
    let report: Value = json!({
        "lane": "reveal-tab",
        "pass": pass,
        "checks": checks.iter().map(|(name, held)| json!({"check": name, "pass": held})).collect::<Vec<_>>(),
        "skipped": skipped(&ev).into_iter().map(|why| json!(why)).collect::<Vec<_>>(),
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
