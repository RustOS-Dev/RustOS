#!/usr/bin/env bash
# run-qemu-uefi.sh — UEFI QEMU runner for RustOS test binaries and the main kernel.
#
# Usage (via .cargo/config.toml runner):
#   run-qemu-uefi.sh <kernel-elf> [extra-qemu-args...]
#
# The script:
#   1. Builds a UEFI disk image from the kernel ELF using crates/create-image.
#   2. Launches qemu-system-x86_64 with OVMF firmware.
#
# Test binaries (anything under target/.../deps/) run headless with a timeout
# and the isa-debug-exit code mapped to a normal process status
# (0x10 -> success, anything else -> failure).
#
# Environment:
#   RUSTOS_QEMU_ARGS     extra QEMU arguments appended to every run
#   RUSTOS_TEST_TIMEOUT  seconds before a test run is killed (default 300)
#   RUSTOS_QEMU_PROFILE  name of a device profile in tests/qemu-profiles/
#                        (for tests, defaults to the test binary's name)

set -e

BINARY="$1"
shift

REPO_ROOT="$(cd "$(dirname "$(realpath "${BASH_SOURCE[0]}")")" && pwd)"
TMPDIR_WORK="$(mktemp -d)"
IMG="$TMPDIR_WORK/rustos.img"

cleanup() {
    rm -rf "$TMPDIR_WORK"
}
trap cleanup EXIT

# Build UEFI disk image from the kernel ELF
(
    cd "$REPO_ROOT/crates/create-image"
    cargo run \
        --quiet \
        -- "$BINARY" "$IMG"
)

# Locate OVMF firmware (path varies by distro)
OVMF=""
for candidate in \
    /usr/share/OVMF/OVMF_CODE.fd \
    /usr/share/OVMF/OVMF_CODE_4M.fd \
    /usr/share/ovmf/OVMF.fd \
    /usr/share/edk2/ovmf/OVMF_CODE.fd \
    /usr/share/qemu/OVMF.fd; do
    if [ -f "$candidate" ]; then
        OVMF="$candidate"
        break
    fi
done

if [ -z "$OVMF" ]; then
    echo "Error: OVMF firmware not found. Install it with: sudo apt-get install ovmf" >&2
    exit 1
fi

ACCEL=()
if [ -w /dev/kvm ]; then
    ACCEL=(-accel kvm -cpu host)
else
    ACCEL=(-cpu max)
fi

QEMU_ARGS=(
    -drive "if=pflash,format=raw,readonly=on,file=$OVMF"
    -drive "format=raw,file=$IMG"
    -device isa-debug-exit,iobase=0xf4,iosize=0x04
    -machine q35
    -m 512M
    "${ACCEL[@]}"
)

case "$BINARY" in
*/deps/*)
    # Test binary: headless, bounded, device profile by test name.
    name="$(basename "$BINARY")"
    name="${name%-*}"
    profile="${RUSTOS_QEMU_PROFILE:-$name}"
    profile_file="$REPO_ROOT/tests/qemu-profiles/$profile.args"
    PROFILE_ARGS=()
    if [ -f "$profile_file" ]; then
        # shellcheck disable=SC2207
        PROFILE_ARGS=($(grep -v '^#' "$profile_file" | sed "s|@TMP@|$TMPDIR_WORK|g"))
    fi
    # shellcheck disable=SC2086
    set +e
    timeout "${RUSTOS_TEST_TIMEOUT:-300}" qemu-system-x86_64 \
        "${QEMU_ARGS[@]}" \
        -serial stdio \
        -display none \
        "${PROFILE_ARGS[@]}" \
        $RUSTOS_QEMU_ARGS \
        "$@"
    status=$?
    set -e
    case $status in
    33) exit 0 ;;   # (0x10 << 1) | 1 — QemuExitCode::Success
    124) echo "[runner] test timed out" >&2; exit 1 ;;
    *) exit 1 ;;
    esac
    ;;
*)
    # shellcheck disable=SC2086
    exec qemu-system-x86_64 \
        "${QEMU_ARGS[@]}" \
        -serial stdio \
        $RUSTOS_QEMU_ARGS \
        "$@"
    ;;
esac
