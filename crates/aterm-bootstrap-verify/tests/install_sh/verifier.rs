// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Static verifier test harness against the [`crate::chain`] corpus.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use aterm_bootstrap_verify::{Ask, Policy, REFUSED, TEST_MASTER_PUBKEYS};

use crate::chain::{self, Answer, Case, Keys, Laid, PolicyAnswer, PolicyView};
use crate::support::{Lib, Rec, Scratch, q, s};

/// The verifier under test: the executable and its SHA-256, and what each batch adds to
/// its environment (a negative control's stand-in reads it).
pub struct Built {
    pub exe: PathBuf,
    pub sha: String,
    pub env: Vec<(String, String)>,
}

/// The verifier the suite runs: `$ATERM_TEST_BOOTSTRAP_VERIFIER` when the Linux guest
/// lane names its static build (debug assertions on, so the test anchor is read), else
/// this build's own, in place.
pub fn under_test() -> PathBuf {
    std::env::var_os("ATERM_TEST_BOOTSTRAP_VERIFIER").map_or_else(
        || PathBuf::from(env!("CARGO_BIN_EXE_aterm-bootstrap-verify")),
        PathBuf::from,
    )
}

impl Built {
    /// The verifier under test ([`under_test`]).
    pub fn new() -> Self {
        Self::at(under_test())
    }

    /// The executable at `exe`, held to its own SHA-256.
    pub fn at(exe: PathBuf) -> Self {
        let bytes = std::fs::read(&exe).unwrap_or_else(|e| panic!("{}: {e}", exe.display()));
        Self {
            sha: chain::sha256(&bytes),
            exe,
            env: Vec::new(),
        }
    }

    /// A batch over install.sh whose verifier is ready: `linux_verifier_ready` took the
    /// seam's executable, and its refusal (none expected) ends the batch saying why.
    pub fn ready(&self) -> Lib {
        let mut lib = Lib::new();
        lib.env("ATERM_INSTALL_VERIFIER", s(&self.exe))
            .env("ATERM_INSTALL_VERIFIER_SHA256", &self.sha);
        for (key, value) in &self.env {
            lib.env(key, value);
        }
        lib.step("linux_verifier_ready x86_64 || { echo \"not ready: $LINUX_VERIFIER_WHY\" >&2; exit 1; }");
        lib
    }

    fn ask(&self, scratch: &Scratch, lines: &[(String, String)]) -> Vec<Answer> {
        let chunks: Vec<&[(String, String)]> = lines.chunks(32).collect();
        let answers = Mutex::new(vec![Vec::new(); chunks.len()]);
        std::thread::scope(|scope| {
            for (n, chunk) in chunks.iter().enumerate() {
                let answers = &answers;
                scope.spawn(move || {
                    let mut lib = self.ready();
                    let calls: Vec<usize> = chunk
                        .iter()
                        .map(|(pins, args)| {
                            lib.call(
                                format!(
                                    "{TEST_MASTER_PUBKEYS}={} linux_release_verify {args}",
                                    q(pins)
                                ),
                                &[],
                            )
                        })
                        .collect();
                    let recs = lib.run(scratch);
                    answers.lock().expect("answers")[n] =
                        calls.iter().map(|call| answer(&recs[*call])).collect();
                });
            }
        });
        answers.into_inner().expect("answers").concat()
    }
}

/// A run's answer as the embedded verifier's harness spells it: `ok\t<line>` for exit 0
/// with nothing on stderr, `refused\t<why>` for exit 1 with one refusal line and nothing
/// on stdout, and `crashed\t…` for anything else.
pub fn answer(rec: &Rec) -> Answer {
    let refusal = rec
        .err
        .strip_prefix(REFUSED)
        .filter(|why| !why.contains('\n'));
    match (rec.rc, refusal) {
        (0, _) if rec.err.is_empty() => format!("ok\t{}", rec.out),
        (1, Some(why)) if rec.out.is_empty() => format!("refused\t{why}"),
        _ => format!(
            "crashed\texit {}, stdout {:?}, stderr {:?}",
            rec.rc, rec.out, rec.err
        ),
    }
}

fn verdict<T>(said: aterm_bootstrap_verify::Verdict<T>) -> Answer {
    match said {
        Ok(_) => "ok\t".into(),
        Err(why) => format!("refused\t{why}"),
    }
}

fn main_args(case: &Case, laid: &Laid) -> String {
    [
        q(s(&laid.release)),
        q(&case.tag),
        q(&case.slug),
        q(&case.arch),
        q(s(&laid.store)),
        q(s(&laid.candidate)),
    ]
    .join(" ")
}

impl chain::Verifier for Built {
    fn mains(&self, scratch: &Scratch, runs: &[(&Case, &Laid)]) -> Vec<Answer> {
        let lines: Vec<(String, String)> = runs
            .iter()
            .map(|(case, laid)| (case.pins.join(" "), main_args(case, laid)))
            .collect();
        self.ask(scratch, &lines)
    }

    fn policies(&self, _scratch: &Scratch, stores: &[&Path]) -> Vec<PolicyAnswer> {
        stores
            .iter()
            .map(|store| {
                aterm_bootstrap_verify::read_install_policy(store.as_os_str())
                    .map(|policy| PolicyView {
                        floors: policy.floors,
                        revoked_machines: policy.revoked_machines,
                        rejected_build: policy.rejected_build,
                    })
                    .map_err(|why| format!("refused\t{why}"))
            })
            .collect()
    }

    fn preflights(&self, scratch: &Scratch, stores: &[&Path]) -> Vec<Answer> {
        let lines: Vec<(String, String)> = stores
            .iter()
            .map(|store| (String::new(), format!("--store-path {}", q(s(store)))))
            .collect();
        self.ask(scratch, &lines)
    }

    fn manifests_at(&self, _scratch: &Scratch, asks: &[(&Case, &Laid, i64)]) -> Vec<Answer> {
        asks.iter()
            .map(|(case, laid, now)| {
                verdict(aterm_bootstrap_verify::verify_manifest(
                    &Ask {
                        directory: laid.release.as_os_str(),
                        tag: &case.tag,
                        slug: &case.slug,
                        arch: Some(&case.arch),
                        manifest_name: "aterm-appcast.toml",
                        now: *now,
                        pins: &case.pins,
                    },
                    &Policy::default(),
                ))
            })
            .collect()
    }

    fn walks(&self, _scratch: &Scratch, dirs: &[&Path]) -> Vec<Answer> {
        dirs.iter()
            .map(|dir| verdict(aterm_bootstrap_verify::check_install_path(dir.as_os_str())))
            .collect()
    }

    fn private_groups(&self, _scratch: &Scratch, gids: &[u32]) -> Vec<bool> {
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        gids.iter()
            .map(|gid| aterm_bootstrap_verify::private_group(uid, *gid))
            .collect()
    }

    fn lib(&self, pins: &[String]) -> Lib {
        let mut lib = self.ready();
        lib.env(TEST_MASTER_PUBKEYS, &pins.join(" "));
        lib
    }
}

#[test]
fn the_static_verifier_agrees_with_the_updater_on_a_generated_corpus() {
    let scratch = Scratch::new();
    chain::agrees_with_the_updater_on_a_generated_corpus(&scratch, &Built::new());
}

#[test]
fn the_differential_catches_a_verifier_that_drops_the_durable_policy() {
    let scratch = Scratch::new();
    let empty = scratch.mkdir("empty-store");
    let stand_in = scratch.exe(
        "forgetful/aterm-bootstrap-verify",
        "#!/bin/sh\n[ \"$#\" -ge 5 ] && [ \"$1\" != --store-path ] || exec \"$STUB_VERIFIER\" \"$@\"\n\
         a=$1 b=$2 c=$3 d=$4\nshift 5\nexec \"$STUB_VERIFIER\" \"$a\" \"$b\" \"$c\" \"$d\" \"$STUB_EMPTY_STORE\" \"$@\"\n",
    );
    let mut forgetful = Built::at(stand_in);
    forgetful.env = vec![
        ("STUB_VERIFIER".into(), s(&under_test()).into()),
        ("STUB_EMPTY_STORE".into(), s(&empty).into()),
    ];
    let wrong = chain::differential(&scratch, &forgetful).wrong;
    assert!(
        wrong
            .iter()
            .any(|w| w.starts_with("signer durably revoked:")),
        "{wrong:?}"
    );
}

#[test]
fn the_differential_catches_a_verifier_that_admits_everything() {
    let scratch = Scratch::new();
    let stand_in = scratch.exe(
        "lenient/aterm-bootstrap-verify",
        "#!/bin/sh\nprintf 'aterm-0.100.0-linux-x86_64\\t%064d\\t80\\t1\\t1\\n' 0\n",
    );
    let tally = chain::differential(&scratch, &Built::at(stand_in));
    assert!(tally.wrong.len() >= 50, "{} caught", tally.wrong.len());
    assert!(
        tally.wrong.iter().any(|w| w.contains("updater Err")),
        "{:?}",
        tally.wrong
    );
}

#[test]
fn the_static_verifier_trusts_the_updaters_paper_master() {
    assert_eq!(
        aterm_bootstrap_verify::MASTER_PUBKEYS,
        aterm_update_core::pins::PAPER_MASTER_PUBKEYS
    );
    let scratch = Scratch::new();
    let keys = Keys::new();
    let case = Case::base(&keys);
    let laid = case.lay(&keys, &scratch.mkdir("unanchored"));
    let mut lib = Built::new().ready();
    let call = lib.call(
        format!("linux_release_verify {}", main_args(&case, &laid)),
        &[],
    );
    let rec = &lib.run(&scratch)[call];
    assert_eq!(
        answer(rec),
        "refused\troster is not signed by the pinned paper master"
    );
    assert_eq!(
        aterm_bootstrap_verify::verify_binary(
            laid.candidate.as_os_str(),
            "arm64",
            &chain::sha256(&case.binary),
            case.binary.len() as u64,
        ),
        Err(aterm_bootstrap_verify::Refusal::new(
            "unsupported Linux architecture"
        ))
    );
}

#[test]
fn a_verdict_is_the_scripts_exit_status_and_its_one_line() {
    let scratch = Scratch::new();
    chain::a_verdict_is_the_scripts_exit_status_and_its_one_line(&scratch, &Built::new());
}

#[test]
fn a_symlinked_candidate_is_refused() {
    let scratch = Scratch::new();
    chain::a_symlinked_candidate_is_refused(&scratch, &Built::new());
}

#[test]
fn a_misspelt_machine_key_is_refused_in_its_own_words() {
    let scratch = Scratch::new();
    chain::a_misspelt_machine_key_is_refused_in_its_own_words(&scratch, &Built::new());
}

#[test]
fn the_durable_policy_binds_the_installed_file() {
    let scratch = Scratch::new();
    chain::the_durable_policy_binds_the_installed_file(&scratch, &Built::new());
}

#[test]
fn a_private_ancestor_shields_writable_children_but_not_earlier_ancestors() {
    let scratch = Scratch::new();
    chain::a_private_ancestor_shields_writable_children_but_not_earlier_ancestors(
        &scratch,
        &Built::new(),
    );
}

#[test]
fn what_the_installer_makes_its_verifier_admits_under_any_umask() {
    let scratch = Scratch::new();
    chain::what_the_installer_makes_its_verifier_admits_under_any_umask(&scratch, &Built::new());
}

#[test]
fn a_disabled_fresh_install_keeps_its_policy_on_retry() {
    let scratch = Scratch::new();
    chain::a_disabled_fresh_install_keeps_its_policy_on_retry(&scratch, &Built::new());
}

#[test]
fn a_revoked_machines_floor_is_withdrawn() {
    let scratch = Scratch::new();
    chain::a_revoked_machines_floor_is_withdrawn(&scratch, &Built::new());
}

#[test]
fn a_deadline_refuses_from_its_own_second() {
    let scratch = Scratch::new();
    chain::a_deadline_refuses_from_its_own_second(&scratch, &Built::new());
}

#[test]
fn an_older_release_is_judged_under_the_current_signed_policy() {
    let scratch = Scratch::new();
    chain::an_older_release_is_judged_under_the_current_signed_policy(&scratch, &Built::new());
}

#[cfg(target_os = "linux")]
#[test]
fn the_install_path_walk_is_the_updaters_on_this_machine() {
    let scratch = Scratch::new();
    chain::the_install_path_walk_is_the_updaters_on_this_machine(&scratch, &Built::new());
}

#[test]
fn the_user_private_group_rule_is_the_updaters() {
    use aterm_bootstrap_verify::{Account, Group, user_private_group};
    use aterm_uds::shared_write as updater;
    let accounts = [
        None,
        Some(("ann", 1000)),
        Some(("ann", 1001)),
        Some(("root", 0)),
    ];
    let groups: [Option<(&str, &[&str])>; 6] = [
        None,
        Some(("ann", &[])),
        Some(("ann", &["ann"])),
        Some(("ann", &["ann", "bob"])),
        Some(("bob", &[])),
        Some(("root", &[])),
    ];
    let primaries: [&[u32]; 4] = [&[], &[1000], &[1000, 1001], &[0]];
    let mut admitted = 0;
    for uid in [0, 1000] {
        for gid in [0, 1000, 1001] {
            for account in &accounts {
                for group in &groups {
                    for others in &primaries {
                        let ours = user_private_group(
                            uid,
                            gid,
                            account
                                .map(|(name, gid)| Account {
                                    name: name.into(),
                                    uid,
                                    gid,
                                })
                                .as_ref(),
                            group
                                .map(|(name, members)| Group {
                                    name: name.into(),
                                    gid,
                                    members: members
                                        .iter()
                                        .map(|m| m.as_bytes().to_vec())
                                        .collect(),
                                })
                                .as_ref(),
                            others,
                        );
                        let theirs = updater::user_private_group(
                            uid,
                            gid,
                            account
                                .map(|(name, gid)| updater::Account {
                                    name: name.into(),
                                    gid,
                                })
                                .as_ref(),
                            group
                                .map(|(name, members)| updater::Group {
                                    name: name.into(),
                                    members: members
                                        .iter()
                                        .map(|m| m.as_bytes().to_vec())
                                        .collect(),
                                })
                                .as_ref(),
                            others,
                        );
                        admitted += usize::from(ours);
                        assert_eq!(
                            ours, theirs,
                            "uid {uid}, gid {gid}, {account:?}, {group:?}, primaries {others:?}"
                        );
                    }
                }
            }
        }
    }
    assert!(admitted > 0, "the corpus holds a private group");
}

/// The accounts the static verifier reads from the files are the updater's enumeration on
/// this machine's own groups, and a private group is private to both. Where the files are
/// not the whole answer (another NSS source), the reader admits no group, which only ever
/// refuses more.
#[cfg(target_os = "linux")]
#[test]
fn the_accounts_are_the_updaters_on_this_machine() {
    use aterm_bootstrap_verify::{Accounts, private_group};
    use aterm_uds::shared_write as updater;
    // SAFETY: getuid and getgid have no preconditions and cannot fail.
    let (uid, primary) = unsafe { (libc::getuid(), libc::getgid()) };
    // SAFETY: a zero count asks only for the number of supplementary groups.
    let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    let mut gids = vec![0; usize::try_from(count).expect("a group count")];
    // SAFETY: `gids` has room for exactly `count` gids.
    let got = unsafe { libc::getgroups(count, gids.as_mut_ptr()) };
    gids.truncate(usize::try_from(got).expect("the groups"));
    gids.insert(0, primary);
    gids.dedup();
    let Some(accounts) = Accounts::read_under(Path::new("/")) else {
        for gid in gids.iter().chain(&[0, 65534]) {
            assert!(
                !private_group(uid, *gid),
                "gid {gid}: an unfinished enumeration admits none"
            );
        }
        return;
    };
    for gid in &gids {
        assert_eq!(
            Some(accounts.primary_members(*gid)),
            updater::primary_members(*gid),
            "gid {gid}"
        );
    }
    for gid in gids.iter().chain(&[0, 65534]) {
        assert_eq!(
            private_group(uid, *gid),
            updater::is_user_private_group(*gid),
            "gid {gid}"
        );
    }
}
