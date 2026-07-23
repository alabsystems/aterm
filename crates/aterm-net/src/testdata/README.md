# Insecure test credentials

`cert.der` and `key.pkcs8.der` are static, intentionally insecure credentials
used only by local loopback TLS tests. The private key is public test data. Do
not install, trust, or reuse either file in any real deployment.
