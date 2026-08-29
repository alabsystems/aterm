<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# TLS verification fixtures

Certificates for `crates/aterm-http/src/verifier.rs`'s differential oracle. All
of them are public test data. **Do not install, trust, or reuse any file here in
any real deployment.**

Every local fixture chains to `root.der`, a throwaway P-256 CA that is in no
machine's trust store anywhere. No private keys are committed — the tests only
verify, they never sign — so nothing here can mint a new certificate; re-run
`mint.sh` (provenance only) to regenerate the set from fresh keys.

| file | what it is | why it exists |
| --- | --- | --- |
| `root.der` | CA, `pathlen:1`, 2025‑01‑01 → 2035‑01‑01 | the anchor every local chain is verified against |
| `inter.der` | CA under `root`, `pathlen:0` | the intermediate the missing‑intermediate case omits |
| `notca.der` | `CA:FALSE`, signed by `root` | an issuer that is not allowed to be one |
| `good.der` | leaf, `root`‑signed, 2026 → 2027, SAN `test.aterm.invalid` | THE positive control |
| `expired.der` | same, 2025 → 2026 | rejected at a `now` past its window, accepted inside it |
| `future.der` | same, 2027 → 2028 | rejected at a `now` before its window, accepted inside it |
| `wronghost.der` | same, SAN `other.aterm.invalid` | rejected for `test.aterm.invalid`, accepted for its own name |
| `viainter.der` | leaf under `inter` | accepted WITH `inter`, rejected without it |
| `vianotca.der` | leaf under `notca` | a `CA:FALSE` certificate must not be able to issue |
| `selfsigned.der` | leaf that is its own issuer | chains to no anchor |
| `nosan.der` | leaf with a CN but no `subjectAltName` | a common‑name match is not a name match |
| `clientonly.der` | leaf with `extendedKeyUsage=clientAuth` | wrong purpose |
| `noeku.der` | leaf with NO `extendedKeyUsage` at all | see the divergence note in `verifier.rs` |
| `keyenciph.der` | leaf with `keyUsage=keyEncipherment` only | no `digitalSignature` bit |
| `ipsan.der` | leaf whose only SAN is `IP:127.0.0.1` | pins the `ServerName::IpAddress` → hostname‑string conversion |
| `tampersig.der` | `good.der` with the last signature byte flipped | a verifier that never checks the signature passes everything else |
| `tampertbs.der` | `good.der` with a SAN byte rewritten inside the signed body | as above, from the other direction |
| `malformed.der` | nine bytes that are not a certificate | the wrong‑reason control: this must report `BadEncoding`, never a trust verdict |
| `gh-leaf.der`, `gh-int0.der`, `gh-int1.der` | github.com's real chain, captured 2026‑08‑29 | proves the machine's real system trust store is consulted |
| `cl-leaf.der`, `cl-int0.der`, `cl-int1.der` | downloads.claude.ai's real chain, captured 2026‑08‑29 | the SECOND positive control, under an independent root |

`mint.sh` regenerates everything except the `gh-*` and `cl-*` captures, and its
header records the two properties the LOCAL corpus must never lose (no leaf over
~398 days, and no `authorityInformationAccess` / `crlDistributionPoints`
anywhere).

## The two captured chains, and why there are two

They are the only fixtures a real system anchor validates, so they are the only
source of an ACCEPT in the shipped configuration — which makes them the only
thing standing between
`crates/aterm-http/tests/verifier_differential.rs` and a suite that a verifier
returning `Err` for every input would pass. There are two of them, under roots
from different operators with different key types, so that one CA leaving the
platform trust store cannot disarm the suite on its own:

| chain | anchor | why this host |
| --- | --- | --- |
| `gh-*` | Sectigo Public Server Authentication Root E46 (ECC, to 2038‑01‑18), via USERTrust ECC | `aterm-update-core` fetches releases from github.com / api.github.com |
| `cl-*` | Google Trust Services GTS Root R1 (RSA), cross‑signed by GlobalSign Root CA | `crates/atpkg/src/vendor.rs:59` allow‑lists downloads.claude.ai |

**Time is pinned on both sides of every test**, so neither chain rots when its
leaf expires — `gh-leaf` on 2026‑09‑30 and `cl-leaf` on 2026‑10‑16, both already
in the past for most readers of this file, and both still perfectly usable.
Pinning does NOT protect against the anchor leaving the trust store; that is what
`the_positive_control_is_armed_and_not_inert` fails loudly about.

Unlike the local fixtures, these DO carry `authorityInformationAccess` — that is
what makes `a_missing_intermediate_is_not_repaired_by_a_network_fetch` a real
test rather than an empty claim.

### Recapture

Needed only when a test says so. One host at a time; the other keeps the suite
armed meanwhile.

```sh
host=github.com   # or downloads.claude.ai
openssl s_client -connect "$host:443" -servername "$host" -showcerts </dev/null \
  > chain.txt
# split chain.txt into its PEM blocks IN ORDER, then for each block N:
#   openssl x509 -in blockN.pem -outform DER -out gh-leaf.der   # N=0
#   openssl x509 -in blockN.pem -outform DER -out gh-int0.der   # N=1
#   openssl x509 -in blockN.pem -outform DER -out gh-int1.der   # N=2
```

Then update `T_REAL` in `tests/verifier_differential.rs` to an instant inside the
NEW leaf's validity window (`openssl x509 -noout -dates` prints it), and check
that the same instant still sits inside the OTHER captured leaf's window — one
constant serves both.
