#!/usr/bin/env python3
"""Boot a RustOS kernel ELF in QEMU and drive the serial console.

usage: qemu_session.py KERNEL_ELF SCRIPT_FILE [extra qemu args...]
SCRIPT_FILE lines: 'wait <regex> [timeout]', 'send <text>', 'sleep <s>'
"""
import os, re, subprocess, sys, time, select, tempfile

elf, script = os.path.abspath(sys.argv[1]), sys.argv[2]
extra = sys.argv[3:]
root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
img = tempfile.mktemp(suffix=".img")
# Extra disk images: @EXT2:<MiB>[:label]@ (mke2fs), @BLANK:<MiB>@ (zeros).
scratch = []
def _disk(m):
    kind, size = m.group(1), int(m.group(2))
    path = tempfile.mktemp(suffix=".disk")
    scratch.append(path)
    with open(path, "wb") as f:
        f.truncate(size * 1024 * 1024)
    if kind in ("EXT2", "EXT4"):
        fs = "ext2" if kind == "EXT2" else "ext4"
        label = m.group(3) or "data"
        subprocess.run(["mke2fs", "-q", "-F", "-t", fs, "-L", label, path], check=True)
        # Seed a file so read paths are exercised before any write.
        subprocess.run(["debugfs", "-w", "-R", "mkdir seed", path], check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    return path
extra = [re.sub(r"@(EXT2|EXT4|BLANK):(\d+)(?::(\w+))?@", _disk, a) for a in extra]
subprocess.run(["cargo", "run", "--quiet", "--", elf, img], cwd=f"{root}/crates/create-image", check=True,
               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
ovmf = next((c for c in ["/usr/share/OVMF/OVMF_CODE_4M.fd", "/usr/share/OVMF/OVMF_CODE.fd",
             "/usr/share/ovmf/OVMF.fd", "/usr/share/edk2/ovmf/OVMF_CODE.fd"] if os.path.exists(c)), None)
if ovmf is None:
    sys.exit("OVMF firmware not found")
cmd = ["qemu-system-x86_64", "-drive", f"if=pflash,format=raw,readonly=on,file={ovmf}",
       "-drive", f"format=raw,file={img}", "-machine", "q35", "-m", "512M", "-cpu", "max", "-smp", os.environ.get("RUSTOS_SMP", "1"),
       "-serial", "stdio", "-display", "none", "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"] + extra
p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
buf = b""
def read_until(rx, timeout):
    global buf
    end = time.time() + timeout
    pat = re.compile(rx.encode())
    while time.time() < end:
        m = pat.search(buf)
        if m:
            out = buf[:m.end()]
            buf = buf[m.end():]
            return out
        r, _, _ = select.select([p.stdout], [], [], 0.2)
        if r:
            chunk = os.read(p.stdout.fileno(), 65536)
            if not chunk:
                break
            buf += chunk
    return None
ok = True
log = b""
for line in open(script):
    line = line.rstrip("\n")
    if not line or line.startswith("#"):
        continue
    op, _, arg = line.partition(" ")
    if op == "wait":
        parts = arg.rsplit(" ", 1)
        rx, to = (parts[0], float(parts[1])) if len(parts) == 2 and parts[1].replace('.','',1).isdigit() else (arg, 60)
        out = read_until(rx, to)
        if out is None:
            sys.stdout.buffer.write(log + buf)
            print(f"\n*** TIMEOUT waiting for {rx!r}")
            ok = False
            break
        log += out
    elif op == "send":
        p.stdin.write(arg.encode().decode('unicode_escape').encode('latin-1') + b"\r")
        p.stdin.flush()
    elif op == "raw":
        p.stdin.write(arg.encode().decode('unicode_escape').encode('latin-1'))
        p.stdin.flush()
    elif op == "sleep":
        time.sleep(float(arg))
if ok:
    time.sleep(0.5)
    r, _, _ = select.select([p.stdout], [], [], 0.5)
    if r:
        buf += os.read(p.stdout.fileno(), 65536)
    sys.stdout.buffer.write(log + buf)
p.kill()
if os.environ.get("RUSTOS_KEEP_DISKS"):
    print("*** kept image", img)
else:
    os.unlink(img)
for f in scratch:
    if os.environ.get("RUSTOS_KEEP_DISKS"):
        print("*** kept disk", f)
    else:
        os.unlink(f)
print("\n*** RESULT:", "PASS" if ok else "FAIL")
sys.exit(0 if ok else 1)
