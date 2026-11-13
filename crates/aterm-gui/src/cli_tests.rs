// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn setup_window_classification_consumes_values_before_scope_flags() {
    let classify = |args: &[&str]| {
        super::initial_setup_window_args(
            &args
                .iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>(),
        )
    };
    for args in [
        &["--font", "--headless"][..],
        &["--shell", "--control-sock"][..],
        &["-d", "--diagnose", "--gpu"][..],
    ] {
        assert!(classify(args), "option values keep their place: {args:?}");
    }
    for args in [
        &["--font", "Menlo", "--headless"][..],
        &["--gpu", "--containment", "user"][..],
        &["--gpu", "--control-sock", "off"][..],
        &["--font-px"][..],
        &["--shell"][..],
    ] {
        assert!(
            !classify(args),
            "a scope flag or missing value refuses setup: {args:?}"
        );
    }
}

#[test]
fn a_vanished_start_folder_stops_a_launch_and_never_an_update_relaunch() {
    let gone = std::env::temp_dir().join("aterm-start-dir-that-is-not-there");
    let gone = gone.to_string_lossy();
    assert!(
        super::start_dir_refusal(&gone, false).is_some_and(|why| why.contains(&*gone)),
        "a person's launch is refused, naming the folder"
    );
    assert_eq!(super::start_dir_refusal(&gone, true), None);
    let here = std::env::temp_dir();
    let here = here.to_string_lossy();
    assert_eq!(super::start_dir_refusal(&here, false), None);
    assert_eq!(super::start_dir_refusal(&here, true), None);
}

#[test]
fn a_closed_pipe_ends_the_answer_quietly_and_any_other_failure_is_said() {
    use std::io::{Error, ErrorKind};
    assert_eq!(super::answer_status(Ok(()), 0), Ok(0));
    assert_eq!(
        super::answer_status(Ok(()), 1),
        Ok(1),
        "the flag's own verdict"
    );
    assert_eq!(
        super::answer_status(Err(Error::from(ErrorKind::BrokenPipe)), 0),
        Ok(0)
    );
    #[cfg(windows)]
    for (os_error, what) in [
        (232, "the pipe is being closed"),
        (109, "the pipe has been ended"),
    ] {
        assert_eq!(
            super::answer_status(Err(Error::from_raw_os_error(os_error)), 3),
            Ok(3),
            "{what} (os error {os_error}) is the reader going away"
        );
    }
    assert!(
        super::answer_status(Err(Error::from(ErrorKind::StorageFull)), 0).is_err(),
        "a full disk is a failure to report"
    );
}

#[test]
fn the_unwaited_answer_line_names_the_waited_for_spelling() {
    let line = super::unwaited_answer_line(&["--version".to_string()]);
    assert!(
        line.starts_with("aterm-gui: ")
            && line.ends_with("run `aterm --window --version` for one the prompt waits for"),
        "{line}"
    );
    let line =
        super::unwaited_answer_line(&["--show-face".to_string(), "Cascadia Code".to_string()]);
    assert!(
        line.contains("`aterm --window --show-face \"Cascadia Code\"`"),
        "{line}"
    );
}

#[test]
fn the_headless_flag_is_recorded_without_descriptor_requests() {
    let cli = super::parse_cli(vec![std::ffi::OsString::from("--headless")]);
    assert!(cli.headless);
    assert_eq!(cli.lifeline_fd, None);
    assert_eq!(cli.ready_fd, None);
    assert!(super::HELP_HEAD.contains("--headless"));
    assert!(!super::HELP_HEAD.contains("[env: "));
}

#[test]
fn the_ready_flag_arms_only_a_headless_launch() {
    use super::ready_request as req;
    assert_eq!(req(None, true, false), Ok(None));
    #[cfg(unix)]
    {
        assert_eq!(req(Some("9"), true, false), Ok(Some(9)));
        assert_eq!(
            req(Some("9"), true, true),
            Ok(None),
            "the first image's, unread"
        );
        let window = req(Some("9"), false, false).expect_err("a window is refused");
        assert!(window.contains("needs --headless"), "{window}");
    }
    for bad in ["0", "2", "-1", "x", ""] {
        let err = req(Some(bad), true, false).expect_err(bad);
        assert!(err.contains("above stdio"), "{err}");
    }
}

/// `-e`'s payload is handed on as the OS gave it: a raw byte that is not
/// UTF-8 (`aterm --window -e prog $'\xff'`) stays that byte, never U+FFFD,
/// and the arguments after it keep their places.
#[cfg(unix)]
#[test]
fn the_command_payload_keeps_the_bytes_it_was_given() {
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
    let raw = std::ffi::OsString::from_vec(vec![0xff, b'a', 0x80]);
    let cli = super::parse_cli(vec!["-e".into(), "printf".into(), raw, "c".into()]);
    let bytes: Vec<Vec<u8>> = cli
        .exec_command
        .expect("the -e payload")
        .iter()
        .map(|a| AsRef::<std::ffi::OsStr>::as_ref(a).as_bytes().to_vec())
        .collect();
    assert_eq!(
        bytes,
        [b"printf".to_vec(), vec![0xff, b'a', 0x80], b"c".to_vec()]
    );
}

#[test]
fn the_lifeline_flag_arms_only_a_headless_launch() {
    use super::lifeline_request as req;
    assert_eq!(req(None, true), Ok(None));
    assert_eq!(req(None, false), Ok(None));
    #[cfg(unix)]
    {
        assert_eq!(req(Some("0"), true), Ok(Some(0)));
        assert_eq!(req(Some("7"), true), Ok(Some(7)));
        let window = req(Some("0"), false).expect_err("a window is refused");
        assert!(window.contains("needs --headless"), "{window}");
        let os = |v: &[&str]| v.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();
        for argv in [
            os(&["--lifeline-fd", "0", "--headless", "-e", "sh"]),
            os(&["--headless", "--lifeline-fd", "0", "-e", "sh"]),
        ] {
            let cli = super::parse_cli(argv);
            assert_eq!(cli.lifeline_fd, Some(0));
            assert_eq!(cli.exec_command, Some(vec!["sh".into()]));
        }
    }
    for bad in ["-1", "x", ""] {
        let err = req(Some(bad), true).expect_err(bad);
        assert!(err.contains("expects a descriptor number"), "{err}");
    }
}

#[test]
fn launch_flags_decide_the_socket_like_every_client() {
    use aterm_types::control_socket::SocketDirective as D;
    let with = |sock: Option<&str>, off: bool| super::LaunchFlags {
        control_sock: sock.map(str::to_string),
        no_control_sock: off,
        ..super::LaunchFlags::default()
    };
    assert_eq!(with(None, false).socket_directive(), D::PerInstance);
    assert_eq!(
        with(Some("/r/a.sock"), false).socket_directive(),
        D::Explicit("/r/a.sock".to_string())
    );
    assert_eq!(with(Some("off"), false).socket_directive(), D::Disabled);
    assert_eq!(
        with(Some("/r/a.sock"), true).socket_directive(),
        D::Disabled
    );
}

#[test]
fn font_px_flag_accepts_exact_runtime_domain_only() {
    for accepted in ["6", "12.5", "200"] {
        assert!(super::valid_font_px_flag(accepted), "{accepted}");
    }
    for rejected in ["5.99", "201", "500", "NaN", "inf", "nope"] {
        assert!(!super::valid_font_px_flag(rejected), "{rejected}");
    }
}

#[test]
fn initial_dimension_flags_accept_exact_documented_domains_only() {
    for accepted in ["20", "80", "500"] {
        assert!(
            super::initial_dimension_flag(accepted, 20, 500).is_some(),
            "columns {accepted}"
        );
    }
    for rejected in ["0", "1", "19", "501", "65536", "nope"] {
        assert!(
            super::initial_dimension_flag(rejected, 20, 500).is_none(),
            "columns {rejected}"
        );
    }
    for accepted in ["5", "24", "300"] {
        assert!(
            super::initial_dimension_flag(accepted, 5, 300).is_some(),
            "lines {accepted}"
        );
    }
    for rejected in ["0", "1", "4", "301", "65536", "nope"] {
        assert!(
            super::initial_dimension_flag(rejected, 5, 300).is_none(),
            "lines {rejected}"
        );
    }
}

#[test]
fn render_flags_ride_the_launch_struct_and_the_last_renderer_flag_wins() {
    use crate::launch::RendererFlag;
    let parse = |args: &[&str]| {
        super::parse_cli(args.iter().map(std::ffi::OsString::from).collect()).launch
    };
    assert_eq!(parse(&["--cpu", "--gpu"]).renderer, Some(RendererFlag::Gpu));
    assert_eq!(parse(&["--gpu", "--cpu"]).renderer, Some(RendererFlag::Cpu));
    assert_eq!(parse(&[]), crate::launch::Launch::NONE);
    let all = parse(&["--font-px", "24", "--font", "Menlo", "--scale", "2"]);
    assert_eq!(all.font_px, Some(24.0));
    assert_eq!(all.font_family.as_deref(), Some("Menlo"));
    assert_eq!(all.scale, Some(2.0));
    assert_eq!(
        parse(&["--font", "  "]).font_family,
        None,
        "a blank family is no pin"
    );
}

/// Unset is never gated on the handoff server: clearing `HKCU\Console\%%Startup` is the
/// escape hatch from a delegation to a class nothing can create.
#[cfg(windows)]
#[test]
fn unset_default_terminal_is_never_gated_while_set_refuses() {
    assert!(
        !crate::defterm_win::handoff_server_available(),
        "no COM handoff server ships in this build"
    );
    let set_err = crate::defterm_win::set_default_terminal()
        .expect_err("set must refuse while nothing can answer the handoff");
    assert_eq!(set_err.kind(), std::io::ErrorKind::Unsupported);

    let before = crate::defterm_win::current_delegation().expect("read");
    crate::defterm_win::unset_default_terminal()
        .expect("unset must not fail on the live machine, gated or not");
    let after = crate::defterm_win::current_delegation().expect("read");
    assert_eq!(
        before, after,
        "unset must not touch a delegation aterm does not own"
    );
}

#[test]
fn help_and_starter_name_the_resolved_config_path() {
    assert!(
        super::HELP_TAIL.contains("CONFIG:  <aterm.toml>  (live settings reload"),
        "the CONFIG line carries the slot, not a literal path"
    );
    assert!(
        super::STARTER_CONFIG.starts_with("# aterm — <aterm.toml>\n"),
        "the starter's header carries the slot, not a literal path"
    );
    let tail = super::help_tail();
    assert!(!tail.contains(super::CONFIG_PATH_SLOT), "{tail}");
    let shown = super::config_path_display();
    assert!(
        tail.contains(&format!("CONFIG:  {shown}  (live settings reload")),
        "{tail}"
    );
    let Some(path) = crate::app_config::config_path() else {
        assert!(shown.starts_with("(unresolved: "), "{shown}");
        return;
    };
    assert_eq!(shown, path.display().to_string());
    assert!(path.ends_with("aterm.toml"), "{}", path.display());
    let starter = super::starter_config_for(&path);
    assert_eq!(
        starter.lines().next(),
        Some(format!("# aterm — {}", path.display()).as_str())
    );
    assert!(!starter.contains(super::CONFIG_PATH_SLOT), "{starter}");
    assert_eq!(
        starter.lines().count(),
        super::STARTER_CONFIG.lines().count(),
        "only the header changes"
    );
}

#[test]
fn starter_config_keys_all_deserialize() {
    use crate::app_config::Config;
    let mut in_table = false;
    let mut checked: Vec<String> = Vec::new();
    for raw in super::STARTER_CONFIG.lines() {
        let line = raw.trim_start();
        let Some(rest) = line.strip_prefix('#') else {
            if line.starts_with('[') {
                in_table = true;
            }
            continue;
        };
        let rest = rest.trim_start();
        if rest.starts_with('[') {
            in_table = true;
            continue;
        }
        let key =
            rest.split('=').next().map(str::trim).filter(|k| {
                !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            });
        if !in_table
            && rest.contains('=')
            && let Some(key) = key
        {
            assert!(
                aterm_toml::from_str::<Config>(rest).is_ok(),
                "STARTER_CONFIG ships an uncommentable key — uncommenting it would \
                 fail the config load and revert to defaults:\n  {rest}"
            );
            checked.push(key.to_string());
        }
    }
    let mut numbers = 0;
    for raw in super::STARTER_CONFIG.lines() {
        let Some(rest) = raw.trim_start().strip_prefix('#') else {
            continue;
        };
        let Some((key, value)) = rest.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        let literal = value.split('#').next().unwrap_or_default().trim();
        let Some(number) = literal
            .parse::<i64>()
            .ok()
            .map(|n| n as f64)
            .or_else(|| literal.parse::<f64>().ok())
        else {
            continue;
        };
        assert_eq!(
            literal,
            crate::prefs::written_number(number).to_string().trim(),
            "STARTER_CONFIG spells `{key}` as aterm never writes it"
        );
        numbers += 1;
    }
    assert!(numbers > 30, "only {numbers} numbers checked");
    for must in [
        "allow_window_ops",
        "allow_notifications",
        "allow_palette_reconfigure",
        "allow_kitty_file_transfer",
        "allow_osc52_query",
        "secure_keyboard_entry",
    ] {
        assert!(
            checked.iter().any(|k| k == must),
            "STARTER_CONFIG key `{must}` was not validated — is it buried under a \
             `[table]` header? Keep all bare top-level keys ABOVE every table."
        );
    }
}

#[test]
fn starter_config_matrix_rain_block_deserializes() {
    use crate::app_config::Config;
    let mut block = String::new();
    let mut in_rain = false;
    for raw in super::STARTER_CONFIG.lines() {
        let Some(rest) = raw.trim_start().strip_prefix('#') else {
            continue;
        };
        let rest = rest.trim_start();
        if rest.starts_with('[') {
            in_rain = rest.starts_with("[matrix_rain]");
            if in_rain {
                block.push_str("[matrix_rain]\n");
            }
            continue;
        }
        let key_ok = rest.split('=').next().map(str::trim).is_some_and(|k| {
            !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
        if in_rain && rest.contains('=') && key_ok {
            block.push_str(rest);
            block.push('\n');
        }
    }
    let parsed: Config = aterm_toml::from_str(&block).unwrap_or_else(|e| {
        panic!(
            "STARTER_CONFIG ships an uncommentable [matrix_rain] line — \
             uncommenting the block would fail the config load:\n{block}\n{e}"
        )
    });
    let mr = parsed
        .matrix_rain
        .expect("the assembled block populates the [matrix_rain] table");
    assert!(mr.enabled.is_some(), "starter documents `enabled`");
    assert!(mr.fps.is_some(), "starter documents `fps`");
    assert!(mr.density.is_some(), "starter documents `density`");
    assert!(mr.speed.is_some(), "starter documents `speed`");
    assert!(mr.trail.is_some(), "starter documents `trail`");
    assert!(mr.alpha.is_some(), "starter documents `alpha`");
    assert!(mr.head_alpha.is_some(), "starter documents `head_alpha`");
    assert!(mr.hue.is_some(), "starter documents `hue`");
    assert!(mr.mutation_ms.is_some(), "starter documents `mutation_ms`");
    assert!(mr.idle_secs.is_some(), "starter documents `idle_secs`");
    assert!(
        mr.suppress_in_alt_screen.is_some(),
        "starter documents `suppress_in_alt_screen`"
    );
    assert!(mr.turn_wave.is_some(), "starter documents `turn_wave`");
    assert!(mr.bell_alert.is_some(), "starter documents `bell_alert`");
    assert!(mr.seed.is_some(), "starter documents `seed`");
    assert_eq!(mr.enabled, Some(false), "the starter example ships OFF");
}
