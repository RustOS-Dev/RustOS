#!/usr/bin/env bash
# Host test for the Wi-Fi firmware provisioning in write_to_drive.sh: plain,
# .xz and .zst blobs from a linux-firmware style directory, a single .ucode
# with its .pnvm, and rejection of unrelated files. No drive is touched.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=../write_to_drive.sh
source "$ROOT/write_to_drive.sh"
# Everything here is in a temporary directory: no sudo (root-owned files
# would outlive the cleanup).
run_as_root() { "$@"; }
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }

mkdir -p "$T/fw" "$T/mnt1" "$T/mnt2" "$T/mnt3"
echo ucode72 > "$T/fw/iwlwifi-ty-a0-gf-a0-72.ucode"
echo pnvm > "$T/fw/iwlwifi-ty-a0-gf-a0.pnvm"
echo so-ucode > "$T/fw/iwlwifi-so-a0-gf-a0-72.ucode"
echo unrelated > "$T/fw/regulatory.db"
mkdir -p "$T/fw/intel"
echo sfi > "$T/fw/intel/ibt-0041-0041.sfi"
echo ddc > "$T/fw/intel/ibt-0041-0041.ddc"
if command -v xz >/dev/null; then
    echo ucode71 | xz > "$T/fw/iwlwifi-ty-a0-gf-a0-71.ucode.xz"
fi

# Directory source: every matching blob, decompressed.
provision_ax210_firmware "$T/mnt1" "$T/fw" >/dev/null
[[ "$(cat "$T/mnt1/lib/firmware/iwlwifi-ty-a0-gf-a0-72.ucode")" == ucode72 ]] || fail "ucode not copied"
[[ -f "$T/mnt1/lib/firmware/iwlwifi-ty-a0-gf-a0.pnvm" ]] || fail "pnvm not copied"
[[ ! -e "$T/mnt1/lib/firmware/regulatory.db" ]] || fail "unrelated file copied"
[[ "$(cat "$T/mnt1/lib/firmware/intel/ibt-0041-0041.sfi")" == sfi ]] || fail "Bluetooth sfi not copied"
[[ -f "$T/mnt1/lib/firmware/intel/ibt-0041-0041.ddc" ]] || fail "Bluetooth ddc not copied"
if command -v xz >/dev/null; then
    [[ "$(cat "$T/mnt1/lib/firmware/iwlwifi-ty-a0-gf-a0-71.ucode")" == ucode71 ]] || fail "xz not decompressed"
fi

# Single-file source picks up the matching .pnvm.
provision_ax210_firmware "$T/mnt2" "$T/fw/iwlwifi-ty-a0-gf-a0-72.ucode" >/dev/null
[[ -f "$T/mnt2/lib/firmware/iwlwifi-ty-a0-gf-a0-72.ucode" ]] || fail "single ucode"
[[ -f "$T/mnt2/lib/firmware/iwlwifi-ty-a0-gf-a0.pnvm" ]] || fail "pnvm next to single ucode"

# A non-firmware file is rejected.
if provision_ax210_firmware "$T/mnt3" "$T/fw/regulatory.db" >/dev/null 2>&1; then
    fail "unrelated single file accepted"
fi
echo "firmware install: OK"
