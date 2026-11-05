// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Actual registered harness inputs for private native labs. This module owns
//! source/plan/preparation only: callers retain their own Owner, CPU and cohort.
//! The caller owns the root's lifetime and preserves failed native execution worlds.

use crate::{
    artifact::{
        build_local::{self, CacheFreshness, HarnessBuild, Plan, Policy, Profile, Request},
        pack::HARNESS_VERSION_ENV,
        tools::{HostTools, Run, Tools as _},
    },
    ctx::Ctx,
    host::{Hosts, Slug, fake::FakeHost},
};
use aterm_uds::deadline;
use std::{
    fs,
    io::{self, Write as _},
    os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

/// The actual registered harness program, never installed by this fixture.
pub const PROGRAM: &str = "claude-code-alab";
/// The actual declared emitted binary.
pub const BIN: &str = "claude-alab";
const HANG: Duration = Duration::from_secs(60);
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// External instrumentation files, all initialized before compilation.
pub struct Counters {
    /// Version-dependent root script.
    pub root: PathBuf,
    /// Unchanged local dependency script.
    pub unrelated: PathBuf,
    /// Declared-data local dependency script.
    pub data: PathBuf,
}
impl Counters {
    /// Read actual execution counts without creating any fixture state.
    /// # Errors
    /// Missing, unreadable or malformed instrumentation data.
    pub fn read(&self) -> io::Result<[u64; 3]> {
        let read = |path: &Path| {
            fs::read_to_string(path)?
                .trim()
                .parse::<u64>()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
        };
        Ok([read(&self.root)?, read(&self.unrelated)?, read(&self.data)?])
    }
}

/// Original source, production plan and prepared build custody for one cold root.
pub struct Fixture {
    /// Canonical private producer home and fixture-only release host.
    pub ctx: Ctx,
    /// Local bare-origin transport; no compiler or publication responses are faked.
    pub host: Arc<FakeHost>,
    /// The real committed positive source tree.
    pub checkout: PathBuf,
    /// The production request.
    pub request: Request,
    /// The actual immutable-SDK production plan.
    pub plan: Plan,
    /// Original toolchain, source, target and artifact preparation custody.
    pub prepared: HarnessBuild,
    /// Explicit paths for cold/warm count observations.
    pub counters: Counters,
}

/// Create real registered inputs and prepare through the shared artifact builder.
/// The SDK is provided by the caller; the existing Cargo home is never replaced.
/// No Owner, admission, CPU pool or compiler build is created here.
/// # Errors
/// Unavailable inputs, unsupported actual driver, Git/metadata failure,
/// existing fixture outputs, or refusal from the production preparation path.
pub fn prepare(root: &Path, sdk_bin: &Path) -> io::Result<Fixture> {
    let prepare = || -> Result<Fixture> {
        let root = root.canonicalize()?;
        let sdk = sdk_bin.canonicalize()?;
        for name in ["targo", "trustc", "trustdoc"] {
            use std::os::unix::fs::PermissionsExt as _;
            let metadata = fs::symlink_metadata(sdk.join(name))?;
            require(
                metadata.is_file()
                    && metadata.permissions().mode() & 0o222 == 0
                    && metadata.permissions().mode() & 0o111 != 0,
                "fixture requires original regular immutable SDK executables",
            )?;
        }
        private_dir(&root.join("output.noindex"))?;
        let host = Arc::new(FakeHost::new());
        let ctx = Ctx::new(
            &root,
            None,
            None,
            aterm_time::unix_secs()
                .ok_or_else(|| io::Error::other("fixture clock unavailable"))?
                .try_into()?,
            Hosts::single(host.clone()),
        );
        let checkout = project(&root, &ctx, &host, &sdk, "positive")?;
        let (request, plan) = make_plan(&root, &ctx, &checkout, &sdk)?;
        require(
            plan.analysis_session && plan.cache_freshness == CacheFreshness::ContentInputsV1,
            "actual immutable SDK lacks complete input freshness or exact analysis-session capability",
        )?;
        let prepared = build_local::prepare_harness(&ctx, &request, &plan)?;
        aterm_capacity::epoch_recipe::RootRecipe::validate_set(&[prepared.recipe(0)?], 1)?;
        let counters = Counters {
            root: root.join("positive.build-count"),
            unrelated: root.join("positive-dependency.build-count"),
            data: root.join("positive-data.build-count"),
        };
        require(
            counters.read()? == [0, 0, 0],
            "fixture preparation executed a build script",
        )?;
        Ok(Fixture {
            ctx,
            host,
            checkout,
            request,
            plan,
            prepared,
            counters,
        })
    };
    prepare().map_err(|error| io::Error::other(error.to_string()))
}

fn require(condition: bool, reason: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(io::Error::other(reason).into())
    }
}

fn private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new().mode(0o700).create(path)
}

fn initialize_counter(root: &Path, name: &str) -> io::Result<()> {
    // A repeated preparation must preserve earlier execution evidence.
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(root.join(name))?
        .write_all(b"0\n")
}

fn publish(root: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let temporary = root.join(format!(".{name}.pending"));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(bytes)?;
    drop(file);
    fs::rename(temporary, root.join(name))
}

fn git(checkout: &Path, args: &[&str]) -> Result<String> {
    let mut command = crate::testkit::git();
    command.arg("-C").arg(checkout).args(args);
    let output = deadline::output_until(&mut command, Instant::now() + HANG)?;
    require(
        output.status.success(),
        &format!(
            "fixture Git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    Ok(String::from_utf8(output.stdout)?
        .trim_end_matches('\n')
        .to_owned())
}

/// Create one original committed fixture source and real private bare origin.
/// # Errors
/// Unsupported phase, existing fixture paths, Git/tool refusal or unavailable SDK.
pub fn project(
    root: &Path,
    ctx: &Ctx,
    host: &FakeHost,
    sdk: &Path,
    phase: &str,
) -> Result<PathBuf> {
    require(
        matches!(phase, "positive" | "survivor" | "failure" | "cancel"),
        "unsupported native harness fixture phase",
    )?;
    // Creating instrumentation siblings during a cold script would change
    // an original source ancestor's stamp and correctly prevent enrollment.
    // Initialize counters before compilation; do not add a warm-up execution.
    initialize_counter(root, &format!("{phase}.build-count"))?;
    if phase == "positive" {
        initialize_counter(root, "positive-dependency.build-count")?;
        initialize_counter(root, "positive-data.build-count")?;
    }
    let checkout = root.join(format!("{phase}-source"));
    private_dir(&checkout)?;
    private_dir(&checkout.join("src"))?;
    private_dir(&checkout.join(".cargo"))?;
    publish(&checkout.join(".cargo"),"config.toml",b"[env]\nHARNESS_PROBE_CHECKOUT={value='committed-checkout-config',force=true}\n[build]\nrustflags=['--cfg=harness_probe_checkout']\n")?;
    let dependency = if phase == "positive" {
        "[dependencies]\nunchanged-probe={path='unchanged-probe'}\ndata-probe={path='data-probe'}\n"
    } else {
        ""
    };
    publish(&checkout, "Cargo.toml", format!("[package]\nname='claude-code-alab'\nversion='0.1.0'\nedition='2024'\n[[bin]]\nname='claude-alab'\npath='src/main.rs'\n[features]\nprobe-flag=[]\n{dependency}[workspace]\n[workspace.metadata.atpkg]\nexpose=['claude-alab']\nbuild-args=['--features','probe-flag','--offline']\n").as_bytes())?;
    let dependency_call = if phase == "positive" {
        "assert_eq!(unchanged_probe::value(),42);if std::env::args().nth(1).as_deref()==Some(\"--data\"){println!(\"data={}\",data_probe::value());return;}"
    } else {
        ""
    };
    publish(&checkout.join("src"), "main.rs", format!("#[cfg(not(harness_probe_checkout))]\ncompile_error!(\"the original checkout compiler configuration was not loaded\");\nfn main(){{assert!(cfg!(feature=\"probe-flag\"));assert_eq!(env!(\"HARNESS_PROBE_CHECKOUT\"),\"committed-checkout-config\");{dependency_call} let stamp=env!(\"ALAB_HARNESS_VERSION\"); if std::env::args().nth(1).as_deref()==Some(\"--version\"){{println!(\"claude-code-alab {{stamp}}\");}}else{{println!(\"native-harness-ok {{stamp}}\");}}}}\n").as_bytes())?;
    if phase == "positive" {
        let dependency = checkout.join("unchanged-probe");
        private_dir(&dependency)?;
        private_dir(&dependency.join("src"))?;
        publish(
                &dependency,
                "Cargo.toml",
                b"[package]\nname='unchanged-probe'\nversion='0.0.0'\nedition='2024'\n[lib]\nname='unchanged_probe'\npath='src/lib.rs'\n",
            )?;
        publish(
            &dependency.join("src"),
            "lib.rs",
            b"pub fn value()->u32{42}\n",
        )?;
        // This unchanged dependency deliberately has no root version/stamp
        // input. Its actual execution count and emitted rlib test reuse.
        publish(&dependency,"build.rs",format!("fn main(){{println!(\"cargo:rerun-if-changed=build.rs\");let path=std::path::PathBuf::from({:?});let previous=std::fs::read_to_string(&path).map(|v|v.trim().parse::<u64>().unwrap()).unwrap_or(0);std::fs::write(path,format!(\"{{}}\\n\",previous+1)).unwrap();}}\n",root.join("positive-dependency.build-count")).as_bytes())?;
        let data = checkout.join("data-probe");
        private_dir(&data)?;
        private_dir(&data.join("src"))?;
        publish(&data,"Cargo.toml",b"[package]\nname='data-probe'\nversion='0.0.0'\nedition='2024'\n[lib]\nname='data_probe'\npath='src/lib.rs'\n")?;
        publish(&data, "value.txt", b"21\n")?;
        publish(&data.join("src"),"lib.rs",b"include!(concat!(env!(\"OUT_DIR\"),\"/generated.rs\"));\npub fn value()->u32{GENERATED}\n")?;
        publish(&data,"build.rs",format!(r#"
fn main() {{
 println!("cargo:rerun-if-changed=build.rs");
 println!("cargo:rerun-if-changed=value.txt");
 let count=std::path::PathBuf::from({:?});
 let previous=std::fs::read_to_string(&count).map(|v|v.trim().parse::<u64>().unwrap()).unwrap_or(0);
 std::fs::write(count,format!("{{}}\n",previous+1)).unwrap();
 let value:u32=std::fs::read_to_string(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("value.txt")).unwrap().trim().parse().unwrap();
 std::fs::write(std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("generated.rs"),format!("const GENERATED:u32={{value}};\n")).unwrap();
}}
"#,root.join("positive-data.build-count")).as_bytes())?;
    }
    // Counter/barrier paths live outside the checkout and are committed as
    // source. Counter writes preserve the pre-created namespace. The live
    // writer/cancellation barriers test custody rather than warm enrollment.
    let script = format!(
        r#"
use std::{{io::Write as _,path::PathBuf,process::{{Command,Stdio}},time::{{Duration,Instant}}}};
fn note(root:&std::path::Path,name:&str,body:&str) {{
 let temporary=root.join(format!(".{{name}}.pending"));
 std::fs::write(&temporary,body).unwrap();std::fs::rename(temporary,root.join(name)).unwrap();
}}
fn main() {{
 let root=PathBuf::from({root:?});
 let phase={phase:?};
 if std::env::args().nth(1).as_deref()==Some("--orphan-writer") {{
  unsafe extern "C" {{ fn getpgrp()->i32; }}
  // SAFETY: read this actual fixture's inherited group; no process is changed.
  let group=unsafe{{getpgrp()}};
  note(&root,"survivor.ready",&format!("pid={{}}\npgid={{group}}\n",std::process::id()));
  let deadline=Instant::now()+Duration::from_secs(60);
  while !root.join("survivor.release").exists() {{
   if let Ok(nonce)=std::fs::read_to_string(root.join("survivor.challenge")) {{
    if !root.join("survivor.alive").exists() {{
     println!("{{{{\"reason\":\"harness-probe-survivor\",\"nonce\":\"{{nonce}}\"}}}}");
     std::io::stdout().flush().unwrap();
     note(&root,"survivor.alive",&format!("pid={{}}\npgid={{group}}\nnonce={{nonce}}\n",std::process::id()));
    }}
   }}
   assert!(Instant::now()<deadline,"surviving writer was not released");std::thread::sleep(Duration::from_millis(10));
  }}
  note(&root,"survivor.done","normal writer closure\n");return;
 }}
 println!("cargo:rerun-if-changed=build.rs");
 println!("cargo:rerun-if-env-changed=ALAB_HARNESS_VERSION");
 let count=root.join(format!("{{phase}}.build-count"));
 let previous=std::fs::read_to_string(&count).map(|v|v.trim().parse::<u64>().unwrap()).unwrap_or(0);
 std::fs::write(&count,format!("{{}}\n",previous+1)).unwrap();
 if phase=="survivor" {{
  let flags=std::env::var("CARGO_MAKEFLAGS").unwrap();
  let fifo=flags.split_whitespace().find_map(|arg|arg.strip_prefix("--jobserver-auth=fifo:")).unwrap();
  let stdout=std::fs::OpenOptions::new().append(true).open(PathBuf::from(fifo).parent().unwrap().join("root-0.stdout")).unwrap();
  let child=Command::new(std::env::current_exe().unwrap()).arg("--orphan-writer").stdin(Stdio::null()).stdout(Stdio::from(stdout)).stderr(Stdio::null()).spawn().unwrap();
  let deadline=Instant::now()+Duration::from_secs(60);
  while !root.join("survivor.ready").exists() {{assert!(Instant::now()<deadline,"same-image writer did not execute");std::thread::sleep(Duration::from_millis(10));}}
  assert!(std::fs::read_to_string(root.join("survivor.ready")).unwrap().starts_with(&format!("pid={{}}\n",child.id())));
  // Deliberately outlive this build-script parent in the original cohort. Only
  // the external fixture's bounded barrier protocol releases this child.
  drop(child);
 }}
 if phase=="failure" {{ panic!("actual intended build failure"); }}
 if phase=="cancel" {{
  std::fs::write(root.join("cancel.ready"),format!("pid={{}}\n",std::process::id())).unwrap();
  let end=Instant::now()+Duration::from_secs(60);
  while !root.join("cancel.release").exists() {{
   if let Ok(nonce)=std::fs::read_to_string(root.join("cancel.challenge")) {{
    if !root.join("cancel.alive").exists() {{
     std::fs::write(root.join(".cancel.alive.pending"),format!("pid={{}}\nnonce={{nonce}}\n",std::process::id())).unwrap();
     std::fs::rename(root.join(".cancel.alive.pending"),root.join("cancel.alive")).unwrap();
    }}
   }}
   assert!(Instant::now()<end,"native cancellation fixture was not released"); std::thread::sleep(Duration::from_millis(10));
  }}
  std::fs::write(root.join("cancel.done"),b"normal successful body\n").unwrap();
 }}
}}
"#,
        root = root
            .to_str()
            .ok_or_else(|| io::Error::other("non-UTF-8 fixture path"))?,
        phase = phase
    );
    publish(&checkout, "build.rs", script.as_bytes())?;
    // Generate and commit the genuine lockfile before planning. This is a
    // real offline Targo operation, not a hand-authored compiler response.
    let mut lock = Run::new(sdk.join("targo"), ["generate-lockfile", "--offline"]).bounded();
    lock.cwd = Some(checkout.clone());
    let locked = HostTools
        .run(&lock)
        .map_err(|error| io::Error::other(error.to_string()))?;
    require(
        locked.success(),
        &format!("actual lock generation failed: {}", locked.text()),
    )?;
    git(&checkout, &["init", "--initial-branch=main"])?;
    git(&checkout, &["config", "user.name", "Native Harness Probe"])?;
    git(
        &checkout,
        &["config", "user.email", "native-harness@invalid.example"],
    )?;
    git(&checkout, &["config", "commit.gpgsign", "false"])?;
    git(&checkout, &["add", "."])?;
    git(&checkout, &["commit", "-m", "actual native harness inputs"])?;
    if phase == "survivor" {
        git(
            &checkout,
            &[
                "commit",
                "--allow-empty",
                "-m",
                "distinct actual survivor store build",
            ],
        )?;
        git(
            &checkout,
            &[
                "commit",
                "--allow-empty",
                "-m",
                "preserve separate edited harness store build",
            ],
        )?;
        git(
            &checkout,
            &[
                "commit",
                "--allow-empty",
                "-m",
                "preserve separate generated-data harness store build",
            ],
        )?;
    }
    let remote_parent = root.join(format!("{phase}-remotes"));
    private_dir(&remote_parent)?;
    let account = remote_parent.join(&ctx.accounts.staging);
    private_dir(&account)?;
    let bare = account.join(format!("{PROGRAM}.git"));
    git(
        root,
        &[
            "init",
            "--bare",
            "--initial-branch=main",
            bare.to_str().unwrap(),
        ],
    )?;
    git(
        &checkout,
        &["remote", "add", "origin", bare.to_str().unwrap()],
    )?;
    git(&checkout, &["push", "--set-upstream", "origin", "main"])?;
    host.add_clone_source(&Slug::new(&ctx.accounts.staging, PROGRAM), &bare);
    Ok(checkout)
}

/// Plan through the actual registered-harness production path.
/// # Errors
/// Native metadata, original source identity or supported plan checks refuse.
pub fn make_plan(root: &Path, ctx: &Ctx, checkout: &Path, sdk: &Path) -> Result<(Request, Plan)> {
    let request = Request {
        program: PROGRAM.into(),
        checkout: checkout.to_owned(),
        plan: false,
        policy: Policy::Unverified,
        profile: Profile::Dev,
        jobs: 2,
        link: false,
        toolchain: Some(sdk.to_owned()),
        out: Some(root.join("output.noindex")),
    };
    let plan = build_local::plan(ctx, &request, &HostTools)?;
    require(
        plan.harness.is_some()
            && plan.bins == [BIN]
            && plan.triple.as_str() == crate::triple::Triple::host().unwrap().as_str(),
        "actual registered native harness plan differs",
    )?;
    require(
        plan.command
            .env_of(HARNESS_VERSION_ENV)
            .and_then(|v| v.to_str())
            == plan
                .harness
                .as_ref()
                .map(|identity| identity.version.as_str()),
        "plan lost its real compiled version stamp",
    )?;
    Ok((request, plan))
}
