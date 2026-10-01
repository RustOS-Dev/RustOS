#!/usr/bin/env bash
# Boot the kernel in QEMU for every scenario in tests/scenarios/ and drive
# the serial console. Usage: tools/run-scenarios.sh [kernel-elf] [scenario...]
# A scenario's QEMU arguments come from the .args file next to it.
# tests/scenarios/linux/ holds scenarios for kernels built with linux-*
# features; they run only when named.
set -e
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
KERNEL="${1:-$ROOT/target/x86_64-rustos/debug/rustos}"
shift || true
SCENARIOS=("$@")
if [ ${#SCENARIOS[@]} -eq 0 ]; then
    SCENARIOS=("$ROOT"/tests/scenarios/*.txt)
fi
fail=0
for s in "${SCENARIOS[@]}"; do
    name="$(basename "$s" .txt)"
    args_file="$(dirname "$s")/$name.args"
    extra=()
    if [ -f "$args_file" ]; then
        # shellcheck disable=SC2207
        extra=($(grep -v '^#' "$args_file"))
    fi
    if python3 "$ROOT/tools/qemu-console-test.py" "$KERNEL" "$s" "${extra[@]}" > "/tmp/scenario-$name.log" 2>&1; then
        echo "PASS $name"
    else
        echo "FAIL $name (log: /tmp/scenario-$name.log)"
        tail -30 "/tmp/scenario-$name.log"
        fail=1
    fi
done
exit $fail
