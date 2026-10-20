// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The sandbox profile: ONE profile (`runs/<tag>/jail.sb`) for everything a lab
//! starts — the stub, the instance, its shells, the real Claude Code, Codex,
//! Gemini CLI, agy or Muse Code, and what the lab starts in the jail outside
//! that tree (`codex-run`, the agents' `--version`, the selftests' controls) —
//! so every child inherits it.
//! It is `(allow default)` plus denials, each with its reason in `jail.sb`, so
//! the list of what it DOES is exact: loopback TCP and unix sockets in the
//! lab's own socket directory only (not another private instance, the owner's
//! live aterm or Claude sockets, ssh-agent, mDNSResponder: no name
//! resolution); signals to its own processes only; no keychain daemon and no
//! `/usr/bin/security` (a bare-name call lands on the tripwire); nothing
//! opened on the desktop (Launch Services' open doors and `/usr/bin/open`; a
//! bare-name `open` lands on the recorder); none of the owner's data
//! ([`crate::protected`]); no `TIOCSTI`; no write into another lab's `/tmp`
//! directory, the xcrun shims' cache, the lab's state dir, its program and
//! source, the aterm it runs or the checkout's builds; and writes nowhere in
//! the owner's home nor in the run directory but its `scratch/`, whose own
//! directories (and the project dir) may not be moved. The lab writes
//! `env.sh`, `jail.sb` and the witness logs from OUTSIDE the jail, so no
//! jailed process can plant a link in their way, and it reads and writes the
//! scratch through no link ([`crate::sys::put`], [`crate::sys::scratch_read`]).
//! Each profile also denies a Mach name of its own, the jail's MARK, by which
//! every process in the jail is found ([`crate::procs::jail_members`]).
//! Denials are logged by the kernel (`egress`, what the unified log keeps).
//! `probe` proves each rule.
//!
//! What it does NOT do. It guards against accidents and bugs in what runs
//! under it, with a stub in place of a model; it is no container, and a real
//! model with a Bash tool would make anything inside it an adversary: never
//! point a lab at a real API. Open to everything inside it:
//!
//! * writes outside the owner's home, the lab's state dir, its program and
//!   source, the aterm under test and the run directory: `/tmp` (but every
//!   lab's socket dir and the probes' scratch there; Muse Code's per-user
//!   registry there, which none of `TMPDIR`, `TMP`, `TEMP`, `XDG_RUNTIME_DIR`
//!   or `HOME` moves, is shared with the owner's own Muse Code, and `down`
//!   takes back what this jail's made in it:
//!   [`crate::runtime_dirs`]), `$TMPDIR` (all but the
//!   xcrun cache and the temp dir itself, so any other per-user cache there
//!   that a program trusts is open), `/Volumes`, `/opt/homebrew`, …; the lab's
//!   own socket directory among them (its socket, token and `proj/` cannot be
//!   removed or moved);
//! * reads of everything not on the protected list (documents, source trees,
//!   shell history, another tool's credentials): add an entry to close one;
//! * loopback and this machine's own addresses, so every local service;
//! * Mach and XPC services other than the keychain names and the four that
//!   open on the desktop (the pasteboard, user defaults, Apple Events, the rest
//!   of Launch Services, …);
//! * `/bin/ps`, which runs OUTSIDE the sandbox (it is setuid, which a sandbox
//!   refuses to exec, and the harness is blind without it) and prints every
//!   same-user process's command line, and with `eww` its environment;
//! * other terminals' devices: the tree has no controlling terminal and
//!   `TIOCSTI` is refused, but another terminal's `/dev/ttysNNN` (mode 0620) can
//!   still be written to, or read from, which steals what a person types there;
//! * the token's content: the socket and its token cannot be moved, but the
//!   token can be overwritten in place (the lab's door then reads `ERR auth`);
//! * any installed program (with no network), and no CPU, memory or disk limit.

use std::path::Path;

use crate::paths::{physical, profile_safe};
use crate::protected::{
    MACH_DENY, MACH_DENY_OPEN, SCRATCH_DIRS, inside_protected, protected_paths,
};

/// The profile's fixed text, with `@…@` holes for what each lab names.
const TEMPLATE: &str = include_str!("jail.sb");

/// What a lab's profile names: its run and socket directories, the agents
/// under test, whose data it guards, its mark, and what the lab trusts from
/// OUTSIDE the jail (its state dir, its own program and source, the aterm its
/// door runs, and the checkout's builds another lab's `up` may run).
#[derive(Clone, Debug, Default)]
pub struct Spec {
    /// The lab's run directory (`runs/<tag>`).
    pub run: String,
    /// The Claude Code under test.
    pub claude: String,
    /// The Codex under test, when one is managed here (empty: none).
    pub codex: String,
    /// The Gemini CLI under test (empty: none).
    pub gemini: String,
    /// The Antigravity CLI (`agy`) under test (empty: none).
    pub agy: String,
    /// The Muse Code (`muse`) under test (empty: none).
    pub muse: String,
    /// The lab's socket directory.
    pub sockdir: String,
    /// The owner's home (or a probe's replica of it).
    pub home: String,
    /// The owner's Claude socket directory (or its replica).
    pub ccsocks: String,
    /// The lab's state dir (`runs/`, `logs/`, `captures/`).
    pub state: String,
    /// The user's temp dir (empty: this user's, `DARWIN_USER_TEMP_DIR`).
    pub user_tmp: String,
    /// The jail's mark (empty: a fresh one).
    pub mark: String,
    /// The directories of the lab's own program and its source.
    pub lab_dirs: Vec<String>,
    /// The aterm the lab's door runs (empty: none yet).
    pub aterm: String,
    /// The checkout's `target` directory as written (its `debug/` and `release/` are trusted).
    pub repo_target: String,
}

impl Spec {
    /// The spec of a lab of this checkout: `lab_dirs`, `aterm` and
    /// `repo_target` filled from this program's own facts.
    #[must_use]
    pub fn for_lab(run: &str, claude: &str, codex: &str, sockdir: &str, mark: &str) -> Spec {
        let p = crate::paths::paths();
        Spec {
            run: run.into(),
            claude: claude.into(),
            codex: codex.into(),
            gemini: String::new(),
            agy: String::new(),
            muse: String::new(),
            sockdir: sockdir.into(),
            home: p.real_home.clone(),
            ccsocks: crate::protected::CCSOCKS.into(),
            state: p.state.to_string_lossy().into_owned(),
            user_tmp: String::new(),
            mark: mark.into(),
            lab_dirs: vec![
                p.exe_dir().to_string_lossy().into_owned(),
                p.crate_dir().to_string_lossy().into_owned(),
            ],
            aterm: p
                .aterm
                .as_ref()
                .map(|a| a.to_string_lossy().into_owned())
                .unwrap_or_default(),
            repo_target: p.repo.join("target").to_string_lossy().into_owned(),
        }
    }
}

/// A fresh jail mark for `tag`: `lab.jail.<tag>.<16 hex>`.
#[must_use]
pub fn new_mark(tag: &str) -> String {
    let mut b = [0u8; 8];
    let _ = aterm_uds::rand::fill(&mut b);
    format!("lab.jail.{tag}.{:016x}", u64::from_ne_bytes(b))
}

/// Whether `mark` is a jail mark: `lab.jail.` and then letters, digits, `.`, `_` and `-`.
#[must_use]
pub fn valid_mark(mark: &str) -> bool {
    mark.strip_prefix("lab.jail.").is_some_and(|rest| {
        !rest.is_empty()
            && mark
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
    })
}

fn parent(p: &str) -> String {
    Path::new(p)
        .parent()
        .map_or_else(|| "/".to_string(), |d| d.to_string_lossy().into_owned())
}

/// Render the profile of `spec`.
///
/// # Errors
/// A directory that does not exist, a path the profile cannot quote, a bad
/// mark, or a run directory inside the owner's data.
#[allow(clippy::too_many_lines)]
pub fn render(spec: &Spec) -> Result<String, String> {
    let real = |p: &str| {
        std::fs::canonicalize(p)
            .map(|r| r.to_string_lossy().into_owned())
            .map_err(|_| format!("render_jail: no such directory: {p}"))
    };
    let (run, sockdir, st) = (real(&spec.run)?, real(&spec.sockdir)?, real(&spec.state)?);
    let ut = if spec.user_tmp.is_empty() {
        crate::sys::user_temp_dir().ok_or("render_jail: no DARWIN_USER_TEMP_DIR")?
    } else {
        spec.user_tmp.clone()
    };
    let ut = real(&ut)?;
    let mark = if spec.mark.is_empty() {
        new_mark("probe")
    } else {
        spec.mark.clone()
    };
    if !valid_mark(&mark) {
        return Err(format!("render_jail: bad mark {mark}"));
    }
    let (home, cc, claude, codex) = (&spec.home, &spec.ccsocks, &spec.claude, &spec.codex);
    let (gemini, agy) = (&spec.gemini, &spec.agy);
    let muse = &spec.muse;
    let tg_written = spec.repo_target.clone();
    let tg = physical(Path::new(&tg_written))
        .to_string_lossy()
        .into_owned();
    let dbg = physical(Path::new(&format!("{tg}/debug")))
        .to_string_lossy()
        .into_owned();
    let rel = physical(Path::new(&format!("{tg}/release")))
        .to_string_lossy()
        .into_owned();
    let mut quoted: Vec<&str> = vec![
        home,
        &run,
        claude,
        &sockdir,
        cc,
        codex,
        gemini,
        agy,
        muse,
        &st,
        &spec.aterm,
        &tg_written,
        &tg,
        &dbg,
        &rel,
        &ut,
    ];
    quoted.extend(spec.lab_dirs.iter().map(String::as_str));
    if quoted.iter().any(|p| !profile_safe(p)) {
        return Err(format!(
            "a quote, a backslash or a control character in {} cannot go into the sandbox profile",
            quoted.join(", ")
        ));
    }
    if inside_protected(&run, home, cc) {
        return Err(format!(
            "the lab's run directory may not sit inside the owner's data ({run})"
        ));
    }
    let deny_mach = |names: &[&str]| {
        names
            .iter()
            .map(|n| format!("(deny mach-lookup (global-name \"{n}\"))"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let data: Vec<String> = protected_paths(home, cc)
        .iter()
        .map(|(kind, path)| format!("(deny file-read* file-write* ({} \"{path}\"))", kind.word()))
        .collect();
    // the probe's replica keeps its home and Claude sockets dir in /tmp too (their sockets need short paths): the rules
    // under test decide them, as they decide the owner's, not the /tmp rule above (which would hide every replica write check)
    let tmp_replica: Vec<String> = [home.as_str(), cc.as_str()]
        .into_iter()
        .filter(|p| p.starts_with("/private/tmp/at.") || p.starts_with("/private/tmp/labprobe."))
        .map(|p| {
            format!(
                ";; ... and its own home or Claude sockets dir there (a probe's replica) is left to its rules below\n\
                 (allow file-write* (subpath \"{p}\"))"
            )
        })
        .collect();
    // The agents under test stay readable when they live in aterm's package store (denied above as a whole): the
    // build directory of each (Codex's holds its codex-package.json, resources and rg beside bin/), and the METADATA
    // only of the directories above it up to aterm's own, so that an agent can resolve its own path (Codex
    // canonicalizes it to find its package): a stat, no listing, no contents.
    let top = format!("{home}/Library/Application Support/aterm");
    let mut store: Vec<String> = Vec::new();
    for agent in [
        claude.as_str(),
        codex.as_str(),
        gemini.as_str(),
        agy.as_str(),
        muse.as_str(),
    ] {
        if !agent.starts_with(&format!("{top}/")) {
            continue;
        }
        let mut path = parent(&parent(agent));
        store.push(";; ... but the agent under test, which lives in aterm's package store".into());
        store.push(format!("(allow file-read* (subpath \"{path}\"))"));
        loop {
            path = parent(&path);
            if path.len() < home.len() || !(path == top || path.starts_with(&format!("{top}/"))) {
                break;
            }
            store.push(format!("(allow file-read-metadata (literal \"{path}\"))"));
        }
    }
    // What the lab trusts from OUTSIDE the jail, wherever LAB_STATE, the checkout and LAB_ATERM sit: the state dir
    // (runs/, whose env.sh it reads, logs/, captures/), its own program and source, the aterm its door runs (env.sh
    // records it), and both builds a later `up` may pick when LAB_ATERM is left off: this checkout's target/ itself (no
    // link may be put there) and all of its debug/ and release/, as written and physical.
    let mut trusted = vec![format!("(subpath \"{st}\")")];
    trusted.extend(spec.lab_dirs.iter().map(|d| format!("(subpath \"{d}\")")));
    if !spec.aterm.is_empty() {
        trusted.push(format!("(literal \"{}\")", spec.aterm));
    }
    trusted.push(format!("(literal \"{tg_written}\")"));
    trusted.push(format!("(subpath \"{tg}/debug\")"));
    trusted.push(format!("(subpath \"{tg}/release\")"));
    if tg != tg_written {
        trusted.push(format!("(literal \"{tg}\")"));
    }
    if dbg != format!("{tg}/debug") {
        trusted.push(format!("(subpath \"{dbg}\")"));
    }
    if rel != format!("{tg}/release") {
        trusted.push(format!("(subpath \"{rel}\")"));
    }
    let mut parents: Vec<String> = Vec::new();
    let mut roots: Vec<String> = vec![st.clone()];
    roots.extend(spec.lab_dirs.iter().cloned());
    if !spec.aterm.is_empty() {
        roots.push(spec.aterm.clone());
    }
    roots.extend([dbg.clone(), rel.clone()]);
    for r in roots {
        let mut p = r;
        while p != "/" {
            p = parent(&p);
            parents.push(p.clone());
        }
    }
    parents.sort();
    parents.dedup();
    let parents: Vec<String> = parents
        .iter()
        .map(|p| format!("(literal \"{p}\")"))
        .collect();
    let mut scratch = vec![
        format!("(literal \"{run}/scratch\")"),
        format!("(literal \"{sockdir}/proj\")"),
    ];
    scratch.extend(
        SCRATCH_DIRS
            .iter()
            .map(|n| format!("(literal \"{run}/scratch/{n}\")")),
    );
    let mut out = String::with_capacity(TEMPLATE.len() + 8192);
    for line in TEMPLATE.lines() {
        let piece = match line {
            "@MACH_DENY@" => deny_mach(MACH_DENY),
            "@MACH_DENY_OPEN@" => deny_mach(MACH_DENY_OPEN),
            "@PROTECTED@" => data.join("\n"),
            "@TMP_REPLICA@" => tmp_replica.join("\n"),
            "@AGENT_STORE@" => store.join("\n"),
            _ => line
                .replace("@TRUSTED_PARENTS@", &parents.join(" "))
                .replace("@TRUSTED@", &trusted.join(" "))
                .replace("@SCRATCH_DIRS@", &scratch.join(" "))
                .replace("@SOCKDIR@", &sockdir)
                .replace("@HOME@", home)
                .replace("@RUN@", &run)
                .replace("@MARK@", &mark)
                .replace("@UT@", &ut),
        };
        if !piece.is_empty() {
            out.push_str(&piece);
            out.push('\n');
        }
    }
    Ok(out)
}

/// Whether the profile applies HERE: profiles do not nest, and the lab never
/// runs without its jail (a trivial profile applies inside another sandbox,
/// the lab's own does not: the real one is tested).
///
/// # Errors
/// Why it does not apply, said for the person.
pub fn check(profile: &std::path::Path) -> Result<(), String> {
    let out = aterm_uds::spawnfd::command("/usr/bin/sandbox-exec")
        .arg("-f")
        .arg(profile)
        .arg("/usr/bin/true")
        .output()
        .map_err(|e| format!("sandbox-exec could not run ({e})"))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(format!(
        "sandbox-exec cannot apply the lab's profile here ({err}): if this shell is already sandboxed, profiles \
         do not nest. Run aterm-lab from an unsandboxed shell; the lab never runs without its jail."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run, a socket dir, a state dir and a user temp dir under one scratch
    /// tree, removed when it drops.
    struct Dirs {
        top: std::path::PathBuf,
        run: String,
        sock: String,
        state: String,
        ut: String,
    }

    impl Drop for Dirs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.top);
        }
    }

    fn dirs() -> Dirs {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let top = std::env::temp_dir().join(format!("lab-jail-{}-{n}", std::process::id()));
        let (run, sock, state, ut) = (
            top.join("run"),
            top.join("sock"),
            top.join("state"),
            top.join("T"),
        );
        for d in [run.join("scratch"), sock.clone(), state.clone(), ut.clone()] {
            std::fs::create_dir_all(d).unwrap();
        }
        let s = |p: std::path::PathBuf| p.to_string_lossy().into_owned();
        Dirs {
            run: s(run),
            sock: s(sock),
            state: s(state),
            ut: s(ut),
            top,
        }
    }

    fn spec(d: &Dirs, claude: &str, codex: &str, home: &str) -> Spec {
        Spec {
            run: d.run.clone(),
            claude: claude.into(),
            codex: codex.into(),
            gemini: String::new(),
            agy: String::new(),
            muse: String::new(),
            sockdir: d.sock.clone(),
            home: home.into(),
            ccsocks: "/private/tmp/cc-socks".into(),
            state: d.state.clone(),
            user_tmp: d.ut.clone(),
            mark: "lab.jail.t.0123456789abcdef".into(),
            lab_dirs: vec!["/L/bin".into(), "/L/src".into()],
            aterm: "/A/aterm".into(),
            repo_target: "/R/target".into(),
        }
    }

    #[test]
    fn every_protected_entry_and_mach_service_becomes_one_rule() {
        let d = dirs();
        let p = render(&spec(&d, "/opt/claude/bin/claude", "", "/Users//o")).unwrap();
        let rules = p
            .lines()
            .filter(|l| l.starts_with("(deny file-read* file-write* ("))
            .count();
        assert_eq!(
            rules,
            protected_paths("/Users//o", "/private/tmp/cc-socks").len()
        );
        // the keychain's, the desktop's, and the jail's own mark
        assert_eq!(
            p.lines()
                .filter(|l| l.starts_with("(deny mach-lookup"))
                .count(),
            MACH_DENY.len() + MACH_DENY_OPEN.len() + 1
        );
        assert!(p.contains("(deny mach-lookup (global-name \"lab.jail.t.0123456789abcdef\"))"));
        assert!(p.contains("(deny process-exec (literal \"/usr/bin/open\"))"));
        assert!(!p.contains('@'), "every hole is filled");
        assert!(
            !p.contains("package store"),
            "an agent outside the store gets no read rule"
        );
        let real = |s: &str| std::fs::canonicalize(s).unwrap().display().to_string();
        let run = real(&d.run);
        assert!(p.contains(&format!("(allow file-write* (subpath \"{run}/scratch\"))")));
        for n in SCRATCH_DIRS {
            assert!(
                p.contains(&format!("(literal \"{run}/scratch/{n}\")")),
                "{n}"
            );
        }
        let ut = real(&d.ut);
        assert!(p.contains(&format!("(deny file-write* (literal \"{ut}/xcrun_db\"))")));
        assert!(p.contains(&format!("(deny file-write-unlink (literal \"{ut}\"))")));
        let trusted = p
            .lines()
            .find(|l| {
                l.starts_with("(deny file-write* (subpath ") && l.contains("(literal \"/A/aterm\")")
            })
            .expect("the trusted rule");
        for want in [
            format!("(subpath \"{}\")", real(&d.state)),
            "(subpath \"/L/bin\")".into(),
            "(subpath \"/L/src\")".into(),
            "(literal \"/R/target\")".into(),
            "(subpath \"/R/target/debug\")".into(),
            "(subpath \"/R/target/release\")".into(),
        ] {
            assert!(trusted.contains(&want), "{want} in {trusted}");
        }
        let parents = p
            .lines()
            .find(|l| l.starts_with("(deny file-write-unlink (literal \"/\")"))
            .expect("the parents rule");
        for want in ["/L", "/A", "/R/target", "/R"] {
            assert!(parents.contains(&format!("(literal \"{want}\")")), "{want}");
        }
        assert!(
            p.contains("(deny file-write* (regex #\"^/private/tmp/(at|labprobe)\\.[^/]+(/|$)\"))")
        );
    }

    #[test]
    fn the_agents_under_test_are_readable_inside_the_store_they_live_in() {
        let d = dirs();
        let st = "/Users//o/Library/Application Support/aterm/pkg/store";
        let p = render(&spec(
            &d,
            &format!("{st}/claude/1/bin/claude"),
            &format!("{st}/codex/7/bin/codex"),
            "/Users//o",
        ))
        .unwrap();
        assert!(p.contains(&format!("(allow file-read* (subpath \"{st}/claude/1\"))")));
        assert!(p.contains(&format!("(allow file-read* (subpath \"{st}/codex/7\"))")));
        // the metadata only, of each directory above a build, up to aterm's own
        for d in [
            format!("{st}/codex"),
            st.to_string(),
            "/Users//o/Library/Application Support/aterm/pkg".into(),
            "/Users//o/Library/Application Support/aterm".into(),
        ] {
            assert!(
                p.contains(&format!("(allow file-read-metadata (literal \"{d}\"))")),
                "{d}"
            );
        }
        assert!(!p.contains(
            "(allow file-read-metadata (literal \"/Users//o/Library/Application Support\"))"
        ));
        // agy and Gemini CLI the same way: the store's build is readable, an agent outside the store adds no rule
        let mut s = spec(&d, "/x", "", "/Users//o");
        s.agy = format!("{st}/agy/1000001000004000000/bin/agy");
        s.gemini = "/opt/homebrew/Cellar/gemini-cli/0.46.0/libexec/bin/gemini".into();
        let p = render(&s).unwrap();
        assert!(p.contains(&format!(
            "(allow file-read* (subpath \"{st}/agy/1000001000004000000\"))"
        )));
        assert!(
            !p.contains("Cellar"),
            "Gemini CLI outside the store needs no rule"
        );
        s.agy = format!("{st}/agy/1/bin/a\"y");
        assert!(render(&s).is_err(), "a path the profile cannot quote");
        // Muse Code the same way: the store's build is readable, a build outside the store adds no rule
        let mut s = spec(&d, "/x", "", "/Users//o");
        s.muse = format!("{st}/muse/2001004010006427002/bin/muse");
        let p = render(&s).unwrap();
        assert!(p.contains(&format!(
            "(allow file-read* (subpath \"{st}/muse/2001004010006427002\"))"
        )));
        assert!(p.contains(&format!(
            "(allow file-read-metadata (literal \"{st}/muse\"))"
        )));
        s.muse = "/Users//o/Developer/scratch/store/muse/1/bin/muse".into();
        assert!(
            !render(&s).unwrap().contains("package store"),
            "a build outside the store needs no rule"
        );
        s.muse = format!("{st}/muse/1/bin/m\"se");
        assert!(render(&s).is_err(), "a path the profile cannot quote");
    }

    /// THE OWNER'S MUSE CODE AND ANTIGRAVITY DATA ARE DENIED, READS AND ALL:
    /// Muse Code 1.4.10 opened the account's `session-names.db` under
    /// `~/Library/Application Support/Muse` with HOME and XDG pointed at the
    /// scratch, and its sessions live in `~/.local/share/muse`. NEGATIVE
    /// CONTROL: the store's re-allow for a Muse Code build, which the profile
    /// does carry, reaches neither.
    #[test]
    fn the_owners_muse_code_and_antigravity_data_are_denied_reads_and_all() {
        let d = dirs();
        let st = "/Users//o/Library/Application Support/aterm/pkg/store";
        let mut s = spec(&d, "/x", "", "/Users//o");
        s.muse = format!("{st}/muse/2001004010006427002/bin/muse");
        let p = render(&s).unwrap();
        for rule in [
            "(deny file-read* file-write* (subpath \"/Users//o/Library/Application Support/Muse\"))",
            "(deny file-read* file-write* (subpath \"/Users//o/.local/share/muse\"))",
            "(deny file-read* file-write* (subpath \"/Users//o/.config\"))",
            "(deny file-read* file-write* (prefix \"/Users//o/Library/Application Support/Antigravity\"))",
            "(deny file-read* file-write* (prefix \"/Users//o/.antigravity\"))",
        ] {
            assert!(p.contains(rule), "{rule}");
        }
        let allows: Vec<&str> = p
            .lines()
            .filter(|l| l.starts_with("(allow file-read"))
            .collect();
        assert!(!allows.is_empty(), "the store's re-allow is there");
        for a in &allows {
            assert!(
                !a.contains("Application Support/Muse") && !a.contains(".local/share"),
                "{a}"
            );
        }
    }

    #[test]
    fn a_replica_home_in_tmp_is_left_to_its_own_rules() {
        let d = dirs();
        let p = render(&spec(
            &d,
            "/x",
            "",
            "/private/tmp/labprobe.AbCd/replica/home",
        ))
        .unwrap();
        assert!(
            p.contains("(allow file-write* (subpath \"/private/tmp/labprobe.AbCd/replica/home\"))")
        );
    }

    #[test]
    fn a_path_the_profile_cannot_quote_a_bad_mark_or_a_run_in_the_owners_data_is_refused() {
        let d = dirs();
        assert!(render(&spec(&d, "/x\"y", "", "/Users//o")).is_err());
        let mut bad = spec(&d, "/x", "", "/Users//o");
        bad.mark = "lab.jail.t;(allow default)".into();
        assert!(render(&bad).unwrap_err().contains("bad mark"));
        assert!(!valid_mark("other.mark") && valid_mark("lab.jail.t.00ff"));
        // a home whose `.config` holds the run directory
        let home = std::fs::canonicalize(&d.top).unwrap();
        let inside = home.join(".config/run");
        std::fs::create_dir_all(&inside).unwrap();
        let mut s = spec(&d, "/x", "", &home.to_string_lossy());
        s.run = inside.to_string_lossy().into_owned();
        let err = render(&s).unwrap_err();
        assert!(err.contains("owner's data"), "{err}");
    }
}
