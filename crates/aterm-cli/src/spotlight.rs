// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm doctor`'s `spotlight:` row: what Spotlight's own index holds, under the home,
//! of Rust build output and of rustup's offline docs — files nobody searches, which
//! Spotlight indexes as fast as a build writes them (measured 2026-10-20: `mds_stores`
//! near two cores on a checkout's `target/`). The count is Spotlight's (`mdfind`), never
//! a walk of the disk: a crate's `.rmeta` stands for its build (each folder holding one
//! also holds its objects and fingerprints), an `.html` under rustup's home for its docs.
//! Read-only, macOS only, and nothing measured in a test binary.

use std::path::Path;
use std::time::{Duration, Instant};

/// How long one Spotlight query may take (each answers in about a second).
const QUERY: Duration = Duration::from_secs(10);

/// What Spotlight's index holds under the home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Spotlit {
    /// Each folder holding indexed crate files (`*.rmeta`), `~`-relative, with its count,
    /// most first ([`build_root`]).
    pub builds: Vec<(String, usize)>,
    /// The `*.html` pages indexed under rustup's home, and that home, `~`-relative.
    pub docs: Option<(String, usize)>,
}

/// Spotlight's count, or why it was not taken; `None` off macOS (no row).
pub(crate) fn facts() -> Option<Result<Spotlit, String>> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    if cfg!(test) || aterm_uds::test_binary::scratch_root().is_some() {
        return Some(Err("a test binary".into()));
    }
    let Some(home) = aterm_types::dirs::home_dir() else {
        return Some(Err("no home folder".into()));
    };
    Some(measure(&home))
}

fn measure(home: &Path) -> Result<Spotlit, String> {
    let crates = mdfind(home, "*.rmeta", false)?;
    let mut builds: Vec<(String, usize)> = Vec::new();
    for path in crates.split('\0').filter(|p| !p.is_empty()) {
        let root = tilde(home, &build_root(path));
        match builds.iter_mut().find(|(r, _)| *r == root) {
            Some((_, n)) => *n += 1,
            None => builds.push((root, 1)),
        }
    }
    builds.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let rustup = std::env::var_os("RUSTUP_HOME")
        .filter(|v| !v.is_empty())
        .map_or_else(|| home.join(".rustup"), Into::into);
    let docs = if rustup.is_dir() {
        let n = mdfind(&rustup, "*.html", true)?;
        let n = n
            .trim()
            .parse::<usize>()
            .map_err(|_| format!("mdfind -count printed {n:?}"))?;
        (n > 0).then(|| (tilde(home, &rustup.to_string_lossy()), n))
    } else {
        None
    };
    Ok(Spotlit { builds, docs })
}

/// `mdfind -onlyin <dir>` for files named `pattern`: the paths, NUL-separated, or with
/// `count` the number.
fn mdfind(dir: &Path, pattern: &str, count: bool) -> Result<String, String> {
    let mut cmd = aterm_uds::spawnfd::command("/usr/bin/mdfind");
    cmd.arg("-onlyin").arg(dir);
    cmd.arg(if count { "-count" } else { "-0" });
    cmd.arg(format!("kMDItemFSName == \"{pattern}\""));
    let out = aterm_uds::deadline::output_until(&mut cmd, Instant::now() + QUERY)
        .map_err(|e| format!("mdfind did not answer: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "mdfind failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The folder a crate file's build lives in, as a person would delete or rename it: a
/// toolchain's sysroot (the path before `/lib/rustlib/`), else the first `target` folder
/// on the path (a checkout's, with every lane inside it), else the first `build` folder
/// (another repository's, as Trust's bootstrap writes), else the folder above the
/// profile that holds `deps/`, else the file's own folder.
pub(crate) fn build_root(path: &str) -> String {
    if let Some((sysroot, _)) = path.split_once("/lib/rustlib/") {
        return sysroot.to_owned();
    }
    let parts: Vec<&str> = path.split('/').collect();
    for name in ["target", "build"] {
        if let Some(i) = parts[..parts.len().saturating_sub(1)]
            .iter()
            .position(|c| *c == name)
        {
            return parts[..=i].join("/");
        }
    }
    if let Some(i) = parts.iter().rposition(|c| *c == "deps")
        && i >= 2
    {
        return parts[..i - 1].join("/");
    }
    parts[..parts.len().saturating_sub(1)].join("/")
}

/// `path` with the home written `~`.
fn tilde(home: &Path, path: &str) -> String {
    let home = home.to_string_lossy();
    match path.strip_prefix(home.as_ref()) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_owned(),
    }
}

/// The row's fact and whether it is clean, pure over the count.
pub(crate) fn row(measured: &Result<Spotlit, String>) -> (bool, String) {
    let lit = match measured {
        Err(why) => return (false, format!("not measured ({why})")),
        Ok(lit) => lit,
    };
    if lit.builds.is_empty() && lit.docs.is_none() {
        return (
            true,
            "Spotlight indexes no Rust build output or rustup docs under ~".into(),
        );
    }
    let mut lines = Vec::new();
    if !lit.builds.is_empty() {
        let total: usize = lit.builds.iter().map(|(_, n)| n).sum();
        let mut top: Vec<String> = lit
            .builds
            .iter()
            .take(3)
            .map(|(root, n)| format!("{root} {n}"))
            .collect();
        if lit.builds.len() > 3 {
            top.push(format!("{} more folders", lit.builds.len() - 3));
        }
        lines.push(format!(
            "Spotlight indexes {total} crate builds (.rmeta) under ~: {}",
            top.join(", ")
        ));
        lines.push(
            "written where the shims' rule did not place them (a direct targo, another \
             repository's build, an installed toolchain); a folder leaves the index when \
             it is deleted or named *.noindex"
                .into(),
        );
    }
    if let Some((home, n)) = &lit.docs {
        lines.push(format!(
            "{n} rustup doc pages indexed in {home}: `aterm init` removes rust-docs"
        ));
    }
    (false, lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A crate file is counted under the folder that would be deleted or renamed: a
    /// checkout's `target` (every lane inside it), another repository's `build`, a
    /// toolchain's sysroot, else the folder above its profile.
    #[test]
    fn a_crate_file_is_counted_under_the_folder_that_holds_its_build() {
        for (path, root) in [
            (
                "/U/a/Developer/fix/target/verify/tippy/debug/deps/libx-1.rmeta",
                "/U/a/Developer/fix/target",
            ),
            (
                "/U/a/Developer/fix/target/debug/deps/libx-1.rmeta",
                "/U/a/Developer/fix/target",
            ),
            (
                "/U/a/trust/build/aarch64-apple-darwin/stage1-rustc/aarch64-apple-darwin/release/deps/librustc-1.rmeta",
                "/U/a/trust/build",
            ),
            (
                "/U/a/Library/Application Support/aterm/pkg/store/trust/9192/lib/rustlib/aarch64-apple-darwin/lib/libstd-1.rmeta",
                "/U/a/Library/Application Support/aterm/pkg/store/trust/9192",
            ),
            ("/U/a/out/lane/debug/deps/libx-1.rmeta", "/U/a/out/lane"),
            ("/U/a/loose/libx.rmeta", "/U/a/loose"),
            // A crate named like a folder is not one.
            ("/U/a/w/target", "/U/a/w"),
        ] {
            assert_eq!(build_root(path), root, "{path}");
        }
        assert_eq!(tilde(Path::new("/U/a"), "/U/a/x"), "~/x");
        assert_eq!(tilde(Path::new("/U/a"), "/U/ab/x"), "/U/ab/x");
    }

    /// Nothing indexed is the one clean answer; build folders name the three largest and
    /// how many more, docs name `aterm init`, and an unmeasured count says why.
    #[test]
    fn the_row_names_what_spotlight_holds_and_the_remedy() {
        let clean = Spotlit {
            builds: vec![],
            docs: None,
        };
        assert_eq!(
            row(&Ok(clean)),
            (
                true,
                "Spotlight indexes no Rust build output or rustup docs under ~".into()
            )
        );
        let lit = Spotlit {
            builds: vec![
                ("$HOME/trust/build".into(), 7900),
                ("~/Developer/fix/target".into(), 254),
                ("~/x/target".into(), 9),
                ("~/y/target".into(), 2),
                ("~/z/target".into(), 1),
            ],
            docs: Some(("~/.rustup".into(), 84_921)),
        };
        let (clean, fact) = row(&Ok(lit));
        assert!(!clean);
        let lines: Vec<&str> = fact.lines().collect();
        assert_eq!(
            lines[0],
            "Spotlight indexes 8166 crate builds (.rmeta) under ~: $HOME/trust/build 7900, \
             ~/Developer/fix/target 254, ~/x/target 9, 2 more folders"
        );
        assert!(lines[1].contains("*.noindex"), "{fact}");
        assert_eq!(
            lines[2],
            "84921 rustup doc pages indexed in ~/.rustup: `aterm init` removes rust-docs"
        );
        let docs_only = Spotlit {
            builds: vec![],
            docs: Some(("~/.rustup".into(), 3)),
        };
        assert_eq!(row(&Ok(docs_only)).1.lines().count(), 1);
        assert_eq!(
            row(&Err("a test binary".into())),
            (false, "not measured (a test binary)".into())
        );
    }
}
