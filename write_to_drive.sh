#!/usr/bin/env bash
# write_to_drive.sh — Build RustOS locally and flash it to a drive.
#
# Usage:
#   ./write_to_drive.sh --drive /dev/sdX
#
# Requirements:
#   - rustup (the nightly pinned in rust-toolchain.toml is installed and used
#     even when another cargo comes first in PATH)
#   - dd (coreutils), lsblk, sfdisk, sgdisk, mkfs.fat
#   - Root privileges (or write access to the target drive)

set -euo pipefail

DRIVE=""
ASSUME_YES=0
AX210_FIRMWARE_SOURCE="${RUSTOS_AX210_FIRMWARE:-}"
PARTITION_SYNC_DELAY_SECONDS=1
AX210_FIRMWARE_SEARCH_DIRS=(
    /lib/firmware
    /usr/lib/firmware
    /lib/firmware/updates
    /usr/lib/linux-firmware
)

# ---------------------------------------------------------------------------
# Helper functions
# ---------------------------------------------------------------------------
reload_partition_table() {
    local device="$1"
    if command -v sudo &>/dev/null && [[ "$(id -u)" -ne 0 ]]; then
        sudo blockdev --rereadpt "$device" || true
        sudo partx -u "$device" 2>/dev/null || true
        if command -v partprobe &>/dev/null; then sudo partprobe "$device" || true; fi
    else
        blockdev --rereadpt "$device" || true
        partx -u "$device" 2>/dev/null || true
        if command -v partprobe &>/dev/null; then partprobe "$device" || true; fi
    fi
}

# Under sudo, build as the invoking user: their rustup and toolchains live in
# their home, and root-owned files in their checkout would break later builds.
# Only partitioning, formatting and writing the drive need root.
BUILD_USER=""
BUILD_HOME="$HOME"
if [[ "$(id -u)" -eq 0 && -n "${SUDO_USER:-}" && "$SUDO_USER" != root ]]; then
    BUILD_USER="$SUDO_USER"
    BUILD_HOME="$(getent passwd "$SUDO_USER" | cut -d: -f6)"
fi

as_build_user() {
    if [[ -n "$BUILD_USER" ]]; then
        sudo -u "$BUILD_USER" -H env "PATH=$PATH" ${RUSTUP_TOOLCHAIN:+"RUSTUP_TOOLCHAIN=$RUSTUP_TOOLCHAIN"} "$@"
    else
        "$@"
    fi
}

# Directory holding the RustOS sources: the checkout this script lives in, or,
# when the script is piped in (curl ... | bash), a clone kept in
# $RUSTOS_SRC_DIR (default ~/.cache/rustos-src of the building user) and
# updated on every run.
resolve_source_dir() {
    local self="${BASH_SOURCE[0]:-}"
    if [[ -n "$self" && -f "$self" ]]; then
        local dir
        dir="$(cd "$(dirname "$(realpath "$self")")" && pwd)"
        if [[ -f "$dir/Cargo.toml" && -d "$dir/crates/create-image" ]]; then
            echo "$dir"
            return
        fi
    fi

    local repo="${RUSTOS_REPO:-https://github.com/RustOS-Dev/RustOS}"
    local branch="${RUSTOS_BRANCH:-main}"
    local cache="${XDG_CACHE_HOME:-$HOME/.cache}"
    [[ -n "$BUILD_USER" ]] && cache="$BUILD_HOME/.cache"
    local src="${RUSTOS_SRC_DIR:-$cache/rustos-src}"
    if ! command -v git &>/dev/null; then
        echo "Error: required tool 'git' is not installed." >&2
        exit 1
    fi
    if [[ -d "$src/.git" ]]; then
        echo "Updating RustOS sources in $src ($branch)..." >&2
        as_build_user git -C "$src" fetch --quiet origin "$branch" >&2
        as_build_user git -C "$src" checkout --quiet --force -B "$branch" FETCH_HEAD >&2
    else
        echo "Cloning RustOS sources into $src ($branch)..." >&2
        as_build_user mkdir -p "$(dirname "$src")"
        as_build_user git clone --quiet --branch "$branch" "$repo" "$src" >&2
    fi
    echo "$src"
}

# Run cargo from the nightly pinned in rust-toolchain.toml. A cargo that is
# not that toolchain (a distro package, or rustup's stable) ignores the
# [unstable] table in .cargo/config.toml and cannot build the kernel.
pinned_cargo() {
    local src="$1" toolchain
    toolchain="$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$src/rust-toolchain.toml")"
    if [[ -z "$toolchain" ]]; then
        echo "Error: no toolchain channel in $src/rust-toolchain.toml." >&2
        exit 1
    fi
    local rustup
    rustup="$(command -v rustup || true)"
    if [[ -z "$rustup" && -x "$BUILD_HOME/.cargo/bin/rustup" ]]; then
        # Not in PATH, e.g. under sudo: use the building user's rustup.
        rustup="$BUILD_HOME/.cargo/bin/rustup"
    fi
    if [[ -z "$rustup" ]]; then
        echo "Error: rustup is required to build RustOS (it uses $toolchain)." >&2
        if command -v cargo &>/dev/null; then
            echo "The cargo in PATH ($(command -v cargo), $(cargo -V 2>/dev/null)) cannot build it." >&2
        fi
        echo "Install rustup from https://rustup.rs and re-run this script." >&2
        exit 1
    fi
    if ! as_build_user "$rustup" run "$toolchain" cargo -V &>/dev/null; then
        echo "Installing Rust toolchain $toolchain (from rust-toolchain.toml)..." >&2
        (cd "$src" && as_build_user "$rustup" toolchain install) >&2 ||
            as_build_user "$rustup" toolchain install "$toolchain" --profile minimal \
                --component rust-src --component llvm-tools \
                --target x86_64-unknown-none >&2
    fi
    # Put the toolchain's own binaries first so that cargo, the rustc it
    # runs and the nested cargo builds in build.rs all come from it.
    local bin
    bin="$(dirname "$(as_build_user "$rustup" which --toolchain "$toolchain" cargo)")"
    export PATH="$bin:$PATH" RUSTUP_TOOLCHAIN="$toolchain"
    CARGO_CMD=("$bin/cargo")
}

# Last chance to back out before the drive is overwritten. Reads the answer
# from the terminal, since stdin is the script itself under curl | bash.
confirm_erase() {
    local device="$1"
    [[ "$ASSUME_YES" == 1 ]] && return
    echo
    lsblk -o NAME,SIZE,MODEL,TRAN,MOUNTPOINTS "$device" 2>/dev/null ||
        lsblk -o NAME,SIZE,MODEL,TRAN,MOUNTPOINT "$device" || true
    echo
    if [[ ! -r /dev/tty ]]; then
        echo "Error: no terminal to confirm on; pass --yes to skip the prompt." >&2
        exit 1
    fi
    local answer
    printf "Type 'yes' to erase everything on %s: " "$device" > /dev/tty
    read -r answer < /dev/tty
    if [[ "$answer" != "yes" ]]; then
        echo "Aborted; nothing was written." >&2
        exit 1
    fi
}

run_as_root() {
    if command -v sudo &>/dev/null && [[ "$(id -u)" -ne 0 ]]; then
        sudo "$@"
    else
        "$@"
    fi
}

populate_rootfs_skeleton() {
    local mount_point="$1"
    run_as_root mkdir -p \
        "$mount_point/bin" \
        "$mount_point/etc" \
        "$mount_point/home" \
        "$mount_point/lib" \
        "$mount_point/lib/firmware" \
        "$mount_point/mnt" \
        "$mount_point/mnt/c" \
        "$mount_point/mnt/d" \
        "$mount_point/proc" \
        "$mount_point/sys" \
        "$mount_point/tmp" \
        "$mount_point/usr" \
        "$mount_point/usr/bin" \
        "$mount_point/var" \
        "$mount_point/var/log"
}

# Firmware files the iwlwifi driver looks for (newest supported API first),
# plus the platform NVM. AX211/AX201 (CNVi) parts use the "so" images.
AX210_FIRMWARE_PATTERNS=(
    "iwlwifi-ty-a0-gf-a0-7[12].ucode"
    "iwlwifi-ty-a0-gf-a0-6[0-9].ucode"
    "iwlwifi-ty-a0-gf-a0-59.ucode"
    "iwlwifi-ty-a0-gf-a0.pnvm"
    "iwlwifi-so-a0-gf-a0-7[12].ucode"
    "iwlwifi-so-a0-gf-a0-6[0-9].ucode"
    "iwlwifi-so-a0-gf-a0.pnvm"
    "iwlwifi-so-a0-hr-b0-7[12].ucode"
    "iwlwifi-so-a0-hr-b0-6[0-9].ucode"
)

# The Bluetooth half of the same cards (btusb loads intel/ibt-*.sfi/.ddc).
AX210_BT_FIRMWARE_PATTERNS=(
    "intel/ibt-0041-0041.sfi"
    "intel/ibt-0041-0041.ddc"
    "intel/ibt-0040-0041.sfi"
    "intel/ibt-0040-0041.ddc"
    "intel/ibt-0040-0040.sfi"
    "intel/ibt-0040-0040.ddc"
)

# Print "<file>" for every matching firmware blob in directory $1
# (plain, .xz or .zst - distributions ship compressed firmware).
find_ax210_firmware_files() {
    local dir="$1" pattern f
    shopt -s nullglob
    for pattern in "${AX210_FIRMWARE_PATTERNS[@]}"; do
        for f in "$dir"/$pattern "$dir"/$pattern.xz "$dir"/$pattern.zst; do
            [[ -f "$f" ]] && printf '%s\n' "$f"
        done
    done
    shopt -u nullglob
}

# Copy the Bluetooth firmware found under $1 (a linux-firmware tree) into
# $2/intel.
install_bt_firmware() {
    local dir="$1" fwdir="$2" pattern f
    shopt -s nullglob
    for pattern in "${AX210_BT_FIRMWARE_PATTERNS[@]}"; do
        for f in "$dir"/$pattern "$dir"/$pattern.xz "$dir"/$pattern.zst; do
            if [[ -f "$f" ]]; then
                run_as_root mkdir -p "$fwdir/intel"
                install_firmware_file "$f" "$fwdir/intel"
            fi
        done
    done
    shopt -u nullglob
}

auto_detect_ax210_firmware_source() {
    local candidate
    for candidate in "${AX210_FIRMWARE_SEARCH_DIRS[@]}"; do
        [[ -d "$candidate" ]] || continue
        if [[ -n "$(find_ax210_firmware_files "$candidate")" ]]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    return 1
}

# Copy one firmware blob into $2, decompressing .xz/.zst.
install_firmware_file() {
    local src="$1" dir="$2" name
    name="${src##*/}"
    case "$src" in
        *.xz)
            name="${name%.xz}"
            xz -dc "$src" | run_as_root tee "$dir/$name" >/dev/null
            ;;
        *.zst)
            name="${name%.zst}"
            zstd -qdc "$src" | run_as_root tee "$dir/$name" >/dev/null
            ;;
        *)
            run_as_root cp "$src" "$dir/$name"
            ;;
    esac
    echo "Provisioned firmware: $dir/$name"
}

provision_ax210_firmware() {
    local mount_point="$1"
    local source="$2"
    local firmware_dir="$mount_point/lib/firmware"
    local f copied=0

    run_as_root mkdir -p "$firmware_dir"

    if [[ -z "$source" ]]; then
        if source="$(auto_detect_ax210_firmware_source)"; then
            echo "Auto-detected Intel WiFi firmware source: $source"
        else
            echo "Intel WiFi firmware not provided and no host copy was auto-detected."
            echo "Copy iwlwifi-ty-a0-gf-a0-72.ucode and iwlwifi-ty-a0-gf-a0.pnvm (from linux-firmware)"
            echo "into /lib/firmware on the RUSTOS_ROOT partition, or re-run with"
            echo "--ax210-firmware <file-or-dir> / RUSTOS_AX210_FIRMWARE."
            return 0
        fi
    fi

    if [[ -d "$source" ]]; then
        install_bt_firmware "$source" "$firmware_dir"
        while IFS= read -r f; do
            [[ -n "$f" ]] || continue
            install_firmware_file "$f" "$firmware_dir"
            copied=1
        done < <(find_ax210_firmware_files "$source")
        if [[ "$copied" -eq 1 ]]; then
            return 0
        fi
        echo "Error: no Intel WiFi firmware blobs were found in '$source'." >&2
        echo "Expected e.g. iwlwifi-ty-a0-gf-a0-72.ucode and iwlwifi-ty-a0-gf-a0.pnvm." >&2
        return 1
    fi

    if [[ -f "$source" ]]; then
        case "${source##*/}" in
            iwlwifi-*.ucode|iwlwifi-*.pnvm|iwlwifi-*.xz|iwlwifi-*.zst)
                install_firmware_file "$source" "$firmware_dir"
                # Pick up the matching .pnvm next to a single .ucode.
                local dir="${source%/*}" base
                base="${source##*/}"
                base="${base%%-[0-9]*}"
                for f in "$dir/$base.pnvm" "$dir/$base.pnvm.xz" "$dir/$base.pnvm.zst"; do
                    [[ -f "$f" ]] && install_firmware_file "$f" "$firmware_dir"
                done
                return 0
                ;;
            *)
                echo "Error: '$source' is not an iwlwifi firmware file." >&2
                return 1
                ;;
        esac
    fi

    echo "Error: AX210 firmware source '$source' does not exist." >&2
    return 1
}

# The functions above can be sourced by tests (tools/test-firmware-install.sh)
# without flashing anything.
main() {
    # ---------------------------------------------------------------------------
    # Argument parsing
    # ---------------------------------------------------------------------------
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --drive)
                if [[ -z "${2:-}" ]]; then
                    echo "Error: --drive requires a path argument" >&2
                    exit 1
                fi
                DRIVE="$2"
                shift 2
                ;;
            --ax210-firmware)
                if [[ -z "${2:-}" ]]; then
                    echo "Error: --ax210-firmware requires a path argument" >&2
                    exit 1
                fi
                AX210_FIRMWARE_SOURCE="$2"
                shift 2
                ;;
            -y|--yes)
                ASSUME_YES=1
                shift
                ;;
            -h|--help)
                echo "Usage: $0 --drive /dev/sdX [--ax210-firmware <file-or-dir>] [--yes]"
                echo
                echo "Builds a local RustOS UEFI disk image and writes it to the"
                echo "specified drive.  The drive will be COMPLETELY OVERWRITTEN."
                echo "If AX210 firmware is auto-detected or explicitly provided,"
                echo "it is copied to /lib/firmware/ on the RustOS FAT32 root filesystem."
                echo
                echo "Example:"
                echo "  $0 --drive /dev/sdb"
                echo "  $0 --drive /dev/sdb --ax210-firmware /path/to/linux-firmware"
                echo
                echo "--yes skips the confirmation prompt before the drive is erased."
                echo "Piped from curl, the script clones RustOS into \$RUSTOS_SRC_DIR"
                echo "(default ~/.cache/rustos-src) from \$RUSTOS_REPO and \$RUSTOS_BRANCH."
                exit 0
                ;;
            *)
                echo "Unknown argument: $1" >&2
                echo "Usage: $0 --drive /dev/sdX [--ax210-firmware <file-or-dir>] [--yes]" >&2
                exit 1
                ;;
        esac
    done

    if [[ -z "$DRIVE" ]]; then
        echo "Error: --drive is required." >&2
        echo "Usage: $0 --drive /dev/sdX [--ax210-firmware <file-or-dir>] [--yes]" >&2
        exit 1
    fi

    # ---------------------------------------------------------------------------
    # Sanity checks
    # ---------------------------------------------------------------------------
    if [[ ! -e "$DRIVE" ]]; then
        echo "Error: device '$DRIVE' does not exist." >&2
        exit 1
    fi

    # Ensure the target is a block device
    if [[ ! -b "$DRIVE" ]]; then
        echo "Error: '$DRIVE' is not a block device." >&2
        exit 1
    fi

    # Refuse to write to a device that has a mounted partition used as / or /boot
    if grep -qs "^${DRIVE}" /proc/mounts; then
        MOUNTS=$(grep "^${DRIVE}" /proc/mounts | awk '{print $2}' | tr '\n' ' ')
        echo "Error: '$DRIVE' (or one of its partitions) is currently mounted at: $MOUNTS" >&2
        echo "Unmount it first before flashing." >&2
        exit 1
    fi

    # Warn if a partition was given instead of the whole disk
    if [[ "$DRIVE" =~ [0-9]$ ]]; then
        echo "Warning: '$DRIVE' looks like a partition. For a bootable image you" >&2
        echo "         usually want the whole disk (e.g. /dev/sdb, not /dev/sdb1)." >&2
    fi

    # ---------------------------------------------------------------------------
    # Tool checks
    # ---------------------------------------------------------------------------
    for tool in lsblk sfdisk mkfs.fat find; do
        if ! command -v "$tool" &>/dev/null; then
            echo "Error: required tool '$tool' is not installed." >&2
            exit 1
        fi
    done

    # ---------------------------------------------------------------------------
    # Build local image
    # ---------------------------------------------------------------------------
    SCRIPT_DIR="$(resolve_source_dir)"
    cd "$SCRIPT_DIR"
    pinned_cargo "$SCRIPT_DIR"

    echo "Updating submodules to pinned repository commits..."
    as_build_user git submodule update --init --recursive

    echo "Building kernel (release)..."
    as_build_user "${CARGO_CMD[@]}" build --release

    KERNEL_ELF=$(find "$SCRIPT_DIR/target" -path "*/release/rustos" -not -name "*.d" | head -1)
    if [[ -z "$KERNEL_ELF" || ! -f "$KERNEL_ELF" ]]; then
        echo "Error: failed to locate release kernel ELF after build." >&2
        exit 1
    fi

    IMG_FILE="$SCRIPT_DIR/rustos-local.img"
    echo "Creating local UEFI disk image: $IMG_FILE"
    (cd "$SCRIPT_DIR/crates/create-image" && as_build_user "${CARGO_CMD[@]}" run --release -- "$KERNEL_ELF" "$IMG_FILE")

    # ---------------------------------------------------------------------------
    # Flash
    # ---------------------------------------------------------------------------
    echo
    echo "Target drive: $DRIVE"
    echo
    echo "WARNING: ALL DATA ON '$DRIVE' WILL BE PERMANENTLY DESTROYED."
    confirm_erase "$DRIVE"
    echo

    echo "Writing image to $DRIVE ..."
    if command -v sudo &>/dev/null && [[ "$(id -u)" -ne 0 ]]; then
        sudo dd if="$IMG_FILE" of="$DRIVE" bs=4M status=progress conv=fsync
        sudo sync
    else
        dd if="$IMG_FILE" of="$DRIVE" bs=4M status=progress conv=fsync
        sync
    fi

    echo
    echo "Done! '$DRIVE' is ready to boot local RustOS build in UEFI mode."
    echo
    echo "Creating storage partition from remaining free space..."

    reload_partition_table "$DRIVE"

    sleep 1

    PTTYPE=$(lsblk -dn -o PTTYPE "$DRIVE" | tr -d '[:space:]')
    if [[ -z "$PTTYPE" ]]; then
        # lsblk gets this from udev; read the disk itself when udev has not caught up.
        PTTYPE=$(run_as_root blkid -p -o value -s PTTYPE "$DRIVE" 2>/dev/null | tr -d '[:space:]' || true)
    fi
    if [[ -z "$PTTYPE" ]]; then
        echo "Error: could not detect partition table type on '$DRIVE' after flashing." >&2
        exit 1
    fi

    if [[ "$PTTYPE" == "gpt" ]]; then
        if ! command -v sgdisk &>/dev/null; then
            echo "Error: detected GPT disk image, but required tool 'sgdisk' is not installed." >&2
            echo "Install it (usually package: gdisk) and re-run." >&2
            exit 1
        fi

        echo "Repairing GPT metadata to use full target drive size..."
        run_as_root sgdisk -e "$DRIVE"
        reload_partition_table "$DRIVE"

        # Give the kernel a brief moment to expose the updated GPT layout.
        sleep "$PARTITION_SYNC_DELAY_SECONDS"

        if run_as_root sgdisk -i 2 "$DRIVE" >/dev/null 2>&1; then
            STORAGE_START_SECTOR=$(
                run_as_root sgdisk -i 2 "$DRIVE" |
                    awk -F: '/First sector:/ {gsub(/^[[:space:]]+/, "", $2); split($2, a, " "); print a[1]}'
            )
            if [[ -z "$STORAGE_START_SECTOR" ]]; then
                echo "Error: failed to determine the start sector for existing GPT partition 2." >&2
                exit 1
            fi

            echo "Resizing existing storage partition 2 to fill remaining space..."
            run_as_root sgdisk -d 2 "$DRIVE"
            run_as_root sgdisk -n 2:${STORAGE_START_SECTOR}:0 -t 2:0700 -c 2:"rustos-storage" "$DRIVE"
        else
            echo "Adding storage partition using sgdisk..."
            run_as_root sgdisk -n 2:0:0 -t 2:0700 -c 2:"rustos-storage" "$DRIVE"
        fi
        reload_partition_table "$DRIVE"
    else
        # For MBR/DOS partition tables, use sfdisk
        PART_SPEC='type=c'
        printf '%s\n' "$PART_SPEC" | run_as_root sfdisk --append "$DRIVE"
        reload_partition_table "$DRIVE"
    fi

    # Give the kernel time to create the partition device node
    sleep "$PARTITION_SYNC_DELAY_SECONDS"

    if [[ "$DRIVE" =~ [0-9]$ ]]; then
        STORAGE_PART="${DRIVE}p2"
    else
        STORAGE_PART="${DRIVE}2"
    fi

    for _ in $(seq 1 20); do
        if [[ -b "$STORAGE_PART" ]]; then
            break
        fi
        sleep 0.2
    done

    if [[ ! -b "$STORAGE_PART" ]]; then
        echo "Error: storage partition device '$STORAGE_PART' was not created." >&2
        exit 1
    fi

    echo "Formatting ${STORAGE_PART} as FAT32 (label: RUSTOS_ROOT)..."
    run_as_root mkfs.fat -F 32 -n RUSTOS_ROOT "$STORAGE_PART"

    # Populate the FAT32 root filesystem with a standard directory skeleton so
    # that the kernel has a proper persistent root from first boot.
    echo "Populating FAT32 storage partition with initial directory skeleton..."
    MOUNT_TMP=$(mktemp -d)
    run_as_root mount -t vfat "$STORAGE_PART" "$MOUNT_TMP"
    populate_rootfs_skeleton "$MOUNT_TMP"
    provision_ax210_firmware "$MOUNT_TMP" "$AX210_FIRMWARE_SOURCE"
    run_as_root umount "$MOUNT_TMP"
    rmdir "$MOUNT_TMP"

    echo
    echo "Done! '$DRIVE' is ready:"
    echo "  - Partition 1: RustOS boot partition"
    echo "  - Partition 2: FAT32 storage/root filesystem"
    echo "Remove the drive safely, then boot your system in UEFI mode."

    # Clean up the local image
    rm -f "$IMG_FILE"
}

# Run unless sourced; BASH_SOURCE is empty when the script comes from stdin.
if [[ "${BASH_SOURCE[0]:-$0}" == "$0" ]]; then
    main "$@"
fi
