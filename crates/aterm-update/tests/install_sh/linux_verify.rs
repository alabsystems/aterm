// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `install.sh`'s embedded Linux verifier (`linux_release_verify_python`) held to the updater.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::chain::{self, Answer, Case, Laid, PolicyAnswer, PolicyView};
use crate::support::{Lib, Scratch, chmod, install_sh, q, run, s, symlink, system_path};

/// Python harness that drives the verifier piped to `python3 -`.
const HARNESS: &str = r#"import contextlib, errno, io, json, os, sys, types
source = sys.stdin.read()
marker = "\nMASTER_PUBKEYS = "
if source.count(marker) != 1:
    sys.exit("harness: the verifier must define MASTER_PUBKEYS exactly once")
mode = os.environ["STUB_MODE"]
if mode == "run":
    head, _, tail = source.partition(marker)
    pins = tuple(os.environ["STUB_MASTER_PUBKEYS"].split())
    sys.argv = sys.argv[1:]
    code = compile(head + marker + repr(pins) + "\n" + tail.partition("\n")[2], "linux_release_verify", "exec")
    exec(code, {"__name__": "__main__"})
    sys.exit(0)
verifier = {"__name__": "linux_release_verify"}
exec(compile(source, "linux_release_verify", "exec"), verifier)
if mode == "pins":
    print("\n".join(verifier["MASTER_PUBKEYS"]))
    sys.exit(0)
refusals = (ValueError, OSError, verifier["subprocess"].SubprocessError)
if os.environ.get("STUB_SIGNATURES") == "once":
    # The verifier's own check, asked once per key, bytes and signature: its answer is
    # OpenSSL's on exactly those, so a batch that repeats a proof spawns no second one.
    checked, check = {}, verifier["verify_ed25519"]
    def verify_once(key, data, signature):
        if (key, data, signature) not in checked:
            checked[key, data, signature] = check(key, data, signature)
        return checked[key, data, signature]
    verifier["verify_ed25519"] = verify_once
def stand_in(spec):
    target, _, value = spec.partition("=")
    value = json.loads(value)
    if target != "os.getxattr":
        return target, lambda *args, **kwargs: value
    def getxattr(path, attribute, *, follow_symlinks=True):
        answer = value.get(attribute, errno.ENODATA)
        if answer is None:
            return b""
        raise OSError(answer, os.strerror(answer), path)
    return "os", types.SimpleNamespace(**{**vars(os), "getxattr": getxattr})
with open(os.environ["STUB_CALLS"], encoding="utf-8") as calls:
    for line in calls:
        name, pins, *args = line.rstrip("\n").split("\t")
        name, _, spec = name.partition("|")
        if name.startswith("json:"):
            name, args = name[5:], [json.loads(arg) for arg in args]
        verifier["MASTER_PUBKEYS"] = tuple(pins.split())
        target, replacement = stand_in(spec) if spec else (None, None)
        kept = verifier.get(target)
        if target:
            verifier[target] = replacement
        out = io.StringIO()
        try:
            with contextlib.redirect_stdout(out):
                result = verifier[name](args) if name == "main" else verifier[name](*args)
            answer = out.getvalue().strip() if name == "main" else json.dumps(result, sort_keys=True)
            print("ok\t" + answer, flush=True)
        except refusals as error:
            print("refused\t" + str(error).replace("\n", " "), flush=True)
        except Exception as error:
            print("crashed\t" + type(error).__name__ + ": " + str(error).replace("\n", " "), flush=True)
        finally:
            if target:
                verifier[target] = kept
"#;

/// Python 3.11+ and OpenSSL 3 binaries for the verifier.
struct Tools {
    python: PathBuf,
    openssl: PathBuf,
}

/// Candidate paths for the verifier's Python and OpenSSL tools.
#[derive(Default)]
struct Search {
    python: Vec<PathBuf>,
    openssl: Vec<PathBuf>,
}

impl Search {
    /// This host's, from the `python3` and `openssl3` rows of `TOOLS`.
    fn host() -> Self {
        Self {
            python: candidates(
                "python3",
                &[
                    "python3.14",
                    "python3.13",
                    "python3.12",
                    "python3.11",
                    "python3",
                ],
            ),
            openssl: candidates("openssl3", &["openssl"]),
        }
    }
}

/// Candidate paths for `row` (`$ATERM_TEST_<ROW>` if set, else default paths and `$PATH`).
fn candidates(row: &str, names: &[&str]) -> Vec<PathBuf> {
    let row = aterm_uds::host::tool(row);
    if let Some(named) = std::env::var_os(row.var()) {
        return vec![PathBuf::from(named)];
    }
    let mut found: Vec<PathBuf> = row.at.iter().map(PathBuf::from).collect();
    found.extend(names.iter().filter_map(|name| crate::support::which(name)));
    found
}

/// First candidate in `search` meeting the floor, or the missing `TOOLS` rows.
fn tools_in(search: &Search) -> Result<Tools, Vec<&'static str>> {
    let python = search.python.iter().find(|python| {
        python.is_file()
            && run(aterm_uds::spawnfd::command(python).args([
                "-I",
                "-c",
                "import tomllib, hashlib; hashlib.file_digest",
            ]))
            .status
                == Some(0)
    });
    let openssl = search.openssl.iter().find(|exe| {
        exe.is_file() && {
            let out = run(aterm_uds::spawnfd::command(exe).arg("version")).stdout;
            out.strip_prefix("OpenSSL ")
                .and_then(|v| v.split('.').next())
                .and_then(|major| major.parse::<u32>().ok())
                .is_some_and(|major| major >= 3)
        }
    });
    match (python, openssl) {
        (Some(python), Some(openssl)) => Ok(Tools {
            python: python.clone(),
            openssl: openssl.clone(),
        }),
        (python, openssl) => Err([("python3", python), ("openssl3", openssl)]
            .into_iter()
            .filter(|(_, found)| found.is_none())
            .map(|(row, _)| row)
            .collect()),
    }
}

/// Embedded verifier harness bound to a script and toolset.
struct Embedded {
    bin: PathBuf,
    python: PathBuf,
    script: PathBuf,
}

impl Embedded {
    /// `install.sh`'s verifier under `scratch`, over this host's tools.
    fn new(scratch: &Scratch) -> Self {
        Self::of(scratch, &install_sh())
    }

    /// `script`'s verifier under `scratch`, over this host's tools.
    fn of(scratch: &Scratch, script: &Path) -> Self {
        Self::with(scratch, &Search::host(), script)
    }

    /// `script`'s verifier under `scratch`, ending `COULD NOT RUN` if tools are absent.
    fn with(scratch: &Scratch, search: &Search, script: &Path) -> Self {
        let tools = tools_in(search).unwrap_or_else(|absent| {
            aterm_uds::host::could_not_run_without(&absent);
            unreachable!("an absent tool ends COULD NOT RUN")
        });
        let bin = scratch.mkdir("verifier-bin");
        scratch.stand_in(
            "verifier-bin/python3",
            "#!/bin/sh\n[ \"$1\" = -I ] || { echo 'install.sh ran python3 without -I' >&2; exit 97; }\nshift\nexec \"$STUB_PYTHON\" -I \"$STUB_HARNESS\" \"$@\"\n",
        );
        if !bin.join("openssl").exists() {
            symlink(&tools.openssl, &bin.join("openssl"));
        }
        scratch.write("verifier-bin/harness.py", HARNESS);
        Self {
            bin,
            python: tools.python,
            script: script.to_path_buf(),
        }
    }

    /// A batch over the script in which `linux_release_verify` reaches the harness.
    fn batch(&self) -> Lib {
        let mut lib = Lib::of(&self.script);
        lib.env("PATH", &format!("{}:{}", s(&self.bin), system_path()))
            .env("STUB_PYTHON", s(&self.python))
            .env("STUB_HARNESS", s(&self.bin.join("harness.py")));
        lib
    }
}

static CALLS: AtomicU32 = AtomicU32::new(0);

/// Run `calls` against `verifier` across worker threads.
fn ask_verifier(scratch: &Scratch, verifier: &Embedded, calls: &[String]) -> Vec<String> {
    ask_verifier_with(scratch, verifier, calls, &[])
}

/// [`ask_verifier`] with `env` set in each batch's environment.
fn ask_verifier_with(
    scratch: &Scratch,
    verifier: &Embedded,
    calls: &[String],
    env: &[(&str, &str)],
) -> Vec<String> {
    let code = "linux_release_verify";
    let chunks: Vec<&[String]> = calls.chunks(32).collect();
    let answers = std::sync::Mutex::new(vec![Vec::new(); chunks.len()]);
    std::thread::scope(|scope| {
        for (n, chunk) in chunks.iter().enumerate() {
            let answers = &answers;
            scope.spawn(move || {
                let file = scratch.write(
                    &format!("calls/{}", CALLS.fetch_add(1, Ordering::Relaxed)),
                    chunk.join("\n") + "\n",
                );
                let mut lib = verifier.batch();
                lib.env("STUB_MODE", "calls").env("STUB_CALLS", s(&file));
                for (key, value) in env {
                    lib.env(key, value);
                }
                let call = lib.call(code, &[]);
                let recs = lib.run(scratch);
                let rec = &recs[call];
                assert_eq!(rec.rc, 0, "the harness ran: {}", rec.err);
                let lines: Vec<String> = rec.out.lines().map(str::to_string).collect();
                assert_eq!(lines.len(), chunk.len(), "one answer per call: {}", rec.err);
                answers.lock().expect("answers")[n] = lines;
            });
        }
    });
    answers.into_inner().expect("answers").concat()
}

fn main_call(case: &Case, laid: &Laid) -> String {
    [
        "main".to_string(),
        case.pins.join(" "),
        s(&laid.release).into(),
        case.tag.clone(),
        case.slug.clone(),
        case.arch.clone(),
        s(&laid.store).into(),
        s(&laid.candidate).into(),
    ]
    .join("\t")
}

/// A JSON argument of a `json:` call.
fn json(value: &aterm_json::Value) -> String {
    aterm_json::to_string(value).expect("JSON")
}

impl chain::Verifier for Embedded {
    fn mains(&self, scratch: &Scratch, runs: &[(&Case, &Laid)]) -> Vec<Answer> {
        let calls: Vec<String> = runs
            .iter()
            .map(|(case, laid)| main_call(case, laid))
            .collect();
        ask_verifier_with(scratch, self, &calls, &[("STUB_SIGNATURES", "once")])
    }

    fn policies(&self, scratch: &Scratch, stores: &[&Path]) -> Vec<PolicyAnswer> {
        let calls: Vec<String> = stores
            .iter()
            .map(|store| format!("read_install_policy\t-\t{}", s(store)))
            .collect();
        ask_verifier(scratch, self, &calls)
            .into_iter()
            .map(|answer| match answer.split_once('\t') {
                Some(("ok", text)) => {
                    let policy: aterm_json::Value =
                        aterm_json::from_str(text).expect("the policy as JSON");
                    let floors: Vec<u64> = policy["floors"]
                        .as_array()
                        .expect("floors")
                        .iter()
                        .map(|f| f.as_u64().expect("a floor"))
                        .collect();
                    Ok(PolicyView {
                        floors: floors.try_into().expect("three floors"),
                        revoked_machines: policy["revoked_machines"]
                            .as_array()
                            .expect("revoked machines")
                            .iter()
                            .map(|m| m.as_str().expect("a machine").to_string())
                            .collect(),
                        rejected_build: policy["rejected_build"].as_u64().expect("a build"),
                    })
                }
                _ => Err(answer),
            })
            .collect()
    }

    fn preflights(&self, scratch: &Scratch, stores: &[&Path]) -> Vec<Answer> {
        let calls: Vec<String> = stores
            .iter()
            .map(|store| format!("main\t-\t--store-path\t{}", s(store)))
            .collect();
        ask_verifier(scratch, self, &calls)
    }

    fn manifests_at(&self, scratch: &Scratch, asks: &[(&Case, &Laid, i64)]) -> Vec<Answer> {
        let calls: Vec<String> = asks
            .iter()
            .map(|(case, laid, now)| {
                [
                    "json:verify_manifest".to_string(),
                    case.pins.join(" "),
                    json(&aterm_json::json!(s(&laid.release))),
                    json(&aterm_json::json!(case.tag)),
                    json(&aterm_json::json!(case.slug)),
                    json(&aterm_json::json!(case.arch)),
                    "[0, 0, 0]".into(),
                    now.to_string(),
                ]
                .join("\t")
            })
            .collect();
        ask_verifier(scratch, self, &calls)
    }

    fn walks(&self, scratch: &Scratch, dirs: &[&Path]) -> Vec<Answer> {
        let calls: Vec<String> = dirs
            .iter()
            .map(|dir| format!("check_install_path\t-\t{}", s(dir)))
            .collect();
        ask_verifier(scratch, self, &calls)
    }

    fn private_groups(&self, scratch: &Scratch, gids: &[u32]) -> Vec<bool> {
        let calls: Vec<String> = gids
            .iter()
            .map(|gid| format!("json:private_group\t-\t{gid}"))
            .collect();
        ask_verifier(scratch, self, &calls)
            .into_iter()
            .map(|answer| match answer.as_str() {
                "ok\ttrue" => true,
                "ok\tfalse" => false,
                other => panic!("private_group answered {other:?}"),
            })
            .collect()
    }

    fn lib(&self, pins: &[String]) -> Lib {
        let mut lib = self.batch();
        lib.env("STUB_MODE", "run")
            .env("STUB_MASTER_PUBKEYS", &pins.join(" "));
        lib
    }
}

/// Without Python 3.11+ and OpenSSL 3, the verifier ends `COULD NOT RUN`.
#[test]
fn a_host_without_the_verifiers_tools_ends_could_not_run() {
    let scratch = Scratch::new();
    assert_eq!(
        tools_in(&Search::default()).err(),
        Some(vec!["python3", "openssl3"])
    );
    let old = Search {
        python: vec![scratch.stand_in("old/python3", "#!/bin/sh\nexit 1\n")],
        openssl: vec![scratch.stand_in("old/openssl", "#!/bin/sh\necho 'LibreSSL 3.3.6'\n")],
    };
    assert_eq!(
        tools_in(&old).err(),
        Some(vec!["python3", "openssl3"]),
        "a copy below the floor is none"
    );
    let words = aterm_uds::host::refusals_for(&["python3", "openssl3"]).expect("two absent");
    assert!(
        words.contains("no Python 3.11+ here: the Linux guest provides it")
            && words.contains("no OpenSSL 3 here: the Linux guest provides it")
            && words.contains("ATERM_TEST_PYTHON3=<path>")
            && !words.contains("brew"),
        "{words}"
    );
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Embedded::with(&scratch, &Search::default(), &install_sh())
    }));
    let carried = refused.as_ref().err().and_then(|payload| {
        payload
            .downcast_ref::<String>()
            .map(|words| words.contains(aterm_uds::host::COULD_NOT_RUN))
    });
    assert_eq!(
        carried,
        Some(true),
        "the verifier ends a host without them COULD NOT RUN"
    );
}

#[test]
fn the_embedded_verifier_agrees_with_the_updater_on_a_generated_corpus() {
    let scratch = Scratch::new();
    chain::agrees_with_the_updater_on_a_generated_corpus(&scratch, &Embedded::new(&scratch));
}

/// Negative control: dropping the durable revocation check fails the differential.
#[test]
fn the_verifier_differential_catches_a_dropped_durable_revocation() {
    let scratch = Scratch::new();
    let text = std::fs::read_to_string(install_sh()).expect("install.sh");
    let rule = r#"if machine["id"] in revoked or machine["id"] in revoked_machines:"#;
    assert_eq!(text.matches(rule).count(), 1, "the rule the control breaks");
    let broken = scratch.write(
        "install.sh",
        text.replace(rule, r#"if machine["id"] in revoked:"#),
    );
    let wrong = chain::differential(&scratch, &Embedded::of(&scratch, &broken)).wrong;
    assert!(
        wrong
            .iter()
            .any(|w| w.starts_with("signer durably revoked:")),
        "{wrong:?}"
    );
}

/// The installer trusts exactly the paper master the updater pins.
#[test]
fn the_embedded_verifier_trusts_the_updaters_paper_master() {
    let scratch = Scratch::new();
    let verifier = Embedded::new(&scratch);
    let mut lib = verifier.batch();
    lib.env("STUB_MODE", "pins");
    let call = lib.call("linux_release_verify", &[]);
    let recs = lib.run(&scratch);
    let pinned: Vec<&str> = recs[call].out.lines().collect();
    assert_eq!(
        pinned,
        aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
        "{}",
        recs[call].err
    );
}

#[test]
fn a_verdict_is_the_scripts_exit_status_and_its_one_line() {
    let scratch = Scratch::new();
    chain::a_verdict_is_the_scripts_exit_status_and_its_one_line(
        &scratch,
        &Embedded::new(&scratch),
    );
}

#[test]
fn a_symlinked_candidate_is_refused() {
    let scratch = Scratch::new();
    chain::a_symlinked_candidate_is_refused(&scratch, &Embedded::new(&scratch));
}

#[test]
fn a_misspelt_machine_key_is_refused_in_its_own_words() {
    let scratch = Scratch::new();
    chain::a_misspelt_machine_key_is_refused_in_its_own_words(&scratch, &Embedded::new(&scratch));
}

#[test]
fn the_durable_policy_binds_the_installed_file() {
    let scratch = Scratch::new();
    chain::the_durable_policy_binds_the_installed_file(&scratch, &Embedded::new(&scratch));
}

#[test]
fn a_private_ancestor_shields_writable_children_but_not_earlier_ancestors() {
    let scratch = Scratch::new();
    chain::a_private_ancestor_shields_writable_children_but_not_earlier_ancestors(
        &scratch,
        &Embedded::new(&scratch),
    );
}

#[test]
fn what_the_installer_makes_its_verifier_admits_under_any_umask() {
    let scratch = Scratch::new();
    chain::what_the_installer_makes_its_verifier_admits_under_any_umask(
        &scratch,
        &Embedded::new(&scratch),
    );
}

#[test]
fn a_disabled_fresh_install_keeps_its_policy_on_retry() {
    let scratch = Scratch::new();
    chain::a_disabled_fresh_install_keeps_its_policy_on_retry(&scratch, &Embedded::new(&scratch));
}

#[test]
fn a_revoked_machines_floor_is_withdrawn() {
    let scratch = Scratch::new();
    chain::a_revoked_machines_floor_is_withdrawn(&scratch, &Embedded::new(&scratch));
}

#[test]
fn a_deadline_refuses_from_its_own_second() {
    let scratch = Scratch::new();
    chain::a_deadline_refuses_from_its_own_second(&scratch, &Embedded::new(&scratch));
}

#[test]
fn an_older_release_is_judged_under_the_current_signed_policy() {
    let scratch = Scratch::new();
    chain::an_older_release_is_judged_under_the_current_signed_policy(
        &scratch,
        &Embedded::new(&scratch),
    );
}

#[test]
fn the_install_path_walk_is_the_updaters_on_this_machine() {
    let scratch = Scratch::new();
    chain::the_install_path_walk_is_the_updaters_on_this_machine(
        &scratch,
        &Embedded::new(&scratch),
    );
}

/// `user_private_group` agrees with the updater across account and group combinations.
#[test]
fn the_user_private_group_rule_is_the_updaters() {
    use aterm_update::linux::install_path::{Account, Group, user_private_group};
    let scratch = Scratch::new();
    let verifier = Embedded::new(&scratch);
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
    let mut cases = Vec::new();
    for uid in [0, 1000] {
        for gid in [0, 1000, 1001] {
            for account in &accounts {
                for group in &groups {
                    for others in &primaries {
                        cases.push((uid, gid, *account, *group, *others));
                    }
                }
            }
        }
    }
    let json = |v: &aterm_json::Value| aterm_json::to_string(v).expect("JSON");
    let calls: Vec<String> = cases
        .iter()
        .map(|(uid, gid, account, group, others)| {
            let account = account.map_or(aterm_json::Value::Null, |(name, gid)| {
                aterm_json::json!([name, gid])
            });
            let group = group.map_or(aterm_json::Value::Null, |(name, members)| {
                aterm_json::json!([name, members])
            });
            [
                "json:user_private_group".to_string(),
                "-".into(),
                uid.to_string(),
                gid.to_string(),
                json(&account),
                json(&group),
                json(&aterm_json::json!(others)),
            ]
            .join("\t")
        })
        .collect();
    let answers = ask_verifier(&scratch, &verifier, &calls);
    let mut admitted = 0;
    for ((uid, gid, account, group, others), answer) in cases.iter().zip(&answers) {
        let account = account.map(|(name, gid)| Account {
            name: name.into(),
            gid,
        });
        let group = group.map(|(name, members)| Group {
            name: name.into(),
            members: members.iter().map(|m| m.as_bytes().to_vec()).collect(),
        });
        let rust = user_private_group(*uid, *gid, account.as_ref(), group.as_ref(), others);
        admitted += usize::from(rust);
        assert_eq!(
            answer,
            &format!("ok\t{}", if rust { "true" } else { "false" }),
            "uid {uid}, gid {gid}, {account:?}, {group:?}, primaries {others:?}"
        );
    }
    assert!(admitted > 0, "the corpus holds a private group");
}
/// `access_acl` agrees with the updater's `acl_carried` across `getxattr` outcomes.
#[test]
fn the_acl_rule_is_the_updaters_on_a_stand_in_nfs4_file() {
    use aterm_update::linux::install_path::acl_carried;
    const POSIX: &str = "system.posix_acl_access";
    const NFS4: &str = "system.nfs4_acl";
    let (enodata, enotsup, eio) = (61, 95, 5);
    let scratch = Scratch::new();
    let verifier = Embedded::new(&scratch);
    let outcomes = [None, Some(enodata), Some(enotsup), Some(eio)];
    let pairs: Vec<(Option<i32>, Option<i32>)> = outcomes
        .iter()
        .flat_map(|posix| outcomes.iter().map(move |nfs4| (*posix, *nfs4)))
        .collect();
    let json = |v: &aterm_json::Value| aterm_json::to_string(v).expect("JSON");
    let calls: Vec<String> = pairs
        .iter()
        .map(|(posix, nfs4)| {
            let table = json(&aterm_json::json!({ POSIX: posix, NFS4: nfs4 }));
            format!("json:access_acl|os.getxattr={table}\t-\t\"/stand-in\"")
        })
        .collect();
    let answers = ask_verifier(&scratch, &verifier, &calls);
    for ((posix, nfs4), answer) in pairs.iter().zip(&answers) {
        let rust = acl_carried(|name| match name.to_bytes() {
            b"system.posix_acl_access" => *posix,
            b"system.nfs4_acl" => *nfs4,
            other => panic!("asked {other:?}"),
        });
        assert_eq!(
            answer,
            &format!("ok\t{rust}"),
            "posix {posix:?}, nfs4 {nfs4:?}"
        );
    }
    let carried = |posix, nfs4| {
        let at = pairs.iter().position(|pair| *pair == (posix, nfs4));
        answers[at.expect("in the corpus")] == "ok\ttrue"
    };
    assert!(carried(Some(enotsup), None), "an NFSv4 ACL");
    assert!(carried(Some(enodata), Some(eio)), "an unreadable NFSv4 ACL");
    assert!(!carried(Some(enodata), Some(enotsup)), "no ACL on ext4");
}

/// `remote_filesystem_type`, `filesystem_type`, and path checks agree with the updater.
#[test]
fn the_filesystem_rule_is_the_updaters() {
    use aterm_update::linux::install_path::{
        REMOTE_FILESYSTEMS, filesystem_type_at, is_user_private_group, remote_filesystem,
    };
    let scratch = Scratch::new();
    let verifier = Embedded::new(&scratch);
    let magics: Vec<u32> = REMOTE_FILESYSTEMS
        .iter()
        .copied()
        .chain([
            0xEF53,
            0x0102_1994,
            0x9123_683E,
            0x5846_5342,
            0x794C_7630,
            0x9FA0,
            0,
            u32::MAX,
        ])
        .collect();
    let root = crate::support::scratch_root();
    let paths = [
        PathBuf::from("/"),
        PathBuf::from("/proc"),
        root.to_path_buf(),
        scratch.mkdir("fs"),
        root.join("no-such-entry"),
    ];
    let json = |v: &aterm_json::Value| aterm_json::to_string(v).expect("JSON");
    let mut calls: Vec<String> = magics
        .iter()
        .map(|magic| format!("json:remote_filesystem_type\t-\t{magic}"))
        .collect();
    calls.extend(paths.iter().map(|path| {
        format!(
            "json:filesystem_type\t-\t{}",
            json(&aterm_json::json!(s(path)))
        )
    }));
    calls.push("json:remote_filesystem|libc=null\t-\t\"/\"".into());
    let open = aterm_tempfile::Builder::new()
        .prefix("install-sh-fs.")
        .tempdir_in(root)
        .expect("an unshielded dir");
    let open_root = open.path().canonicalize().expect("its physical path");
    chmod(&open_root, 0o755);
    let own = open_root.join("own");
    std::fs::create_dir(&own).expect("mkdir");
    chmod(&own, 0o775);
    let store = own.join("store");
    std::fs::create_dir(&store).expect("mkdir");
    chmod(&store, 0o755);
    for stand_in in ["", "|filesystem_type=26985", "|filesystem_type=null"] {
        calls.push(format!("check_install_path{stand_in}\t-\t{}", s(&store)));
    }
    let answers = ask_verifier(&scratch, &verifier, &calls);
    let (types, rest) = answers.split_at(magics.len());
    let (reads, rest) = rest.split_at(paths.len());
    let (unloaded, walks) = rest.split_at(1);
    for (magic, answer) in magics.iter().zip(types) {
        assert_eq!(
            answer,
            &format!("ok\t{}", remote_filesystem(*magic)),
            "{magic:#x}"
        );
    }
    for (path, answer) in paths.iter().zip(reads) {
        let rust = filesystem_type_at(path);
        assert_eq!(
            answer,
            &format!("ok\t{}", json(&aterm_json::json!(rust))),
            "{}",
            path.display()
        );
    }
    assert_eq!(filesystem_type_at(Path::new("/proc")), Some(0x9FA0));
    assert_eq!(unloaded, ["ok\ttrue"], "no C library: remote");
    // SAFETY: Getgid has no preconditions and cannot fail.
    let admitted = is_user_private_group(unsafe { libc::getgid() });
    assert_eq!(
        walks[0].starts_with("ok\t"),
        admitted,
        "on this filesystem: {}",
        walks[0]
    );
    for refused in &walks[1..] {
        assert!(refused.starts_with("refused\t"), "{refused}");
    }
}

/// `primary_members` agrees with the updater's passwd enumeration on this machine.
#[test]
fn the_passwd_enumeration_is_the_updaters_on_this_machine() {
    use aterm_update::linux::install_path::{is_user_private_group, primary_members};
    let scratch = Scratch::new();
    let verifier = Embedded::new(&scratch);
    // SAFETY: Getuid and getgid have no preconditions and cannot fail.
    let (uid, primary) = unsafe { (libc::getuid(), libc::getgid()) };
    let gids = [0, primary, 65534];
    let mut calls: Vec<String> = gids
        .iter()
        .map(|gid| format!("json:primary_members\t-\t{gid}"))
        .collect();
    calls.push("json:primary_members|libc=null\t-\t0".into());
    for stand_in in [
        "null".to_string(),
        format!("[{uid}, {}]", uid + 1),
        format!("[{uid}]"),
    ] {
        calls.push(format!(
            "json:private_group|primary_members={stand_in}\t-\t{primary}"
        ));
    }
    let answers = ask_verifier(&scratch, &verifier, &calls);
    let (lists, rest) = answers.split_at(gids.len());
    for (gid, answer) in gids.iter().zip(lists) {
        let python: aterm_json::Value = aterm_json::from_str(
            answer
                .strip_prefix("ok\t")
                .unwrap_or_else(|| panic!("gid {gid}: {answer}")),
        )
        .expect("JSON");
        assert_eq!(
            python,
            aterm_json::json!(primary_members(*gid)),
            "gid {gid}"
        );
    }
    assert_eq!(rest[0], "ok\tnull", "no C library: no enumeration");
    assert_eq!(rest[1], "ok\tfalse", "an enumeration that failed");
    assert_eq!(rest[2], "ok\tfalse", "another account's primary group");
    assert_eq!(
        rest[3],
        format!("ok\t{}", is_user_private_group(primary)),
        "this user's own enumeration"
    );
}
/// The verifier the installer pipes to `python3` imports nothing from the
/// directory the install runs in.
#[test]
fn the_verifier_imports_nothing_from_the_directory_the_install_runs_in() {
    let scratch = Scratch::new();
    let planted = scratch.mkdir("downloads");
    let mark = planted.join("planted-ran");
    scratch.write(
        "downloads/base64.py",
        format!("open({:?}, 'w').close()\n", s(&mark)),
    );
    let shipped = install_sh();
    let text = std::fs::read_to_string(&shipped).expect("read install.sh");
    let open = text.replace(
        "python3 -I - \"$@\" <<'ATERM_LINUX_VERIFY_PY'",
        "python3 - \"$@\" <<'ATERM_LINUX_VERIFY_PY'",
    );
    assert_ne!(open, text, "the fixture edit applies to install.sh");
    let unisolated = scratch.write("install-unisolated.sh", open);
    for (script, imports) in [(&shipped, false), (&unisolated, true)] {
        let _ = std::fs::remove_file(&mark);
        let mut lib = Lib::of(script);
        let call = lib.call(
            format!("cd {} && linux_release_verify", q(s(&planted))),
            &[],
        );
        let recs = lib.run(&scratch);
        assert_ne!(
            recs[call].rc, 0,
            "an argument-less call is refused: {}",
            recs[call].err
        );
        assert_eq!(
            mark.exists(),
            imports,
            "{}: the planted module {} imported: {}",
            script.display(),
            if imports { "must be" } else { "was" },
            recs[call].err
        );
    }
}
