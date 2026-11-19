// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The selected cutter's negative-only formatting prefix, before step 1's expensive work.

use super::*;

pub(super) fn check(real: &Real, commit: &str, prover: &Prover) -> Result<(), InputFinding> {
    let unavailable = |why: String| {
        InputFinding::Unanswered(format!(
            "selected-source formatting could not be judged: {why}"
        ))
    };
    real.require_compiler(commit, prover).map_err(unavailable)?;
    let before = if cfg!(target_os = "macos") {
        Some(real.inputs().map_err(unavailable)?)
    } else {
        None
    };
    let root = crate::input_record::directory(&real.release);
    let _checkout = crate::input_record::claim(&real.release).map_err(unavailable)?;
    crate::input_record::checkout(&real.git(), &root, commit).map_err(unavailable)?;

    let mut environment = verify_environment();
    environment.home.clone_from(&real.home);
    environment.trust_stage2_bin = Some(prover.bin.clone());
    let lock = atpkg::trust_lock::found_for(&root, Some(real.compiler_store.prefix()));
    let tools = aterm_verify::toolchain::Toolchain::discover_with_store_guarded(
        Some(&prover.bin),
        &aterm_verify::toolchain::rustup_home(
            environment.rustup_home.as_deref(),
            &environment.home,
        ),
        Some(real.compiler_store.prefix()),
        &environment.path,
        aterm_verify::toolchain::pinned_channel(&root).as_deref(),
        lock.as_ref(),
        &mut |bin| {
            real.require_compiler(commit, prover)?;
            if bin != prover.bin {
                return Err("the formatter discovery selected another compiler".into());
            }
            Ok(())
        },
    )
    .map_err(unavailable)?;
    if tools.stage2_dir != prover.bin || !tools.have_targo() || tools.trustfmt.is_none() {
        return Err(unavailable(
            "the candidate's held Trust build has no usable formatter".into(),
        ));
    }

    let scratch = aterm_tempfile::tempdir().map_err(|error| unavailable(error.to_string()))?;
    let mut context = aterm_verify::Ctx::new_with_tools(
        root.clone(),
        aterm_verify::Scope::workspace(),
        environment,
        scratch.path().to_owned(),
        tools,
    )
    .with_checks(&[aterm_verify::Check::Fmt])
    .with_memory(aterm_verify::memory::Book::live());
    context.head_at_start = Some(commit.to_owned());
    let mut output = Vec::new();
    let mut log = Vec::new();
    let code = aterm_verify::run(
        &context,
        &mut output,
        &mut log,
        &aterm_verify::Show {
            cone: None,
            log: None,
        },
    )
    .map_err(|error| unavailable(error.to_string()))?;
    real.require_compiler(commit, prover).map_err(unavailable)?;
    crate::release_tools::clean_at(&root, commit).map_err(unavailable)?;
    if before
        .as_ref()
        .is_some_and(|seal| !real.inputs().is_ok_and(|current| current == *seal))
    {
        return Err(unavailable(
            "compiler or SDK inputs changed during formatting".into(),
        ));
    }
    let mut retained = crate::durable::open_append(&real.step_log(Step::Checks), true)
        .map_err(|error| unavailable(error.to_string()))?;
    retained
        .write_all(&log)
        .map_err(|error| unavailable(error.to_string()))?;
    match code {
        0 => {
            say_to(
                &real.transcript(),
                "release: selected-source formatting found no changes; release-input and qualification checks remain owed",
            );
            Ok(())
        }
        aterm_verify::exit::FAILED => Err(InputFinding::Failed(format!(
            "selected-source formatting: {}",
            String::from_utf8_lossy(&output),
        ))),
        _ => Err(unavailable(String::from_utf8_lossy(&output).into_owned())),
    }
}

// The actual formatter fixture needs the release compiler's Unix managed-lease custody.
#[cfg(all(test, unix))]
#[path = "release_steps_format_tests.rs"]
mod tests;
