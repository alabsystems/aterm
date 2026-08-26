// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pending-program stubs (R6): from the moment the lean app first launches (and
//! immediately after adoption), EVERY default-set program name resolves on `PATH`,
//! and running one is always helpful — never "command not found".
//!
//! A stub is a tiny `/bin/sh` file at `<prefix>/bin/<tool>` (the same seam
//! [`crate::flow`]'s tombstone shims use — no parallel machinery) that execs
//! `atpkg __pending <tool>`: a short honest message with the LIVE install state, plus
//! a bump of that program to the front of the install queue. It grants nothing —
//! the authoritative roster stays the signed index — and it is replaced by the real
//! shim the moment the program installs, atomically: `platform::install_shim`
//! already lands via temp+`rename(2)` (see `activate.rs`), so the name resolves to
//! SOMETHING at every instant — stub before, real shim after, no window of absence,
//! no `EEXIST`. `prune_stale_shims` skips it (a stub resolves to no store target)
//! and `active_builds` never counts it.
//!
//! # The fallback chain (a stub must never dangle)
//!
//! An embedded absolute atpkg path breaks on app relocation or self-update, and a
//! stub that prints sh's own "not found" would violate the guarantee. So the script
//! falls back, in order:
//!
//! 1. the embedded co-located `atpkg` path, if it exists and is executable;
//! 2. `command -v atpkg` (the `~/.local/bin` alias / shell hook);
//! 3. a static honest message — and exit 127.
//!
//! The per-launch seed-pass reconcile REWRITES stubs, refreshing embedded paths as
//! a matter of course.
//!
//! # Trust posture
//!
//! Stub names pass [`crate::store::shim_allowed`] by construction (they are only
//! ever written through [`crate::store::ToolName`]) — the sensitive-name refusal
//! applies to stubs identically. A stub is recognized by its marker line and
//! removed only when it still carries it: a real shim, a tombstone, a dev link or
//! any hand-made file is never touched.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;

use crate::store::{Layout, ToolName};

/// The compile-time default-set roster the stubs cover BEFORE the first signed index
/// resolves: `(name, one authored what-it-is line)`. Names only — no versions, no
/// URLs — so staleness is harmless: the index-resolve reconcile adds stubs for newly
/// published names and removes ones the signed index no longer lists.
pub const DEFAULT_SET_STUB_NAMES: &[(&str, &str)] = &[
    ("ay", "ALab's SMT solver"),
    ("clean", "ALab's theorem prover"),
    ("nn", "ALab's neural-network tool"),
    ("ny", "ALab's neural-network verifier"),
    (
        "trust",
        "the Trust compiler bundle — a Rust compiler that verifies what it compiles",
    ),
    (
        "trust-cg",
        "the Trust compiler's codegen member (coherence group)",
    ),
    (
        "trust-ir",
        "the Trust compiler's IR member (coherence group)",
    ),
    ("trust-mc", "the Trust model checker"),
    (
        "trust-vc",
        "the Trust compiler's verification-condition member (coherence group)",
    ),
    ("ty", "ALab's specification checker"),
];

/// The authored one-line description for `name`, if the compiled roster carries one.
/// A program published after this binary gets the honest generic line instead.
#[must_use]
pub fn describe(name: &str) -> Option<&'static str> {
    DEFAULT_SET_STUB_NAMES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, d)| *d)
}

/// The marker line that identifies a pending stub — the recognition gate for every
/// rewrite/removal below. Version-suffixed so a future stub format can coexist.
const STUB_MARKER: &str = "# atpkg pending-program stub v1";

/// The static last-resort message (fallback 3) — the one line the stub can always
/// say without an atpkg to ask.
pub const STUB_UNREACHABLE_MSG: &str =
    "aterm's package manager is not reachable — open aterm to finish installing";

/// Single-quote `s` for safe embedding in a `/bin/sh` script (the POSIX `'\''`
/// escape). [`crate::store::shim_allowed`] does not forbid shell metacharacters, so
/// the stub body must never let a crafted name break out of its quotes. (A local
/// twin of the platform backend's private helper — same rule, one screen away from
/// its use.)
fn sh_single_quote(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// The stub script body for `tool`, embedding `atpkg` as the co-located fallback-1
/// path. Pure, so the shape is pinned by tests.
///
/// NOTE the `exec "$ATPKG"` spelling: `platform::parse_sh_shim_target` recognizes
/// real shims by a trimmed line starting `exec '`, so the stub deliberately execs
/// through a variable — a stub must resolve as NO store target (`resolve_shim` ⇒
/// `None`), or `active_builds`/`which`/the front door would mistake it for an
/// installed tool.
#[must_use]
fn stub_content(tool: &ToolName, atpkg: &Path) -> String {
    if cfg!(windows) {
        stub_content_cmd(tool, atpkg)
    } else {
        stub_content_sh(tool, atpkg)
    }
}

/// The POSIX `/bin/sh` stub body (macOS/Linux — the shim there is a plain
/// executable file).
#[must_use]
fn stub_content_sh(tool: &ToolName, atpkg: &Path) -> String {
    let name = sh_single_quote(tool.as_str());
    let mut s = String::from("#!/bin/sh\n");
    s.push_str(STUB_MARKER);
    s.push_str("\n# Replaced by the real shim when the program installs.\nATPKG=");
    s.push_str(&sh_single_quote(&atpkg.to_string_lossy()));
    s.push_str("\nif [ -x \"$ATPKG\" ]; then\n  exec \"$ATPKG\" __pending ");
    s.push_str(&name);
    s.push_str("\nfi\nif command -v atpkg >/dev/null 2>&1; then\n  exec atpkg __pending ");
    s.push_str(&name);
    s.push_str("\nfi\nprintf '%s\\n' ");
    s.push_str(&sh_single_quote(STUB_UNREACHABLE_MSG));
    s.push_str(" 1>&2\nexit 127\n");
    s
}

/// The batch twin: on Windows the shim slot is `bin/<tool>.cmd`
/// ([`crate::store::ToolName::shim_file`]), and this module used to lay a
/// `#!/bin/sh` body into it — cmd.exe then read POSIX shell as batch and the
/// "pending" promise rendered as `'#!' is not recognized…` garbage. Same
/// three fallbacks as the sh body, batch-spelled; the marker rides a `rem`
/// line ([`is_pending_stub`] accepts both spellings) so recognition, rewrite
/// and removal keep working on the file cmd.exe can actually run. `exit /b
/// 127` throughout — the stub contract is "the tool did not run" regardless
/// of which fallback answered. Tool names are `ToolName`-vetted and the atpkg
/// path rides plain double quotes, the exact conventions of the real `.cmd`
/// shim writer (`platform::cmd_shim_content`).
#[must_use]
fn stub_content_cmd(tool: &ToolName, atpkg: &Path) -> String {
    let name = tool.as_str();
    let atpkg = atpkg.to_string_lossy();
    let mut s = String::from("@echo off\r\nrem ");
    s.push_str(STUB_MARKER);
    s.push_str("\r\nrem Replaced by the real shim when the program installs.\r\n");
    s.push_str(&format!(
        "if exist \"{atpkg}\" (\r\n  \"{atpkg}\" __pending \"{name}\"\r\n  exit /b 127\r\n)\r\n"
    ));
    s.push_str(&format!(
        "where atpkg >nul 2>nul\r\nif not errorlevel 1 (\r\n  atpkg __pending \"{name}\"\r\n  exit /b 127\r\n)\r\n"
    ));
    s.push_str(&format!(
        "echo {STUB_UNREACHABLE_MSG} 1>&2\r\nexit /b 127\r\n"
    ));
    s
}

/// Whether `name` can ride a batch script without becoming syntax:
/// [`crate::store::shim_allowed`] admits quotes, `%`, carets and ampersands —
/// the sh body neutralizes those with single-quoting, but cmd.exe has no
/// robust equivalent (`%VAR%` expands even inside double quotes). A name that
/// cannot be embedded inertly gets NO stub on Windows (fail closed: a missing
/// courtesy stub costs one "command not found"; an injectable script costs
/// arbitrary execution under the user's account). Real roster names are
/// `[a-z0-9-]` and all pass.
#[must_use]
fn cmd_stub_name_safe(name: &str) -> bool {
    !name.chars().any(|c| {
        matches!(
            c,
            '"' | '%' | '^' | '&' | '<' | '>' | '|' | '!' | '\r' | '\n'
        )
    })
}

/// The co-located `atpkg` alias beside the running executable — fallback 1's
/// embedded path. Canonicalized so an argv0 alias (`atpkg` → `aterm`) or a
/// `~/.local/bin` symlink resolves to the real bundle before the sibling join.
fn embedded_atpkg_path() -> std::path::PathBuf {
    // `EXE_SUFFIX` (".exe" on Windows, "" elsewhere): a bare `atpkg` join
    // embedded a path that exists on no Windows install — the same probe bug
    // the GUI's co-located resolver fixed — so fallback 1 always missed there
    // and every stub run leaned on PATH luck.
    let atpkg = format!("atpkg{}", std::env::consts::EXE_SUFFIX);
    std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join(&atpkg)))
        .unwrap_or_else(|| std::path::PathBuf::from(atpkg))
}

/// Whether the file at `path` is a pending stub THIS module wrote — the recognition
/// gate for rewrite and removal. Bounded, symlink-refusing read; anything else
/// (absent, a real shim symlink, a tombstone, a hand-made file) answers `false`.
#[must_use]
pub fn is_pending_stub(path: &Path) -> bool {
    // A real Unix-era symlink shim is not even a regular file; the bounded reader
    // refuses it before content is considered. Both marker spellings are
    // recognized on every platform — the bare sh line and the batch `rem`
    // line — so a store migrated across platforms (or a stub laid by the old
    // sh-everywhere writer on Windows) still reconciles instead of squatting.
    crate::metadata_io::read_bounded_regular_utf8(path, 64 * 1024).is_ok_and(|text| {
        text.lines().any(|l| {
            let l = l.trim();
            l == STUB_MARKER
                || l.strip_prefix("rem ")
                    .is_some_and(|rest| rest.trim() == STUB_MARKER)
        })
    })
}

/// Whether `tool` currently resolves to a pending stub in this layout — the front
/// door's second arm (`store_resolves || pending_stub_exists`).
#[must_use]
pub fn pending_stub_exists(layout: &Layout, tool: &str) -> bool {
    ToolName::new(tool).is_some_and(|t| is_pending_stub(&layout.shim(&t)))
}

/// Lay (or refresh) the pending stub for `tool`, atomically (temp `0755` +
/// `rename(2)`, the tombstone writer's shape). NEVER over anything that is not
/// already a pending stub: a resolvable shim, a tombstone, or any unrecognized file
/// wins and the write is a clean no-op — the stub is the lowest-precedence occupant
/// of the name.
pub fn write_pending_stub(layout: &Layout, tool: &ToolName) -> io::Result<()> {
    if cfg!(windows) && !cmd_stub_name_safe(tool.as_str()) {
        // See `cmd_stub_name_safe`: no inert embedding exists, so no stub.
        return Ok(());
    }
    let shim = layout.shim(tool);
    match std::fs::symlink_metadata(&shim) {
        Err(_) => {}                          // absent: ours to claim
        Ok(_) if is_pending_stub(&shim) => {} // ours: rewrite refreshes the embedded path
        Ok(_) => return Ok(()), // someone else's file (shim/tombstone/hand-made): never touch
    }
    let bin = layout.bin_dir();
    layout.ensure_dir(&bin)?;
    let body = stub_content(tool, &embedded_atpkg_path());
    write_executable_atomic(&shim, &body)
}

/// Temp+rename an executable script onto `dest` (the tombstone writer's discipline,
/// restated here because that helper hard-codes its own body).
fn write_executable_atomic(dest: &Path, body: &str) -> io::Result<()> {
    let file_name = dest
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "stub has no file name"))?;
    let mut tmp_name = String::from(".");
    tmp_name.push_str(file_name);
    tmp_name.push_str(".stub-");
    tmp_name.push_str(&crate::dec_u64(u64::from(std::process::id())));
    let tmp = dest.with_file_name(tmp_name);
    let _ = std::fs::remove_file(&tmp);
    crate::call2(std::fs::write, tmp.as_path(), body.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    if let Err(e) = std::fs::rename(&tmp, dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Remove `program`'s stub iff the name still resolves to a pending stub — the
/// per-program removal discipline (`atpkg uninstall <p>`, an index de-listing).
pub fn remove_stub(layout: &Layout, program: &str) {
    if let Some(tool) = ToolName::new(program) {
        let shim = layout.shim(&tool);
        if is_pending_stub(&shim) {
            let _ = std::fs::remove_file(&shim);
        }
    }
}

/// Remove EVERY pending stub in `bin/` — `uninstall --all` / a recorded decline.
/// Recognition-gated per file, so nothing that is not a stub can be swept.
pub fn remove_all_stubs(layout: &Layout) {
    let Ok(entries) = std::fs::read_dir(layout.bin_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if is_pending_stub(&path) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Lay the compile-time roster's stubs at ADOPTION time — before a single network
/// byte moves, so `PATH` coverage exists from the first instant the machine wants
/// the toolset. Skips programs already installed and ones the user removed on
/// purpose; failures are per-name and best-effort (a stub is a courtesy, never a
/// gate on the pass).
pub fn lay_adoption_stubs(layout: &Layout) {
    let installed = crate::ops::active_builds(layout);
    let removed = layout.removed_programs();
    for (name, _) in DEFAULT_SET_STUB_NAMES {
        if installed.contains_key(*name) || removed.contains(*name) {
            continue;
        }
        if let Some(tool) = ToolName::new(name) {
            let _ = write_pending_stub(layout, &tool);
        }
    }
}

/// The index-resolve reconcile: `wanted` is the SIGNED set the pass will keep
/// complete (installable ∧ not-removed), `installed` the active builds. Adds/
/// refreshes a stub for every wanted-but-absent name (embedded paths refreshed as a
/// matter of course), then sweeps every pending stub whose name is no longer
/// wanted-and-missing — de-listed, removed on purpose, or now installed under a
/// name its real shims do not expose.
pub fn reconcile(layout: &Layout, wanted: &BTreeSet<String>, installed: &BTreeMap<String, u64>) {
    let mut keep: BTreeSet<&str> = BTreeSet::new();
    for name in wanted {
        if installed.contains_key(name.as_str()) {
            continue;
        }
        // The ToolName gate IS the sensitive-name refusal: an index listing `sudo`
        // gets no stub, exactly as it gets no shim.
        let Some(tool) = ToolName::new(name) else {
            continue;
        };
        if write_pending_stub(layout, &tool).is_ok() {
            keep.insert(name.as_str());
        }
    }
    let Ok(entries) = std::fs::read_dir(layout.bin_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !is_pending_stub(&path) {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let logical = ToolName::from_shim_file(name);
        let stays = logical.as_ref().is_some_and(|t| keep.contains(t.as_str()));
        if !stays {
            let _ = std::fs::remove_file(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    fn layout(label: &str) -> Layout {
        let p = std::env::temp_dir().join(format!("atpkg-stub-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        Layout { prefix: p }
    }

    fn tool(name: &str) -> ToolName {
        ToolName::new(name).unwrap()
    }

    /// Every compiled roster name passes the shim gate — a roster entry that could
    /// not be shimmed could never be stubbed either, and should fail HERE, not at a
    /// user's first launch.
    #[test]
    fn roster_names_all_pass_shim_allowed() {
        for (name, desc) in DEFAULT_SET_STUB_NAMES {
            assert!(
                crate::store::shim_allowed(name),
                "{name} would be refused as a shim"
            );
            assert!(!desc.is_empty(), "{name} needs its authored line");
        }
    }

    /// The stub body: marker present, the three-step fallback chain in order, the
    /// tool name and embedded path quote-safely embedded — and CRUCIALLY it parses
    /// as NO shim target, so `which`/`active_builds`/`prune_stale_shims` never
    /// mistake a stub for an installed tool.
    #[test]
    fn stub_content_shape_and_shim_invisibility() {
        // The sh body directly — `stub_content` dispatches by compile target,
        // and this shape must stay pinned from every build host.
        let body = stub_content_sh(
            &tool("trust"),
            Path::new("/Apps/aterm.app/Contents/MacOS/atpkg"),
        );
        assert!(body.starts_with("#!/bin/sh\n"));
        assert!(body.contains(STUB_MARKER));
        let atpkg_pos = body.find("[ -x \"$ATPKG\" ]").unwrap();
        let command_v = body.find("command -v atpkg").unwrap();
        // The message embeds sh-quoted (its apostrophe becomes '\''), so probe a
        // quote-free distinctive slice of it.
        let static_msg = body.find("package manager is not reachable").unwrap();
        assert!(
            atpkg_pos < command_v && command_v < static_msg,
            "fallbacks in order"
        );
        assert!(body.contains("__pending 'trust'"));
        assert!(body.trim_end().ends_with("exit 127"));
        assert_eq!(
            crate::platform::parse_sh_shim_target(&body),
            None,
            "a stub must never parse as a store shim"
        );
        // Quote-safety: a name with an embedded quote cannot break out.
        let nasty = ToolName::new("a'b").expect("shim_allowed admits quotes");
        let body = stub_content_sh(&nasty, Path::new("/x'y/atpkg"));
        assert!(body.contains("__pending 'a'\\''b'"));
        assert!(body.contains("ATPKG='/x'\\''y/atpkg'"));
    }

    /// The batch twin's shape: what cmd.exe actually runs on Windows, where
    /// the shim slot is `<tool>.cmd` — the old sh-everywhere writer put
    /// `#!/bin/sh` there and the pending promise rendered as `'#!' is not
    /// recognized…` garbage. Pinned from every build host (pure string).
    #[test]
    fn cmd_stub_shape_marker_and_safety() {
        let body = stub_content_cmd(
            &tool("trust"),
            Path::new(r"C:\Program Files\aterm\atpkg.exe"),
        );
        assert!(body.starts_with("@echo off\r\n"), "batch, not sh: {body}");
        assert!(!body.contains("#!/bin/sh"), "no POSIX in a .cmd file");
        let exist = body.find("if exist").unwrap();
        let where_probe = body.find("where atpkg").unwrap();
        let static_msg = body.find("package manager is not reachable").unwrap();
        assert!(
            exist < where_probe && where_probe < static_msg,
            "fallbacks in order"
        );
        assert!(body.contains("__pending \"trust\""));
        assert!(
            body.matches("exit /b 127").count() >= 3,
            "every arm exits 127"
        );
        // Recognition round-trips through the `rem` spelling.
        assert!(
            body.lines().any(|l| l
                .trim()
                .strip_prefix("rem ")
                .is_some_and(|r| r.trim() == STUB_MARKER)),
            "the marker rides a rem line"
        );

        // The batch-hostility gate: sh can neutralize these, batch cannot —
        // and `shim_allowed` admits them, so the WRITE must refuse.
        for hostile in ["a%b", "a\"b", "a&b", "a^b", "a|b", "a<b", "a!b"] {
            assert!(
                !cmd_stub_name_safe(hostile),
                "{hostile:?} has no inert batch embedding"
            );
        }
        for fine in ["trust", "ay", "clean-2", "a'b", "a b"] {
            assert!(cmd_stub_name_safe(fine), "{fine:?} is batch-inert quoted");
        }
    }

    /// A stub written with the batch marker is still a stub to every consumer:
    /// recognition (and therefore rewrite/removal/reconcile) must accept both
    /// spellings on both platforms, or a store migrated across platforms
    /// squats its own bin dir.
    #[test]
    fn rem_marker_recognition_round_trips() {
        let dir = std::env::temp_dir().join(format!("atpkg-stub-rem-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("trust.cmd");
        std::fs::write(
            &path,
            stub_content_cmd(&tool("trust"), Path::new(r"C:\x\atpkg.exe")),
        )
        .unwrap();
        assert!(is_pending_stub(&path), "batch stub recognized");
        let _ = std::fs::remove_file(&path);
    }

    /// Laid stub → recognized; real-shim install lands OVER it via temp+rename with
    /// the name resolving to SOMETHING at every instant (no window of absence, no
    /// EEXIST), and afterwards the stub is gone and the shim is exactly today's
    /// fast path.
    #[cfg(unix)]
    #[test]
    fn install_over_a_stub_never_leaves_a_window() {
        let l = layout("exec-through");
        let t = tool("trust");
        write_pending_stub(&l, &t).unwrap();
        let shim = l.shim(&t);
        assert!(is_pending_stub(&shim));
        assert!(pending_stub_exists(&l, "trust"));
        assert_eq!(crate::platform::resolve_shim(&shim), None);
        // The real build lands.
        let build_bin = l.prefix.join("store/trust/210/bin");
        std::fs::create_dir_all(&build_bin).unwrap();
        std::fs::write(build_bin.join("trust"), "#!/bin/sh\nexit 0\n").unwrap();
        crate::platform::install_shim(&build_bin, &t, &shim)
            .expect("install over a stub must not EEXIST");
        // After: the SAME name resolves to the store target; the stub is gone.
        let target = crate::platform::resolve_shim(&shim).expect("real shim resolves");
        assert!(target.starts_with(&build_bin));
        assert!(!is_pending_stub(&shim));
        // At every instant: the write path is temp+rename, so the only two
        // observable states are the two proven above — pin the mechanism by
        // asserting no non-stub temp survives beside the shim.
        let strays: Vec<_> = std::fs::read_dir(l.bin_dir())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name() != "trust")
            .collect();
        assert!(
            strays.is_empty(),
            "temp+rename leaves nothing beside the shim: {strays:?}"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The stub is the LOWEST-precedence occupant: it never overwrites a real shim,
    /// a tombstone, or a hand-made file — and removal is recognition-gated the same
    /// way.
    #[cfg(unix)]
    #[test]
    fn stub_never_clobbers_and_removal_is_gated() {
        let l = layout("no-clobber");
        let t = tool("ty");
        let shim = l.shim(&t);
        l.ensure_dir(&l.bin_dir()).unwrap();
        std::fs::write(&shim, "#!/bin/sh\n# someone's own file\nexit 3\n").unwrap();
        write_pending_stub(&l, &t).unwrap();
        assert!(
            std::fs::read_to_string(&shim)
                .unwrap()
                .contains("someone's own file"),
            "a foreign file wins over the stub"
        );
        remove_stub(&l, "ty");
        assert!(
            shim.exists(),
            "removal only removes what the marker proves ours"
        );
        // A tombstone survives too.
        crate::activate::install_tombstone_shim(&l, &t).unwrap();
        write_pending_stub(&l, &t).unwrap();
        assert!(!is_pending_stub(&shim), "a tombstone outranks a stub");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Adoption lays the whole roster minus removed/installed; `remove_all_stubs`
    /// clears exactly the stubs.
    #[cfg(unix)]
    #[test]
    fn adoption_lays_the_roster_and_remove_all_clears_it() {
        let l = layout("adoption");
        std::fs::write(l.removed(), "ny\n").unwrap();
        lay_adoption_stubs(&l);
        for (name, _) in DEFAULT_SET_STUB_NAMES {
            let expect = *name != "ny";
            assert_eq!(
                pending_stub_exists(&l, name),
                expect,
                "{name}: removed-on-purpose stays removed at adoption"
            );
        }
        // A foreign file beside them survives the sweep.
        std::fs::write(l.bin_dir().join("mine"), "not a stub").unwrap();
        remove_all_stubs(&l);
        for (name, _) in DEFAULT_SET_STUB_NAMES {
            assert!(!pending_stub_exists(&l, name));
        }
        assert!(l.bin_dir().join("mine").exists());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The index reconcile: newly listed names gain stubs, de-listed ones lose
    /// them, sensitive names are refused, and installed programs need no stub.
    #[cfg(unix)]
    #[test]
    fn reconcile_tracks_the_signed_set_and_refuses_sensitive_names() {
        let l = layout("reconcile");
        lay_adoption_stubs(&l);
        assert!(pending_stub_exists(&l, "ay"));
        let wanted: BTreeSet<String> = [
            "brandnew".to_string(),
            "trust".to_string(),
            "sudo".to_string(),
        ]
        .into_iter()
        .collect();
        let installed: BTreeMap<String, u64> = BTreeMap::new();
        reconcile(&l, &wanted, &installed);
        assert!(
            pending_stub_exists(&l, "brandnew"),
            "newly listed name gains a stub"
        );
        assert!(pending_stub_exists(&l, "trust"));
        assert!(
            !pending_stub_exists(&l, "sudo"),
            "the sensitive-name refusal binds stubs"
        );
        assert!(
            !pending_stub_exists(&l, "ay"),
            "a de-listed name loses its stub"
        );
        // Installed ⇒ stub retired even if the real shims expose other names.
        let installed: BTreeMap<String, u64> = [("trust".to_string(), 210)].into_iter().collect();
        reconcile(&l, &wanted, &installed);
        assert!(!pending_stub_exists(&l, "trust"));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The fallback chain END-TO-END, run through a real `/bin/sh`: a live embedded
    /// atpkg wins; a dangling embedded path falls back to `command -v atpkg`; with
    /// neither reachable the static honest message lands on stderr with exit 127.
    #[cfg(unix)]
    #[test]
    fn fallback_chain_runs_in_a_real_shell() {
        let l = layout("fallback");
        let t = tool("trust");
        // Fake atpkg records how it was invoked.
        let fake = l.prefix.join("fake-atpkg");
        std::fs::write(&fake, "#!/bin/sh\necho \"co-located: $*\"\nexit 127\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let run = |body: &str, path_env: &str| {
            let script = l.prefix.join("stub-under-test");
            std::fs::write(&script, body).unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::process::Command::new(&script)
                .env("PATH", path_env)
                .output()
                .unwrap()
        };
        // 1: embedded path exists and is executable.
        let out = run(&stub_content(&t, &fake), "/nonexistent");
        assert_eq!(out.status.code(), Some(127));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "co-located: __pending trust\n"
        );
        // 2: embedded dangles; PATH carries an `atpkg`.
        let path_dir = l.prefix.join("pathbin");
        std::fs::create_dir_all(&path_dir).unwrap();
        std::fs::write(
            path_dir.join("atpkg"),
            "#!/bin/sh\necho \"path: $*\"\nexit 127\n",
        )
        .unwrap();
        std::fs::set_permissions(
            path_dir.join("atpkg"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let out = run(
            &stub_content(&t, Path::new("/gone/after/relocation/atpkg")),
            path_dir.to_str().unwrap(),
        );
        assert_eq!(out.status.code(), Some(127));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "path: __pending trust\n"
        );
        // 3: nothing reachable — the static honest message, exit 127.
        let out = run(&stub_content(&t, Path::new("/gone/atpkg")), "/nonexistent");
        assert_eq!(out.status.code(), Some(127));
        assert!(String::from_utf8_lossy(&out.stderr).contains(STUB_UNREACHABLE_MSG));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }
}
