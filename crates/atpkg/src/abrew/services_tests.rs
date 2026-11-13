// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn row(name: &str, status: &'static str, user: Option<&str>, loaded: bool) -> ServiceRow {
    ServiceRow {
        name: name.to_string(),
        service_name: format!("sh.brew.{name}"),
        running: status == "started",
        schedulable: false,
        pid: None,
        status,
        user: user.map(str::to_string),
        file: PathBuf::from(format!(
            "/Users//x/Library/LaunchAgents/sh.brew.{name}.plist"
        )),
        exit_code: None,
        loaded,
        registered: loaded,
        loaded_file: None,
        details: None,
    }
}

#[test]
fn table_matches_brew_column_math() {
    let rows = vec![
        row("colima", "started", Some("tester"), true),
        row("emacs", "started", Some("tester"), true),
    ];
    let table = print_table(&rows, Path::new("/Users//x"));
    assert_eq!(
        table,
        "Name   Status User   File\n\
         colima started         tester ~/Library/LaunchAgents/sh.brew.colima.plist\n\
         emacs  started         tester ~/Library/LaunchAgents/sh.brew.emacs.plist\n"
    );
}

#[test]
fn unloaded_rows_hide_the_file() {
    let rows = vec![row("mysql", "none", None, false)];
    let table = print_table(&rows, Path::new("/Users//x"));
    assert!(table.contains("mysql none            "), "{table}");
    assert!(!table.contains("plist"), "{table}");
}

#[test]
fn json_shape_and_key_order() {
    let rows = vec![row("colima", "started", Some("tester"), true)];
    let json = to_json(&rows);
    assert_eq!(
        json,
        "[\n  {\n    \"name\": \"colima\",\n    \"status\": \"started\",\n    \
         \"user\": \"tester\",\n    \"file\": \"/Users//x/Library/LaunchAgents/sh.brew.colima.plist\",\n    \
         \"exit_code\": null\n  }\n]"
    );
}

#[test]
fn status_numbers_both_launchctl_forms() {
    assert_eq!(
        status_numbers("state = running\n\tpid = 8123\n", true),
        (Some(8123), None)
    );
    assert_eq!(
        status_numbers("\"PID\" = 42;\n\"LastExitStatus\" = 0;", false),
        (Some(42), Some(0))
    );
    assert_eq!(status_numbers("last exit code = 2", true), (None, Some(2)));
}

#[test]
fn status_symbol_precedence() {
    assert_eq!(
        status_symbol(Some(9), true, Some(1), false, false),
        "started"
    );
    assert_eq!(status_symbol(None, false, None, false, true), "none");
    assert_eq!(status_symbol(None, true, Some(0), true, false), "scheduled");
    assert_eq!(status_symbol(None, true, Some(0), false, false), "stopped");
    assert_eq!(status_symbol(None, true, Some(3), false, false), "error");
    assert_eq!(status_symbol(None, true, None, false, true), "unknown");
    assert_eq!(status_symbol(Some(0), true, None, false, false), "other");
}

#[test]
fn plist_names_default_and_explicit() {
    assert_eq!(
        plist_names("mysql", None, None),
        vec![
            "sh.brew.mysql".to_string(),
            "homebrew.mxcl.mysql".to_string()
        ]
    );
    let raw = aterm_json::json!({"name": {"macos": "com.example.d", "linux": "d"}});
    assert_eq!(
        plist_names("d", Some(&raw), None),
        vec!["com.example.d".to_string()]
    );
    assert_eq!(
        plist_names("d", Some(&raw), Some("sh.tap.d")),
        vec!["sh.tap.d".to_string()],
        "a tap record's explicit name wins over the API object's"
    );
}

#[test]
fn info_text_and_json_render() {
    let mut r = row("colima", "started", Some("tester"), true);
    r.pid = Some(79468);
    r.registered = true;
    r.loaded_file = Some(r.file.display().to_string());
    r.details = Some(ServiceDetails {
        command: "/opt/homebrew/opt/colima/bin/colima start -f".to_string(),
        working_dir: Some("/Users//x".to_string()),
        log_path: Some("/opt/homebrew/var/log/colima.log".to_string()),
        error_log_path: Some("/opt/homebrew/var/log/colima.log".to_string()),
        ..ServiceDetails::default()
    });
    assert_eq!(
        info_output(&r, false),
        "colima (sh.brew.colima)\nRunning: true\nLoaded: true\nSchedulable: false\nUser: tester\nPID: 79468\n"
    );
    let v = info_output(&r, true);
    assert!(
        v.contains("File: /Users//x/Library/LaunchAgents/sh.brew.colima.plist true\n"),
        "{v}"
    );
    assert!(v.contains("Registered at login: true\n"), "{v}");
    assert!(
        v.contains("Command: /opt/homebrew/opt/colima/bin/colima start -f\n"),
        "{v}"
    );
    let plain = row("hello", "none", None, false);
    assert_eq!(
        info_output(&plain, false),
        "hello (sh.brew.hello)\nRunning: false\nLoaded: false\nSchedulable: false\n"
    );
    let json = to_info_json(&[r]);
    assert!(
        json.contains("\"service_name\": \"sh.brew.colima\""),
        "{json}"
    );
    assert!(json.contains("\"pid\": 79468"), "{json}");
    assert!(json.contains("\"root_dir\": null"), "{json}");
    assert!(json.contains("\"interval\": null"), "{json}");
}

#[test]
fn info_sh_quote_and_manual_command() {
    assert_eq!(sh_quote("/opt/x/bin/tool"), "/opt/x/bin/tool");
    assert_eq!(sh_quote("with space"), "'with space'");
    assert_eq!(sh_quote(""), "''");
    let raw = aterm_json::json!({
        "run": ["/bin/d", "--flag"],
        "environment_variables": {"PATH": "/bin", "TOKEN": "a b"}
    });
    let cmd = manual_command(
        "d",
        &["/bin/d".into(), "--flag".into()],
        &raw,
        &|t: &str| t.to_string(),
    );
    assert_eq!(cmd, "TOKEN=\"a b\" /bin/d --flag");
}

#[test]
fn info_loaded_file_from_print_output() {
    assert_eq!(
        loaded_file_of("x = {\n\tpath = /Users//x/a.plist\n", true).as_deref(),
        Some("/Users//x/a.plist")
    );
    assert_eq!(loaded_file_of("anything", false), None);
}

#[test]
fn plist_string_reads_label_and_user() {
    let dir = std::env::temp_dir().join(format!("abrew-svc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("x.plist");
    std::fs::write(
        &file,
        "<plist><dict><key>Label</key>\n<string>sh.brew.x</string>\
         <key>UserName</key><string>root</string></dict></plist>",
    )
    .unwrap();
    assert_eq!(plist_string(&file, "Label").as_deref(), Some("sh.brew.x"));
    assert_eq!(plist_string(&file, "UserName").as_deref(), Some("root"));
    std::fs::remove_dir_all(&dir).ok();
}
