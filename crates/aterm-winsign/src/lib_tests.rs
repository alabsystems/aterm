// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::RefCell;
use std::collections::BTreeMap;

/// One recorded tool launch: program, argv, extra env.
type Run = (PathBuf, Vec<String>, Vec<(String, String)>);

/// One path, spelled the way the Windows machine this fake models would
/// spell it: `/` and `\` are both separators, runs of them collapse, and a
/// trailing one is nothing.
fn win_path(path: &Path) -> String {
    let s = path.to_string_lossy().replace('/', "\\");
    let body = s.trim_start_matches('\\');
    let lead = "\\".repeat((s.len() - body.len()).min(2));
    let segments: Vec<&str> = body.split('\\').filter(|s| !s.is_empty()).collect();
    format!("{lead}{}", segments.join("\\"))
}

/// `win_path` on both sides of an `assert_eq!`, so a mismatch prints the two Windows spellings rather than two host-mangled ones.
macro_rules! assert_same_path {
    ($got:expr, $want:expr $(,)?) => {
        assert_eq!(win_path($got.as_ref()), win_path($want.as_ref()))
    };
}

struct Fake {
    env: BTreeMap<String, String>,
    files: Vec<PathBuf>,
    dirs: BTreeMap<PathBuf, Vec<String>>,
    written: RefCell<Vec<(PathBuf, String)>>,
    removed: RefCell<Vec<PathBuf>>,
    runs: RefCell<Vec<Run>>,
    reply: Ran,
}

impl Fake {
    fn new() -> Self {
        Fake {
            env: BTreeMap::new(),
            files: Vec::new(),
            dirs: BTreeMap::new(),
            written: RefCell::new(Vec::new()),
            removed: RefCell::new(Vec::new()),
            runs: RefCell::new(Vec::new()),
            reply: Ran::default(),
        }
    }
}

impl Host for Fake {
    fn env(&self, key: &str) -> Option<String> {
        self.env.get(key).cloned()
    }
    fn is_file(&self, path: &Path) -> bool {
        let want = win_path(path);
        self.files.iter().any(|f| win_path(f) == want)
    }
    fn list_dir(&self, path: &Path) -> Vec<String> {
        let want = win_path(path);
        self.dirs
            .iter()
            .find(|(dir, _)| win_path(dir) == want)
            .map(|(_, names)| names.clone())
            .unwrap_or_default()
    }
    fn write(&self, path: &Path, text: &str) -> Result<(), String> {
        self.written
            .borrow_mut()
            .push((path.to_path_buf(), text.to_string()));
        Ok(())
    }
    fn remove_file(&self, path: &Path) {
        self.removed.borrow_mut().push(path.to_path_buf());
    }
    fn temp_dir(&self) -> PathBuf {
        PathBuf::from(r"C:\tmp")
    }
    fn pid(&self) -> u32 {
        4242
    }
    fn home(&self) -> Option<PathBuf> {
        Some(PathBuf::from(r"C:\Users\u"))
    }
    fn sdk_arch(&self) -> &'static str {
        "x64"
    }
    fn run(
        &self,
        program: &Path,
        args: &[String],
        env: &[(String, String)],
    ) -> Result<Ran, String> {
        self.runs
            .borrow_mut()
            .push((program.to_path_buf(), args.to_vec(), env.to_vec()));
        Ok(self.reply.clone())
    }
    fn is_windows(&self) -> bool {
        true
    }
}

/// `signtool verify /pa /v` over an Artifact Signing signature: each chain
/// from the root down, every certificate one level deeper than its issuer,
/// the signer's leaf last.
const VERIFIED: &str = "Verifying: aterm.exe

Signature Index: 0 (Primary Signature)
Hash of file (sha256): AB

Signing Certificate Chain:
    Issued to: Microsoft Identity Verification Root Certificate Authority 2020
    Issued by: Microsoft Identity Verification Root Certificate Authority 2020
    Expires:   Mon Apr 16 18:44:40 2045
    SHA1 hash: 11

        Issued to: Microsoft ID Verified Code Signing PCA 2021
        Issued by: Microsoft Identity Verification Root Certificate Authority 2020
        Expires:   Mon Apr 01 20:05:20 2036
        SHA1 hash: 22

            Issued to: Microsoft ID Verified CS EOC CA 01
            Issued by: Microsoft ID Verified Code Signing PCA 2021
            Expires:   Mon Apr 13 17:31:53 2026
            SHA1 hash: 33

                Issued to: Andrew Yates
                Issued by: Microsoft ID Verified CS EOC CA 01
                Expires:   Wed Sep 16 00:00:00 2026
                SHA1 hash: 44

The signature is timestamped: Mon Sep 15 10:00:00 2026
Timestamp Verified by:
    Issued to: Microsoft Identity Verification Root Certificate Authority 2020
    Issued by: Microsoft Identity Verification Root Certificate Authority 2020
    Expires:   Mon Apr 16 18:44:40 2045
    SHA1 hash: 11

        Issued to: Microsoft Public RSA Timestamping CA 2020
        Issued by: Microsoft Identity Verification Root Certificate Authority 2020
        Expires:   Mon Nov 19 20:42:31 2035
        SHA1 hash: 55

            Issued to: Microsoft Public RSA Time Stamping Authority
            Issued by: Microsoft Public RSA Timestamping CA 2020
            Expires:   Thu Feb 15 20:36:12 2027
            SHA1 hash: 66

Successfully verified: aterm.exe

Number of files successfully Verified: 1
Number of warnings: 0
Number of errors: 0
";

const UNTRUSTED: &str = "Verifying: aterm.exe
Signature Index: 0 (Primary Signature)
Signing Certificate Chain:
    Issued to: aterm dev
    Issued by: aterm dev
    Expires:   Sat Sep 15 00:00:00 2029
    SHA1 hash: 33

The signature is timestamped: Mon Sep 15 10:00:00 2026

SignTool Error: A certificate chain processed, but terminated in a root certificate which is not trusted by the trust provider.

Number of errors: 1
";

fn ran(code: i32, text: &str) -> Ran {
    Ran {
        code: Some(code),
        stdout: text.to_string(),
        stderr: String::new(),
    }
}

#[test]
fn the_tier_is_the_anchor_and_nothing_else() {
    assert_eq!(tier_from(""), Tier::Inactive);
    assert_eq!(
        tier_from("Andrew Yates"),
        Tier::Active {
            publisher: "Andrew Yates".into()
        }
    );
}

/// The signer is the leaf certificate (printed last in the chain), with LF
/// or CRLF line endings.
#[test]
fn a_verified_chain_is_read_back_as_its_leaf_not_its_root() {
    for text in [VERIFIED.to_string(), VERIFIED.replace('\n', "\r\n")] {
        let r = parse_verify_output(&ran(0, &text));
        assert!(r.verified && r.signed && r.timestamped && !r.untrusted_root);
        assert_eq!(r.issued_to.as_deref(), Some("Andrew Yates"));
        assert_eq!(
            r.issued_by.as_deref(),
            Some("Microsoft ID Verified CS EOC CA 01")
        );
    }
}

/// Another customer's leaf under the same root is refused by the pin, as is
/// a pin on the root's name or a flat block with two certificates at the
/// same depth.
#[test]
fn the_pin_names_one_leaf_and_never_the_root_it_chains_to() {
    let root = "Microsoft Identity Verification Root Certificate Authority 2020";
    let other = VERIFIED.replace(
        "Issued to: Andrew Yates",
        "Issued to: Mallory Unrelated Customer LLC",
    );
    let other_report = parse_verify_output(&ran(0, &other));
    assert_eq!(
        judge_reason(&tier_from("Andrew Yates"), &other_report),
        Err(RefusalReason::PublisherMismatch {
            got: "Mallory Unrelated Customer LLC".into(),
            publisher: "Andrew Yates".into(),
        })
    );
    assert!(judge(&tier_from("Andrew Yates"), &other_report).is_err());
    for (text, expected_leaf) in [
        (VERIFIED, "Andrew Yates"),
        (other.as_str(), "Mallory Unrelated Customer LLC"),
    ] {
        let report = parse_verify_output(&ran(0, text));
        assert_eq!(
            judge_reason(&tier_from(root), &report),
            Err(RefusalReason::PublisherMismatch {
                got: expected_leaf.into(),
                publisher: root.into(),
            })
        );
        assert!(judge(&tier_from(root), &report).is_err());
    }

    let flat = "Verifying: aterm.exe
Signing Certificate Chain:
    Issued to: Andrew Yates
    Issued by: Microsoft ID Verified CS EOC CA 01

    Issued to: Microsoft ID Verified CS EOC CA 01
    Issued by: Microsoft ID Verified Code Signing PCA 2021

The signature is timestamped: Mon Sep 15 10:00:00 2026
Successfully verified: aterm.exe
";
    let r = parse_verify_output(&ran(0, flat));
    assert_eq!((r.issued_to.clone(), r.issued_by.clone()), (None, None));
    assert_eq!(
        judge_reason(&tier_from("Andrew Yates"), &r),
        Err(RefusalReason::MissingChain)
    );
    assert!(judge(&tier_from("Andrew Yates"), &r).is_err());
}

#[test]
fn a_self_signed_chain_is_signed_but_not_verified() {
    let r = parse_verify_output(&ran(1, UNTRUSTED));
    assert!(!r.verified && r.signed && r.untrusted_root && r.timestamped);
    assert_eq!(r.issued_to.as_deref(), Some("aterm dev"));
}

#[test]
fn an_unsigned_file_reads_as_unsigned() {
    let r = parse_verify_output(&ran(1, "SignTool Error: No signature found.\n"));
    assert!(!r.signed && !r.verified && r.issued_to.is_none());
}

#[test]
fn the_active_tier_refuses_everything_but_the_anchored_trusted_timestamped_leaf() {
    let active = tier_from("Andrew Yates");
    let verified = parse_verify_output(&ran(0, VERIFIED));
    assert_eq!(judge_reason(&active, &verified), Ok(()));
    assert!(judge(&active, &verified).is_ok());

    let wrong = tier_from("Someone Else");
    assert_eq!(
        judge_reason(&wrong, &verified),
        Err(RefusalReason::PublisherMismatch {
            got: "Andrew Yates".into(),
            publisher: "Someone Else".into(),
        })
    );
    assert!(judge(&wrong, &verified).is_err());

    let untrusted = parse_verify_output(&ran(1, UNTRUSTED));
    assert_eq!(
        judge_reason(&active, &untrusted),
        Err(RefusalReason::UntrustedRoot)
    );
    assert!(judge(&active, &untrusted).is_err());

    let unverified_policy = parse_verify_output(&ran(1, "SignTool Error: Digest mismatch.\n"));
    assert_eq!(
        judge_reason(&active, &unverified_policy),
        Err(RefusalReason::UnverifiedPolicy)
    );
    assert!(judge(&active, &unverified_policy).is_err());

    let unsigned = parse_verify_output(&ran(1, "SignTool Error: No signature found.\n"));
    assert_eq!(
        judge_reason(&active, &unsigned),
        Err(RefusalReason::Unsigned)
    );
    assert!(judge(&active, &unsigned).is_err());

    let no_ts = VERIFIED.replace("The signature is timestamped", "The signature is not");
    let no_ts_report = parse_verify_output(&ran(0, &no_ts));
    assert_eq!(
        judge_reason(&active, &no_ts_report),
        Err(RefusalReason::NotTimestamped)
    );
    assert!(judge(&active, &no_ts_report).is_err());

    assert_eq!(judge_reason(&Tier::Inactive, &untrusted), Ok(()));
    assert!(judge(&Tier::Inactive, &untrusted).is_ok());
}

#[test]
fn sign_args_per_lane_carry_a_sha256_digest_and_a_timestamp() {
    let files = vec![PathBuf::from(r"C:\d\aterm.exe")];
    let ts = Lane::TrustedSigning {
        endpoint: "https://eus.codesigning.azure.net".into(),
        account: "acct".into(),
        profile: "prof".into(),
    };
    let a = sign_args(
        &ts,
        default_timestamp(&ts),
        Some(Path::new(r"C:\n\Azure.CodeSigning.Dlib.dll")),
        Some(Path::new(r"C:\tmp\m.json")),
        &files,
    );
    assert_eq!(
        a,
        [
            "sign",
            "/v",
            "/fd",
            "SHA256",
            "/tr",
            "http://timestamp.acs.microsoft.com",
            "/td",
            "SHA256",
            "/dlib",
            r"C:\n\Azure.CodeSigning.Dlib.dll",
            "/dmdf",
            r"C:\tmp\m.json",
            r"C:\d\aterm.exe"
        ]
    );
    let th = Lane::Thumbprint("AB".repeat(20));
    let a = sign_args(&th, default_timestamp(&th), None, None, &files);
    assert_eq!(&a[8..10], &["/sha1", &"AB".repeat(20)]);
    assert!(
        !a.iter().any(|x| x == "/f" || x == "/p"),
        "no PFX/password lane, ever"
    );
    assert_eq!(
        verify_args(Path::new("a.exe")),
        ["verify", "/pa", "/v", "a.exe"]
    );
}

#[test]
fn metadata_json_escapes_what_a_name_could_carry() {
    let m = trusted_signing_metadata("https://x", r#"a"b\c"#, "p");
    assert!(m.contains(r#""CodeSigningAccountName": "a\"b\\c""#), "{m}");
    assert!(m.contains(r#""Endpoint": "https://x""#));
    assert!(m.contains(r#""CertificateProfileName": "p""#));
}

#[test]
fn metadata_json_excludes_the_interactive_browser_credential() {
    let m = trusted_signing_metadata("https://x", "a", "p");
    assert!(
        m.contains(r#""ExcludeCredentials": ["InteractiveBrowserCredential"]"#),
        "{m}"
    );
}

#[test]
fn the_lane_is_the_committed_identity_whole_or_none() {
    assert!(pinned_lane().is_ok(), "{:?}", pinned_lane());
    assert_eq!(lane_from(["", "", ""], ""), Ok(None));
    assert_eq!(
        lane_from(["https://e", "a", "p"], ""),
        Ok(Some(Lane::TrustedSigning {
            endpoint: "https://e".into(),
            account: "a".into(),
            profile: "p".into(),
        }))
    );
    assert_eq!(
        lane_from(["", "", ""], &"ab".repeat(20)),
        Ok(Some(Lane::Thumbprint("AB".repeat(20))))
    );
    assert!(lane_from(["", "a", ""], "").is_err(), "a partial Lane A");
    assert!(lane_from(["https://e", "a", "p"], &"ab".repeat(20)).is_err());
    assert!(lane_from(["", "", ""], "nope").is_err());
}

#[test]
fn signtool_is_found_newest_sdk_first_and_every_miss_is_named() {
    let mut h = Fake::new();
    h.env.insert("ProgramFiles(x86)".into(), r"C:\PFx86".into());
    h.env.insert("ProgramFiles".into(), r"C:\PF".into());
    let bin = PathBuf::from(r"C:\PFx86\Windows Kits\10\bin");
    h.dirs.insert(
        bin.clone(),
        vec!["10.0.9600.0".into(), "10.0.26100.0".into(), "x64".into()],
    );
    let want = win_join(&bin, ["10.0.26100.0", "x64", "signtool.exe"]);
    h.files
        .push(win_join(&bin, ["10.0.9600.0", "x64", "signtool.exe"]));
    h.files.push(want.clone());
    assert_eq!(
        win_path(&find_signtool(&h, None).unwrap()),
        r"C:\PFx86\Windows Kits\10\bin\10.0.26100.0\x64\signtool.exe"
    );
    assert!(find_signtool(&h, Some(Path::new(r"C:\nope.exe"))).is_err());
    let e = find_signtool(&Fake::new(), None).unwrap_err();
    assert!(e.contains("signtool on PATH"), "{e}");
    let mut empty = Fake::new();
    empty.env.insert("ProgramFiles".into(), r"C:\PF".into());
    let e = find_signtool(&empty, None).unwrap_err();
    assert!(e.contains(r"C:\PF\Windows Kits\10\bin\<10.x.y.z>"), "{e}");
}

#[test]
fn the_dlib_is_found_under_the_nuget_cache_newest_version_first() {
    let mut h = Fake::new();
    let pkg = PathBuf::from(r"C:\Users\u\.nuget\packages\microsoft.trusted.signing.client");
    h.dirs
        .insert(pkg.clone(), vec!["1.0.52".into(), "1.0.60".into()]);
    let want = win_join(&pkg, ["1.0.60", "bin", "x64", "Azure.CodeSigning.Dlib.dll"]);
    h.files.push(win_join(
        &pkg,
        ["1.0.52", "bin", "x64", "Azure.CodeSigning.Dlib.dll"],
    ));
    h.files.push(want.clone());
    assert_eq!(
        win_path(&find_dlib(&h, None).unwrap()),
        r"C:\Users\u\.nuget\packages\microsoft.trusted.signing.client\1.0.60\bin\x64\Azure.CodeSigning.Dlib.dll"
    );
    h.env.insert("NUGET_PACKAGES".into(), r"D:\cache".into());
    let e = find_dlib(&h, None).unwrap_err();
    assert!(
        e.contains(r"D:\cache\microsoft.trusted.signing.client\<version>"),
        "{e}"
    );
}

#[test]
fn windows_paths_are_joined_with_backslashes_on_every_host() {
    assert_eq!(
        win_join(r"C:\PFx86", ["Windows Kits", "10", "bin"]),
        PathBuf::from(r"C:\PFx86\Windows Kits\10\bin")
    );
    assert_eq!(win_join(r"C:\", ["x"]), PathBuf::from(r"C:\x"));
    assert_eq!(win_join("C:", ["x"]), PathBuf::from(r"C:x"));
    assert_eq!(win_join("D:/cache", ["p"]), PathBuf::from(r"D:/cache\p"));
    assert_eq!(
        win_join(r"C:\a", std::iter::empty::<&str>()),
        PathBuf::from(r"C:\a")
    );
}

#[test]
fn the_renamed_package_lineage_wins_over_the_unlisted_one() {
    let mut h = Fake::new();
    let cache = PathBuf::from(r"C:\Users\u\.nuget\packages");
    let old = win_join(&cache, ["microsoft.trusted.signing.client"]);
    let new = win_join(&cache, ["microsoft.artifactsigning.client"]);
    h.dirs.insert(old.clone(), vec!["1.0.95".into()]);
    h.dirs.insert(new.clone(), vec!["1.0.128".into()]);
    let old_dll = win_join(&old, ["1.0.95", "bin", "x64", "Azure.CodeSigning.Dlib.dll"]);
    let new_dll = win_join(
        &new,
        ["1.0.128", "bin", "x64", "Azure.CodeSigning.Dlib.dll"],
    );
    h.files.push(old_dll.clone());
    assert_eq!(
        find_dlib(&h, None).unwrap(),
        old_dll,
        "the unlisted lineage still serves"
    );
    h.files.push(new_dll.clone());
    assert_eq!(
        find_dlib(&h, None).unwrap(),
        new_dll,
        "the current lineage is preferred"
    );
    let e = find_dlib(&Fake::new(), None).unwrap_err();
    assert!(
        e.contains("microsoft.artifactsigning.client")
            && e.contains("microsoft.trusted.signing.client"),
        "{e}"
    );
}

#[test]
fn versions_sort_numerically() {
    assert_eq!(
        versions_newest_first(["1.0.9", "1.0.10", "x", "1.+0.99", "1.0.52"]),
        ["1.0.52", "1.0.10", "1.0.9"]
    );
}

#[test]
fn sign_without_a_lane_refuses_by_name_and_help_exits_zero() {
    let mut h = Fake::new();
    h.files.push(PathBuf::from(r"C:\d\aterm.exe"));
    assert_eq!(run_with(vec!["help".into()], &h, &Tier::Inactive, None), 0);
    assert_eq!(
        run_with(
            vec!["sign".into(), r"C:\d\aterm.exe".into()],
            &h,
            &Tier::Inactive,
            None
        ),
        2
    );
    assert_eq!(
        run_with(
            vec!["sign".into(), r"C:\d\aterm.exe".into()],
            &h,
            &tier_from("Andrew Yates"),
            None
        ),
        2
    );
    assert_eq!(run_with(vec!["bogus".into()], &h, &Tier::Inactive, None), 2);
    assert_eq!(
        run_with(
            vec!["sign".into(), "--thumbprint".into(), "cd".repeat(20)],
            &h,
            &Tier::Inactive,
            None
        ),
        2,
        "the identity is committed, never a flag"
    );
    assert!(h.runs.borrow().is_empty(), "nothing was executed");
}

#[test]
fn sign_with_a_thumbprint_runs_signtool_then_reads_the_signature_back() {
    let mut h = Fake::new();
    let exe = PathBuf::from(r"C:\d\aterm.exe");
    let st = PathBuf::from(r"C:\sdk\signtool.exe");
    h.files.push(exe.clone());
    h.files.push(st.clone());
    h.reply = ran(0, VERIFIED);
    let code = run_with(
        vec![
            "sign".into(),
            "--signtool".into(),
            st.display().to_string(),
            exe.display().to_string(),
        ],
        &h,
        &Tier::Inactive,
        Some(&Lane::Thumbprint("CD".repeat(20))),
    );
    assert_eq!(code, 0);
    let runs = h.runs.borrow();
    assert_eq!(runs.len(), 2, "one sign, one verify");
    assert_eq!(runs[0].0, st);
    assert_eq!(runs[0].1[0], "sign");
    assert!(runs[0].1.contains(&"CD".repeat(20)));
    assert_eq!(runs[1].1, verify_args(&exe));
}

#[test]
fn the_dotnet_runtime_is_found_global_first_and_user_local_needs_dotnet_root() {
    let mut h = Fake::new();
    h.env.insert("ProgramFiles".into(), r"C:\PF".into());
    h.env
        .insert("LOCALAPPDATA".into(), r"C:\Users\u\AppData\Local".into());
    let local = PathBuf::from(r"C:\Users\u\AppData\Local\Microsoft\dotnet");
    h.dirs.insert(
        PathBuf::from(r"C:\Users\u\AppData\Local\Microsoft\dotnet\shared\Microsoft.NETCore.App"),
        vec!["8.0.31".into(), "6.0.36".into()],
    );
    let r = find_dotnet_runtime(&h).unwrap();
    assert_same_path!(&r.root, &local);
    assert_eq!(r.version, "8.0.31");
    assert!(r.needs_root_env);
    let global = PathBuf::from(r"C:\PF\dotnet");
    h.dirs.insert(
        PathBuf::from(r"C:\PF\dotnet\shared\Microsoft.NETCore.App"),
        vec!["9.0.4".into()],
    );
    let r = find_dotnet_runtime(&h).unwrap();
    assert_same_path!(&r.root, &global);
    assert_eq!((r.version.as_str(), r.needs_root_env), ("9.0.4", false));
    let mut old = Fake::new();
    old.env.insert("ProgramFiles".into(), r"C:\PF".into());
    old.dirs.insert(
        PathBuf::from(r"C:\PF\dotnet\shared\Microsoft.NETCore.App"),
        vec!["6.0.36".into()],
    );
    let e = find_dotnet_runtime(&old).unwrap_err();
    assert!(
        e.contains(r"C:\PF\dotnet\shared\Microsoft.NETCore.App\<8+>"),
        "{e}"
    );
}

/// `USAGE`'s exit codes: 1 is "the signing or the verdict is bad", 2 is
/// "nothing was signed and no verdict was read".
#[test]
fn a_signtool_that_ran_and_refused_is_exit_one_not_two() {
    let exe = PathBuf::from(r"C:\d\aterm.exe");
    let st = PathBuf::from(r"C:\sdk\signtool.exe");
    let mut h = Fake::new();
    h.files.extend([exe.clone(), st.clone()]);
    let lane = Lane::Thumbprint("A1B2C3D4E5F60718293A4B5C6D7E8F90A1B2C3D4".into());
    h.reply = ran(
        1,
        "SignTool Error: An unexpected internal error has occurred.",
    );
    assert_eq!(
        run_with(
            vec![
                "sign".into(),
                "--signtool".into(),
                st.display().to_string(),
                exe.display().to_string(),
            ],
            &h,
            &Tier::Inactive,
            Some(&lane),
        ),
        1,
        "a refused signature must not wear the missing-tool code"
    );
    assert_eq!(h.runs.borrow().len(), 1, "it stopped at the failed sign");

    let mut gone = Fake::new();
    gone.files.push(exe.clone());
    assert_eq!(
        run_with(
            vec!["sign".into(), exe.display().to_string()],
            &gone,
            &Tier::Inactive,
            Some(&lane),
        ),
        2,
        "nothing to sign with is still exit 2"
    );
}

#[test]
fn a_trusted_signing_sign_passes_dotnet_root_for_a_user_local_runtime() {
    let mut h = Fake::new();
    let exe = PathBuf::from(r"C:\d\aterm.exe");
    let st = PathBuf::from(r"C:\sdk\signtool.exe");
    let dlib = PathBuf::from(r"C:\n\Azure.CodeSigning.Dlib.dll");
    h.files.extend([exe.clone(), st.clone(), dlib.clone()]);
    let lane = Lane::TrustedSigning {
        endpoint: "https://eus.codesigning.azure.net".into(),
        account: "acct".into(),
        profile: "prof".into(),
    };
    let sign = vec![
        "sign".into(),
        "--signtool".into(),
        st.display().to_string(),
        "--dlib".into(),
        dlib.display().to_string(),
        exe.display().to_string(),
    ];
    h.env
        .insert("LOCALAPPDATA".into(), r"C:\Users\u\AppData\Local".into());
    for k in ["AZURE_TENANT_ID", "AZURE_CLIENT_ID", "AZURE_CLIENT_SECRET"] {
        h.env.insert(k.into(), "x".into());
    }
    h.dirs.insert(
        PathBuf::from(r"C:\Users\u\AppData\Local\Microsoft\dotnet\shared\Microsoft.NETCore.App"),
        vec!["8.0.31".into()],
    );
    h.reply = ran(0, VERIFIED);
    assert_eq!(
        run_with(sign.clone(), &h, &tier_from("Andrew Yates"), Some(&lane)),
        0
    );
    assert_eq!(
        h.written.borrow().len(),
        1,
        "metadata written once through Host"
    );
    assert_same_path!(
        &h.written.borrow()[0].0,
        Path::new(r"C:\tmp\aterm-winsign-4242.json")
    );
    assert_eq!(
        h.removed.borrow().len(),
        1,
        "metadata removed once through Host"
    );
    assert_same_path!(
        &h.removed.borrow()[0],
        Path::new(r"C:\tmp\aterm-winsign-4242.json")
    );
    let runs = h.runs.borrow();
    assert_eq!(runs.len(), 2);
    assert!(runs[0].1.contains(&"/dlib".to_string()));
    let child_env = &runs[0].2;
    assert_eq!(child_env.len(), 1, "one variable, and only that one");
    assert!(
        h.written.borrow()[0]
            .1
            .contains("InteractiveBrowserCredential"),
        "the metadata shuts the browser sign-in out"
    );
    assert_eq!(child_env[0].0, "DOTNET_ROOT");
    assert_eq!(
        win_path(Path::new(&child_env[0].1)),
        r"C:\Users\u\AppData\Local\Microsoft\dotnet"
    );
    assert!(runs[1].2.is_empty(), "verify needs no runtime");
    drop(runs);
    assert_eq!(
        run_with(
            vec![
                "verify".into(),
                "--signtool".into(),
                st.display().to_string(),
                exe.display().to_string(),
            ],
            &h,
            &tier_from("Someone Else"),
            None,
        ),
        1,
        "active tier refuses a verified exe signed by a different publisher"
    );
    let mut none = Fake::new();
    none.files.extend([exe.clone(), st.clone(), dlib.clone()]);
    none.env = h.env.clone();
    none.env.remove("LOCALAPPDATA");
    assert_eq!(run_with(sign, &none, &Tier::Inactive, Some(&lane)), 2);
    assert!(none.runs.borrow().is_empty());
}

#[test]
fn verify_reports_an_untrusted_chain_with_exit_one_even_when_inactive() {
    let mut h = Fake::new();
    let exe = PathBuf::from(r"C:\d\aterm.exe");
    let st = PathBuf::from(r"C:\sdk\signtool.exe");
    h.files.push(exe.clone());
    h.files.push(st.clone());
    let verify = vec![
        "verify".into(),
        "--signtool".into(),
        st.display().to_string(),
        exe.display().to_string(),
    ];
    h.reply = ran(1, UNTRUSTED);
    assert_eq!(run_with(verify.clone(), &h, &Tier::Inactive, None), 1);
    h.reply = ran(0, VERIFIED);
    assert_eq!(run_with(verify.clone(), &h, &Tier::Inactive, None), 0);
}

#[test]
fn find_signtool_resolves_windows_semicolon_path_on_every_host() {
    let mut h = Fake::new();
    h.env.insert(
        "PATH".into(),
        r#"C:\empty;"C:\Windows Kits\bin";D:\other"#.into(),
    );
    h.files
        .push(PathBuf::from(r"C:\Windows Kits\bin\signtool.exe"));
    assert_same_path!(
        find_signtool(&h, None).expect("signtool found on Windows PATH"),
        Path::new(r"C:\Windows Kits\bin\signtool.exe")
    );
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let regular = dir.path().join("meta.json");
    RealHost.write(&regular, "{}").expect("write regular file");
    assert_eq!(std::fs::read_to_string(&regular).expect("read"), "{}");
    RealHost
        .write(&regular, "{\"replaced\":true}")
        .expect("atomic overwrite regular file");
    assert_eq!(
        std::fs::read_to_string(&regular).expect("read replaced"),
        "{\"replaced\":true}"
    );
    #[cfg(unix)]
    {
        let target = dir.path().join("victim.txt");
        std::fs::write(&target, "keep").expect("seed victim");
        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        assert!(RealHost.write(&link, "clobber").is_err());
        assert_eq!(std::fs::read_to_string(&target).expect("intact"), "keep");
    }
}

#[test]
fn a_trusted_signing_sign_without_an_azure_principal_refuses_before_signtool() {
    let mut h = Fake::new();
    let exe = PathBuf::from(r"C:\d\aterm.exe");
    let st = PathBuf::from(r"C:\sdk\signtool.exe");
    let dlib = PathBuf::from(r"C:\n\Azure.CodeSigning.Dlib.dll");
    h.files.extend([exe.clone(), st.clone(), dlib.clone()]);
    let lane = Lane::TrustedSigning {
        endpoint: "https://eus.codesigning.azure.net".into(),
        account: "acct".into(),
        profile: "prof".into(),
    };
    let sign = vec![
        "sign".into(),
        "--signtool".into(),
        st.display().to_string(),
        "--dlib".into(),
        dlib.display().to_string(),
        exe.display().to_string(),
    ];
    assert_eq!(
        run_inner(&sign, &h, &Tier::Inactive, Ok(Some(&lane))),
        Err(NO_PRINCIPAL.to_string())
    );
    assert!(h.runs.borrow().is_empty(), "nothing was executed");
    assert!(h.written.borrow().is_empty(), "no metadata written");
}

#[test]
fn an_invalid_pin_fails_only_the_verbs_that_sign() {
    let mut h = Fake::new();
    let exe = PathBuf::from(r"C:\d\aterm.exe");
    let st = PathBuf::from(r"C:\sdk\signtool.exe");
    h.files.extend([exe.clone(), st.clone()]);
    h.reply = ran(0, VERIFIED);
    let why = "THUMBPRINT is a certificate's 40 hex digits (SHA-1); got `x`";
    let bad = Err(why);
    assert_eq!(run_inner(&["help".into()], &h, &Tier::Inactive, bad), Ok(0));
    let verify = vec![
        "verify".into(),
        "--signtool".into(),
        st.display().to_string(),
        exe.display().to_string(),
    ];
    assert_eq!(run_inner(&verify, &h, &Tier::Inactive, bad), Ok(0));
    let sign = vec!["sign".into(), exe.display().to_string()];
    assert_eq!(
        run_inner(&sign, &h, &Tier::Inactive, bad),
        Err(why.to_string())
    );
    assert_eq!(
        run_inner(&["doctor".into()], &h, &Tier::Inactive, bad),
        Ok(2)
    );
}
