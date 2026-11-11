#!/bin/sh
# Refresh the udev rules so the Flipper is usable without replugging.
if command -v udevadm >/dev/null 2>&1; then
    udevadm control --reload-rules >/dev/null 2>&1 || true
    udevadm trigger --subsystem-match=tty >/dev/null 2>&1 || true
fi
exit 0
