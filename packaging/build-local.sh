#!/usr/bin/env bash
# Build the release artefacts locally:
#   dist/flipper-companion-<version>-linux-<arch>.tar.gz
#   dist/flipper-companion_<version>-1_<debarch>.deb   (when nfpm is available)
#   dist/flipper-companion-<version>-1.<rpmarch>.rpm   (when nfpm is available)
#   dist/SHA256SUMS
#
# The binaries link against the system C libraries and the usual font/OpenSSL
# stack (Slint renders text through fontconfig/freetype), so they are built
# with the host glibc toolchain rather than musl.
#
# Usage: packaging/build-local.sh [--no-nfpm]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)"
[ -n "$VERSION" ] || { echo "cannot read the version from Cargo.toml" >&2; exit 1; }

case "$(uname -m)" in
    x86_64|amd64) TAR_ARCH=amd64; DEB_ARCH=amd64; RPM_ARCH=x86_64 ;;
    aarch64|arm64) TAR_ARCH=arm64; DEB_ARCH=arm64; RPM_ARCH=aarch64 ;;
    *) echo "unsupported host architecture: $(uname -m)" >&2; exit 1 ;;
esac

WITH_NFPM=1
[ "${1:-}" = "--no-nfpm" ] && WITH_NFPM=0

cargo build --release --locked --workspace

rm -rf dist stage pkgroot
mkdir -p dist "stage/flipper-companion-$VERSION-linux-$TAR_ARCH"
STAGE="stage/flipper-companion-$VERSION-linux-$TAR_ARCH"

cp target/release/flipper target/release/flipper-companion "$STAGE/"
cp LICENSE README.md CHANGELOG.md NOTICE.md "$STAGE/"
cp -r docs "$STAGE/docs"
cp udev/42-flipperzero.rules "$STAGE/udev.rules"

tar -C stage -czf "dist/flipper-companion-$VERSION-linux-$TAR_ARCH.tar.gz" \
    "flipper-companion-$VERSION-linux-$TAR_ARCH"

if [ "$WITH_NFPM" = 1 ] && command -v nfpm >/dev/null 2>&1; then
    mkdir -p pkgroot
    cp target/release/flipper target/release/flipper-companion pkgroot/
    cp packaging/flipper-companion.desktop packaging/postinstall.sh pkgroot/
    cp crates/flipper-gui/ui/icon.svg pkgroot/flipper-companion.svg
    cp udev/42-flipperzero.rules pkgroot/udev.rules
    cp LICENSE README.md CHANGELOG.md pkgroot/
    cp -r docs pkgroot/docs
    (cd pkgroot && PKG_VERSION="$VERSION" PKG_ARCH="$DEB_ARCH" \
        nfpm package -f "$ROOT/packaging/nfpm.yaml" -p deb -t "$ROOT/dist/")
    (cd pkgroot && PKG_VERSION="$VERSION" PKG_ARCH="$RPM_ARCH" \
        nfpm package -f "$ROOT/packaging/nfpm.yaml" -p rpm -t "$ROOT/dist/")
else
    echo "nfpm not found (or --no-nfpm): skipping .deb/.rpm" >&2
fi

(cd dist && sha256sum ./* > SHA256SUMS)
echo "artefacts in $ROOT/dist:"
ls -lh dist
