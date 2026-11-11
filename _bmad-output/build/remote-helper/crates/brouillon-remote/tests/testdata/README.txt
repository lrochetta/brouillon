Test-only TLS material for the in-process fake FTPS server (tests/ftp_fake). NOT secrets: the keys protect nothing.

ca.der         self-signed test CA (ECDSA P-256, 100 years)
leaf.der       server certificate for localhost / 127.0.0.1 signed by the CA   (+ leaf.key.der, PKCS#8)
other.der      self-signed certificate for localhost, unrelated to the CA     (+ other.key.der, PKCS#8)

Regenerated with `openssl ecparam / req / x509`; the tests compute fingerprints from the DER files, so replacing them is safe.
