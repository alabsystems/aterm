// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `agh auth import NAME --from-gh` (design §6.7): the token comes from `gh auth token`, run
//! as a child through `Fx::child`. agh never reads a keychain.

use std::path::Path;

use aterm_uds::secret::Secret;

use crate::client::{Github, Meter};
use crate::config::Dirs;
use crate::error::{Error, Kind, Result, SideEffect, Sub};
#[cfg(any(test, debug_assertions))]
use crate::fx::Mode;
use crate::fx::{
    Fx,
    child::{ChildOut, ChildSpec},
};
use crate::identity::EnvSource;
use crate::identity::provision::{
    AddFrom, AddOptions, AddRequest, Added, add_with_metered, refuse_loose_stdin, refuse_off_unix,
};

/// The absolute paths gh is looked for at, in order.
pub const GH_ABSOLUTE: [&str; 3] = ["/opt/homebrew/bin/gh", "/usr/local/bin/gh", "/usr/bin/gh"];

/// The environment variables passed to gh: exactly these, when set.
pub const GH_ENV: [&str; 5] = ["HOME", "PATH", "USER", "LOGNAME", "XDG_CONFIG_HOME"];

/// The bound on the one gh call: 60 s, aterm-machines' budget for the same call.
pub const GH_BUDGET: std::time::Duration = std::time::Duration::from_secs(60);

/// What `auth import` asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ImportRequest {
    /// `--user LOGIN`: the gh account. `None`: gh's active account.
    pub user: Option<String>,
    /// `auth add`'s request: name, tier, owners, write, drafts. Its `from` is ignored:
    /// the token comes from gh, as `AddFrom::Stdin`.
    pub add: AddRequest,
}

/// Run `gh auth token --hostname github.com [--user LOGIN]` through `gh.fx()`'s child door
/// and provision its answer as `auth add --stdin` would, through `add_with`:
/// `<config>/agh/tokens/NAME.token` (0600), the login bound with `GET /user`, agh.toml
/// edited. The answer is held in a `Secret` and never printed or shown. It runs with or
/// without a terminal, as `auth add` does (owner ruling 2026-10-26).
///
/// # Errors
///
/// - CouldNotRun, next `agh auth add NAME --file PATH --tier TIER`, when no gh exists at
///   `GH_ABSOLUTE`, and the child's could_not_run with that next when gh cannot start or
///   does not finish within `GH_BUDGET`;
/// - Unauthenticated, next the same, when gh exits non-zero. Its stderr is never quoted,
///   because gh's stderr may hold a hint with an account name, and the reason is the exit
///   status;
/// - Invalid when gh's stdout is not one token line of the shape `auth add` takes (20–255
///   printable ASCII bytes);
/// - the provisioning errors of [`super::provision::add_with`]. Its Usage for
///   `options.legacy_loose` (the token comes as stdin), and its Refused off unix, are
///   answered before gh is looked for.
pub fn import_from_gh(
    gh: &Github,
    dirs: &Dirs,
    env: &dyn EnvSource,
    req: &ImportRequest,
    options: AddOptions,
) -> Result<Added> {
    import_from_gh_metered(gh, dirs, env, req, options).map(|(added, _)| added)
}

/// `import_from_gh`, with the one-profile client's meter.
///
/// # Errors
///
/// As [`import_from_gh`].
pub(crate) fn import_from_gh_metered(
    gh: &Github,
    dirs: &Dirs,
    env: &dyn EnvSource,
    req: &ImportRequest,
    options: AddOptions,
) -> Result<(Added, Meter)> {
    // The token comes as stdin, which `--legacy-loose` refuses: refused before gh is asked
    // for a token that would only be thrown away.
    refuse_loose_stdin(options, &AddFrom::Stdin)?;
    // Off unix `auth add` refuses whatever the token: gh is not looked for there.
    refuse_off_unix()?;
    let next = file_next(&req.add);
    let Some(program) = program(gh.fx()) else {
        return Err(
            Error::local(format!("gh is not installed at {}", GH_ABSOLUTE.join(", ")))
                .with_next(next),
        );
    };
    let mut spec = ChildSpec::new(Path::new(program))
        .arg("auth")
        .arg("token")
        .arg("--hostname")
        .arg("github.com")
        .budget(GH_BUDGET);
    if let Some(login) = &req.user {
        spec = spec.arg("--user").arg(login);
    }
    for name in GH_ENV {
        if let Some(value) = env.var_os(name) {
            spec = spec.env(name, value);
        }
    }
    // `gh auth token` only reads: a run that failed changed nothing on GitHub.
    let mut out = gh.fx().child(&spec).map_err(|error| {
        let error = error.with_side_effect(SideEffect::None);
        if error.sub == Sub::Local {
            error.with_next(next.clone())
        } else {
            error
        }
    })?;
    let token = token_line(&out);
    aterm_uds::secret::scrub(&mut out.stdout);
    aterm_uds::secret::scrub(&mut out.stderr);
    let token = match (out.code, token) {
        (Some(0), Some(token)) => token,
        (Some(0), None) => {
            return Err(
                Error::new(Kind::Invalid, "gh auth token printed no single token line")
                    .with_next(next),
            );
        }
        (Some(code), _) => {
            return Err(Error::new(
                Kind::Unauthenticated,
                format!("gh auth token exited {code}"),
            )
            .with_next(next));
        }
        (None, _) => {
            return Err(Error::new(
                Kind::Unauthenticated,
                "gh auth token was killed by a signal",
            )
            .with_next(next));
        }
    };
    let add = AddRequest {
        from: AddFrom::Stdin,
        ..req.add.clone()
    };
    let mut stdin = std::io::Cursor::new(token.expose());
    add_with_metered(gh, dirs, &add, options, &mut stdin)
}

/// The `auth add --file` a person or an agent runs where gh cannot answer.
fn file_next(add: &AddRequest) -> String {
    format!(
        "agh auth add {} --file PATH --tier {}",
        add.name,
        add.tier.slug()
    )
}

/// The gh to run: under a simulated `Fx`, `GH_ABSOLUTE[0]`, so a test's scripted child
/// matches on every host; otherwise the first of `GH_ABSOLUTE` that is a file (a symlink
/// followed, as Homebrew's `bin/gh` is one).
fn program(fx: &Fx) -> Option<&'static str> {
    if simulated(fx) {
        return GH_ABSOLUTE.first().copied();
    }
    GH_ABSOLUTE
        .into_iter()
        .find(|path| std::fs::metadata(path).is_ok_and(|meta| meta.is_file()))
}

/// Whether `fx` is a test's simulated seam.
#[cfg(any(test, debug_assertions))]
fn simulated(fx: &Fx) -> bool {
    fx.mode() == Mode::Simulated
}

/// A release build has no simulated seam.
#[cfg(not(any(test, debug_assertions)))]
fn simulated(fx: &Fx) -> bool {
    let _ = fx;
    false
}

/// gh's stdout as one token line, moved into a `Secret`: at most one trailing `\n` or `\r\n`
/// cut, and what is left a token of the shape provisioning takes (`identity::token_bytes`:
/// 20–255 bytes, each printable ASCII, so no whitespace or control byte).
fn token_line(out: &ChildOut) -> Option<Secret> {
    crate::identity::token_bytes(&out.stdout)
        .ok()
        .map(Secret::new)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::{
        AddFrom, AddOptions, AddRequest, ChildOut, GH_ABSOLUTE, ImportRequest, import_from_gh,
        token_line,
    };
    use crate::client::GithubBuilder;
    use crate::config::Dirs;
    use crate::error::Kind;
    use crate::fx::Fx;
    use crate::fx::child::ScriptedChild;
    use crate::identity::MapEnv;
    use crate::policy::{Router, Tier};
    use crate::wire::Endpoints;
    use crate::wire::scripted::ScriptedTransport;

    #[test]
    fn legacy_loose_is_refused_before_gh_runs() {
        let token = agh_fake::token("loose-import");
        let fx = Arc::new(
            Fx::simulated(1_760_000_000).with_children(vec![ScriptedChild {
                program: PathBuf::from(GH_ABSOLUTE[0]),
                args: ["auth", "token", "--hostname", "github.com"]
                    .map(str::to_string)
                    .to_vec(),
                out: out(format!("{token}\n").as_bytes()),
            }]),
        );
        // A transport with nothing scripted, at loopback ports nothing listens on: a call
        // this test should never make fails here and reaches no network.
        let gh = GithubBuilder::new(Router::empty())
            .endpoints(Endpoints::loopback(1, 1, 1, 1))
            .transport(Arc::new(ScriptedTransport::new()))
            .fx(fx.clone())
            .build();
        let req = ImportRequest {
            user: None,
            add: AddRequest {
                name: "dev2".to_string(),
                from: AddFrom::Stdin,
                tier: Tier::Dev,
                owners: vec!["fixture-dev".to_string()],
                write: false,
                drafts: false,
            },
        };
        // Nothing is read or written before the refusal, so the scratch root stays empty.
        let root = crate::test_fixtures::scratch_dir("import-loose");
        let dirs = Dirs::under(root.path());
        let error = import_from_gh(
            &gh,
            &dirs,
            &MapEnv::new(),
            &req,
            AddOptions::default().legacy_loose(true),
        )
        .expect_err("--legacy-loose takes a --file");
        assert_eq!(error.kind, Kind::Usage);
        assert!(fx.children_run().is_empty(), "{:?}", fx.children_run());
        let made: Vec<_> = std::fs::read_dir(root.path())
            .expect("the scratch root")
            .collect();
        assert!(made.is_empty(), "{made:?}");
    }

    fn out(stdout: &[u8]) -> ChildOut {
        ChildOut {
            code: Some(0),
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
        }
    }

    #[test]
    fn one_line_with_at_most_one_newline_is_a_token() {
        let token = agh_fake::token("line");
        for stdout in [token.clone(), format!("{token}\n"), format!("{token}\r\n")] {
            let secret = token_line(&out(stdout.as_bytes())).expect("one token line");
            assert_eq!(secret.expose(), token.as_bytes());
        }
        for stdout in [
            String::new(),
            "\n".to_string(),
            format!("{token}\n\n"),
            format!("{token} x\n"),
            format!("{token}\tx"),
            format!("a\n{token}\n"),
            format!("{token}\u{7f}"),
            // The shapes `auth add` refuses: under 20 bytes, over 255, and a byte past ASCII.
            "short-line\n".to_string(),
            "g".repeat(300),
            format!("{token}\u{e9}"),
        ] {
            assert!(token_line(&out(stdout.as_bytes())).is_none(), "{stdout:?}");
        }
    }
}
