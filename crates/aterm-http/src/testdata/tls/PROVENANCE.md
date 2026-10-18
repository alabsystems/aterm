<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# How the local TLS fixtures were minted

The DERs in this directory are committed, and nothing in the build or the test
run regenerates them. This file records how the local set (everything but the
`gh-*` chain, whose capture is in `README.md`) was minted on 2026-08-29, so a
re-mint keeps the corpus's rules. It replaces `mint.sh`, which nothing ran.

## The two rules the corpus keeps

1. **No leaf may be valid for more than about 398 days.** Apple rejects a
   longer-lived TLS server certificate outright
   (`errSecCertificateValidityPeriodTooLong`, -67901) before it reaches the
   reason a negative case is testing, which turns a suite of reject cases green
   for the wrong reason. The repository's habit elsewhere of minting
   century-long test certificates
   (`crates/aterm-http/tests/testdata/loopback-cert.der` runs to 2126) breaks
   here.
2. **No leaf and no CA may carry `authorityInformationAccess` or
   `crlDistributionPoints`.** With neither extension there is no URL for macOS
   `trustd` or Windows `crypt32` to fetch, so a chain this corpus deliberately
   broke cannot be repaired by a network round trip mid-test.

No private key is committed: the tests only verify. A re-mint makes a fresh,
unrelated key for every subject, so every DER changes.

## The invocations

OpenSSL 3.x is required for `-not_before`/`-not_after` (certificates whose
window is in the past or the future). macOS's `/usr/bin/openssl` is LibreSSL
and has neither flag; Homebrew's `/opt/homebrew/bin/openssl` does. Run from
this directory, with `HOST=test.aterm.invalid` and `OTHER=other.aterm.invalid`.

The anchors:

```text
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout root.key -out root.pem \
  -not_before 20250101000000Z -not_after 20350101000000Z -subj "/CN=aterm oracle root CA" \
  -addext "basicConstraints=critical,CA:TRUE,pathlen:1" -addext "keyUsage=critical,keyCertSign,cRLSign"

openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout inter.key -out inter.csr \
  -subj "/CN=aterm oracle intermediate CA"
# inter.ext: basicConstraints=critical,CA:TRUE,pathlen:0
#            keyUsage=critical,keyCertSign,cRLSign
openssl x509 -req -in inter.csr -CA root.pem -CAkey root.key -set_serial 0x02 \
  -not_before 20250101000000Z -not_after 20350101000000Z -extfile inter.ext -out inter.pem

# A CA:FALSE certificate that is nevertheless used to sign a leaf below.
openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout notca.key -out notca.csr \
  -subj "/CN=aterm oracle NOT-a-CA"
# notca.ext: basicConstraints=critical,CA:FALSE
#            keyUsage=critical,digitalSignature
openssl x509 -req -in notca.csr -CA root.pem -CAkey root.key -set_serial 0x23 \
  -not_before 20250101000000Z -not_after 20350101000000Z -extfile notca.ext -out notca.pem
```

Each leaf is one key, one CSR and one signature, with an extension file of four
lines: `basicConstraints=critical,CA:FALSE`, then the key-usage, extended
key-usage and subject-alternative-name lines of the table below (`-` means the
line is left out):

```text
openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout NAME.key -out NAME.csr -subj "/CN=CN"
openssl x509 -req -in NAME.csr -CA SIGNER.pem -CAkey SIGNER.key -set_serial SERIAL \
  -not_before NOT_BEFORE -not_after NOT_AFTER -extfile NAME.ext -out NAME.pem
```

| NAME | SIGNER | SERIAL | NOT_BEFORE → NOT_AFTER | CN | keyUsage | extendedKeyUsage | subjectAltName |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `good` | root | 0x11 | 20260101000000Z → 20270101000000Z | HOST | `critical,digitalSignature` | `serverAuth` | `DNS:HOST` |
| `expired` | root | 0x12 | 20250101000000Z → 20260101000000Z | HOST | `critical,digitalSignature` | `serverAuth` | `DNS:HOST` |
| `future` | root | 0x13 | 20270101000000Z → 20280101000000Z | HOST | `critical,digitalSignature` | `serverAuth` | `DNS:HOST` |
| `wronghost` | root | 0x14 | 20260101000000Z → 20270101000000Z | OTHER | `critical,digitalSignature` | `serverAuth` | `DNS:OTHER` |
| `viainter` | inter | 0x15 | 20260101000000Z → 20270101000000Z | HOST | `critical,digitalSignature` | `serverAuth` | `DNS:HOST` |
| `nosan` | root | 0x21 | 20260101000000Z → 20270101000000Z | HOST | `critical,digitalSignature` | `serverAuth` | - |
| `clientonly` | root | 0x22 | 20260101000000Z → 20270101000000Z | HOST | `critical,digitalSignature` | `clientAuth` | `DNS:HOST` |
| `vianotca` | notca | 0x24 | 20260101000000Z → 20270101000000Z | HOST | `critical,digitalSignature` | `serverAuth` | `DNS:HOST` |
| `ipsan` | root | 0x31 | 20260101000000Z → 20270101000000Z | 127.0.0.1 | `critical,digitalSignature` | `serverAuth` | `IP:127.0.0.1` |
| `keyenciph` | root | 0x32 | 20260101000000Z → 20270101000000Z | HOST | `critical,keyEncipherment` | `serverAuth` | `DNS:HOST` |
| `noeku` | root | 0x33 | 20260101000000Z → 20270101000000Z | HOST | `critical,digitalSignature` | - | `DNS:HOST` |

The self-signed leaf, its own issuer, chains to nothing:

```text
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout selfsigned.key -out selfsigned.pem \
  -not_before 20260101000000Z -not_after 20270101000000Z -subj "/CN=HOST" \
  -addext "basicConstraints=critical,CA:FALSE" -addext "keyUsage=critical,digitalSignature" \
  -addext "extendedKeyUsage=serverAuth" -addext "subjectAltName=DNS:HOST"
```

Every PEM above becomes its DER with `openssl x509 -in NAME.pem -outform DER
-out NAME.der`; then the PEM, CSR, extension and key files are deleted.

## The corruptions and the control

- `tampersig.der` is `good.der` with the last byte (inside the signature) XORed
  with 1.
- `tampertbs.der` is `good.der` with the first byte of `test.aterm.invalid`
  (inside the signed body's SAN) rewritten to `b`.
- `malformed.der` is the nine bytes `30 82 00 05 de ad be ef 00`: not a
  certificate at all, so a DER parse failure must be reported as `BadEncoding`
  and never counted as a trust verdict.
