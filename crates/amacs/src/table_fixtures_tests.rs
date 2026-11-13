// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Synthetic tables of the owner's file's shape: `@example.test` addresses, a long-text
//! column, quoted commas, `""`, a line break inside a quoted cell, ragged rows and empty
//! cells, every line-ending mix, no final newline, a stray quote, and a TSV twin. No test
//! reads a real file.

/// The header and four rows, LF line ends: a short row (m3) and a long one (m4).
pub(crate) const MAIN: &str = concat!(
    "computer,AI login email,services,codes arrive in (my guess),why I guessed,your correction (leave blank if my guess is right)\n",
    "m1,alice@example.test,Claude,email,\"The sign-in page said \"\"check your inbox\"\", so email.\",\n",
    "m2,bob@example.test,\"Claude, GitHub\",SMS,\"Two reasons: first\nthe phone prompt, then the code\",\n",
    "m3,carol@example.test,Claude,authenticator\n",
    "m4,dave@example.test,,,,,extra\n",
);

/// CRLF between records and LF inside a quoted cell, as Excel writes; no final newline.
pub(crate) const MIXED: &str = concat!(
    "computer,email,why,fix\r\n",
    "m1,erin@example.test,\"one\nand two\",\r\n",
    "m2,frank@example.test,\"Said \"\"no\"\"\",later",
);

/// Every break CRLF, inside the quoted cell too.
pub(crate) const ALL_CRLF: &str = concat!(
    "computer,email,why,fix\r\n",
    "m1,gina@example.test,\"line one\r\nline two\",\r\n",
    "m2,hal@example.test,plain,\r\n",
);

/// A quote inside a bare cell, text after a closing quote, and a quote that never closes.
pub(crate) const STRAY: &str = concat!("a,b\"c,d\n", "\"x\"y,z\n", "\"never closes,\nstill open",);

/// [`MAIN`] with tabs: commas and quotes in values need no quotes, a tab or line break does.
pub(crate) const TSV: &str = concat!(
    "computer\tAI login email\tservices\twhy I guessed\tyour correction\n",
    "m1\talice@example.test\tClaude, GitHub\tsaid \"inbox\", so email\t\n",
    "m2\tbob@example.test\tClaude\t\"Two\tparts\"\t\n",
);
