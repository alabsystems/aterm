// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// launchd's list names a job's switch by its own row in the `disabled services` block
/// (`disabled`, or `true` before macOS 13, is off; `enabled` or `false` is on), and a job with no
/// row there is unlisted, whatever a row of another block (`login item associations`) says; a
/// list cut short, never printed or with a row in other words is no answer, in words naming what
/// launchctl printed. Only a successful read answers. MUTANT: reading every `=>` row of the text
/// takes the association block's row for the job's switch.
#[test]
fn a_system_job_switch_is_read_from_launchds_disabled_list() {
    let label = "systems.alab.aterm-lan";
    let list = |rows: &str| {
        format!(
            "\tdisabled services = {{\n{rows}\t}}\n\tlogin item associations = {{\n\t\t\"{label}\" \
             => disabled\n\t}}\n"
        )
    };
    let row = |state: &str| format!("\t\t\"{label}\" => {state}\n");
    assert_eq!(
        job_switch_in(&list(&row("disabled")), label),
        Ok(JobSwitch::Off)
    );
    assert_eq!(
        job_switch_in(&list(&row("true")), label),
        Ok(JobSwitch::Off)
    );
    assert_eq!(
        job_switch_in(&list(&row("enabled")), label),
        Ok(JobSwitch::On)
    );
    assert_eq!(
        job_switch_in(&list(&row("false")), label),
        Ok(JobSwitch::On)
    );
    assert_eq!(
        job_switch_in(&list("\t\t\"com.openssh.sshd\" => disabled\n"), label),
        Ok(JobSwitch::Unlisted),
        "a row of another block is not the job's switch"
    );
    let cut = job_switch_in(
        "\tdisabled services = {\n\t\t\"com.openssh.sshd\" => disabled\n",
        label,
    )
    .unwrap_err();
    assert!(cut.contains("ends before its close"), "{cut}");
    let never = job_switch_in("Unrecognized subcommand\n", label).unwrap_err();
    assert!(never.contains("\"Unrecognized subcommand\""), "{never}");
    let odd = job_switch_in(&list(&row("unknown")), label).unwrap_err();
    assert!(odd.contains("\"unknown\""), "{odd}");
    assert_eq!(
        switched_off_of(Some(0), &list(&row("disabled")), label),
        Some(true)
    );
    assert_eq!(switched_off_of(Some(0), &list(""), label), Some(false));
    assert_eq!(
        switched_off_of(Some(0), "Unrecognized subcommand\n", label),
        None
    );
    assert_eq!(
        switched_off_of(Some(1), &list(&row("disabled")), label),
        None
    );
    assert_eq!(switched_off_of(None, &list(&row("disabled")), label), None);
}
