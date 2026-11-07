// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::cli::RenewalExit;
use std::cell::RefCell;
use std::os::unix::fs::{PermissionsExt as _, symlink};

const EMAIL: &str = "approved@example.test";
const INPUT: &[u8] = br#"{"schema":1,"kind":"mailbox","target":"approved@example.test","transport":"https","endpoint":"https://mail.example.test/inbox","bearer_token":"synthetic-bound-secret"}"#;

fn environment(home: &Path) -> Env {
    Env {
        state: home.join("state"),
        aterm_state: None,
        cwd: home.to_owned(),
        home: Some(home.to_owned()),
        claude_dir: None,
        claude_config_unusable: None,
        claude_profile: None,
        claude_custom_oauth: false,
        claude_session: None,
        claude_pid: None,
        codex_thread: None,
        codex_home: None,
        codex_login: None,
        agy_conversation: None,
        now: 1000,
        sid: String::new(),
        sock: None,
        config: None,
        claude_tmp: None,
        claude_running: None,
        tz_s: 0,
    }
}

struct Host {
    failed: bool,
    outcome: Option<RenewalExit>,
    seen: RefCell<Option<PathBuf>>,
}

impl AccountsHost for Host {
    fn now(&self) -> i64 {
        1000
    }
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("no auth read")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("no ordinary login")
    }
    fn renew_flow(&self, _: &str, _: &str, _: &RenewBrowser) -> Result<i32, String> {
        panic!("no ambient mailbox fallback")
    }
    fn renew_flow_with_inputs(
        &self,
        staging: &str,
        email: &str,
        _: &RenewBrowser,
        input: &Path,
    ) -> Result<i32, String> {
        assert_eq!(email, EMAIL);
        assert!(input.starts_with(staging));
        let parsed = MailboxInputs::read_staged(input, email).unwrap();
        assert_eq!(
            parsed.mailbox().bearer_token.as_deref(),
            Some("synthetic-bound-secret")
        );
        assert_eq!(
            std::fs::metadata(input).unwrap().permissions().mode() & 0o777,
            0o600
        );
        self.seen.replace(Some(input.to_owned()));
        if self.failed {
            Err("provider outcome unknown".into())
        } else {
            Ok(self.outcome.as_ref().map_or(0, |exit| exit.code))
        }
    }
    fn renew_outcome_with_inputs(
        &self,
        staging: &str,
        email: &str,
        browser: &RenewBrowser,
        input: &Path,
    ) -> Result<RenewalExit, crate::harness::cli::RenewalFailure> {
        let code = self.renew_flow_with_inputs(staging, email, browser, input)?;
        Ok(self.outcome.clone().unwrap_or(RenewalExit::from(code)))
    }
}

#[test]
fn shared_claude_mailbox_uses_exact_private_input_and_cleans_every_outcome() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let env = environment(temp.path());
    let store = accounts::Store::at_for(&env.state, Cli::Claude, temp.path());
    let row = accounts::Row {
        email: Some(EMAIL.into()),
        ..Default::default()
    };
    let inputs = MailboxInputs::decode(INPUT, EMAIL).unwrap();
    let browser = RenewBrowser::Remote {
        opener: Some(temp.path().join("opener")),
    };
    for (failed, outcome) in [
        (false, None),
        (true, None),
        (
            false,
            Some(RenewalExit {
                code: 1,
                met: Some(crate::harness::open_url::Met::new(
                    crate::harness::open_url::MetWord::BlockPage,
                    Some("claude.ai"),
                )),
            }),
        ),
    ] {
        let browser_failed = outcome.is_some();
        let host = Host {
            failed,
            outcome,
            seen: RefCell::new(None),
        };
        let context = Context {
            publication: None,
            admission: None,
            env: &env,
            store: &store,
            host: &host,
        };
        let result =
            ClaudeOauth.execute_attempt_with_inputs(&context, &row, temp.path(), &browser, &inputs);
        assert!(if failed || browser_failed {
            matches!(&result, Execution::Unknown(_))
        } else {
            matches!(&result, Execution::Completed)
        });
        if browser_failed {
            let Execution::Unknown(why) = result else {
                unreachable!();
            };
            assert!(why.contains("Cloudflare"), "{why}");
            assert!(why.contains("claude.ai"), "{why}");
            assert!(!why.contains("synthetic-bound-secret"), "{why}");
        }
        let path = host.seen.borrow().clone().unwrap();
        assert!(!path.exists());
        assert!(!path.parent().unwrap().exists());
    }
    let host = Host {
        failed: false,
        outcome: None,
        seen: RefCell::new(None),
    };
    let context = Context {
        publication: None,
        admission: None,
        env: &env,
        store: &store,
        host: &host,
    };
    assert!(matches!(
        ClaudeOauth.execute_attempt_with_inputs(
            &context,
            &row,
            temp.path(),
            &RenewBrowser::Default,
            &inputs,
        ),
        Execution::NotStarted(_)
    ));
    assert!(host.seen.borrow().is_none());
}

#[test]
fn shared_mailbox_read_refuses_changed_target_links_open_files_and_missing_inputs() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let staged = MailboxInputs::decode(INPUT, EMAIL)
        .unwrap()
        .stage_in(temp.path())
        .unwrap();
    assert!(MailboxInputs::read_staged(staged.path(), "someone-else@example.test").is_err());
    std::fs::set_permissions(staged.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(MailboxInputs::read_staged(staged.path(), EMAIL).is_err());
    std::fs::set_permissions(staged.path(), std::fs::Permissions::from_mode(0o600)).unwrap();
    let source = temp.path().join("source");
    std::fs::rename(staged.path(), &source).unwrap();
    symlink(&source, staged.path()).unwrap();
    assert!(MailboxInputs::read_staged(staged.path(), EMAIL).is_err());
    std::fs::remove_file(staged.path()).unwrap();
    assert!(MailboxInputs::read_staged(staged.path(), EMAIL).is_err());
    std::fs::hard_link(&source, staged.path()).unwrap();
    assert!(MailboxInputs::read_staged(staged.path(), EMAIL).is_err());
}

mod vendor {
    use super::*;
    use crate::ai::connector::{Flow, Region};
    use crate::harness::{codex_alab, headless_oauth::MailboxConfig, login_drive};

    struct Inert {
        codex: PathBuf,
    }

    impl AccountsHost for Inert {
        fn now(&self) -> i64 {
            1000
        }
        fn stdin_is_terminal(&self) -> bool {
            false
        }
        fn auth_status(&self, _: &str) -> Result<Captured, String> {
            panic!("mailbox routing must not run an auth command")
        }
        fn unaskable(&self) -> Option<String> {
            None
        }
        fn login(&self, _: Option<&str>) -> Result<i32, String> {
            panic!("mailbox routing must not run an ordinary login")
        }
        fn codex_launch_program(&self) -> Option<PathBuf> {
            Some(self.codex.clone())
        }
        fn codex_sign_in_program(&self) -> Option<PathBuf> {
            Some(self.codex.clone())
        }
        fn vendor_sign_in(&self, _: &login_drive::Request<'_>) -> login_drive::Drove {
            panic!("approved mailbox inputs must not fall back to ambient sign-in")
        }
        fn machine_var(&self, _: &str) -> Option<OsString> {
            panic!("an injected mailbox route must not inspect the machine environment")
        }
    }

    struct Injected<'a> {
        inert: &'a Inert,
        home: &'a Path,
        staging: &'a Path,
        mailbox: &'a MailboxConfig,
        calls: RefCell<Vec<Cli>>,
        outcome: u8,
    }

    impl AccountsHost for Injected<'_> {
        fn now(&self) -> i64 {
            self.inert.now()
        }
        fn stdin_is_terminal(&self) -> bool {
            self.inert.stdin_is_terminal()
        }
        fn auth_status(&self, login: &str) -> Result<Captured, String> {
            self.inert.auth_status(login)
        }
        fn unaskable(&self) -> Option<String> {
            self.inert.unaskable()
        }
        fn login(&self, dir: Option<&str>) -> Result<i32, String> {
            self.inert.login(dir)
        }
        fn codex_launch_program(&self) -> Option<PathBuf> {
            self.inert.codex_launch_program()
        }
        fn codex_sign_in_program(&self) -> Option<PathBuf> {
            self.inert.codex_sign_in_program()
        }
        fn vendor_sign_in(&self, request: &login_drive::Request<'_>) -> login_drive::Drove {
            self.inert.vendor_sign_in(request)
        }
        fn machine_var(&self, name: &str) -> Option<OsString> {
            self.inert.machine_var(name)
        }
        fn vendor_sign_in_with_mailbox(
            &self,
            request: &login_drive::Request<'_>,
            home: &Path,
            mailbox: MailboxConfig,
        ) -> login_drive::Drove {
            assert_eq!(home, self.home);
            assert_eq!(request.staging, self.staging);
            assert_eq!(request.row.expected_identity_email(), Some(EMAIL));
            assert_eq!(request.store.cli(), request.cli);
            assert!(
                mailbox == *self.mailbox,
                "the approved mailbox must stay exact"
            );
            match request.cli {
                Cli::Codex => assert!(request.hosts.contains(&"auth.openai.com")),
                Cli::Kimi => {
                    assert_eq!(request.row.vendor.region, Some(Region::Global));
                    assert!(request.hosts.contains(&"auth.kimi.ai"));
                }
                _ => panic!("only the approved Codex and Kimi routes belong here"),
            }
            self.calls.borrow_mut().push(request.cli);
            login_drive::Drove {
                outcome: match self.outcome {
                    0 => login_drive::Outcome::NotRun("injected mailbox refusal".into()),
                    1 => login_drive::Outcome::Failed,
                    _ => login_drive::Outcome::SignedIn {
                        email: Some(EMAIL.into()),
                        page: true,
                    },
                },
                host: None,
            }
        }
    }

    fn row(cli: Cli) -> accounts::Row {
        accounts::Row {
            label: format!("approved-{}", cli.id()),
            email: Some(EMAIL.into()),
            vendor: accounts::Vendor {
                region: (cli == Cli::Kimi).then_some(Region::Global),
                flow: (cli == Cli::Codex).then_some(Flow::Device),
            },
            ..Default::default()
        }
    }

    fn execute(
        cli: Cli,
        context: &Context<'_>,
        row: &accounts::Row,
        staging: &Path,
        inputs: &MailboxInputs,
    ) -> Execution {
        match cli {
            Cli::Codex => CodexOauth.execute_attempt_with_inputs(context, row, staging, inputs),
            Cli::Kimi => KimiOauth.execute_attempt_with_mailbox(
                context,
                row,
                staging,
                Some(&inputs.mailbox()),
            ),
            _ => panic!("closed fixture routes"),
        }
    }

    fn fixture() -> (aterm_tempfile::TempDir, Env, Inert) {
        let temp = aterm_tempfile::tempdir().unwrap();
        let env = environment(temp.path());
        // This only supplies bytes for Codex's admission check. It is never executed.
        let codex = codex_alab::lay_package(&temp.path().join("codex-package"), true);
        assert!(codex_alab::auth_path_unread(&codex).is_none());
        (temp, env, Inert { codex })
    }

    #[test]
    fn approved_mailbox_routes_keep_their_injected_host_and_inputs() {
        let (temp, env, inert) = fixture();
        let inputs = MailboxInputs::decode(INPUT, EMAIL).unwrap();
        let mailbox = inputs.mailbox();
        for cli in [Cli::Codex, Cli::Kimi] {
            let store = accounts::Store::at_for(&env.state, cli, temp.path());
            let row = row(cli);
            for outcome in 0..3 {
                let host = Injected {
                    inert: &inert,
                    home: temp.path(),
                    staging: temp.path(),
                    mailbox: &mailbox,
                    calls: RefCell::new(Vec::new()),
                    outcome,
                };
                let context = Context {
                    publication: None,
                    admission: None,
                    env: &env,
                    store: &store,
                    host: &host,
                };
                let result = execute(cli, &context, &row, temp.path(), &inputs);
                assert_eq!(host.calls.borrow().as_slice(), [cli]);
                match (outcome, result) {
                    (0, Execution::NotStarted(why)) => assert_eq!(why, "injected mailbox refusal"),
                    (1, Execution::Unknown(_)) | (2, Execution::Completed) => {}
                    _ => panic!("the typed host outcome must be preserved"),
                }
            }
        }
    }

    #[test]
    fn default_hosts_refuse_both_approved_mailbox_routes() {
        let (temp, env, host) = fixture();
        let inputs = MailboxInputs::decode(INPUT, EMAIL).unwrap();
        assert!(!host.native_credential_setup());
        for cli in [Cli::Codex, Cli::Kimi] {
            let store = accounts::Store::at_for(&env.state, cli, temp.path());
            let row = row(cli);
            let context = Context {
                publication: None,
                admission: None,
                env: &env,
                store: &store,
                host: &host,
            };
            assert!(matches!(
                execute(cli, &context, &row, temp.path(), &inputs),
                Execution::NotStarted(_)
            ));
        }
    }
}
