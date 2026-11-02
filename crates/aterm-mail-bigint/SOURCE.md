<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 OR MIT -->

# Mail arithmetic provenance

Derived from RustCrypto `crypto-bigint` 0.5.5, crates.io release source.
Upstream: https://github.com/RustCrypto/crypto-bigint/tree/v0.5.5
The Apache-2.0 and MIT license texts are retained beside this file.

The retained implementation supplies fixed-width unsigned integers and modular
arithmetic for Proton SRP. It has no upstream package dependency. Randomness
comes from aterm; upstream random, DER, RLP, serde and generic-array features are
not exposed. Allocation-based integer support is not enabled. Constant-time
choices and wiping use the `subtle` and `zeroize` versions already present in
aterm's TLS dependency closure.

The imported unit arithmetic tests are kept. The encoding property test is
expressed over deterministic edge cases so it needs no property-test package.
