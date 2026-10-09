`oidc-test-key.pem` is a disposable Ed25519 signing key for the mock identity
provider in unit tests. It is public test data and must never be used by a real
identity provider. Production Diffrook only verifies signatures using IdP public
keys and does not load this file.
