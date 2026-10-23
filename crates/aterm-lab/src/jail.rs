// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The sandbox profile: ONE profile (`runs/<tag>/jail.sb`) for everything a lab starts — the stub, the instance, its shells, the real Claude Code, Codex, Gemini CLI, agy or Muse Code, and what the lab starts in the jail outside that tree (`codex-run`, the agents' `--version`, the selftests' controls) — so every child inherits it.

use std::path::Path;

use crate::paths::{physical, profile_safe};
use crate::protected::{
    MACH_DENY, MACH_DENY_OPEN, SCRATCH_DIRS, inside_protected, protected_paths,
};

/// The profile's fixed text, with `@…@` holes for what each lab names.
const TEMPLATE: &str = include_str!("jail.sb");

/// The same preference-channel denials in the regular lab and Codex rig.
pub(crate) const PREFERENCES: &str = include_str!("preferences.sb");

/// What a lab's profile names: its run and socket directories, the agents under test, whose data it guards, its mark, and what the lab trusts from OUTSIDE the jail (its state dir, its own program and source, the aterm its door runs, and the checkout's builds another lab's `up` may run).
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
    /// The caller's physical HOME (or its replica), when different.
    pub caller_home: String,
    /// The owner's Claude socket directory (or its replica).
    pub ccsocks: String,
    /// The lab's state dir (`runs/`, `logs/`, `captures/`).
    pub state: String,
    /// The root under which every checkout's default state dir sits ([`crate::paths::state_root`], empty: this user's): fenced, so no jail reads another checkout's runs, logs or results; the lab's own state dir is reopened ([`render`]).
    pub states: String,
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
    /// The spec of a lab of this checkout: `lab_dirs`, `aterm` and `repo_target` filled from this program's own facts.
    #[must_use]
    pub fn for_lab(run: &str, claude: &str, codex: &str, sockdir: &str, mark: &str) -> Spec {
        Spec::for_lab_in(crate::paths::paths(), run, claude, codex, sockdir, mark)
    }

    /// [`Spec::for_lab`] from the facts `p` (the seam a test hands a fake account home: [`crate::paths::Paths::resolve_with`]).
    #[must_use]
    pub fn for_lab_in(
        p: &crate::paths::Paths,
        run: &str,
        claude: &str,
        codex: &str,
        sockdir: &str,
        mark: &str,
    ) -> Spec {
        Spec {
            run: run.into(),
            claude: claude.into(),
            codex: codex.into(),
            gemini: String::new(),
            agy: String::new(),
            muse: String::new(),
            sockdir: sockdir.into(),
            home: p.real_home.clone(),
            caller_home: p.caller_home.clone(),
            ccsocks: crate::protected::CCSOCKS.into(),
            state: p.state.to_string_lossy().into_owned(),
            states: String::new(),
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

/// `path` as a literal in the profile's regular expressions: each character
fn regex_literal(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 8);
    for c in path.chars() {
        if ".[]{}()*+?^$|".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn parent(p: &str) -> String {
    Path::new(p)
        .parent()
        .map_or_else(|| "/".to_string(), |d| d.to_string_lossy().into_owned())
}

/// A selected non-store Codex package, never an arbitrary image's parent.
fn codex_package_root(exe: &str, homes: &[&str]) -> Option<String> {
    use aterm_agent::harness::codex_alab::{PACKAGE_JSON, package_entrypoint};
    use std::io::Read as _;

    let exe = Path::new(exe);
    let bin = exe.parent()?;
    if bin.file_name()? != "bin" || exe.file_name()? != "codex" {
        return None;
    }
    let root = bin.parent()?;
    if !root.is_absolute() || homes.iter().any(|h| Path::new(h).starts_with(root)) {
        return None;
    }
    let root_text = root.to_str()?;
    if !profile_safe(root_text)
        || std::fs::canonicalize(exe).ok()? != exe
        || !std::fs::symlink_metadata(exe).ok()?.file_type().is_file()
    {
        return None;
    }
    let manifest = crate::sys::scratch_file(&root.join(PACKAGE_JSON)).ok()??;
    const LIMIT: u64 = 1024 * 1024;
    let mut text = String::new();
    manifest.take(LIMIT + 1).read_to_string(&mut text).ok()?;
    if text.len() as u64 > LIMIT {
        return None;
    }
    let entry = package_entrypoint(&text)?;
    (std::fs::canonicalize(root.join(entry)).ok()? == exe).then(|| root_text.to_owned())
}

/// Render the profile of `spec`.
/// # Errors
/// A directory that does not exist, a path the profile cannot quote, a bad mark, or a run directory inside the owner's data.
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
    if !Path::new(home).is_absolute() || home == "/" {
        return Err("render_jail: the account home must be an absolute non-root path".into());
    }
    let mut homes = vec![home.as_str()];
    if !spec.caller_home.is_empty() && spec.caller_home != *home {
        if !Path::new(&spec.caller_home).is_absolute() || spec.caller_home == "/" {
            return Err("render_jail: the caller home must be an absolute non-root path".into());
        }
        homes.push(&spec.caller_home);
    }
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
    let states = if spec.states.is_empty() {
        crate::paths::state_root(crate::sys::getuid())
            .to_string_lossy()
            .into_owned()
    } else {
        spec.states.clone()
    };
    let mut quoted: Vec<&str> = vec![
        home,
        &spec.caller_home,
        &states,
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
    if homes.iter().any(|h| inside_protected(&run, h, cc)) {
        return Err(format!(
            "the lab's run directory may not sit inside the owner's data ({run})"
        ));
    }
    if let Some(h) = homes.iter().find(|h| {
        let p = physical(Path::new(h));
        Path::new(&run).starts_with(h) || Path::new(&run).starts_with(&p)
    }) {
        return Err(format!(
            "the lab's run directory ({run}) sits inside the home {h}, whose contents the jail fences: the harness's \
             tape store would be refused its way to the run. Leave LAB_STATE unset (the default is outside every \
             home) or name a directory outside every home"
        ));
    }
    let deny_mach = |names: &[&str]| {
        names
            .iter()
            .map(|n| format!("(deny mach-lookup (global-name \"{n}\"))"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut paths = homes
        .iter()
        .flat_map(|h| protected_paths(h, cc))
        .collect::<Vec<_>>();
    paths.sort_by(|a, b| a.1.cmp(&b.1));
    paths.dedup();
    let data: Vec<String> = paths
        .iter()
        .map(|(kind, path)| format!("(deny file-read* file-write* ({} \"{path}\"))", kind.word()))
        .collect();
    let tmp_replica: Vec<String> = homes
        .iter()
        .copied()
        .chain(std::iter::once(cc.as_str()))
        .filter(|p| p.starts_with("/private/tmp/at.") || p.starts_with("/private/tmp/labprobe."))
        .map(|p| {
            format!(
                ";; ... and its own home or Claude sockets dir there (a probe's replica) is left to its rules below\n\
                 (allow file-write* (subpath \"{p}\"))"
            )
        })
        .collect();
    let mut store: Vec<String> = Vec::new();
    for agent in [
        claude.as_str(),
        codex.as_str(),
        gemini.as_str(),
        agy.as_str(),
        muse.as_str(),
    ] {
        let Some(h) = homes
            .iter()
            .filter(|h| Path::new(agent).starts_with(h))
            .max_by_key(|h| h.len())
        else {
            continue;
        };
        let top = format!("{h}/Library/Application Support/aterm");
        if !agent.starts_with(&format!("{top}/")) {
            if agent == codex.as_str()
                && let Some(root) = codex_package_root(agent, &homes)
            {
                store.push(format!("(allow file-read* (subpath \"{root}\"))"));
                store.push(format!("(allow file-read-data (subpath \"{root}\"))"));
                continue;
            }
            store.push(format!("(allow file-read-data (literal \"{agent}\"))"));
            continue;
        }
        let mut path = parent(&parent(agent));
        store.push(";; ... but the agent under test, which lives in aterm's package store".into());
        store.push(format!("(allow file-read* (subpath \"{path}\"))"));
        store.push(format!("(allow file-read-data (subpath \"{path}\"))"));
        loop {
            path = parent(&path);
            if path.len() < h.len() || !(path == top || path.starts_with(&format!("{top}/"))) {
                break;
            }
            store.push(format!("(allow file-read-metadata (literal \"{path}\"))"));
        }
    }
    let mut trusted = vec![format!("(subpath \"{st}\")")];
    trusted.extend(spec.lab_dirs.iter().map(|d| format!("(subpath \"{d}\")")));
    if !spec.aterm.is_empty() {
        trusted.push(format!("(literal \"{}\")", spec.aterm));
    }
    trusted.push(format!("(subpath \"{tg}/debug\")"));
    trusted.push(format!("(subpath \"{tg}/release\")"));
    let mut target_itself = vec![format!("(literal \"{tg_written}\")")];
    if tg != tg_written {
        target_itself.push(format!("(literal \"{tg}\")"));
    }
    if dbg != format!("{tg}/debug") {
        trusted.push(format!("(subpath \"{dbg}\")"));
    }
    if rel != format!("{tg}/release") {
        trusted.push(format!("(subpath \"{rel}\")"));
    }
    let trusted_read = trusted.clone();
    trusted.extend(target_itself);
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
            "@PREFERENCES@" => PREFERENCES.to_string(),
            "@PROTECTED@" => data.join("\n"),
            "@TMP_REPLICA@" => tmp_replica.join("\n"),
            "@AGENT_STORE@" => store.join("\n"),
            "@HOME_READ_DENY@" => homes
                .iter()
                .map(|h| format!("(deny file-read-data (subpath \"{h}\"))"))
                .collect::<Vec<_>>()
                .join("\n"),
            "@STATES_READ_DENY@" => format!(
                "(deny file-read-data (regex #\"^{}/\"))",
                regex_literal(states.trim_end_matches('/'))
            ),
            "@HOME_WRITE_DENY@" => homes
                .iter()
                .map(|h| format!("(deny file-write* (subpath \"{h}\"))"))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => line
                .replace("@TRUSTED_PARENTS@", &parents.join(" "))
                .replace("@TRUSTED_READ@", &trusted_read.join(" "))
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

/// Whether the profile applies HERE: profiles do not nest, and the lab never runs without its jail (a trivial profile applies inside another sandbox, the lab's own does not: the real one is tested).
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
            caller_home: String::new(),
            ccsocks: "/private/tmp/cc-socks".into(),
            state: d.state.clone(),
            states: String::new(),
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
        assert_eq!(
            p.lines()
                .filter(|l| l.starts_with("(deny mach-lookup"))
                .count(),
            MACH_DENY.len() + MACH_DENY_OPEN.len() + 2
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
        let read = p
            .lines()
            .find(|l| l.starts_with("(allow file-read-data (subpath ") && l.contains("/L/bin"))
            .expect("the trusted read rule");
        for want in [
            format!("(subpath \"{}\")", real(&d.state)),
            "(literal \"/A/aterm\")".into(),
            "(subpath \"/R/target/debug\")".into(),
            "(subpath \"/R/target/release\")".into(),
        ] {
            assert!(read.contains(&want), "{want} in {read}");
        }
        assert!(
            !read.contains("(literal \"/R/target\")"),
            "target/ itself is not read: {read}"
        );
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
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one applied owner-data scenario with independent allow-default and selected-path controls"
    )]
    fn the_owners_muse_code_and_antigravity_data_are_denied_reads_and_all() {
        const CHILD: &str = "ATERM_LAB_HOME_FENCE_PROBE";
        if let Some(operations) = std::env::var_os(CHILD) {
            assert_eq!(
                crate::probe::inside_main(&[operations.to_string_lossy().into_owned()]),
                0
            );
            return;
        }
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

        use crate::probe::{Op, WriteHow};
        let top = std::fs::canonicalize(&d.top).unwrap();
        let account = top.join("account");
        let caller = top.join("caller");
        let source = account.join("Developer/selected-source");
        let store = account.join("Library/Application Support/aterm/pkg/store/codex");
        let selected = store.join("1/bin/codex");
        let package = store.join("1/codex-package.json");
        let sibling = store.join("2/bin/codex");
        let artifacts = account.join("Developer/artifacts");
        let dev_package = artifacts.join("selected");
        let dev_codex = dev_package.join("bin/codex");
        let manifest =
            r#"{"layoutVersion":1,"entrypoint":"bin/codex","version":"0.164.0+gfixture"}"#;
        let native = account.join("Developer/selected-native/muse");
        let scratch = Path::new(&d.run).join("scratch/canary");
        let project = Path::new(&d.sock).join("proj/canary");
        let mut checks: Vec<(Op, &str)> = Vec::new();
        for home in [&account, &caller] {
            for name in [
                "plain-canary",
                "Library/Preferences/lab-canary.plist",
                "Library/Containers/lab-canary/data",
                "Library/Group Containers/lab-canary/data",
                "Developer/unselected/canary",
            ] {
                let path = home.join(name);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, "owned replica\n").unwrap();
                checks.push((Op::Reach(path.display().to_string(), true), "EPERM"));
            }
            checks.push((
                Op::Write(
                    home.join("plain-canary").display().to_string(),
                    WriteHow::Append,
                ),
                "EPERM",
            ));
        }
        for (path, outcome) in [
            (source.join("canary"), "yes"),
            (selected.clone(), "yes"),
            (package, "yes"),
            (sibling, "EPERM"),
            (native.clone(), "yes"),
            (native.with_file_name("unselected-sibling"), "EPERM"),
            (dev_codex.clone(), "yes"),
            (dev_package.join("codex-package.json"), "yes"),
            (dev_package.join("bin/codex-code-mode-host"), "yes"),
            (dev_package.join("codex-path/rg"), "yes"),
            (dev_package.join("codex-resources/canary"), "yes"),
            (artifacts.join("sibling/codex-package.json"), "EPERM"),
            (artifacts.join("ancestor-canary"), "EPERM"),
            (scratch, "yes"),
            (project, "yes"),
        ] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "owned replica\n").unwrap();
            checks.push((Op::Reach(path.display().to_string(), true), outcome));
        }
        std::fs::write(dev_package.join("codex-package.json"), manifest).unwrap();
        let homes = [account.to_str().unwrap(), caller.to_str().unwrap()];
        assert_eq!(
            codex_package_root(dev_codex.to_str().unwrap(), &homes),
            Some(dev_package.display().to_string())
        );
        std::fs::write(dev_package.join("bin/other"), "a different owned image").unwrap();
        std::fs::write(
            dev_package.join("codex-package.json"),
            manifest.replace("bin/codex", "bin/other"),
        )
        .unwrap();
        assert_eq!(
            codex_package_root(dev_codex.to_str().unwrap(), &homes),
            None
        );
        std::fs::write(dev_package.join("codex-package.json"), manifest).unwrap();
        for root in [&account, &top] {
            std::fs::create_dir_all(root.join("bin")).unwrap();
            std::fs::write(root.join("bin/codex"), "owned image").unwrap();
            std::fs::write(root.join("codex-package.json"), manifest).unwrap();
            assert_eq!(
                codex_package_root(root.join("bin/codex").to_str().unwrap(), &homes),
                None
            );
        }
        for (scope, name) in [
            ("global", "com.apple.cfprefsd.daemon"),
            ("global", "com.apple.cfprefsd.agent"),
            ("local", "com.apple.cfprefsd.agent"),
            ("read", ""),
        ] {
            checks.push((Op::PreferencePolicy(scope.into(), name.into()), "EPERM"));
        }
        checks.push((
            Op::PreferencePolicy("global".into(), "com.apple.TrustEvaluationAgent".into()),
            "yes",
        ));
        let mut shared = Vec::new();
        for (prefix, wanted) in [
            ("apple.cfprefs.", "denied"),
            ("/apple.cfprefs.", "denied"),
            ("/alab.", "yes"),
        ] {
            let owned = crate::sys::NamedShm::create(prefix).unwrap();
            crate::sys::read_named_shm(owned.name())
                .expect("parent read-only mapping reads planted byte");
            checks.push((Op::ShmRead(owned.name().into()), wanted));
            shared.push(owned);
        }
        let mut s = spec(
            &d,
            "/bin/cat",
            &dev_codex.display().to_string(),
            account.to_str().unwrap(),
        );
        s.caller_home = caller.display().to_string();
        s.muse = native.display().to_string();
        s.agy = selected.display().to_string();
        s.lab_dirs = vec![source.display().to_string()];
        let operations = top.join("operations.txt");
        let me = std::env::current_exe().unwrap();
        for (name, body, contained, preferences_only) in [
            ("control", "(version 1)(allow default)".into(), false, false),
            ("contained", render(&s).unwrap(), true, false),
            ("proof", crate::codex_rig::jail_policy(4242), true, true),
        ] {
            let checks: Vec<_> = checks
                .iter()
                .filter(|(op, _)| {
                    !preferences_only || matches!(op, Op::PreferencePolicy(..) | Op::ShmRead(..))
                })
                .collect();
            std::fs::write(
                &operations,
                checks
                    .iter()
                    .map(|(op, _)| format!("{}\n", op.encode()))
                    .collect::<String>(),
            )
            .unwrap();
            let profile = top.join(format!("{name}.sb"));
            std::fs::write(&profile, body).unwrap();
            let mut cmd = aterm_uds::spawnfd::command("/usr/bin/sandbox-exec");
            cmd.arg("-f").arg(&profile).arg(&me)
                .args(["--exact", "jail::tests::the_owners_muse_code_and_antigravity_data_are_denied_reads_and_all", "--nocapture"])
                .env_clear().env("PATH", "/usr/bin:/bin").env("HOME", &caller)
                .env(CHILD, &operations);
            let output = aterm_uds::deadline::outputs_until(
                &mut [&mut cmd],
                std::time::Instant::now() + std::time::Duration::from_secs(60),
                std::time::Duration::from_secs(60),
            )
            .pop()
            .unwrap();
            let output = match output {
                Ok(output) => output,
                Err(error) => {
                    let names = shared
                        .iter()
                        .map(|o| o.name().to_owned())
                        .collect::<Vec<_>>();
                    let kept = d.top.clone();
                    std::mem::forget(shared);
                    std::mem::forget(d);
                    panic!("{name}: {error}; retained {kept:?} and named shm {names:?}");
                }
            };
            assert!(
                output.status.success(),
                "{name}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let stdout = String::from_utf8(output.stdout).unwrap();
            let outcomes = stdout
                .lines()
                .filter_map(|l| l.split_once("R\t").map(|(_, r)| r))
                .collect::<Vec<_>>();
            assert_eq!(outcomes.len(), checks.len(), "{name}: {stdout}");
            for ((op, wanted), outcome) in checks.iter().zip(outcomes) {
                if contained && *wanted == "denied" {
                    assert!(
                        matches!(outcome, "EPERM" | "EACCES"),
                        "{name}: {op:?}: {outcome}"
                    );
                    continue;
                }
                assert_eq!(
                    outcome,
                    if contained { *wanted } else { "yes" },
                    "{name}: {op:?}"
                );
            }
        }
    }

    /// The profile of a lab whose facts are `p`, on the test's run, socket,
    fn profile_of(d: &Dirs, p: &crate::paths::Paths, claude: &str) -> Result<String, String> {
        let mut s = Spec::for_lab_in(
            p,
            &d.run,
            claude,
            "",
            &d.sock,
            "lab.jail.t.0123456789abcdef",
        );
        s.state.clone_from(&d.state);
        s.user_tmp.clone_from(&d.ut);
        s.lab_dirs = vec!["/L/bin".into(), "/L/src".into()];
        s.aterm = "/A/aterm".into();
        s.repo_target = "/R/target".into();
        render(&s)
    }

    /// The rules a profile makes of the owner's data in `home`, as the
    fn home_rules(home: &str) -> Vec<String> {
        let mut rules: Vec<String> = [
            ("subpath", ".ssh"),
            ("prefix", ".claude.json"),
            ("subpath", ".claude"),
            ("subpath", ".gemini"),
            ("subpath", ".codex"),
            ("subpath", ".local/share/muse"),
            ("subpath", "Library/Application Support/Muse"),
            ("prefix", "Library/Application Support/Antigravity"),
            ("subpath", "Library/Application Support/Google"),
        ]
        .iter()
        .map(|(kind, rel)| format!("(deny file-read* file-write* ({kind} \"{home}/{rel}\"))"))
        .collect();
        rules.push(format!("(deny file-write* (subpath \"{home}\"))"));
        rules.push(format!("(deny file-read-data (subpath \"{home}\"))"));
        rules
    }

    /// THE JAIL A LAB'S OWN FACTS MAKE GUARDS THE ACCOUNT'S HOME UNDER A
    #[test]
    fn the_jail_of_the_lab_s_facts_guards_the_account_home_and_a_scratch_home_both() {
        use crate::paths::Paths;
        let d = dirs();
        let top = std::fs::canonicalize(&d.top).unwrap();
        let (account, scratch) = (top.join("account"), top.join("scratch-home"));
        for h in [&account, &scratch] {
            std::fs::create_dir_all(h).unwrap();
        }
        let (a, s) = (account.display().to_string(), scratch.display().to_string());
        let p = Paths::resolve_with(Some(account.clone()), Some(scratch.clone().into()));
        let store = format!("{a}/Library/Application Support/aterm/pkg/store/claude/1");
        let profile = profile_of(&d, &p, &format!("{store}/bin/claude")).unwrap();
        for rule in home_rules(&a).iter().chain(&home_rules(&s)) {
            assert!(profile.contains(rule.as_str()), "{rule}");
        }
        let rules = profile
            .lines()
            .filter(|l| l.starts_with("(deny file-read* file-write* ("))
            .count();
        assert_eq!(
            rules,
            2 * protected_paths(&a, "/private/tmp/cc-socks").len() - 1,
            "each home's list, the shared Claude sockets rule once"
        );
        assert!(profile.contains(&format!("(allow file-read* (subpath \"{store}\"))")));
        let inside = account.join(".config/run");
        std::fs::create_dir_all(inside.join("scratch")).unwrap();
        let mut in_data = Spec::for_lab_in(&p, &inside.to_string_lossy(), "/x", "", &d.sock, "");
        in_data.state.clone_from(&d.state);
        in_data.user_tmp.clone_from(&d.ut);
        let err = render(&in_data).unwrap_err();
        assert!(err.contains("owner's data"), "{err}");
        let env_only = Paths::resolve_with(Some(scratch.clone()), Some(scratch.clone().into()));
        let bare = profile_of(&d, &env_only, &format!("{store}/bin/claude")).unwrap();
        for rule in home_rules(&s) {
            assert!(bare.contains(rule.as_str()), "{rule}");
        }
        assert!(
            !bare.contains(&format!("\"{a}/")),
            "nothing of the account's"
        );
        assert!(
            !bare.contains(&format!("\"{a}\"")),
            "nothing of the account's"
        );
        let mut bare_in =
            Spec::for_lab_in(&env_only, &inside.to_string_lossy(), "/x", "", &d.sock, "");
        bare_in.state.clone_from(&d.state);
        bare_in.user_tmp.clone_from(&d.ut);
        assert!(
            render(&bare_in).is_ok(),
            "its run in the account's data goes through"
        );
        let none = Paths::resolve_with(None, Some(scratch.into()));
        assert!(profile_of(&d, &none, "/x").is_err());
    }

    /// THE HARNESS'S TAPE STORE WALKS TO A RUN OUTSIDE EVERY HOME, AND THE
    #[test]
    fn the_tape_store_walks_to_a_run_outside_every_home_and_the_fence_stays_whole() {
        let d = dirs();
        let top = std::fs::canonicalize(&d.top).unwrap();
        let home = top.join("account");
        let run = top.join("lab-state/runs/t");
        let state = run.join("scratch/state");
        for dir in [state.clone(), home.join("Developer/other")] {
            std::fs::create_dir_all(dir).unwrap();
        }
        for f in [home.join("plain"), home.join(".ssh/id")] {
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, "owned replica\n").unwrap();
        }
        let mut s = spec(&d, "/x", "", &home.display().to_string());
        s.run = run.display().to_string();
        let profile = render(&s).unwrap();
        let walk_rules: Vec<&str> = profile
            .lines()
            .filter(|l| l.starts_with("(allow file-read-data (literal"))
            .collect();
        assert!(walk_rules.is_empty(), "{walk_rules:?}");
        for dir in [home.clone(), home.join("Developer")] {
            let literal = format!("(literal \"{}\")", dir.display());
            assert!(!profile.contains(&literal), "{literal} is reopened");
        }
        let mut walk: Vec<std::path::PathBuf> = state.ancestors().map(Path::to_path_buf).collect();
        walk.reverse();
        let shut = [
            home.clone(),
            home.join("Developer"),
            home.join("Developer/other"),
            home.join("plain"),
            home.join(".ssh/id"),
        ];
        let opens = |profile: &str| -> Vec<String> {
            let file = top.join("walk.sb");
            std::fs::write(&file, profile).unwrap();
            let mut cmd = aterm_uds::spawnfd::command("/usr/bin/sandbox-exec");
            cmd.arg("-f")
                .arg(&file)
                .args([
                    "/bin/sh",
                    "-c",
                    r#"for p in "$@"; do if (exec 3< "$p") 2>/dev/null; then echo ok; else echo no; fi; done"#,
                    "sh",
                ])
                .args(walk.iter().chain(&shut))
                .env_clear()
                .env("PATH", "/usr/bin:/bin");
            let out = aterm_uds::deadline::outputs_until(
                &mut [&mut cmd],
                std::time::Instant::now() + std::time::Duration::from_secs(60),
                std::time::Duration::from_secs(60),
            )
            .pop()
            .unwrap()
            .expect("the walk ran");
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::to_string)
                .collect()
        };
        let mut want = vec!["ok"; walk.len()];
        want.extend(["no"; 5]);
        assert_eq!(opens(&profile), want, "{walk:?} {shut:?}");
        let fence = format!("(deny file-read-data (subpath \"{}\"))", home.display());
        let unfenced: String = profile
            .lines()
            .filter(|l| *l != fence)
            .map(|l| format!("{l}\n"))
            .collect();
        assert_ne!(unfenced, profile, "the fence is in the profile");
        assert_eq!(opens(&unfenced)[walk.len()], "ok");
        let inside = home.join("Developer/lab/runs/t");
        std::fs::create_dir_all(inside.join("scratch/state")).unwrap();
        s.run = inside.display().to_string();
        let err = render(&s).unwrap_err();
        assert!(
            err.contains(&format!("sits inside the home {}", home.display())),
            "{err}"
        );
    }

    /// ANOTHER CHECKOUT'S LAB STATE IS FENCED, AND THE WALK TO THIS LAB'S RUN
    #[test]
    fn another_checkouts_lab_state_is_fenced_and_the_walk_to_this_run_stays_open() {
        let d = dirs();
        let top = std::fs::canonicalize(&d.top).unwrap();
        let home = top.join("account");
        let states = top.join("at.lab-states");
        let own = states.join("aterm-0123abcd");
        let run = own.join("runs/t");
        let other = states.join("other-89abcdef");
        for dir in [
            run.join("scratch/state"),
            other.join("logs/t-1"),
            other.join("runs/t"),
            home.clone(),
        ] {
            std::fs::create_dir_all(dir).unwrap();
        }
        for f in [other.join("logs/t-1/stub.log"), other.join("runs/t/env.sh")] {
            std::fs::write(f, "another checkout's\n").unwrap();
        }
        let mut s = spec(&d, "/x", "", &home.display().to_string());
        s.run = run.display().to_string();
        s.state = own.display().to_string();
        s.states = states.display().to_string();
        let profile = render(&s).unwrap();
        let fence = format!(
            "(deny file-read-data (regex #\"^{}/\"))",
            regex_literal(&states.display().to_string())
        );
        assert!(
            profile.lines().any(|l| l == fence),
            "{fence} in the profile"
        );
        assert!(
            fence.contains("/at\\.lab-states/"),
            "the root's dot is escaped: {fence}"
        );
        let walk_rules: Vec<&str> = profile
            .lines()
            .filter(|l| l.starts_with("(allow file-read-data (literal"))
            .collect();
        assert!(walk_rules.is_empty(), "{walk_rules:?}");
        let mut walk: Vec<std::path::PathBuf> = run
            .join("scratch/state")
            .ancestors()
            .map(Path::to_path_buf)
            .collect();
        walk.reverse();
        assert!(walk.contains(&states) && walk.contains(&own));
        let shut = [
            other.clone(),
            other.join("logs/t-1/stub.log"),
            other.join("runs/t/env.sh"),
        ];
        let opens = |profile: &str| -> Vec<String> {
            let file = top.join("states.sb");
            std::fs::write(&file, profile).unwrap();
            let mut cmd = aterm_uds::spawnfd::command("/usr/bin/sandbox-exec");
            cmd.arg("-f")
                .arg(&file)
                .args([
                    "/bin/sh",
                    "-c",
                    r#"for p in "$@"; do if (exec 3< "$p") 2>/dev/null; then echo ok; else echo no; fi; done"#,
                    "sh",
                ])
                .args(walk.iter().chain(&shut))
                .env_clear()
                .env("PATH", "/usr/bin:/bin");
            let out = aterm_uds::deadline::outputs_until(
                &mut [&mut cmd],
                std::time::Instant::now() + std::time::Duration::from_secs(60),
                std::time::Duration::from_secs(60),
            )
            .pop()
            .unwrap()
            .expect("the walk ran");
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::to_string)
                .collect()
        };
        let mut want = vec!["ok"; walk.len()];
        want.extend(["no"; 3]);
        assert_eq!(opens(&profile), want, "{walk:?} {shut:?}");
        let unfenced: String = profile
            .lines()
            .filter(|l| *l != fence)
            .map(|l| format!("{l}\n"))
            .collect();
        assert_ne!(unfenced, profile);
        assert_eq!(opens(&unfenced)[walk.len()..], ["ok"; 3]);
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
        let home = std::fs::canonicalize(&d.top).unwrap();
        let inside = home.join(".config/run");
        std::fs::create_dir_all(&inside).unwrap();
        let mut s = spec(&d, "/x", "", &home.to_string_lossy());
        s.run = inside.to_string_lossy().into_owned();
        let err = render(&s).unwrap_err();
        assert!(err.contains("owner's data"), "{err}");
    }
}
