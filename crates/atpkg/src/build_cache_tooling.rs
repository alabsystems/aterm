// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The tooling cache's share of the reclaim.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::{Planned, RECLAIM_IDLE, Unused};

const DEPTH: usize = 2;
/// What [`reclaim`] renames an entry `<name>` to before removing it: the stage names from
/// `.provision-reclaim-<name>` ([`super::stage_aside`]), each read as a stage by this prefix.
const STAGE: &str = ".provision-reclaim-";

/// The base of the stage names entry `name` takes: `.provision-reclaim-<name>`.
fn stage_base(name: &std::ffi::OsStr) -> std::ffi::OsString {
    let mut base = std::ffi::OsString::from(STAGE);
    base.push(name);
    base
}

/// The tooling cache under `home`, on macOS's rule (`aterm_uds::host::user_cache`):
/// `~/Library/Caches/aterm/tooling.noindex`.
#[must_use]
pub fn root(home: &Path) -> PathBuf {
    aterm_uds::host::user_cache("macos", Some(home), None, aterm_uds::host::TOOLING)
        .unwrap_or_else(|| home.join("Library/Caches/aterm/tooling.noindex"))
}

fn last_used(path: &Path, depth: usize) -> Option<SystemTime> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    let mut newest = meta.modified().ok()?;
    if meta.is_file()
        && let Ok(read) = meta.accessed()
    {
        newest = newest.max(read);
    }
    if meta.is_dir() && depth > 0 {
        for entry in std::fs::read_dir(path).into_iter().flatten().flatten() {
            if let Some(used) = last_used(&entry.path(), depth - 1) {
                newest = newest.max(used);
            }
        }
    }
    Some(newest)
}

// Scratch rows are reclaimed only when both EXEC_STAMP and RAN exist as regular files, since
// elapsed time or a reboot cannot prove an image ran.
fn why(entry: &Path, now: SystemTime) -> Option<Unused> {
    let name = entry.file_name()?.to_str()?;
    if !std::fs::symlink_metadata(entry).is_ok_and(|m| m.is_dir()) {
        return None;
    }
    if let Some(original) = name.strip_prefix(STAGE) {
        return (!original.starts_with(aterm_uds::host::TOOLING_SCRATCH))
            .then_some(Unused::Unfinished);
    }
    if name.starts_with(aterm_uds::host::TOOLING_SCRATCH) {
        let given_up = [aterm_uds::deadline::EXEC_STAMP, aterm_uds::deadline::RAN]
            .iter()
            .all(|stamp| std::fs::symlink_metadata(entry.join(stamp)).is_ok_and(|m| m.is_file()));
        return given_up.then_some(Unused::GivenUp);
    }
    let idle = now
        .duration_since(last_used(entry, DEPTH)?)
        .unwrap_or(Duration::ZERO);
    let after = RECLAIM_IDLE;
    (idle >= after).then_some(Unused::Tooling { idle, after })
}

fn entries(root: &Path) -> Vec<PathBuf> {
    if !std::fs::symlink_metadata(root).is_ok_and(|m| m.is_dir()) {
        return Vec::new();
    }
    let mut found: Vec<PathBuf> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect();
    found.sort();
    found
}

/// Whether [`reclaim`] removes `entry`, judged `why`, in place: a stage, or a given-up scratch.
/// Every other entry is staged first under a name of its own.
const fn in_place(why: Unused) -> bool {
    matches!(why, Unused::Unfinished | Unused::GivenUp)
}

/// Advisory tooling cleanup candidates at `now`. An entry that needs staging is listed only
/// while it can be staged ([`super::stage_for`]). [`reclaim`] checks active use before removal;
/// this read-only observation does not probe process ownership.
#[must_use]
pub fn plan(root: &Path, now: SystemTime) -> Vec<Planned> {
    entries(root)
        .into_iter()
        .filter_map(|entry| {
            let why = why(&entry, now)?;
            if !in_place(why) {
                super::stage_for(&entry, root, &stage_base(entry.file_name()?))?;
            }
            Some(Planned {
                path: entry.clone(),
                why,
                paths: vec![entry],
                held: false,
            })
        })
        .collect()
}

/// Remove what nobody uses from the tooling cache `root` at `now` ([`why`]), `record` asked first
/// for each (`false` keeps it); each removed whole ([`super::remove::tree`]) is returned with why,
/// and a stage some entry of which stayed with what stayed ([`Unused::Stayed`]), for the next pass.
/// Each stage is visited once a pass, as the one listing of `root` reads it, and an entry is
/// staged under the lowest free stage name from `.provision-reclaim-<name>`, never over another
/// entry: a stage that keeps an entry costs only itself, never the next entry of its name.
pub fn reclaim(
    root: &Path,
    now: SystemTime,
    record: &mut dyn FnMut(&Path, Unused) -> bool,
) -> Vec<(PathBuf, Unused)> {
    reclaim_on(root, now, record, &super::fence::Fence::default())
}

pub(super) fn reclaim_on(
    root: &Path,
    now: SystemTime,
    record: &mut dyn FnMut(&Path, Unused) -> bool,
    fence: &super::fence::Fence<'_>,
) -> Vec<(PathBuf, Unused)> {
    let mut removed = Vec::new();
    for entry in entries(root) {
        let Some(why) = why(&entry, now) else {
            continue;
        };
        let base = entry.file_name().map(stage_base);
        if !in_place(why)
            && base
                .as_ref()
                .is_none_or(|b| super::stage_for(&entry, root, b).is_none())
        {
            continue;
        }
        // An interrupted stage may still be reported by the executable's old spelling.
        let originals = if in_place(why) {
            originals_of_stage(root, &entry)
        } else {
            vec![entry.clone()]
        };
        let clear = |current: &Path| {
            let paths: Vec<&Path> = originals
                .iter()
                .map(PathBuf::as_path)
                .chain([current])
                .collect();
            fence.clear_all(&paths)
        };
        if !clear(&entry) || !record(&entry, why) || !clear(&entry) {
            continue;
        }
        let stage = if in_place(why) {
            entry.clone()
        } else {
            let Some(stage) = base.and_then(|b| super::stage_aside(&entry, root, &b).ok()) else {
                continue;
            };
            stage
        };
        if !clear(&stage) {
            continue;
        }
        let left = super::remove::tree(&stage);
        if left.whole() {
            removed.push((entry, why));
        } else {
            removed.push((
                stage,
                Unused::Stayed {
                    stayed: left.stayed,
                    freed: left.freed,
                },
            ));
        }
    }
    removed
}

/// The original tooling row a stage name records, including a later unique stage's suffix.
fn originals_of_stage(root: &Path, stage: &Path) -> Vec<PathBuf> {
    let Some(name) = stage
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix(STAGE))
    else {
        return vec![stage.to_path_buf()];
    };
    // `fish-3` may be the original row's name or the third stage of `fish`: both aliases
    // retain ownership when an old executable spelling cannot distinguish the two.
    let mut originals = vec![root.join(name)];
    if let Some((head, tail)) = name.rsplit_once('-')
        && !tail.is_empty()
        && tail.bytes().all(|byte| byte.is_ascii_digit())
    {
        originals.push(root.join(head));
    }
    originals
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    const DAY: Duration = Duration::from_secs(24 * 60 * 60);

    #[cfg(unix)]
    fn file(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"x").unwrap();
    }

    /// Set `path`'s access and modification times to `at`, without following a link.
    #[cfg(unix)]
    fn stamp(path: &Path, at: SystemTime) {
        let file = std::fs::File::open(path).unwrap();
        file.set_times(std::fs::FileTimes::new().set_accessed(at).set_modified(at))
            .unwrap();
    }

    /// Every file and folder of `dir`, to [`DEPTH`] below it, last touched at `at`.
    #[cfg(unix)]
    fn age_tree(dir: &Path, at: SystemTime) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            if entry.file_type().unwrap().is_dir() {
                age_tree(&entry.path(), at);
            }
            stamp(&entry.path(), at);
        }
        stamp(dir, at);
    }

    /// The idle rule over the tooling cache: a row unused a month goes, one a run used
    /// stays (an exec's access time is use), a completed row's stage is finished, and a
    /// link is never followed. Scratches with no exec stamp stay even when old or already
    /// staged: no age proves their copied image finished running. NEGATIVE CONTROL: the same completed
    /// row with one file's access time set yesterday (as an exec sets it) stays.
    #[cfg(unix)]
    #[test]
    fn a_tool_unused_a_month_goes_and_one_a_run_used_stays() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path().join("tooling.noindex");
        let now = SystemTime::now();
        let month = now - RECLAIM_IDLE - DAY;
        for row in ["pwsh-7.6.7", "fish-4.9.3", "binaryen-133"] {
            file(&root.join(row).join("bin/tool"));
            file(&root.join(row).join("stamp.json"));
            age_tree(&root.join(row), month);
        }
        let fish = root.join("fish-4.9.3/bin/tool");
        std::fs::File::open(&fish)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_accessed(now - DAY))
            .unwrap();
        file(&root.join(".provision-pwsh-abc/download/x.tar.gz"));
        age_tree(&root.join(".provision-pwsh-abc"), now - 2 * DAY);
        let unknown = [
            ".provision-fish-unknown",
            ".provision-reclaim-.provision-fish-staged-unknown",
        ];
        for scratch in unknown {
            file(&root.join(scratch).join("tree/bin/fish"));
            age_tree(&root.join(scratch), month);
        }
        file(&root.join(".provision-fish-new/src/Cargo.toml"));
        file(&root.join(".provision-reclaim-old/bin/tool"));
        let outside = dir.path().join("elsewhere");
        file(&outside.join("bin/tool"));
        age_tree(&outside, month);
        std::os::unix::fs::symlink(&outside, root.join("linked-1.0")).unwrap();

        let planned: Vec<(String, Unused)> = plan(&root, now)
            .into_iter()
            .map(|p| {
                (
                    p.path.file_name().unwrap().to_string_lossy().into_owned(),
                    p.why,
                )
            })
            .collect();
        let names: Vec<&str> = planned.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [".provision-reclaim-old", "binaryen-133", "pwsh-7.6.7"],
            "{planned:#?}"
        );
        assert!(planned.iter().any(|(n, w)| n == "pwsh-7.6.7"
            && matches!(w, Unused::Tooling { after, .. } if *after == RECLAIM_IDLE)));
        let held_tool = root.join("pwsh-7.6.7/bin/tool");
        let processes = |dirs: &[PathBuf], _: &[u32]| {
            let users = dirs
                .iter()
                .any(|dir| held_tool.starts_with(dir))
                .then(|| aterm_uds::holders::PathUse {
                    pid: 23,
                    how: "exe",
                    path: held_tool.clone(),
                })
                .into_iter()
                .collect();
            Ok(aterm_uds::holders::PathScan {
                users,
                ..aterm_uds::holders::PathScan::default()
            })
        };
        let mut asked_held = false;
        assert!(
            reclaim_on(
                &root,
                now,
                &mut |path, _| {
                    asked_held |= path.ends_with("pwsh-7.6.7");
                    false
                },
                &super::super::fence::Fence::new(&processes)
            )
            .is_empty()
        );
        assert!(
            !asked_held,
            "a running tool is kept before recording or staging"
        );
        assert!(held_tool.is_file());
        let mut asked = Vec::new();
        let removed = reclaim(&root, now, &mut |p, why| {
            asked.push((p.to_path_buf(), why));
            !p.ends_with("binaryen-133")
        });
        assert_eq!(asked.len(), 3, "{asked:#?}");
        assert!(
            asked.iter().all(|(path, _)| {
                !path.ends_with(".provision-pwsh-abc")
                    && unknown.iter().all(|scratch| !path.ends_with(scratch))
            }),
            "scratch retention precedes the intent callback: {asked:#?}"
        );
        let gone: Vec<&Path> = removed.iter().map(|(p, _)| p.as_path()).collect();
        assert_eq!(gone.len(), 2, "{removed:#?}");
        for kept in [
            "fish-4.9.3/bin/tool",
            "binaryen-133/bin/tool",
            ".provision-fish-new",
            ".provision-pwsh-abc/download/x.tar.gz",
        ] {
            assert!(root.join(kept).exists(), "{kept} stays");
        }
        for scratch in unknown {
            assert_eq!(
                std::fs::read(root.join(scratch).join("tree/bin/fish")).unwrap(),
                b"x",
                "the actual uncertain image stays at its original path"
            );
            assert!(
                !root.join(format!("{STAGE}{scratch}")).exists(),
                "an uncertain image is not renamed either"
            );
        }
        for went in ["pwsh-7.6.7", ".provision-reclaim-old"] {
            assert!(!root.join(went).exists(), "{went} goes");
        }
        assert!(
            !root.join(".provision-reclaim-pwsh-7.6.7").exists(),
            "the stage went with it"
        );
        assert!(
            outside.join("bin/tool").exists(),
            "a link is never followed"
        );
        assert!(root.join("linked-1.0").exists(), "nor removed");
    }

    /// Only an actual run proof admits a given-up scratch. Old, malformed and pre-reboot
    /// exec stamps without proof stay; so do live scratches and non-file/link markers.
    #[cfg(unix)]
    #[test]
    fn a_given_up_scratch_goes_only_after_its_run_is_proven() {
        use aterm_uds::deadline::{EXEC_STAMP, FIRST_EXEC_HANG, RAN};
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path().join("tooling.noindex");
        let now = SystemTime::now();
        let kept = [
            (".provision-fish-spent", Some("")),
            (".provision-fish-rebooted", Some("18446744073709551615")),
            (".provision-fish-fresh", Some("0")),
            (".provision-fish-live", None),
            (".provision-fish-proof-live", None),
            (".provision-fish-proof-dir", Some("0")),
            (".provision-fish-proof-link", Some("0")),
            (".provision-fish-stamp-link", None),
            (".provision-fish-vetoed", Some("0")),
        ];
        let proven = ".provision-fish-proven";
        for (name, stamp) in kept.into_iter().chain([(proven, Some("0"))]) {
            file(
                &root
                    .join(name)
                    .join("target.noindex/release/build/x/build-script-build"),
            );
            if let Some(at) = stamp {
                std::fs::write(root.join(name).join(EXEC_STAMP), at).unwrap();
            }
            age_tree(&root.join(name), now - RECLAIM_IDLE - DAY);
        }
        std::fs::write(
            root.join(".provision-fish-fresh").join(EXEC_STAMP),
            aterm_uds::process::awake_ms()
                .map(|ms| ms.to_string())
                .unwrap_or_default(),
        )
        .unwrap();
        for name in [
            proven,
            ".provision-fish-proof-live",
            ".provision-fish-stamp-link",
            ".provision-fish-vetoed",
        ] {
            std::fs::write(root.join(name).join(RAN), b"").unwrap();
        }
        std::fs::create_dir(root.join(".provision-fish-proof-dir").join(RAN)).unwrap();
        let outside = dir.path().join("outside-proof");
        std::fs::write(&outside, b"outside").unwrap();
        std::os::unix::fs::symlink(&outside, root.join(".provision-fish-proof-link").join(RAN))
            .unwrap();
        std::os::unix::fs::symlink(
            &outside,
            root.join(".provision-fish-stamp-link").join(EXEC_STAMP),
        )
        .unwrap();

        let names = |planned: Vec<Planned>| {
            planned
                .into_iter()
                .map(|p| {
                    assert_eq!(p.why, Unused::GivenUp);
                    p.path.file_name().unwrap().to_string_lossy().into_owned()
                })
                .collect::<Vec<_>>()
        };
        let expected = vec![proven, ".provision-fish-vetoed"];
        assert_eq!(names(plan(&root, now)), expected);
        assert_eq!(
            names(plan(&root, now + RECLAIM_IDLE + FIRST_EXEC_HANG)),
            expected,
            "even another month and 600 seconds cannot replace a run proof"
        );
        let mut asked = Vec::new();
        let removed = reclaim(&root, now, &mut |path, why| {
            assert_eq!(why, Unused::GivenUp);
            asked.push(path.file_name().unwrap().to_string_lossy().into_owned());
            !path.ends_with(".provision-fish-vetoed")
        });
        assert_eq!(asked, expected, "unproved images never reach the callback");
        let gone: Vec<String> = removed
            .iter()
            .map(|(p, why)| {
                assert_eq!(*why, Unused::GivenUp, "{}", p.display());
                p.file_name().unwrap().to_string_lossy().into_owned()
            })
            .collect();
        assert_eq!(gone, [proven], "{removed:#?}");
        assert!(!root.join(proven).exists());
        for (name, _) in kept {
            assert_eq!(
                std::fs::read(
                    root.join(name)
                        .join("target.noindex/release/build/x/build-script-build")
                )
                .unwrap(),
                b"x",
                "{name} stays at its original path"
            );
            assert!(!root.join(format!("{STAGE}{name}")).exists());
        }
        assert_eq!(std::fs::read(outside).unwrap(), b"outside");
    }

    /// A STUCK STAGE KEEPS NO TOOL OF ITS NAME: beside `.provision-reclaim-pwsh-7.6.7`, a stage an
    /// earlier pass left that keeps one entry (read as another user's: the remover leaves it as it
    /// is), the same tool unused a month again is staged under the next free name and goes whole
    /// in the same pass; the stuck stage is asked for and reported once, and the plan lists each
    /// once.
    #[cfg(unix)]
    #[test]
    fn a_stuck_stage_keeps_no_tool_of_its_name() {
        use std::os::unix::fs::MetadataExt as _;
        struct Clear;
        impl Drop for Clear {
            fn drop(&mut self) {
                crate::build_cache::remove::seams::clear();
            }
        }
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path().join("tooling.noindex");
        let now = SystemTime::now();
        let tool = root.join("pwsh-7.6.7");
        file(&tool.join("bin/tool"));
        age_tree(&tool, now - RECLAIM_IDLE - DAY);
        let stage = root.join(".provision-reclaim-pwsh-7.6.7");
        let held = stage.join("bin/held");
        file(&held);
        let _clear = Clear;
        crate::build_cache::remove::seams::other_user(
            std::fs::symlink_metadata(&held).unwrap().ino(),
        );

        let planned: Vec<PathBuf> = plan(&root, now).into_iter().map(|p| p.path).collect();
        assert_eq!(planned, [stage.clone(), tool.clone()]);
        let mut asked = Vec::new();
        let removed = reclaim(&root, now, &mut |p, why| {
            asked.push((p.to_path_buf(), why));
            true
        });
        assert_eq!(asked.len(), 2, "{asked:?}");
        assert_eq!(asked[0], (stage.clone(), Unused::Unfinished));
        assert_eq!(asked[1].0, tool);
        assert_eq!(removed.len(), 2, "each once: {removed:?}");
        assert_eq!(
            removed[0],
            (
                stage.clone(),
                Unused::Stayed {
                    stayed: 1,
                    freed: 0
                }
            )
        );
        assert!(
            removed[1].0 == tool && matches!(removed[1].1, Unused::Tooling { .. }),
            "{removed:?}"
        );
        assert!(!tool.exists() && !root.join(".provision-reclaim-pwsh-7.6.7-1").exists());
        assert_eq!(std::fs::read(&held).unwrap(), b"x");
    }

    /// A TOOL THAT CANNOT BE STAGED IS NEITHER PLANNED NOR ASKED FOR: a tool unused a month, in a
    /// tooling cache this user may add no name to (0o555), or (macOS) flagged `uchg` itself, which
    /// refuses its rename, is not listed by the plan, its intent is never asked, and it stays
    /// whole. NEGATIVE CONTROL: the root's write bit back and the flag off, the plan lists it and
    /// a pass takes it.
    #[cfg(unix)]
    #[test]
    fn a_tool_that_cannot_be_staged_is_neither_planned_nor_asked_for() {
        use std::os::unix::fs::PermissionsExt as _;
        /// Gives the root and the tool their owner's write bit back, and (macOS) the tool its
        /// flags off, when dropped, so a failed test leaves its scratch removable.
        struct Let(PathBuf, PathBuf);
        impl Drop for Let {
            fn drop(&mut self) {
                for dir in [&self.0, &self.1] {
                    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755));
                }
                #[cfg(target_os = "macos")]
                let _ = crate::platform::set_file_flags(&self.1, 0);
            }
        }
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path().join("tooling.noindex");
        let now = SystemTime::now();
        let tool = root.join("pwsh-7.6.7");
        file(&tool.join("bin/tool"));
        age_tree(&tool, now - RECLAIM_IDLE - DAY);
        let fixed = Let(root.clone(), tool.clone());
        let unstaged = |why: &str| {
            assert!(plan(&root, now).is_empty(), "{why}");
            let mut asked = Vec::new();
            let removed = reclaim(&root, now, &mut |p, why| {
                asked.push((p.to_path_buf(), why));
                true
            });
            assert!(
                asked.is_empty() && removed.is_empty(),
                "{why}: {asked:?} {removed:?}"
            );
            assert!(tool.join("bin/tool").is_file(), "{why}");
        };
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o555)).unwrap();
        unstaged("a root that takes no name");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        #[cfg(target_os = "macos")]
        {
            crate::platform::set_file_flags(&tool, libc::UF_IMMUTABLE).unwrap();
            assert!(std::fs::rename(&tool, root.join("moved")).is_err());
            unstaged("a flagged tool");
            crate::platform::set_file_flags(&tool, 0).unwrap();
        }

        let planned: Vec<PathBuf> = plan(&root, now).into_iter().map(|p| p.path).collect();
        assert_eq!(planned, std::slice::from_ref(&tool));
        let removed = reclaim(&root, now, &mut |_, _| true);
        assert!(
            removed.len() == 1 && removed[0].0 == tool && !tool.exists(),
            "{removed:?}"
        );
        drop(fixed);
    }

    #[test]
    fn the_root_is_the_resolvers_tooling_cache() {
        let home = Path::new("/Users//someone");
        assert_eq!(
            root(home),
            Path::new("/Users//someone/Library/Caches/aterm/tooling.noindex")
        );
        assert!(plan(&home.join("nowhere-at-all"), SystemTime::now()).is_empty());
    }
}
