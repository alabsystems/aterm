// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_value_is_the_three_variables_each_once_in_their_order() {
    let text = "\r\nAZURE_CLIENT_SECRET=s3~cr.et_-\r\nAZURE_TENANT_ID=t\n\nAZURE_CLIENT_ID=c";
    assert_eq!(
        parse(text),
        Ok([
            ("AZURE_TENANT_ID", "t"),
            ("AZURE_CLIENT_ID", "c"),
            ("AZURE_CLIENT_SECRET", "s3~cr.et_-"),
        ])
    );
}

#[test]
fn a_refusal_names_the_line_or_variable_and_never_a_value() {
    let full = "AZURE_TENANT_ID=t\nAZURE_CLIENT_ID=c\n";
    for (text, why) in [
        (
            "AZURE_TENANT_ID=t\nAZURE_CLIENT_ID=c\n",
            "no AZURE_CLIENT_SECRET line",
        ),
        (
            &*format!("{full}AZURE_CLIENT_SECRET=hunter2\nAZURE_CLIENT_SECRET=hunter3"),
            "AZURE_CLIENT_SECRET is given twice",
        ),
        (
            &*format!("{full}AZURE_CLIENT_SECRET=hunter 2"),
            "AZURE_CLIENT_SECRET's value is empty or holds a space, a control or a non-ASCII character",
        ),
        (
            &*format!("{full}AZURE_CLIENT_SECRET="),
            "AZURE_CLIENT_SECRET's value is empty or holds a space, a control or a non-ASCII character",
        ),
        (
            &*format!("{full}hunter2"),
            "line 3 is not AZURE_TENANT_ID=, AZURE_CLIENT_ID= or AZURE_CLIENT_SECRET=",
        ),
        (
            &*format!("{full}AZURE_USERNAME=hunter2"),
            "line 3 is not AZURE_TENANT_ID=, AZURE_CLIENT_ID= or AZURE_CLIENT_SECRET=",
        ),
    ] {
        let got = parse(text).unwrap_err();
        assert_eq!(got, why, "{text:?}");
        assert!(!got.contains("hunter"), "{got}");
    }
}
