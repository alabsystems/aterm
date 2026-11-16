// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const TOOL: &str = r"C:\Users\a b\AppData\Local\aterm\build\aterm-winship.exe";
const HOST: &str = r"C:\WINDOWS\System32\conhost.exe";
const START: &str = "2026-11-12T04:30:00";

/// Reads one argument back the way the C runtime and `std::env::args` do: the
/// inverse [`quoted`] must have.
fn read_back(arg: &str) -> String {
    let inner = arg
        .strip_prefix('"')
        .and_then(|a| a.strip_suffix('"'))
        .expect("quoted");
    let mut out = String::new();
    let mut slashes = 0usize;
    for c in inner.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => panic!("an unescaped quote inside {arg}"),
            c => {
                out.push_str(&"\\".repeat(slashes));
                slashes = 0;
                out.push(c);
            }
        }
    }
    // 2n backslashes before the closing quote are n backslashes; an odd run
    // would have made the quote a literal one.
    assert_eq!(slashes % 2, 0, "{arg} ends in an escaped quote");
    out.push_str(&"\\".repeat(slashes / 2));
    out
}

#[test]
fn time_takes_a_minute_of_the_day_only() {
    assert_eq!(time("04:30"), Ok("04:30".to_string()));
    assert_eq!(time("23:59"), Ok("23:59".to_string()));
    assert_eq!(time("00:00"), Ok("00:00".to_string()));
    // Negative controls.
    for bad in [
        "24:00", "12:60", "4:30", "0430", "04:3", "ab:cd", "04:30:00", "", "+4:30",
    ] {
        assert!(time(bad).is_err(), "{bad:?} was taken");
    }
}

#[test]
fn the_first_start_is_the_next_one_never_one_already_missed() {
    // 2026-11-11T19:14:22 on the local clock.
    let now = 1_794_424_462;
    assert_eq!(crate::pointer::rfc3339(now), "2026-11-11T19:14:22Z");
    assert_eq!(
        start_boundary(now, "04:30"),
        Ok("2026-11-12T04:30:00".to_string()),
        "04:30 has passed today"
    );
    assert_eq!(
        start_boundary(now, "19:14"),
        Ok("2026-11-12T19:14:00".to_string()),
        "the minute that is running has started already"
    );
    // Control: a time still ahead today starts today.
    assert_eq!(
        start_boundary(now, "19:15"),
        Ok("2026-11-11T19:15:00".to_string())
    );
    assert_eq!(
        start_boundary(now, "23:59"),
        Ok("2026-11-11T23:59:00".to_string())
    );
    assert!(start_boundary(now, "24:00").is_err());
}

#[test]
fn an_argument_is_read_back_as_it_was_given() {
    for path in [
        r"D:\src\aterm",
        r"C:\src\aterm\",
        r"C:\",
        r"C:\a b\c\\",
        r"\\server\share\aterm",
    ] {
        assert_eq!(read_back(&quoted(path).expect("quotes")), path);
    }
    assert_eq!(
        quoted(r"C:\src\aterm\"),
        Ok(r#""C:\src\aterm\\""#.to_string())
    );
    assert_eq!(quoted(r"C:\"), Ok(r#""C:\\""#.to_string()));
    // Negative control: without the doubling the trailing backslash escapes the quote.
    let naive = format!("\"{}\"", r"C:\src\aterm\");
    assert!(std::panic::catch_unwind(|| read_back(&naive)).is_err());
    // A path it cannot quote is refused.
    assert!(quoted("C:\\a\"b").is_err());
}

#[test]
fn the_task_runs_update_headless_from_the_kept_copy_on_the_named_repository() {
    let args =
        arguments(Task::Update, Path::new(TOOL), Path::new(r"D:\src\aterm")).expect("arguments");
    assert_eq!(
        args,
        r#"--headless "C:\Users\a b\AppData\Local\aterm\build\aterm-winship.exe" update --log --repo "D:\src\aterm""#
    );
    let trailing =
        arguments(Task::Update, Path::new(TOOL), Path::new(r"D:\src\aterm\")).expect("arguments");
    assert!(
        trailing.ends_with(r#" --repo "D:\src\aterm\\""#),
        "{trailing}"
    );
    // Negative controls: the task starts in the system directory, so a
    // relative repository is refused; so is a path that cannot be quoted.
    assert!(arguments(Task::Update, Path::new(TOOL), Path::new(".")).is_err());
    assert!(arguments(Task::Update, Path::new(TOOL), Path::new(r"src\aterm")).is_err());
    assert!(
        arguments(
            Task::Update,
            Path::new("C:\\a\"b\\aterm-winship.exe"),
            Path::new(r"D:\src")
        )
        .is_err()
    );
}

fn element<'a>(xml: &'a str, name: &str) -> &'a str {
    let (open, close) = (format!("<{name}>"), format!("</{name}>"));
    let from = xml
        .find(&open)
        .unwrap_or_else(|| panic!("no <{name}> in {xml}"))
        + open.len();
    let len = xml[from..]
        .find(&close)
        .unwrap_or_else(|| panic!("no </{name}> in {xml}"));
    assert_eq!(
        xml.matches(&open).count(),
        1,
        "<{name}> is there more than once"
    );
    &xml[from..from + len]
}

#[test]
fn the_task_xml_states_the_conditions_schtasks_flags_cannot() {
    let xml = task_xml(
        Task::Update,
        Path::new(HOST),
        Path::new(TOOL),
        Path::new(r"D:\src\a&b <x>"),
        START,
        Some(r"DESK\owner"),
    )
    .expect("xml");
    for (name, want) in [
        ("DisallowStartIfOnBatteries", "false"),
        ("StopIfGoingOnBatteries", "false"),
        ("StartWhenAvailable", "true"),
        ("WakeToRun", "false"),
        ("MultipleInstancesPolicy", "IgnoreNew"),
        ("LogonType", "InteractiveToken"),
        ("RunLevel", "LeastPrivilege"),
        ("UserId", r"DESK\owner"),
        ("StartBoundary", START),
        ("DaysInterval", "1"),
        ("RunOnlyIfIdle", "false"),
        ("Command", HOST),
    ] {
        assert_eq!(element(&xml, name), want, "<{name}>");
    }
    // The paths are XML text: escaped once, and the same command once unescaped.
    let arguments = element(&xml, "Arguments");
    assert!(
        arguments.ends_with("--repo &quot;D:\\src\\a&amp;b &lt;x&gt;&quot;"),
        "{arguments}"
    );
    assert!(
        arguments
            .starts_with("--headless &quot;C:\\Users\\a b\\AppData\\Local\\aterm\\build\\aterm-winship.exe&quot; update --log"),
        "{arguments}"
    );
    assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n<Task "));
    // Controls: no caller named leaves the account to schtasks, and a relative
    // repository is refused before any XML is made.
    let anonymous = task_xml(
        Task::Update,
        Path::new(HOST),
        Path::new(TOOL),
        Path::new(r"D:\src"),
        START,
        None,
    )
    .expect("xml");
    assert!(!anonymous.contains("<UserId>"), "{anonymous}");
    assert!(
        task_xml(
            Task::Update,
            Path::new(HOST),
            Path::new(TOOL),
            Path::new("."),
            START,
            None
        )
        .is_err()
    );
}

#[test]
fn the_xml_file_is_utf_16_with_a_byte_order_mark() {
    assert_eq!(
        utf16("<a>é"),
        [0xFF, 0xFE, b'<', 0, b'a', 0, b'>', 0, 0xE9, 0]
    );
}

#[test]
fn schtasks_registers_from_the_file_and_removes_by_name() {
    let xml = Path::new(r"C:\Users\a b\AppData\Local\aterm\build\aterm-winship-update.xml");
    assert_eq!(
        shown(&create_args(Task::Update, xml)),
        r#"schtasks.exe /Create /F /TN aterm-winship-update /XML "C:\Users\a b\AppData\Local\aterm\build\aterm-winship-update.xml""#
    );
    assert_eq!(
        shown(&delete_args(Task::Update)),
        "schtasks.exe /Delete /F /TN aterm-winship-update"
    );
    assert_eq!(
        shown(&delete_args(Task::Serve)),
        "schtasks.exe /Delete /F /TN aterm-winship-release-serve"
    );
    assert_eq!(
        shown(&query_args(Task::Serve)),
        "schtasks.exe /Query /TN aterm-winship-release-serve"
    );
}

#[test]
fn the_serve_task_polls_every_ten_minutes_one_run_at_a_time() {
    let xml = task_xml(
        Task::Serve,
        Path::new(HOST),
        Path::new(TOOL),
        Path::new(r"D:\src\aterm"),
        START,
        Some(r"DESK\owner"),
    )
    .expect("xml");
    for (name, want) in [
        ("Interval", SERVE_EVERY),
        ("StopAtDurationEnd", "false"),
        ("StartBoundary", START),
        ("MultipleInstancesPolicy", "IgnoreNew"),
        ("DisallowStartIfOnBatteries", "false"),
        ("StopIfGoingOnBatteries", "false"),
        ("StartWhenAvailable", "true"),
        ("WakeToRun", "false"),
        ("LogonType", "InteractiveToken"),
        ("RunLevel", "LeastPrivilege"),
        ("Command", HOST),
    ] {
        assert_eq!(element(&xml, name), want, "<{name}>");
    }
    assert_eq!(SERVE_EVERY, "PT10M");
    assert_eq!(
        element(&xml, "Arguments"),
        "--headless &quot;C:\\Users\\a b\\AppData\\Local\\aterm\\build\\aterm-winship.exe&quot; \
         release-serve --once --log --repo &quot;D:\\src\\aterm&quot;"
    );
    // Controls: the serve task is no daily one, and the update task does not repeat.
    assert!(!xml.contains("<CalendarTrigger>"), "{xml}");
    let daily = task_xml(
        Task::Update,
        Path::new(HOST),
        Path::new(TOOL),
        Path::new(r"D:\src\aterm"),
        START,
        None,
    )
    .expect("xml");
    assert!(!daily.contains("<Repetition>"), "{daily}");
    assert!(element(&daily, "Arguments").contains(" update --log --repo "));
    assert_eq!(
        Task::ALL.map(Task::name),
        ["aterm-winship-update", "aterm-winship-release-serve"]
    );
}

#[test]
fn the_serve_task_starts_within_two_minutes_never_in_the_past() {
    // 2026-11-11T19:14:22 on the local clock: the start of 19:16.
    assert_eq!(soon_boundary(1_794_424_462), "2026-11-11T19:16:00");
    // On a minute's first second, still the second minute after it.
    assert_eq!(soon_boundary(1_794_424_440), "2026-11-11T19:16:00");
    // Across midnight.
    assert_eq!(soon_boundary(1_794_441_599), "2026-11-12T00:01:00");
}
