// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Upstream tools, acquired from the same pins as workspace provisioning.

use std::path::{Component, Path, PathBuf};

use aterm_uds::host::upstream::{Archive, Build, Layout, PinnedTool};

use super::{BottleInput, BottleOutcome, BottleRequest, upstream};
use crate::api::Signing;
use crate::artifact::{arch, relocate, tarwrite};
use crate::channel::Scratch;
use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::triple::{Arch, Os, Triple};

mod lean;
mod node;
mod openssl;

fn platform(triple: Triple) -> &'static str {
    match (triple.os(), triple.arch()) {
        (Os::Darwin, Arch::Aarch64) => "macos-aarch64",
        (Os::Darwin, Arch::X86_64) => "macos-x86_64",
        (Os::Linux, Arch::Aarch64) => "linux-aarch64",
        (Os::Linux, Arch::X86_64) => "linux-x86_64",
        (Os::Windows, Arch::Aarch64) => "windows-aarch64",
        (Os::Windows, Arch::X86_64) => "windows-x86_64",
    }
}

pub(super) fn can_pack(recipe: &PinnedTool, triple: Triple) -> Result<()> {
    if !matches!(
        recipe.name,
        "gitleaks" | "cpython" | "openssl-signing" | "lean4" | "node"
    ) {
        return Err(Error::cannot_run(format!(
            "{} has no pinned runtime qualifier",
            recipe.name
        )));
    }
    if recipe.archive_for(platform(triple)).is_none() {
        return Err(Error::cannot_run(format!(
            "{} has no pinned upstream archive for {triple}",
            recipe.name
        )));
    }
    if Triple::host() != Some(triple) {
        return Err(Error::cannot_run(format!(
            "{} qualification needs a native {triple} worker",
            recipe.name
        )));
    }
    Ok(())
}

fn relative(value: &str) -> Result<&Path> {
    let path = Path::new(value);
    if value.is_empty()
        || !path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err(Error::refused(
            "upstream recipe contains a non-relative file path",
        ));
    }
    Ok(path)
}

fn copy_file(root: &Path, relative_path: &Path, destination: &Path) -> Result<()> {
    let source = root.join(relative_path);
    if !std::fs::symlink_metadata(&source).is_ok_and(|metadata| metadata.is_file()) {
        return Err(Error::refused(format!(
            "upstream archive lacks regular {}",
            relative_path.display()
        )));
    }
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| Error::io("create upstream package directory", &error))?;
    }
    crate::identity::copy_regular_file(&source, destination)
        .map_err(|error| Error::io(format!("copy {}", source.display()), &error))?;
    Ok(())
}

fn installed_executable(recipe: &PinnedTool) -> Result<PathBuf> {
    match recipe.layout {
        Layout::Executable => Ok(Path::new("bin").join(recipe.name)),
        Layout::RuntimeTree => Ok(relative(recipe.executable)?.to_path_buf()),
    }
}

fn prepare(
    request: &upstream::Request,
    recipe: &PinnedTool,
    work: &Path,
    download: &dyn Fn(&str, &Path, u64) -> Result<()>,
) -> Result<PathBuf> {
    if request.program != recipe.name || request.version != recipe.version {
        return Err(Error::refused(format!(
            "{} must use its shared upstream pin {}",
            recipe.name, recipe.version
        )));
    }
    let archive: &Archive = recipe
        .archive_for(platform(request.triple))
        .ok_or_else(|| {
            Error::refused(format!(
                "{} has no archive for {}",
                recipe.name, request.triple
            ))
        })?;
    let url = archive.url_at(recipe.version);
    let filename = url
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| Error::refused("upstream archive URL has no filename"))?;
    let input = work.join(relative(filename)?);
    download(&url, &input, archive.bytes)?;
    let (file, metadata) = crate::identity::open_regular_nonblock(&input)
        .map_err(|error| Error::io("open pinned upstream archive", &error))?;
    if metadata.len() != archive.bytes {
        return Err(Error::refused(format!(
            "upstream archive has {} bytes; the pin requires {}",
            metadata.len(),
            archive.bytes
        )));
    }
    let bytes = crate::identity::read_regular_file_bytes(file, &metadata, archive.bytes)
        .map_err(|error| Error::io("read pinned upstream archive", &error))?;
    if atpkg::abrew::fetch::sha256_bytes(&bytes) != archive.sha256 {
        return Err(Error::refused(
            "upstream archive differs from its pinned SHA-256",
        ));
    }
    let unpacked = work.join("unpacked");
    atpkg::extract::extract_tree(
        &input,
        &unpacked,
        super::archive_container(&input)?,
        recipe.max_extract_bytes,
        atpkg::extract::ExtractOptions {
            strip_components: recipe.strip,
            symlinks: atpkg::extract::SymlinkPolicy::InRoot,
            allow_global_pax: true,
        },
    )
    .map_err(|error| Error::refused(format!("unpack pinned upstream archive: {error}")))?;
    let build = match recipe.build {
        Build::Prebuilt => None,
        Build::OpenSsl => Some(openssl::build(&unpacked, work)?),
    };
    let stage = work.join("install");
    let executable = stage.join(installed_executable(recipe)?);
    match recipe.layout {
        Layout::Executable => copy_file(&unpacked, relative(recipe.executable)?, &executable)?,
        Layout::RuntimeTree => crate::artifact::bundle::copy_tree(&unpacked, &stage)?,
    }
    let omitted = if request.triple.os() == Os::Darwin {
        recipe.darwin_omit
    } else {
        &[]
    };
    for name in omitted {
        let path = stage.join(relative(name)?);
        if !std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_file()) {
            return Err(Error::refused(format!(
                "Darwin omission is not a regular file: {name}"
            )));
        }
        std::fs::remove_file(&path).map_err(|error| Error::io(format!("omit {name}"), &error))?;
    }
    if !atpkg::toolchain::is_executable_file(&executable) {
        return Err(Error::refused(
            "upstream executable has no execute permission",
        ));
    }
    arch::gate(request.triple, std::slice::from_ref(&executable))?;
    if recipe.licenses.is_empty() {
        return Err(Error::refused(
            "upstream recipe declares no license material",
        ));
    }
    for license in recipe.licenses {
        let path = relative(license)?;
        copy_file(
            &unpacked,
            path,
            &stage.join("share/licenses").join(recipe.name).join(path),
        )?;
    }
    let origin = stage.join("share/atpkg/upstream.json");
    std::fs::create_dir_all(stage.join("share/atpkg"))
        .map_err(|error| Error::io("create provenance directory", &error))?;
    let mut data = aterm_json::json!({
        "program": recipe.name, "version": recipe.version, "url": url,
        "sha256": archive.sha256, "bytes": archive.bytes, "target": request.triple.as_str()
    });
    if !omitted.is_empty() {
        data.as_object_mut()
            .expect("provenance object")
            .insert("omitted".into(), aterm_json::json!(omitted));
    }
    if let Some(build) = build {
        data.as_object_mut()
            .expect("provenance object")
            .insert("build".into(), build);
    }
    std::fs::write(&origin, data.to_string())
        .map_err(|error| Error::io("write upstream provenance", &error))?;
    Ok(stage)
}

pub(super) fn pack(
    ctx: &Ctx,
    request: &upstream::Request,
    recipe: &PinnedTool,
) -> Result<BottleOutcome> {
    can_pack(recipe, request.triple)?;
    let work = Scratch::new("pinned-upstream")?;
    let tree = prepare(request, recipe, work.path(), &|url, path, cap| {
        aterm_update_core::download_to(url, path, cap).map_err(Error::cannot_run)
    })?;
    if recipe.name == "cpython" {
        precompile_python(&tree, recipe, work.path())?;
    }
    let backend = relocate::backend_for_host().map_err(Error::cannot_run)?;
    let result = super::pack_bottle_with(
        ctx,
        &BottleRequest {
            program: request.program.clone(),
            input: BottleInput::InstallTree(tree),
            version: request.version.clone(),
            build: request.build,
            triple: request.triple,
            strip_components: 0,
            dep_bottles: Vec::new(),
            donor_roots: Vec::new(),
            merge_from: request.merge_from.clone(),
            key: None,
            out: request.out.clone(),
            signing: Signing::Unsigned,
            upload: false,
        },
        backend.as_ref(),
        &tarwrite::Zstd,
    )?;
    qualify(&result, recipe)?;
    Ok(result)
}

fn run(program: &Path, args: &[&str], cwd: &Path) -> Result<std::process::Output> {
    run_with_env(program, args, cwd, &[])
}

fn run_with_env(
    program: &Path,
    args: &[&str],
    cwd: &Path,
    env: &[(&str, &str)],
) -> Result<std::process::Output> {
    let mut command = aterm_uds::spawnfd::command(program);
    command
        .env_clear()
        .env("HOME", cwd)
        .env("PATH", "/usr/bin:/bin")
        .envs(env.iter().copied())
        .current_dir(cwd)
        .args(args);
    aterm_uds::deadline::output_until(
        &mut command,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    )
    .map_err(|error| Error::cannot_run(format!("pinned tool qualification: {error}")))
}

fn qualify(result: &BottleOutcome, recipe: &PinnedTool) -> Result<()> {
    let manifest = crate::lane::rows::local(&result.manifest)?;
    let row = manifest
        .artifact_for(result.triple.as_str())
        .ok_or_else(|| Error::refused("qualified tool package has no target row"))?;
    let work = Scratch::new(&format!("{}-qualification", recipe.name))?;
    let stage = work.path().join("moved store with spaces");
    atpkg::verify_for_publication(
        row,
        &result.asset,
        &stage,
        &atpkg::install::StageHooks::NONE,
    )
    .map_err(|error| Error::refused(format!("tool client staging: {error}")))?;
    let before = atpkg::tree::tree_root(&stage)
        .map_err(|error| Error::io("hash qualified runtime", &error))?;
    let program = stage.join(installed_executable(recipe)?);
    match recipe.name {
        "gitleaks" => qualify_tree(&program, &result.version, work.path(), &run),
        "cpython" => qualify_python(&program, &result.version, &stage, work.path(), &run),
        "openssl-signing" => openssl::qualify(&program, &result.version, work.path(), &run),
        "lean4" => lean::qualify(&program, &result.version, work.path(), &run),
        "node" => node::qualify(&program, &result.version, work.path(), &run),
        _ => Err(Error::refused("pinned tool has no runtime qualifier")),
    }?;
    let after = atpkg::tree::tree_root(&stage)
        .map_err(|error| Error::io("hash runtime after qualification", &error))?;
    if before != after {
        return Err(Error::refused(
            "runtime qualification changed the package tree",
        ));
    }
    Ok(())
}

type Runner<'a> = dyn Fn(&Path, &[&str], &Path) -> Result<std::process::Output> + 'a;

fn qualify_tree(program: &Path, version: &str, work: &Path, runner: &Runner<'_>) -> Result<()> {
    let answer = runner(program, &["version"], work)?;
    if !answer.status.success() || String::from_utf8_lossy(&answer.stdout).trim() != version {
        return Err(Error::refused(
            "gitleaks executable version differs from the package",
        ));
    }
    let input = work.join("scan input");
    std::fs::create_dir_all(&input).map_err(|error| Error::io("create scanner fixture", &error))?;
    let config = work.join("gitleaks.toml");
    std::fs::write(&config, "[[rules]]\nid = 'qualification'\ndescription = 'Package qualification marker'\nregex = 'ALAB_SEEDED_[0-9]{6}'\n")
        .map_err(|error| Error::io("write scanner configuration", &error))?;
    let report = work.join("report.json");
    let config_text = config.to_string_lossy();
    let input_text = input.to_string_lossy();
    let report_text = report.to_string_lossy();
    let args = [
        "dir",
        "--no-banner",
        "--no-color",
        "--redact",
        "--exit-code",
        "17",
        "--config",
        &config_text,
        "--report-format",
        "json",
        "--report-path",
        &report_text,
        &input_text,
    ];
    let file = input.join("fixture.txt");
    std::fs::write(&file, "a clean package fixture\n")
        .map_err(|error| Error::io("write clean scanner fixture", &error))?;
    if !runner(program, &args, work)?.status.success() {
        return Err(Error::refused(
            "gitleaks rejected the clean package fixture",
        ));
    }
    std::fs::write(&file, "ALAB_SEEDED_123456\n")
        .map_err(|error| Error::io("write scanner detection fixture", &error))?;
    if runner(program, &args, work)?.status.code() != Some(17) {
        return Err(Error::refused(
            "gitleaks did not reject the seeded package fixture",
        ));
    }
    let bytes = crate::identity::read_regular_bytes(&report, 1024 * 1024)
        .map_err(|error| Error::io("read scanner qualification report", &error))?;
    let data = aterm_json::from_slice::<aterm_json::Value>(&bytes)
        .map_err(|error| Error::refused(format!("scanner report: {error}")))?;
    if !data.as_array().is_some_and(|rows| {
        rows.len() == 1
            && rows[0].get("RuleID").and_then(aterm_json::Value::as_str) == Some("qualification")
    }) {
        return Err(Error::refused(
            "gitleaks report did not identify the seeded finding",
        ));
    }
    Ok(())
}

const PYTHON_PRECOMPILE: &str = r#"
import compileall, hashlib, importlib.util, json, marshal, os, pathlib, platform, py_compile, sys, types
root = pathlib.Path(sys.argv[1])
version = '.'.join(map(str, sys.version_info[:3]))
assert version == sys.argv[2]
assert os.environ.get('PYTHONHASHSEED') == '0'
sources = sorted(root.rglob('*.py'))
def source_hash():
    digest = hashlib.sha256()
    for path in sources:
        digest.update(path.relative_to(root).as_posix().encode() + b'\0')
        digest.update(hashlib.sha256(path.read_bytes()).digest())
    return digest.hexdigest()
before = source_hash()
assert compileall.compile_dir(str(root), quiet=1, force=True, workers=1,
    optimize=[0, 1, 2], stripdir=str(root),
    invalidation_mode=py_compile.PycInvalidationMode.CHECKED_HASH)
assert before == source_hash()
def check_code(code):
    assert not os.path.isabs(code.co_filename)
    assert '..' not in pathlib.PurePosixPath(code.co_filename).parts
    for value in code.co_consts:
        if isinstance(value, types.CodeType):
            check_code(value)
pycs = sorted(root.rglob('*.pyc'))
assert pycs
for path in pycs:
    data = path.read_bytes()
    assert data[:4] == importlib.util.MAGIC_NUMBER
    assert int.from_bytes(data[4:8], 'little') == 3
    check_code(marshal.loads(data[16:]))
print(json.dumps({
    'version': version, 'implementation': sys.implementation.name,
    'cache_tag': sys.implementation.cache_tag,
    'compiler_sha256': hashlib.sha256(pathlib.Path(sys.executable).read_bytes()).hexdigest(),
    'compiler_host': platform.machine(), 'magic_number': importlib.util.MAGIC_NUMBER.hex(),
    'invalidation': 'checked-hash', 'optimize': [0, 1, 2], 'hash_seed': 0,
    'filenames': 'relative-to-runtime-root', 'pyc_files': len(pycs),
    'source_files': len(sources), 'source_tree_sha256': before,
}))
"#;

fn precompile_python(stage: &Path, recipe: &PinnedTool, work: &Path) -> Result<()> {
    let executable = stage.join(installed_executable(recipe)?);
    let stage_text = stage.to_string_lossy();
    let core = recipe.version.split('+').next().unwrap_or(recipe.version);
    let output = run_with_env(
        &executable,
        &["-B", "-P", "-s", "-c", PYTHON_PRECOMPILE, &stage_text, core],
        work,
        &[("PYTHONHASHSEED", "0")],
    )?;
    if !output.status.success() {
        return Err(Error::refused(format!(
            "CPython hash-based precompile failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let report: aterm_json::Value = aterm_json::from_slice(&output.stdout)
        .map_err(|error| Error::refused(format!("CPython precompile report: {error}")))?;
    validate_precompile_report(&report, core)?;
    let provenance = stage.join("share/atpkg/upstream.json");
    let bytes = crate::identity::read_regular_bytes(&provenance, 1024 * 1024)
        .map_err(|error| Error::io("read Python provenance", &error))?;
    let mut data: aterm_json::Value = aterm_json::from_slice(&bytes)
        .map_err(|error| Error::refused(format!("Python provenance: {error}")))?;
    data.as_object_mut()
        .ok_or_else(|| Error::refused("Python provenance is not an object"))?
        .insert("bytecode".into(), report);
    std::fs::write(&provenance, data.to_string())
        .map_err(|error| Error::io("write Python precompile provenance", &error))?;
    Ok(())
}

fn validate_precompile_report(report: &aterm_json::Value, version: &str) -> Result<()> {
    let text = |key| report.get(key).and_then(aterm_json::Value::as_str);
    let digest =
        |key| text(key).is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()));
    if text("version") != Some(version)
        || text("implementation") != Some("cpython")
        || text("invalidation") != Some("checked-hash")
        || text("filenames") != Some("relative-to-runtime-root")
        || report.get("optimize") != Some(&aterm_json::json!([0, 1, 2]))
        || report.get("hash_seed").and_then(aterm_json::Value::as_u64) != Some(0)
        || report
            .get("pyc_files")
            .and_then(aterm_json::Value::as_u64)
            .is_none_or(|n| n == 0)
        || report
            .get("source_files")
            .and_then(aterm_json::Value::as_u64)
            .is_none_or(|n| n == 0)
        || !digest("compiler_sha256")
        || !digest("source_tree_sha256")
        || !text("magic_number")
            .is_some_and(|s| s.len() == 8 && s.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(Error::refused(
            "CPython precompile report does not match the pinned transform",
        ));
    }
    Ok(())
}

const PYTHON_PROBE: &str = r#"
import bz2, ctypes, hashlib, json, lzma, os, sqlite3, ssl, subprocess, sys, sysconfig, tempfile, tomllib, zlib
from pip._vendor.distlib.scripts import ScriptMaker
payload = b'ALab pinned Python qualification'
assert tomllib.loads('qualified = true')['qualified'] is True
for compress, decompress in [(zlib.compress, zlib.decompress), (bz2.compress, bz2.decompress), (lzma.compress, lzma.decompress)]:
    assert decompress(compress(payload)) == payload
assert len(hashlib.sha256(payload).digest()) == 32
ctypes.CDLL(None)
with sqlite3.connect(':memory:') as db:
    assert db.execute('select 6 * 7').fetchone() == (42,)
with tempfile.TemporaryDirectory(dir=os.getcwd()) as script_dir:
    scripts = ScriptMaker(None, script_dir).make('alab-python-probe = pip._internal.cli.main:main')
    assert scripts
    subprocess.run([scripts[0], '--version'], check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
print(json.dumps({
    'version': '.'.join(map(str, sys.version_info[:3])),
    'prefix': os.path.realpath(sys.prefix),
    'base_prefix': os.path.realpath(sys.base_prefix),
    'executable': os.path.realpath(sys.executable),
    'stdlib': os.path.realpath(sysconfig.get_path('stdlib')),
    'ssl_ca_count': len(ssl.create_default_context().get_ca_certs()),
}))
"#;

fn validate_python_probe(
    data: &aterm_json::Value,
    version: &str,
    prefix: &Path,
    base: &Path,
    executable: &Path,
) -> Result<()> {
    let text = |name| data.get(name).and_then(aterm_json::Value::as_str);
    let core = version.split('+').next().unwrap_or(version);
    let expected = |field, path: &Path| text(field).is_some_and(|s| Path::new(s) == path);
    let stdlib_in_base = text("stdlib").is_some_and(|s| {
        let path = Path::new(s);
        path.starts_with(base.join("lib"))
            && !path.components().any(|part| part == Component::ParentDir)
    });
    if text("version") != Some(core)
        || !expected("prefix", prefix)
        || !expected("base_prefix", base)
        || !expected("executable", executable)
        || !stdlib_in_base
        || data
            .get("ssl_ca_count")
            .and_then(aterm_json::Value::as_u64)
            .is_none_or(|count| count == 0)
    {
        return Err(Error::refused(
            "CPython qualification found a wrong version, external runtime path, or empty TLS trust store",
        ));
    }
    Ok(())
}

fn python_probe(
    program: &Path,
    version: &str,
    prefix: &Path,
    base: &Path,
    work: &Path,
    runner: &Runner<'_>,
    flags: &[&str],
) -> Result<()> {
    let mut args = flags.to_vec();
    args.extend(["-c", PYTHON_PROBE]);
    let answer = runner(program, &args, work)?;
    if !answer.status.success() {
        return Err(Error::refused(format!(
            "CPython runtime probe failed: {}",
            String::from_utf8_lossy(&answer.stderr)
        )));
    }
    let data = aterm_json::from_slice(&answer.stdout)
        .map_err(|error| Error::refused(format!("CPython runtime probe: {error}")))?;
    let canonical = |path: &Path| {
        std::fs::canonicalize(path)
            .map_err(|error| Error::io("resolve qualified Python runtime", &error))
    };
    validate_python_probe(
        &data,
        version,
        &canonical(prefix)?,
        &canonical(base)?,
        &canonical(program)?,
    )
}

fn qualify_python(
    program: &Path,
    version: &str,
    stage: &Path,
    work: &Path,
    runner: &Runner<'_>,
) -> Result<()> {
    let variants: &[&[&str]] = &[&[], &["-I"], &["-E"], &["-O"], &["-OO"]];
    for flags in variants {
        python_probe(program, version, stage, stage, work, runner, flags)?;
    }
    let core = version.split('+').next().unwrap_or(version);
    let minor = core.rsplit_once('.').map_or(core, |(minor, _)| minor);
    for name in ["pip3".to_string(), format!("pip{minor}")] {
        let output = runner(&stage.join("bin").join(&name), &["--version"], work)?;
        if !output.status.success() || !String::from_utf8_lossy(&output.stdout).starts_with("pip ")
        {
            return Err(Error::refused(format!("CPython's {name} wrapper failed")));
        }
    }
    let venv = work.join("venv with spaces");
    let venv_text = venv.to_string_lossy();
    if !runner(program, &["-m", "venv", &venv_text], work)?
        .status
        .success()
    {
        return Err(Error::refused(
            "CPython could not create a virtual environment",
        ));
    }
    let venv_python = venv.join("bin/python");
    for flags in variants {
        python_probe(&venv_python, version, &venv, stage, work, runner, flags)?;
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::process::ExitStatusExt;
    use std::sync::Arc;

    use super::*;
    use crate::artifact::bottle::tests::{FakeBackend, TarEntry, build_tar, thin_macho_bytes};
    use crate::artifact::tarwrite::RawFrames;
    use crate::host::fake::FakeHost;
    use crate::testkit::Keys;
    use crate::triple::{AARCH64_APPLE_DARWIN, X86_64_APPLE_DARWIN};

    struct Fixture {
        work: aterm_tempfile::TempDir,
        archive: PathBuf,
        recipe: PinnedTool,
        request: upstream::Request,
    }

    impl Fixture {
        fn new(entries: &[TarEntry<'_>]) -> Self {
            let work = aterm_tempfile::tempdir().expect("scratch");
            let archive = work.path().join("fixture.tar");
            build_tar(&archive, entries);
            let bytes = std::fs::read(&archive).expect("fixture bytes");
            let hash = Box::leak(atpkg::abrew::fetch::sha256_bytes(&bytes).into_boxed_str());
            let archives = Box::leak(Box::new([Archive {
                platform: "macos-aarch64",
                url: "https://example.invalid/gitleaks_{v}.tar",
                sha256: hash,
                bytes: bytes.len() as u64,
            }]));
            Self {
                request: upstream::Request {
                    program: "gitleaks".into(),
                    version: "8.30.1".into(),
                    build: 1,
                    triple: AARCH64_APPLE_DARWIN,
                    merge_from: None,
                    out: work.path().join("out"),
                },
                recipe: PinnedTool {
                    name: "gitleaks",
                    version: "8.30.1",
                    executable: "gitleaks",
                    layout: Layout::Executable,
                    build: Build::Prebuilt,
                    strip: 0,
                    max_extract_bytes: super::super::MAX_BOTTLE_EXTRACT_BYTES,
                    licenses: &["LICENSE"],
                    darwin_omit: &[],
                    hosts: &["macos-aarch64"],
                    archives,
                },
                archive,
                work,
            }
        }

        fn prepare(&self) -> Result<PathBuf> {
            prepare(
                &self.request,
                &self.recipe,
                self.work.path(),
                &|url, destination, cap| {
                    assert_eq!(url, self.recipe.archives[0].url_at(self.recipe.version));
                    assert_eq!(cap, self.recipe.archives[0].bytes);
                    std::fs::copy(&self.archive, destination)
                        .map_err(|error| Error::io("copy fixture", &error))?;
                    Ok(())
                },
            )
        }

        fn pack_and_stage(&self, tree: PathBuf) -> (BottleOutcome, PathBuf) {
            let ctx = Keys::mint("m3", &[]).ctx(self.work.path(), Arc::new(FakeHost::new()));
            let request = BottleRequest {
                program: self.request.program.clone(),
                input: BottleInput::InstallTree(tree),
                version: self.request.version.clone(),
                build: 1,
                triple: self.request.triple,
                strip_components: 0,
                dep_bottles: vec![],
                donor_roots: vec![],
                merge_from: None,
                key: None,
                out: self.request.out.clone(),
                signing: Signing::Unsigned,
                upload: false,
            };
            let package = super::super::pack_bottle_with(
                &ctx,
                &request,
                &FakeBackend::default(),
                &RawFrames::WHOLE,
            )
            .expect("package");
            let manifest = crate::lane::rows::local(&package.manifest).expect("manifest");
            let row = manifest
                .artifact_for(request.triple.as_str())
                .expect("target");
            let moved = self.work.path().join("client store with spaces");
            atpkg::verify_for_publication(
                row,
                &package.asset,
                &moved,
                &atpkg::install::StageHooks::NONE,
            )
            .expect("client accepts relocated package");
            (package, moved)
        }
    }

    #[test]
    fn pinned_archive_becomes_unsigned_client_package_with_license_and_provenance() {
        let executable = thin_macho_bytes(0x0100_000c);
        let fixture = Fixture::new(&[
            TarEntry::File("gitleaks", &executable, 0o755),
            TarEntry::File("LICENSE", b"MIT license fixture\n", 0o644),
            TarEntry::File("README.md", b"not installed", 0o644),
        ]);
        let tree = fixture.prepare().expect("pinned tree");
        assert!(!tree.join("README.md").exists());
        let (package, moved) = fixture.pack_and_stage(tree);
        assert!(package.signature.is_none());
        assert!(!package.uploaded);
        assert_eq!(
            std::fs::read(moved.join("bin/gitleaks")).expect("program"),
            executable
        );
        assert_eq!(
            std::fs::read(moved.join("share/licenses/gitleaks/LICENSE")).expect("license"),
            b"MIT license fixture\n"
        );
        let origin: aterm_json::Value = aterm_json::from_slice(
            &std::fs::read(moved.join("share/atpkg/upstream.json")).expect("provenance"),
        )
        .expect("origin JSON");
        assert_eq!(
            origin["sha256"].as_str(),
            Some(fixture.recipe.archives[0].sha256)
        );
        assert_eq!(
            origin["target"].as_str(),
            Some(fixture.request.triple.as_str())
        );
    }

    #[test]
    fn cpython_archive_preserves_the_runtime_and_interpreter_aliases() {
        let executable = thin_macho_bytes(0x0100_000c);
        let pin = aterm_uds::host::upstream::CPYTHON;
        let omitted: Vec<_> = pin
            .darwin_omit
            .iter()
            .map(|path| format!("python/{path}"))
            .collect();
        let mut entries = vec![
            TarEntry::File("python/bin/python3.14", &executable, 0o755),
            TarEntry::Symlink("python/bin/python3", "python3.14"),
            TarEntry::Symlink("python/bin/python", "python3.14"),
            TarEntry::File("python/lib/python3.14/LICENSE.txt", b"PSF license\n", 0o644),
            TarEntry::File(
                "python/lib/python3.14/tomllib/__init__.py",
                b"# stdlib\n",
                0o644,
            ),
            TarEntry::File(
                "python/lib/python3.14/lib-dynload/_ssl.so",
                &executable,
                0o755,
            ),
        ];
        entries.extend(
            omitted
                .iter()
                .map(|path| TarEntry::File(path, b"Windows-only launcher", 0o644)),
        );
        let mut fixture = Fixture::new(&entries);
        fixture.request.program = pin.name.into();
        fixture.request.version = pin.version.into();
        fixture.recipe = PinnedTool {
            archives: fixture.recipe.archives,
            ..pin
        };
        let tree = fixture.prepare().expect("Python runtime tree");
        let (_, moved) = fixture.pack_and_stage(tree);
        for name in ["python3.14", "python3", "python"] {
            assert_eq!(
                std::fs::read(moved.join("bin").join(name)).unwrap(),
                executable
            );
        }
        assert_eq!(
            std::fs::read(moved.join("lib/python3.14/tomllib/__init__.py")).unwrap(),
            b"# stdlib\n"
        );
        assert_eq!(
            std::fs::read(moved.join("lib/python3.14/lib-dynload/_ssl.so")).unwrap(),
            executable
        );
        assert_eq!(
            std::fs::read(moved.join("lib/python3.14/LICENSE.txt")).unwrap(),
            b"PSF license\n"
        );
        let origin: aterm_json::Value = aterm_json::from_slice(
            &std::fs::read(moved.join("share/atpkg/upstream.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(origin["version"].as_str(), Some(pin.version));
        assert_eq!(
            origin["sha256"].as_str(),
            Some(fixture.recipe.archives[0].sha256)
        );
        assert_eq!(
            origin.get("omitted"),
            Some(&aterm_json::json!(pin.darwin_omit))
        );
        for name in pin.darwin_omit {
            assert!(!moved.join(name).exists());
        }
    }

    #[test]
    fn lean_archive_keeps_its_compiler_std_and_licenses_without_a_lean_shim() {
        let exe = thin_macho_bytes(0x0100_000c);
        let pin = aterm_uds::host::upstream::LEAN4;
        let mut fixture = Fixture::new(&[
            TarEntry::File("lean/bin/lake", &exe, 0o755),
            TarEntry::File("lean/bin/lean", &exe, 0o755),
            TarEntry::File("lean/lib/lean/Std/Tactic/BVDecide.olean", b"Std", 0o644),
            TarEntry::File("lean/lib/lean/libleanrt.a", b"runtime", 0o644),
            TarEntry::File("lean/LICENSE", b"Apache", 0o644),
            TarEntry::File("lean/LICENSES", b"runtime licenses", 0o644),
        ]);
        fixture.request.program = pin.name.into();
        fixture.request.version = pin.version.into();
        fixture.recipe = PinnedTool {
            archives: fixture.recipe.archives,
            ..pin
        };
        let tree = fixture.prepare().unwrap();
        let (out, moved) = fixture.pack_and_stage(tree);
        assert_eq!(
            crate::lane::rows::local(&out.manifest).unwrap().exposes,
            ["lake"]
        );
        assert_eq!(std::fs::read(moved.join("bin/lean")).unwrap(), exe);
        assert_eq!(
            std::fs::read(moved.join("lib/lean/Std/Tactic/BVDecide.olean")).unwrap(),
            b"Std"
        );
        assert_eq!(
            std::fs::read(moved.join("lib/lean/libleanrt.a")).unwrap(),
            b"runtime"
        );
        for name in pin.licenses {
            assert!(moved.join(name).is_file());
            assert!(moved.join("share/licenses/lean4").join(name).is_file());
        }
        assert!(can_pack(&pin, X86_64_APPLE_DARWIN).is_err());
    }

    #[test]
    fn each_pinned_recipe_keeps_its_own_extraction_bound() {
        let exe = thin_macho_bytes(0x0100_000c);
        let mut fixture = Fixture::new(&[
            TarEntry::File("gitleaks", &exe, 0o755),
            TarEntry::File("LICENSE", b"license", 0o644),
        ]);
        fixture.recipe.max_extract_bytes = 1;
        assert!(
            fixture
                .prepare()
                .unwrap_err()
                .to_string()
                .contains("unpack pinned upstream archive")
        );
        for pin in [
            aterm_uds::host::upstream::GITLEAKS,
            aterm_uds::host::upstream::CPYTHON,
            aterm_uds::host::upstream::OPENSSL_SIGNING,
        ] {
            assert_eq!(
                pin.max_extract_bytes,
                super::super::MAX_BOTTLE_EXTRACT_BYTES
            );
        }
    }

    #[test]
    fn cpython_probe_rejects_wrong_versions_external_paths_and_no_tls_roots() {
        let prefix = Path::new("/fixture/runtime");
        let executable = prefix.join("bin/python3.14");
        let good = aterm_json::json!({
            "version": "3.14.8", "prefix": prefix, "base_prefix": prefix,
            "executable": executable, "stdlib": "/fixture/runtime/lib/python3.14",
            "ssl_ca_count": 1,
        });
        let version = aterm_uds::host::upstream::CPYTHON.version;
        validate_python_probe(&good, version, prefix, prefix, &executable).unwrap();
        for (field, value) in [
            ("version", aterm_json::json!("3.9.0")),
            ("prefix", aterm_json::json!("/outside")),
            ("base_prefix", aterm_json::json!("/outside")),
            ("executable", aterm_json::json!("/usr/bin/python3")),
            ("stdlib", aterm_json::json!("/fixture/runtime/../outside")),
            ("ssl_ca_count", aterm_json::json!(0)),
        ] {
            let mut bad = good.clone();
            bad.as_object_mut().unwrap().insert(field.into(), value);
            assert!(
                validate_python_probe(&bad, version, prefix, prefix, &executable).is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn cpython_precompile_report_requires_the_pinned_version_and_hash_transform() {
        let good = aterm_json::json!({
            "version": "3.14.8", "implementation": "cpython",
            "invalidation": "checked-hash", "filenames": "relative-to-runtime-root",
            "optimize": [0, 1, 2], "hash_seed": 0, "pyc_files": 3, "source_files": 1,
            "compiler_sha256": "a".repeat(64), "source_tree_sha256": "b".repeat(64),
            "magic_number": "01020304",
        });
        validate_precompile_report(&good, "3.14.8").unwrap();
        for (field, value) in [
            ("version", aterm_json::json!("3.13.0")),
            ("invalidation", aterm_json::json!("timestamp")),
            ("hash_seed", aterm_json::json!(1)),
            ("source_tree_sha256", aterm_json::json!("missing")),
        ] {
            let mut bad = good.clone();
            bad.as_object_mut().unwrap().insert(field.into(), value);
            assert!(
                validate_precompile_report(&bad, "3.14.8").is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn corrupt_size_or_hash_refuses_before_extraction() {
        for wrong_size in [false, true] {
            let mut fixture = Fixture::new(&[]);
            let mut archive = fixture.recipe.archives[0];
            if wrong_size {
                archive.bytes += 1;
            } else {
                archive.sha256 = "bad hash";
            }
            fixture.recipe.archives = Box::leak(Box::new([archive]));
            let error = fixture
                .prepare()
                .expect_err("integrity refusal")
                .to_string();
            assert!(
                error.contains(if wrong_size { "bytes" } else { "SHA-256" }),
                "{error}"
            );
            assert!(!fixture.work.path().join("unpacked").exists());
        }
    }

    #[test]
    fn wrong_pin_or_unavailable_target_refuses_before_download() {
        for wrong_version in [false, true] {
            let mut fixture = Fixture::new(&[]);
            if wrong_version {
                fixture.request.version = "0.0.0".into();
            } else {
                fixture.request.triple = X86_64_APPLE_DARWIN;
            }
            prepare(
                &fixture.request,
                &fixture.recipe,
                fixture.work.path(),
                &|_, _, _| panic!("invalid request must not download"),
            )
            .expect_err("invalid request");
        }
    }

    #[test]
    fn target_license_permissions_and_regular_program_are_required() {
        let arm = thin_macho_bytes(0x0100_000c);
        let x64 = thin_macho_bytes(0x0100_0007);
        for (entries, expected) in [
            (vec![TarEntry::File("gitleaks", &arm, 0o755)], "LICENSE"),
            (
                vec![TarEntry::File("gitleaks", &arm, 0o644)],
                "execute permission",
            ),
            (
                vec![TarEntry::File("gitleaks", &x64, 0o755)],
                "ARCHITECTURE",
            ),
            (
                vec![
                    TarEntry::File("actual", &arm, 0o755),
                    TarEntry::Symlink("gitleaks", "actual"),
                ],
                "regular",
            ),
        ] {
            let fixture = Fixture::new(&entries);
            let error = fixture.prepare().expect_err("layout refusal").to_string();
            assert!(error.contains(expected), "expected {expected}: {error}");
        }
    }

    #[test]
    fn qualification_requires_the_version_clean_scan_and_reported_seeded_finding() {
        for failure in [
            None,
            Some("version"),
            Some("clean"),
            Some("detect"),
            Some("report"),
        ] {
            let work = aterm_tempfile::tempdir().expect("scratch");
            let result = qualify_tree(
                Path::new("gitleaks"),
                "8.30.1",
                work.path(),
                &|_, args, cwd| {
                    let mut code = 0;
                    let mut stdout = Vec::new();
                    if args == ["version"] {
                        stdout.extend_from_slice(if failure == Some("version") {
                            b"0.0.0"
                        } else {
                            b"8.30.1"
                        });
                    } else {
                        let text = std::fs::read_to_string(cwd.join("scan input/fixture.txt"))
                            .expect("input");
                        if text.contains("ALAB_SEEDED") {
                            code = if failure == Some("detect") { 0 } else { 17 };
                            std::fs::write(
                                cwd.join("report.json"),
                                if failure == Some("report") {
                                    "[]"
                                } else {
                                    "[{\"RuleID\":\"qualification\"}]"
                                },
                            )
                            .expect("report");
                        } else if failure == Some("clean") {
                            code = 17;
                        }
                    }
                    Ok(std::process::Output {
                        status: std::process::ExitStatus::from_raw(code << 8),
                        stdout,
                        stderr: Vec::new(),
                    })
                },
            );
            assert_eq!(
                result.is_ok(),
                failure.is_none(),
                "qualification case {failure:?}: {result:?}"
            );
        }
    }
}
