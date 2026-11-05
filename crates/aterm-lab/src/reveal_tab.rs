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

/// Whether any file directly in `folder` holds `needle`.
fn folder_holds(folder: &Path, needle: &str) -> bool {
    std::fs::read_dir(folder).is_ok_and(|listing| {
        listing.flatten().any(|entry| {
            std::fs::read(entry.path()).is_ok_and(|bytes| {
                bytes
                    .windows(needle.len())
                    .any(|window| window == needle.as_bytes())
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
    text.lines()
        .map(|line| {
            if line.contains(NEEDLE) {
                "[a line with a value, withheld]".to_owned()
            } else {
                line.chars().take(160).collect()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
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
    let printed = std::fs::read_to_string(&typescript).unwrap_or_default();
    let logged = std::fs::read_to_string(&log).unwrap_or_default();
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
    let observed = observe(&mut inst, &world, &mut ev);
    crate::instance::after_stop(observed, inst.stop())?;
    drop(lab);
    Ok(ev)
}

/// Everything read of the running instance.
fn observe(inst: &mut Instance, world: &World, ev: &mut Evidence) -> Result<(), Fail> {
    inst.await_socket(HANG)?;
    let doc = within("the Editor tab", || private_doc(inst))?;
    ev.editor_opened = true;
    let folders = scratch_folders(world);
    ev.file_holds_values = folders.iter().any(|folder| folder_holds(folder, NEEDLE));

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
    ev.tab_text_clean = !tab.contains(NEEDLE);
    let blocks = said(&terminal_request(|args| inst.ctl(args), &["blocks"]));
    ev.blocks_hold_command = blocks.contains("--show-passwords");
    ev.blocks_clean = !blocks.contains(NEEDLE);
    // The log keeps a command's program, never its output or arguments: a run is there, and no
    // value, the in-memory content tier included.
    let log = said(&terminal_request(
        |args| inst.ctl(args),
        &["log", "tier=content"],
    ));
    ev.log_clean = log.contains("open r") && !log.contains(NEEDLE);

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
    ev.after_close_clean = after.iter().all(|read| !read.contains(NEEDLE));
    Ok(())
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
    checks
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
        for ev in [served, leaked, read] {
            assert_eq!(judge(&ev).iter().filter(|(_, held)| !*held).count(), 1);
        }
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
