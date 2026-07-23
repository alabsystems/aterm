<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# Security Policy

## Supported release

Security maintenance is best-effort for the latest public source release. The
first supported public release is `v0.1.0`. It contains source code, but no
prebuilt binary, installer, public updater channel, or managed ALab package.

## Reporting a vulnerability

Do not open a public issue or pull request with exploit details, credentials,
sensitive terminal contents, or private artifacts.

Email Andrew Yates at <andrewyates.name@gmail.com>. Please include:

- the affected public version or commit;
- the operating system and relevant configuration;
- the impact and realistic attack scenario;
- steps to reproduce and the smallest safe proof of concept; and
- any suggested mitigation.

If the report needs confidentiality beyond plain email, say so in the first
message so another channel can be arranged. There is no response-time SLA.

## In scope

Examples of issues that should be reported privately include:

- bypasses of control-socket authentication or scoped capabilities;
- command or input injection across the control boundary;
- memory-safety faults triggered by untrusted terminal input;
- updater, package-signature, or release-provenance failures;
- capture-path escapes or unintended disclosure of terminal contents; and
- committed or emitted credentials.

Ordinary documentation errors, feature requests, and non-security bugs may use
[GitHub Issues](https://github.com/alabsystems/aterm/issues).
