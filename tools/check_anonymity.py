#!/usr/bin/env python3
"""Fail when a repository file leaks personal or environment-specific data.

The check is intentionally conservative: it looks for absolute home paths,
e-mail addresses, private or public IP addresses, MAC addresses, Flipper
serials, host names, credentials and a list of known personal markers.

Only documentation ranges (RFC 2606, RFC 5737) and loopback addresses are
accepted.  Run it from a pre-push hook or in CI:

    python3 tools/check_anonymity.py [--all]

By default every file tracked by git is scanned; ``--all`` also walks
untracked files (handy before the first commit).
"""

from __future__ import annotations

import argparse
import ipaddress
import os
import re
import subprocess
import sys
from pathlib import Path

# Extra literals to look for (device names, account handles, ...) are kept out
# of the repository: provide them through the PERSONAL_MARKERS environment
# variable (comma separated) or an untracked `.personal-markers` file, one
# marker per line.
MARKER_FILE = Path(".personal-markers")


def personal_markers() -> list[str]:
    markers = [item.strip() for item in os.environ.get("PERSONAL_MARKERS", "").split(",")]
    markers = [marker for marker in markers if marker]

    if MARKER_FILE.is_file():
        for line in MARKER_FILE.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if line and not line.startswith("#"):
                markers.append(line)

    return markers

PATTERNS = {
    "home path": re.compile(r"/(?:home|Users)/([A-Za-z0-9._-]+)"),
    "windows home": re.compile(r"[A-Za-z]:\\\\Users\\\\[A-Za-z0-9._-]+"),
    "e-mail": re.compile(r"\b[A-Za-z0-9._%+-]+@([A-Za-z0-9.-]+\.[A-Za-z]{2,})\b"),
    "ipv4": re.compile(r"\b(?:\d{1,3}\.){3}\d{1,3}\b"),
    "mac address": re.compile(r"\b(?:[0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}\b"),
    "flipper serial": re.compile(r"\bflip_[A-Za-z0-9]{3,}\b"),
    "host name": re.compile(r"\b[A-Za-z0-9-]+\.(?:local|lan|home|internal|corp)\b(?![\w-])"),
    "private key": re.compile(r"-----BEGIN (?:OPENSSH|RSA|EC|PGP) PRIVATE KEY-----"),
    "token": re.compile(r"\b(?:ghp_[A-Za-z0-9]{20,}|glpat-[A-Za-z0-9_-]{20,}|AKIA[0-9A-Z]{16})\b"),
}

# Values that are safe to publish.
ALLOWED_EMAIL_DOMAINS = {
    "example.com",
    "example.org",
    "example.net",
    "localhost",
    "github.com",
    "gitlab.com",
}
ALLOWED_IPS = {
    "0.0.0.0",
    "127.0.0.1",
    "1.2.3.4",  # placeholder used in version-parsing tests
    "255.255.255.255",
}
ALLOWED_HOME_NAMES = {"user", "example", "runner", "builder"}

BINARY_SUFFIXES = {
    ".png", ".jpg", ".jpeg", ".gif", ".webp", ".ico", ".pdf",
    ".gz", ".xz", ".zip", ".tar", ".dfu", ".fap", ".bin", ".o", ".rlib",
}


def tracked_files(include_untracked: bool) -> list[Path]:
    command = ["git", "ls-files", "-z"]
    if include_untracked:
        command = ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"]

    result = subprocess.run(command, capture_output=True, check=True)
    return [Path(name) for name in result.stdout.decode().split("\0") if name]


def is_allowed_ip(value: str) -> bool:
    if value in ALLOWED_IPS:
        return True

    try:
        address = ipaddress.ip_address(value)
    except ValueError:
        # Not an address at all (e.g. a version such as 1.4.341.0).
        return True

    # RFC 5737 documentation ranges and the RFC 3849 IPv6 prefix.
    return (
        address in ipaddress.ip_network("192.0.2.0/24")
        or address in ipaddress.ip_network("198.51.100.0/24")
        or address in ipaddress.ip_network("203.0.113.0/24")
        or address in ipaddress.ip_network("2001:db8::/32")
    )


def scan_text(path: Path, text: str) -> list[str]:
    findings: list[str] = []

    markers = personal_markers()

    for line_number, line in enumerate(text.splitlines(), start=1):
        for marker in markers:
            if marker in line:
                findings.append(f"{path}:{line_number}: personal marker `{marker}`")

        for label, pattern in PATTERNS.items():
            for match in pattern.finditer(line):
                value = match.group(0)

                if label == "home path" and match.group(1) in ALLOWED_HOME_NAMES:
                    continue
                if label == "e-mail" and match.group(1).lower() in ALLOWED_EMAIL_DOMAINS:
                    continue
                if label == "ipv4" and is_allowed_ip(value):
                    continue
                if label == "flipper serial" and value in markers:
                    continue
                if label == "host name" and value.startswith(("example.", "localhost")):
                    continue

                findings.append(f"{path}:{line_number}: possible {label} `{value}`")

    return findings


def scan_file(path: Path) -> list[str]:
    try:
        data = path.read_bytes()
    except OSError as error:
        return [f"{path}: cannot read ({error})"]

    # Images and archives are scanned as bytes so that embedded textual
    # metadata (PNG tEXt chunks, for instance) is still caught.
    text = data.decode("utf-8", errors="replace")
    return scan_text(path, text)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--all", action="store_true", help="also scan untracked files")
    arguments = parser.parse_args()

    findings: list[str] = []

    for path in tracked_files(arguments.all):
        if not path.exists() or path.is_dir():
            continue
        if path.suffix.lower() in BINARY_SUFFIXES and path.suffix.lower() != ".png":
            continue
        findings.extend(scan_file(path))

    if findings:
        print("Personal data check failed:", file=sys.stderr)
        for finding in findings:
            print(f"  {finding}", file=sys.stderr)
        print(
            "\nReplace these values with documentation placeholders "
            "(example.com, 192.0.2.0/24, synthetic serials) before publishing.",
            file=sys.stderr,
        )
        return 1

    print(f"Personal data check: {len(tracked_files(arguments.all))} file(s) scanned, nothing found.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
