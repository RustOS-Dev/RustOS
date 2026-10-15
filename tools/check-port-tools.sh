#!/bin/sh
# Check that the build host has what the desktop ports need
# (tools/release-ports.sh: libcxx, weston, labwc, edex-de), before a build
# that takes an hour or more stops partway. Each tool is run, not only
# looked up, so a broken installation (a tool that cannot load its own
# libraries) shows here too. Prints every problem; exits non-zero if any.
#
#   tools/check-port-tools.sh
fail=0
problem() {
    [ "$fail" = 1 ] || echo "check-port-tools: the build host cannot build the desktop ports:" >&2
    echo "  - $*" >&2
    fail=1
}
# runs LABEL CMD...: CMD must start and exit 0.
runs() {
    label="$1"
    shift
    if ! command -v "$1" >/dev/null 2>&1; then
        problem "$label: not found"
        return 1
    fi
    if ! out=$("$@" 2>&1); then
        problem "$label: does not run: $(printf '%s\n' "$out" | head -1)"
        return 1
    fi
}

runs "C compiler (${HOST_CC:-gcc})" "${HOST_CC:-gcc}" --version
runs "C++ compiler (${HOST_CXX:-g++})" "${HOST_CXX:-g++}" --version
for t in cmake ninja make patch git curl tar xz pkg-config gperf bison flex glslangValidator \
    glib-compile-resources glib-mkenums glib-genmarshal python3; do
    runs "$t" "$t" --version
done
# The ports use meson from PATH, or a pip --user install.
MESON="${MESON:-meson}"
command -v "$MESON" >/dev/null 2>&1 || MESON="$HOME/.local/bin/meson"
if runs meson "$MESON" --version; then
    v=$("$MESON" --version)
    [ "$(printf '%s\n%s\n' 1.4 "$v" | sort -V | head -1)" = 1.4 ] ||
        problem "meson: $v is too old (1.4 or newer; pip install --user meson)"
fi
for m in mako yaml ply jinja2 packaging; do
    python3 -c "import $m" 2>/dev/null || problem "Python module $m: missing"
done
# Mesa's OpenCL C compiler (mesa_clc) is built for the host against its LLVM.
# Meson finds llvm-config or a versioned one (Debian and Ubuntu install
# llvm-config-18 on PATH and llvm-config only with the llvm package).
LLVM_CONFIG=""
for c in llvm-config $(IFS=:; for d in $PATH; do ls "$d" 2>/dev/null; done |
    grep -x 'llvm-config-[0-9][0-9]*' | sort -t- -k3 -rn | uniq); do
    if command -v "$c" >/dev/null 2>&1; then
        LLVM_CONFIG="$c"
        break
    fi
done
if [ -n "$LLVM_CONFIG" ]; then
    runs "$LLVM_CONFIG" "$LLVM_CONFIG" --version
else
    problem "llvm-config: not found (LLVM development files)"
fi
for p in libclc LLVMSPIRVLib hwdata expat libffi glib-2.0; do
    pkg-config --exists "$p" 2>/dev/null || problem "$p development files: not found (pkg-config $p)"
done

[ "$fail" = 0 ] && exit 0
cat >&2 <<'EOF'
The desktop ports need these on the build host (docs/PORTING.md). Packages:
  Debian/Ubuntu: cmake ninja-build meson gperf bison flex glslang-tools hwdata
    python3-mako python3-yaml python3-ply python3-jinja2 python3-packaging
    libglib2.0-dev libglib2.0-dev-bin libexpat1-dev libffi-dev llvm-18-dev
    libclang-18-dev libclang-cpp18-dev libclc-18-dev libllvmspirvlib-18-dev
  Fedora: cmake ninja-build meson gperf bison flex glslang hwdata
    python3-mako python3-pyyaml python3-ply python3-jinja2 python3-packaging
    glib2-devel expat-devel libffi-devel llvm-devel clang-devel libclc-devel
    spirv-llvm-translator-devel
  Arch: cmake ninja meson gperf bison flex glslang hwdata python-mako
    python-yaml python-ply python-jinja python-packaging glib2-devel expat
    libffi llvm clang libclc spirv-llvm-translator
Or set RUSTOS_DESKTOP=0 for an image without the desktop.
EOF
exit 1
