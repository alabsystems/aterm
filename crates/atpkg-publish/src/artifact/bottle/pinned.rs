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
mod pkgconf;

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
        "gitleaks" | "cpython" | "openssl-signing" | "lean4" | "node" | "pkgconf"
    ) {
        return Err(Error::cannot_run(format!(
            "{} has no pinned runtime qualifier",
            recipe.name
        )));
    }
    let archive = recipe.archive_for(platform(triple)).ok_or_else(|| {
        Error::cannot_run(format!(
            "{} has no pinned upstream archive for {triple}",
            recipe.name
        ))
    })?;
    let environment: Vec<String> = recipe
        .shim_env
        .iter()
        .map(|entry| (*entry).to_owned())
        .collect();
    let environment = atpkg::shim_env::ShimEnv::admit(&environment).map_err(Error::refused)?;
    for (name, value) in environment.entries() {
        if name == "LEAN_CC" && !atpkg::toolchain::is_executable_file(Path::new(value)) {
            return Err(Error::cannot_run(
                "this pinned Lean runtime requires an available host C compiler",
            ));
        }
    }
    let url = archive.url_at(recipe.version);
    let archive_path = archive_file(&url)?;
    if recipe.build == Build::Pkgconf && url.ends_with(".tar.xz") {
        pkgconf::source_decoder()?;
    } else {
        super::archive_container(archive_path)?;
    }
    let (_, bottle) = super::bottle_row(recipe.name, triple)?;
    crate::artifact::pkgdoc::check_exposes(
        recipe.name,
        &bottle
            .exposes
            .iter()
            .map(|name| (*name).into())
            .collect::<Vec<_>>(),
    )?;
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

fn archive_file(url: &str) -> Result<&Path> {
    let filename = url
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| Error::refused("upstream archive URL has no filename"))?;
    relative(filename)
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

pub(super) fn invocation_environment(
    bottle: &crate::programs::BottleDef,
    version: &str,
) -> Vec<String> {
    let crate::programs::BottleOrigin::Pinned(recipe) = bottle.origin else {
        return Vec::new();
    };
    // Generic install-tree packing can carry another independently prepared
    // version. Only a registered recipe supplies additional invocation state;
    // the pack-upstream route admits its exact version before preparing anything.
    recipe.versioned(version).map_or_else(Vec::new, |recipe| {
        recipe
            .shim_env
            .iter()
            .map(|value| (*value).to_owned())
            .collect()
    })
}

fn installed_executable(recipe: &PinnedTool) -> Result<PathBuf> {
    match recipe.layout {
        Layout::Executable => Ok(Path::new("bin").join(recipe.name)),
        Layout::RuntimeTree => Ok(relative(recipe.executable)?.to_path_buf()),
    }
}

/// Add only the catalog's admitted exposure names beside a single executable.
/// Relative links keep the upstream name available inside its managed runtime
/// without making a reserved interpreter name a global shim.
fn expose_executable(stage: &Path, recipe: &PinnedTool, triple: Triple) -> Result<()> {
    if !matches!(recipe.layout, Layout::Executable) {
        return Ok(());
    }
    let (_, bottle) = super::bottle_row(recipe.name, triple)?;
    let executable = installed_executable(recipe)?;
    let target = executable
        .file_name()
        .ok_or_else(|| Error::refused("pinned executable has no file name"))?;
    for exposed in bottle.exposes {
        let name = atpkg::store::ToolName::new(exposed)
            .ok_or_else(|| Error::refused("pinned exposure is not an admitted shim name"))?;
        let alias = Path::new("bin").join(name.as_str());
        if alias != executable {
            crate::artifact::bundle::symlink(Path::new(target), &stage.join(alias))?;
        }
    }
    Ok(())
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
    let input = work.join(archive_file(&url)?);
    let source_xz = recipe.build == Build::Pkgconf && url.ends_with(".tar.xz");
    let container = if source_xz {
        atpkg::extract::Container::Tar
    } else {
        super::archive_container(&input)?
    };
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
    // Only this closed source recipe needs xz. Authenticate the original bytes
    // first, then feed its bounded tar to the existing vetted extractor.
    let input = if source_xz {
        pkgconf::unpack_source(&input, work, recipe.max_extract_bytes)?
    } else {
        input
    };
    let unpacked = work.join("unpacked");
    atpkg::extract::extract_tree(
        &input,
        &unpacked,
        container,
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
        Build::Pkgconf => Some(pkgconf::build(&unpacked, work)?),
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
    expose_executable(&stage, recipe, request.triple)?;
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
    // An upstream published as a GitHub release asset rides the download host's outages on
    // the API route; any other upstream is read from its own host.
    let reader = aterm_update_core::release_asset::Reader::anonymous();
    let tree = prepare(request, recipe, work.path(), &|url, path, cap| {
        reader
            .to_file_at(url, path, cap)
            .map_err(|e| Error::cannot_run(e.to_string()))
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
    let expected: Vec<String> = recipe
        .shim_env
        .iter()
        .map(|entry| (*entry).to_owned())
        .collect();
    if manifest.shim_env != expected {
        return Err(Error::refused(
            "qualified invocation environment differs from its recipe",
        ));
    }
    let environment =
        atpkg::shim_env::ShimEnv::admit(&manifest.shim_env).map_err(Error::refused)?;
    let entries: Vec<_> = environment
        .entries()
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let invoke =
        |program: &Path, args: &[&str], cwd: &Path| run_with_env(program, args, cwd, &entries);
    match recipe.name {
        "gitleaks" => qualify_tree(&program, &result.version, work.path(), &run),
        "cpython" => qualify_python(&program, &result.version, &stage, work.path(), &run),
        "openssl-signing" => openssl::qualify(&program, &result.version, work.path(), &run),
        "pkgconf" => {
            pkgconf::qualify(&program, &result.version, work.path(), &run)?;
            pkgconf::qualify(
                &stage.join("bin/pkg-config"),
                &result.version,
                work.path(),
                &run,
            )
        }
        "lean4" => lean::qualify(&program, &result.version, work.path(), &invoke),
        "node" => {
            node::qualify(&program, &result.version, work.path(), &run)?;
            let (_, bottle) = super::bottle_row(recipe.name, result.triple)?;
            for exposed in bottle.exposes {
                let alias = stage.join("bin").join(exposed);
                if alias != program {
                    node::qualify(&alias, &result.version, work.path(), &run)?;
                }
            }
            Ok(())
        }
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
#[path = "pinned_tests.rs"]
mod tests;
