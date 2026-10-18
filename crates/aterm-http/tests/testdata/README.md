# Insecure test credentials

`loopback-cert.der` and `loopback-key.pkcs8.der` are a static, intentionally
insecure self-signed certificate and its key, used only by the loopback TLS
tests in `tests/tls_stream.rs`. The private key is public test data. Do not
install, trust, or reuse either file in any real deployment.
