"""Fail if a production binary embeds the disposable OIDC test signing key."""
import base64
from pathlib import Path
import sys
import hashlib
import tarfile


def main():
    fixture = Path(__file__).resolve().parent.parent / "tests/fixtures/oidc-test-key.pem"
    pem = fixture.read_bytes()
    encoded = b"".join(
        line.strip() for line in pem.splitlines() if not line.startswith(b"-----")
    )
    der = base64.b64decode(encoded, validate=True)
    # This fixture is a PKCS8 Ed25519 key; its final 32 bytes are the private seed.
    candidates = [pem.strip(), encoded, der, der[-32:]]
    if len(sys.argv) < 2:
        sys.exit("Usage: check-release-test-key.py BINARY_OR_TAR_GZ [...]")
    for argument in sys.argv[1:]:
        path = Path(argument)
        if path.name.endswith(".tar.gz"):
            with tarfile.open(path, "r:gz") as archive:
                binary = archive.extractfile("diffrook").read()
        else:
            binary = path.read_bytes()
        if any(value in binary for value in candidates):
            sys.exit(f"FAIL: {path.name} contains OIDC test private key material")
        print(f"PASS: {path.name}: OIDC test private key material is absent")
        print("Binary SHA256:", hashlib.sha256(binary).hexdigest())
        print("PEM delimiter occurrences:", binary.count(b"BEGIN PRIVATE KEY"))
    # PEM parsers can embed delimiters without embedding any private key.


if __name__ == "__main__":
    main()
