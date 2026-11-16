// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `spec-check`: the obligations only the Trust checkers discharge
//! ([`aterm_spec::check`]), over every model `aterm_models::link_all` links.

use std::sync::Arc;

fn worker_executor(pool: Arc<aterm_capacity::Jobserver>) -> Arc<aterm_spec::check::WorkerExecutor> {
    Arc::new(move |worker, stopped, run| {
        if worker == 0 {
            // Cargo retains this job through the child's entire lifetime.
            run();
        } else if let Ok(_token) = pool.acquire(stopped) {
            run();
        }
        // Declined auxiliaries claim no items. The inherited worker keeps draining the queue;
        // a panic ends the failed scope and interrupts outstanding auxiliary admission.
    })
}

fn main() {
    let pool = match std::env::var_os(aterm_capacity::IMPLICIT_POOL_ENV) {
        None => None,
        Some(value) => match aterm_capacity::Jobserver::adopt(&value) {
            Ok(pool) => Some(Arc::new(pool)),
            Err(why) => {
                eprintln!("spec-check: could not run: shared CPU pool: {why}");
                std::process::exit(3);
            }
        },
    };
    let _domains = aterm_models::link_all();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let run = || aterm_spec::check::main(&args);
    let code = match pool {
        Some(pool) => aterm_spec::check::with_worker_executor(worker_executor(pool), run),
        None => run(),
    };
    std::process::exit(code);
}

#[cfg(all(test, unix))]
#[path = "spec-check/spec-check_tests.rs"]
mod tests;
