# flipper-companion — convenience wrapper around Cargo.
#
# The canonical interface remains Cargo; this Makefile only provides short
# aliases for the commands used every day.

CARGO ?= cargo
CLI   := target/debug/flipper
GUI   := target/debug/flipper-companion
CLI_RELEASE := target/release/flipper
GUI_RELEASE := target/release/flipper-companion

.DEFAULT_GOAL := all
.PHONY: all build release test check fmt fmt-check clippy run cli validate flash \
        devices firmware clean install-udev dist publish-crates anonymity hooks help

# ---------------------------------------------------------------------------
# Default: debug build of every crate
# ---------------------------------------------------------------------------
all: build

build:
	$(CARGO) build --workspace
	@printf '\nBuilt:\n  CLI: %s\n  GUI: %s\n' "$(CLI)" "$(GUI)"

release:
	$(CARGO) build --release --workspace
	@printf '\nBuilt:\n  CLI: %s\n  GUI: %s\n' "$(CLI_RELEASE)" "$(GUI_RELEASE)"

# ---------------------------------------------------------------------------
# Quality gates
# ---------------------------------------------------------------------------
test:
	$(CARGO) test --workspace

fmt:
	@if command -v rustfmt >/dev/null 2>&1; then \
		$(CARGO) fmt --all; \
	else \
		echo "rustfmt is not installed (rustup component add rustfmt)"; \
		exit 1; \
	fi

fmt-check:
	@if command -v rustfmt >/dev/null 2>&1; then \
		$(CARGO) fmt --all -- --check; \
	else \
		echo "rustfmt is not installed (rustup component add rustfmt)"; \
		exit 1; \
	fi

clippy:
	@if command -v cargo-clippy >/dev/null 2>&1; then \
		$(CARGO) clippy --workspace --all-targets -- -D warnings; \
	else \
		echo "clippy is not installed (rustup component add clippy)"; \
		exit 1; \
	fi

# Full local quality gate, same as CI.
check: fmt-check clippy test

# ---------------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------------
run:
	$(CARGO) run -p flipper-companion

cli:
	$(CARGO) run -q -p flipper-companion-cli -- $(ARGS)

# Convenience wrappers around the CLI.
devices:
	$(CARGO) run -q -p flipper-companion-cli -- devices

firmware:
	$(CARGO) run -q -p flipper-companion-cli -- firmware list --channel release

validate:
	@test -n "$(FILE)" || { echo "usage: make validate FILE=path/to/image.dfu"; exit 1; }
	$(CARGO) run -q -p flipper-companion-cli -- validate "$(FILE)"

flash:
	@test -n "$(FILE)" || { echo "usage: make flash FILE=path/to/image.dfu"; exit 1; }
	$(CARGO) run -q -p flipper-companion-cli -- flash "$(FILE)" $(if $(YES),--yes,)

# ---------------------------------------------------------------------------
# Release artefacts and publication
# ---------------------------------------------------------------------------
# Build the tarball and, when nfpm is installed, the .deb and .rpm packages.
dist:
	packaging/build-local.sh

# Publish every workspace crate to crates.io in dependency order.
publish-crates:
	packaging/publish-crates.sh

# Check that no personal or environment-specific data is about to be published.
anonymity:
	python3 tools/check_anonymity.py

# Install the git hooks (pre-push personal data check).
hooks:
	git config core.hooksPath .githooks
	@echo "git hooks installed: .githooks (pre-push => tools/check_anonymity.py)"

# ---------------------------------------------------------------------------
# Housekeeping
# ---------------------------------------------------------------------------
install-udev:
	su - -c 'install -m 0644 "$(CURDIR)/udev/42-flipperzero.rules" /etc/udev/rules.d/42-flipperzero.rules'
	su - -c 'udevadm control --reload-rules && udevadm trigger --subsystem-match=tty'
	@echo "udev rules installed."

clean:
	$(CARGO) clean

help:
	@printf '%s\n' \
		'Usage: make [target]' \
		'' \
		'  make                 build the workspace (debug)' \
		'  make release         build optimized binaries' \
		'  make test            run all tests (99, no hardware needed)' \
		'  make check           fmt-check + clippy + test (same as CI)' \
		'  make fmt             format the workspace' \
		'  make clippy          lint with -D warnings' \
		'  make run             launch the Slint desktop application' \
		'  make cli ARGS="..."  run the CLI, e.g. ARGS="devices"' \
		'  make validate FILE=...   validate a .dfu image offline' \
		'  make flash FILE=... [YES=1]   flash a .dfu image' \
		'  make devices         list connected Flipper devices' \
		'  make firmware        list the latest release channel' \
		'  make install-udev    install Linux USB permissions (root via su)' \
		'  make dist            build tarball, .deb and .rpm (nfpm)' \
		'  make publish-crates  publish the workspace crates to crates.io' \
		'  make anonymity       check for personal data before publishing' \
		'  make hooks           install the git hooks' \
		'  make clean           remove build artifacts'
