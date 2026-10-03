#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-2.0-or-later
#
# Boot RustOS in QEMU with a PCI GPU passed through (VFIO), for driver
# development without rebooting the host: nouveau on an NVIDIA card while
# the host runs on its integrated GPU (the round 4 plan's loop for the
# RTX 5070), or amdgpu on a second AMD card.
#
#   sudo tools/vfio-run.sh [--gpu 0000:01:00.0] [--audio 0000:01:00.1]
#                          [--driver nouveau|amdgpu] [--kernel ELF] [-- qemu args]
#
# Host setup, once: IOMMU on in the firmware setup, `amd_iommu=on iommu=pt`
# (or `intel_iommu=on iommu=pt`) on the host kernel command line, and the
# card not driven by the host (no nvidia/nouveau loaded for it). The script
# binds the GPU and its audio function to vfio-pci and gives them back to
# their previous drivers on exit.
#
# The guest gets a second disk, a RustOS storage partition holding the
# storage-partition firmware (firmware/storage.list: NVIDIA GSP) and an
# etc/kernel.conf that enables the driver (linux.enable=...,
# linux.debug=1, log.persist=1); the kernel log is copied out of it into
# target/vfio-logs/ when QEMU exits.
set -euo pipefail

ROOT="$(cd "$(dirname "$(realpath "${BASH_SOURCE[0]}")")/.." && pwd)"
GPU="0000:01:00.0"
AUDIO=""
DRIVER="nouveau"
KERNEL=""
MEM="${RUSTOS_VFIO_MEM:-8G}"
QEMU_EXTRA=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --gpu) GPU="$2"; shift 2 ;;
        --audio) AUDIO="$2"; shift 2 ;;
        --driver) DRIVER="$2"; shift 2 ;;
        --kernel) KERNEL="$2"; shift 2 ;;
        --) shift; QEMU_EXTRA=("$@"); break ;;
        -h|--help) sed -n '3,22p' "$0"; exit 0 ;;
        *) echo "vfio-run: unknown option $1" >&2; exit 1 ;;
    esac
done
[[ "$GPU" == *:*:*.* ]] || GPU="0000:$GPU"
if [[ -z "$AUDIO" ]]; then
    # The HDMI/DP audio function sits next to the GPU (function 1).
    cand="${GPU%.*}.1"
    [[ -e "/sys/bus/pci/devices/$cand" ]] && AUDIO="$cand"
fi
[[ "$(id -u)" -eq 0 ]] || { echo "vfio-run: run as root (binding devices to vfio-pci)" >&2; exit 1; }
[[ -e "/sys/bus/pci/devices/$GPU" ]] || { echo "vfio-run: no PCI device $GPU (see lspci -D)" >&2; exit 1; }
[[ -d /sys/kernel/iommu_groups/0 ]] || { echo "vfio-run: the host IOMMU is off (see the header of this script)" >&2; exit 1; }
for tool in qemu-system-x86_64 sfdisk mkfs.fat mcopy mmd; do
    command -v "$tool" >/dev/null || { echo "vfio-run: $tool is needed (qemu, util-linux, dosfstools, mtools)" >&2; exit 1; }
done
modprobe vfio-pci

# Build the kernel (release driver set with the GPU drivers) unless given.
if [[ -z "$KERNEL" ]]; then
    owner="$(stat -c %U "$ROOT")"
    (cd "$ROOT" && sudo -u "$owner" cargo build --release --features linux-drivers,linux-gpu)
    KERNEL="$ROOT/target/x86_64-rustos/release/rustos"
fi

# --- bind to vfio-pci, remembering the previous drivers -------------------
declare -A PREV
bind_vfio() {
    local dev="$1" drv=""
    [[ -e "/sys/bus/pci/devices/$dev/driver" ]] && drv="$(basename "$(readlink "/sys/bus/pci/devices/$dev/driver")")"
    PREV[$dev]="$drv"
    [[ "$drv" == vfio-pci ]] && return
    [[ -n "$drv" ]] && echo "$dev" > "/sys/bus/pci/devices/$dev/driver/unbind"
    echo vfio-pci > "/sys/bus/pci/devices/$dev/driver_override"
    echo "$dev" > /sys/bus/pci/drivers_probe
    echo "vfio-run: $dev bound to vfio-pci (was ${drv:-unbound})"
}
restore() {
    for dev in "${!PREV[@]}"; do
        echo > "/sys/bus/pci/devices/$dev/driver_override" || true
        [[ -e "/sys/bus/pci/devices/$dev/driver" ]] && echo "$dev" > "/sys/bus/pci/devices/$dev/driver/unbind" || true
        echo "$dev" > /sys/bus/pci/drivers_probe || true
        echo "vfio-run: $dev returned to ${PREV[$dev]:-no driver}"
    done
}
WORK="$(mktemp -d)"
cleanup() { restore; rm -rf "$WORK"; }
trap cleanup EXIT
bind_vfio "$GPU"
[[ -n "$AUDIO" ]] && bind_vfio "$AUDIO"

# --- the storage disk: firmware and kernel.conf ---------------------------
sh "$ROOT/tools/fetch-firmware.sh" "$ROOT/target/firmware-storage" "$ROOT/firmware/storage.list"
DISK="$WORK/storage.img"
truncate -s 512M "$DISK"
echo 'label: gpt
start=2048, type=EBD0A0A2-B9E5-4433-87C0-68B6B72699C7, name="rustos-storage"' | sfdisk -q "$DISK"
OFF=$((2048 * 512))
mkfs.fat -F 32 -n RUSTOS --offset 2048 "$DISK" >/dev/null
export MTOOLS_SKIP_CHECK=1
mmd -i "$DISK@@$OFF" ::/lib ::/lib/firmware ::/etc ::/log
mcopy -s -i "$DISK@@$OFF" "$ROOT/target/firmware-storage/." ::/lib/firmware/
printf 'linux.enable=%s\nlinux.debug=1\nlog.persist=1\n' "$DRIVER" > "$WORK/kernel.conf"
mcopy -i "$DISK@@$OFF" "$WORK/kernel.conf" ::/etc/kernel.conf

# --- run -------------------------------------------------------------------
DEVICES=(-device "vfio-pci,host=$GPU,multifunction=on")
[[ -n "$AUDIO" ]] && DEVICES+=(-device "vfio-pci,host=$AUDIO")
export RUSTOS_QEMU_ARGS="-enable-kvm -cpu host -smp 4 -m $MEM ${DEVICES[*]} -drive file=$DISK,format=raw,if=none,id=storage -device nvme,drive=storage,serial=rustos-storage ${QEMU_EXTRA[*]:-}"
echo "vfio-run: booting with linux.enable=$DRIVER; serial console below"
"$ROOT/run-qemu-uefi.sh" "$KERNEL" || true

mkdir -p "$ROOT/target/vfio-logs"
log="$ROOT/target/vfio-logs/kernel-$(date +%Y%m%d-%H%M%S).log"
mcopy -i "$DISK@@$OFF" ::/log/kernel.log "$log" 2>/dev/null && echo "vfio-run: kernel log saved to $log" ||
    echo "vfio-run: no persistent kernel log on the storage disk"
