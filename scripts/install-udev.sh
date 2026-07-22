#!/usr/bin/env bash
# Install the tartarust udev rule so the Tartarus Pro input/hidraw nodes
# and /dev/uinput are accessible without root. Run with sudo:
#
#   sudo scripts/install-udev.sh
#
# After this finishes the daemon can run as your normal user. No re-login is
# needed (the rule grants /dev/uinput to "plugdev", and uaccess handles the
# device nodes for the active seat) — UNLESS you were just added to "plugdev"
# below, in which case start a new login session first.
set -euo pipefail

RULE_NAME="99-tartarust.rules"
DEST_DIR="/etc/udev/rules.d"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC_RULE="${SCRIPT_DIR}/../docs/packaging/${RULE_NAME}"

if [[ "${EUID}" -ne 0 ]]; then
    echo "error: must run as root (use: sudo $0)" >&2
    exit 1
fi

if [[ ! -f "${SRC_RULE}" ]]; then
    echo "error: rule file not found at ${SRC_RULE}" >&2
    exit 1
fi

# The non-root user this is being installed for (the human who ran sudo).
TARGET_USER="${SUDO_USER:-}"

echo "==> Ensuring 'plugdev' group exists"
groupadd -f plugdev

if [[ -n "${TARGET_USER}" ]]; then
    if id -nG "${TARGET_USER}" | tr ' ' '\n' | grep -qx plugdev; then
        echo "==> User '${TARGET_USER}' is already in 'plugdev'"
    else
        echo "==> Adding user '${TARGET_USER}' to 'plugdev' (re-login required for this to take effect)"
        usermod -aG plugdev "${TARGET_USER}"
        RELOGIN_NEEDED=1
    fi
fi

echo "==> Ensuring the uinput kernel module is loaded now and at boot"
modprobe uinput || echo "    (modprobe uinput failed — it may be built into the kernel; continuing)"
echo uinput > /etc/modules-load.d/uinput.conf

echo "==> Installing ${RULE_NAME} to ${DEST_DIR}"
install -m 0644 "${SRC_RULE}" "${DEST_DIR}/${RULE_NAME}"

echo "==> Reloading udev rules and re-triggering matching devices"
udevadm control --reload
udevadm trigger --subsystem-match=input --subsystem-match=hidraw --subsystem-match=misc

echo
echo "Done. The Tartarus Pro input/hidraw nodes and /dev/uinput should now be"
echo "accessible to your user."
echo
echo "Note: on many distros the input event nodes and /dev/uinput already default"
echo "to GROUP=input (mode 660). If your user is in the 'input' group, the daemon"
echo "may have worked even before this rule. This rule makes access explicit and"
echo "portable via 'plugdev' + uaccess."
if [[ "${RELOGIN_NEEDED:-0}" -eq 1 ]]; then
    echo
    echo "NOTE: '${TARGET_USER}' was just added to 'plugdev'. Log out and back in"
    echo "      (or run 'newgrp plugdev' in your shell) before starting the daemon."
fi
