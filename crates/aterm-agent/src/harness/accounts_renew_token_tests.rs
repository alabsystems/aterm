// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One renewal dispatch owns the checked sign-in and the subsequent token capture.

use super::*;
use crate::harness::cli::RenewBrowser;
use aterm_json::Value;

struct RenewTokenHost {
    token: Host,
    store: Store,
    window: bool,
    flows: RefCell<Vec<(String, String)>>,
}

impl AccountsHost for RenewTokenHost {
    fn stdin_is_terminal(&self) -> bool {
        true
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn on_macos(&self) -> bool {
        true
    }
    fn now(&self) -> i64 {
        i64::try_from(NOW).unwrap()
    }
    fn current_exe(&self) -> Option<PathBuf> {
        std::env::current_exe().ok()
    }
    fn opens_sign_in_window(&self) -> bool {
        self.window
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("renewal must check its staging home, not an ambient login")
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("renewal must use its own sign-in flow")
    }
    fn renew_flow(&self, home: &str, email: &str, browser: &RenewBrowser) -> Result<i32, String> {
        assert_eq!(email, EMAIL);
        assert_eq!(matches!(browser, RenewBrowser::Window { .. }), self.window);
        self.flows
            .borrow_mut()
            .push((home.to_owned(), email.to_owned()));
        std::fs::write(
            Path::new(home).join(".claude.json"),
            format!(r#"{{"oauthAccount":{{"emailAddress":"{email}"}}}}"#),
        )
        .unwrap();
        Ok(0)
    }
    fn status_in_home(&self, home: &str) -> Result<Captured, String> {
        let profile = std::fs::read_to_string(Path::new(home).join(".claude.json")).unwrap();
        let profile: Value = aterm_json::from_str(&profile).unwrap();
        assert_eq!(
            profile
                .get("oauthAccount")
                .and_then(|v| v.get("emailAddress"))
                .and_then(Value::as_str),
            Some(EMAIL)
        );
        Ok(Captured {
            stdout: format!(r#"{{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","email":"{EMAIL}","orgId":"o-1","subscriptionType":"max"}}"#).into_bytes(),
            code: Some(0),
        })
    }
    fn verb_env(&self) -> Vec<(OsString, OsString)> {
        self.token.verb_env()
    }
    fn token_vault(&self) -> Option<Box<dyn TokenVault>> {
        self.token.token_vault()
    }
    fn setup_token_program(&self) -> Option<(Program, String)> {
        assert_eq!(
            self.flows.borrow().len(),
            1,
            "sign-in precedes token capture"
        );
        let logins = self
            .store
            .load_logins(i64::try_from(NOW).unwrap())
            .unwrap()
            .0;
        assert!(
            logins.get("work").is_some(),
            "renewal is committed before minting"
        );
        let released = self.store.signin_now().unwrap();
        assert!(
            released.is_some(),
            "token capture must not retain the sign-in lock"
        );
        drop(released);
        self.token.setup_token_program()
    }
    fn token_hang(&self) -> Duration {
        aterm_spec::wait::MUST_HAPPEN
    }
}

#[test]
fn one_renew_invocation_commits_its_login_and_captures_its_window_token() {
    for (window, mode, minted) in [
        (true, "renew-window-token", true),
        (true, "renew-window-closed", false),
        (false, "success", false),
    ] {
        let scratch = Scratch::new("renew-and-token");
        let lab = Lab::new();
        let world = World::new(mode, false);
        std::fs::write(world.home().join("go"), b"go").unwrap();
        let host = RenewTokenHost {
            token: Host::minting(VaultOf::Real(lab.paths.clone()), &world),
            store: scratch.store(),
            window,
            flows: RefCell::default(),
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let _ = notes();
        let code = journal::with_keyer(keyer, || {
            run_accounts(
                &AccountsCmd::Renew {
                    accounts: vec![EMAIL.into()],
                    due: false,
                    on: None,
                },
                &scratch.env(),
                &host,
                &mut out,
                &mut err,
            )
        });
        assert_eq!(
            code,
            ExitCode::SUCCESS,
            "renewal stands even if token capture fails: {} {}",
            String::from_utf8_lossy(&out),
            String::from_utf8_lossy(&err)
        );
        assert_eq!(host.flows.borrow().len(), 1);
        let logins = scratch
            .store()
            .load_logins(i64::try_from(NOW).unwrap())
            .unwrap()
            .0;
        let login = logins.get("work").expect("committed renewal");
        assert_eq!(login.renewed_at, Some(i64::try_from(NOW).unwrap()));
        assert!(login.renewal_generation > 0);
        assert_eq!(login.last_success_generation, login.renewal_generation);
        let dir = login.dir.as_deref().expect("new login directory");
        assert_eq!(dir, host.flows.borrow()[0].0);
        assert!(Path::new(dir).join(".claude.json").is_file());
        assert!(scratch.store().signin_now().unwrap().is_some());
        assert_eq!(host.token.asked_program.get(), window);
        assert_eq!(world.home().join("pid").exists(), window);
        assert_eq!(lab.holds_the_token(), minted);
        assert_eq!(scratch.table().rows.contains_key(KEY), minted);
        if minted {
            let row = scratch.table().rows.remove(KEY).unwrap();
            assert_eq!(row.label.as_deref(), Some("work"));
            assert_eq!(row.vault, InVault::InVault);
            assert_eq!(
                row.expires_at,
                Some(row.minted_at.unwrap() + A_YEAR - 43_200)
            );
            assert_eq!(
                std::fs::read_to_string(world.home().join("pasted")).unwrap(),
                "test-code#test-state"
            );
        }
        // Where no sign-in window opens, no token can be made, and no step is said for one, by
        // any word.
        if !window {
            let said = String::from_utf8_lossy(&out);
            assert!(!said.contains("aterm harness accounts token "), "{said}");
        }
        let value = sentinel();
        for bytes in [
            out,
            err,
            std::fs::read(scratch.store().shared_dir().join(TABLE_FILE)).unwrap_or_default(),
            std::fs::read(scratch.store().logins_file()).unwrap_or_default(),
            std::fs::read(scratch.store().state_file()).unwrap_or_default(),
        ] {
            assert!(
                !bytes
                    .windows(16)
                    .any(|part| value.as_bytes().windows(16).any(|token| part == token)),
                "a public result or account record leaked token bytes"
            );
        }
        assert!(!format!("{:?}", notes()).contains(&value[..16]));
    }
}
