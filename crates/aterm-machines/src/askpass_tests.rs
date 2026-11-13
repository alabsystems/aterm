// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const PASSWORD: &str = "admin@192.168.64.5's password: ";
const KBD_INT: &str = "(admin@192.168.64.5) Password:";

fn run(prompt: &str, confirm: bool, person: bool, input: &[u8]) -> (u8, Vec<u8>, String) {
    let mut input = input;
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = answer(prompt, confirm, person, &mut input, &mut out, &mut err);
    (code, out, String::from_utf8(err).unwrap())
}

#[test]
fn a_password_prompt_is_answered_with_the_first_line_of_input() {
    for prompt in [PASSWORD, KBD_INT] {
        let (code, out, err) = run(prompt, false, false, b"hunter2\n");
        assert_eq!(
            (code, out.as_slice(), err.as_str()),
            (0, &b"hunter2\n"[..], "")
        );
    }
}

#[test]
fn a_confirmation_a_passphrase_a_code_or_a_person_is_never_answered() {
    for (prompt, confirm, person) in [
        (
            "Are you sure you want to continue connecting (yes/no)?",
            true,
            false,
        ),
        (PASSWORD, true, false),
        (
            "Enter passphrase for key '/Users//a/.ssh/id_ed25519': ",
            false,
            false,
        ),
        ("Verification code: ", false, false),
        (PASSWORD, false, true),
    ] {
        let (code, out, err) = run(prompt, confirm, person, b"hunter2\n");
        assert_eq!(code, 1, "{prompt}");
        assert!(out.is_empty(), "{prompt}");
        assert!(!err.contains("hunter2"), "{err}");
    }
}

#[test]
fn a_second_prompt_of_one_login_finds_the_password_used() {
    let (code, out, err) = run(PASSWORD, false, false, b"");
    assert_eq!(code, 1);
    assert!(out.is_empty());
    assert!(err.contains("already used"), "{err}");
}
