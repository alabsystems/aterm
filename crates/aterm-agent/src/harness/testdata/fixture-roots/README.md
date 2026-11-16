<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# Insecure test credentials

A throwaway certificate authority and one server certificate under it, used only by
`codex_app_login_fixture_roots_tests.rs`: its loopback provider serves TLS with the leaf, and
the native fixture's read pins the CA as its only root. All three files are public test data.
Do not install, trust or reuse any of them.

| file | what it is |
| --- | --- |
| `ca.der` | P-256 CA, `CA:TRUE, pathlen:0`, `keyCertSign`, 2026-11-14 to 2126-10-21. In no trust store; its private key was deleted when it was minted, so nothing more can be signed under it. |
| `leaf.der` | P-256 leaf under `ca.der`, same validity, `serverAuth`, SAN `IP:127.0.0.1` and `DNS:localhost` only. |
| `leaf-key.pkcs8.der` | The leaf's private key, PKCS#8. Public test data: it can serve TLS for loopback names alone, under a CA nothing trusts. |

The validity is a century so that no test fails by a date. The tests verify with rustls's
webpki verifier (`aterm_http::Trust::Roots`), which sets no maximum on it.

## How they were minted (OpenSSL 4.0.3)

```sh
openssl ecparam -name prime256v1 -genkey -noout -out ca.key
openssl req -x509 -new -key ca.key -sha256 -days 36500 \
  -subj "/CN=aterm-agent fixture-roots test CA" \
  -addext "basicConstraints=critical,CA:TRUE,pathlen:0" \
  -addext "keyUsage=critical,keyCertSign" -out ca.pem
openssl ecparam -name prime256v1 -genkey -noout -out leaf.key
openssl req -new -key leaf.key -subj "/CN=aterm-agent fixture-roots test leaf" -out leaf.csr
printf '%s\n' 'basicConstraints=critical,CA:FALSE' 'keyUsage=critical,digitalSignature' \
  'extendedKeyUsage=serverAuth' 'subjectAltName=IP:127.0.0.1,DNS:localhost' > leaf.ext
openssl x509 -req -in leaf.csr -CA ca.pem -CAkey ca.key -CAcreateserial -sha256 -days 36500 \
  -extfile leaf.ext -out leaf.pem
openssl x509 -in ca.pem -outform DER -out ca.der
openssl x509 -in leaf.pem -outform DER -out leaf.der
openssl pkcs8 -topk8 -nocrypt -in leaf.key -outform DER -out leaf-key.pkcs8.der
rm ca.key leaf.key leaf.csr leaf.ext ca.srl ca.pem leaf.pem
```
