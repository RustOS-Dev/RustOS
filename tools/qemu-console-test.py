#!/usr/bin/env python3
"""Boot a RustOS kernel ELF in QEMU and drive the serial console.

usage: qemu_session.py KERNEL_ELF SCRIPT_FILE [extra qemu args...]
SCRIPT_FILE lines: 'wait <regex> [timeout]', 'send <text>', 'sleep <s>'
"""
import atexit, os, re, subprocess, sys, time, select, tempfile

elf, script = os.path.abspath(sys.argv[1]), sys.argv[2]
extra = sys.argv[3:]
root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
img = tempfile.mktemp(suffix=".img")
# Extra disk images: @EXT2:<MiB>[:label]@, @EXT4:<MiB>[:label]@ (mke2fs,
# with a seed directory; ext4 also gets an htree-indexed directory "big"),
# @EXT4J:<MiB>[:label]@ (ext4 whose journal holds a committed transaction
# not yet written back: /hello.txt reads OLD-CONTENT until the journal is
# replayed, NEW-CONTENT after), @TREE:<MiB>:<dir>@ (ext4 holding the files of
# <dir>, relative to the repository; tools/fetch-<name>.sh creates it when
# missing), @EXT4F:<MiB>:<feature,...>@ (ext4 made with mke2fs -O
# <features> and seeded with small files, an inline-sized directory and a
# larger file; "casefold" adds -E encoding=utf8, "bigalloc" a 16 KiB
# cluster), @BLANK:<MiB>@ (zeros). Their paths are
# exported to host commands as $RUSTOS_DISK0, $RUSTOS_DISK1, ...
scratch = []
def _quiet(cmd, **kw):
    subprocess.run(cmd, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, **kw)
def _disk(m):
    kind, size = m.group(1), int(m.group(2))
    path = tempfile.mktemp(suffix=".disk")
    if kind == "EXT4F":
        feats = m.group(3)
        os.environ[f"RUSTOS_DISK{len(scratch)}"] = path
        scratch.append(path)
        with open(path, "wb") as f:
            f.truncate(size * 1024 * 1024)
        src = tempfile.mkdtemp()
        open(os.path.join(src, "small.txt"), "w").write("inline hello\n")
        open(os.path.join(src, "medium.txt"), "w").write("m" * 99 + "\n")
        os.mkdir(os.path.join(src, "idir"))
        for n in ("a", "b", "c"):
            open(os.path.join(src, "idir", n), "w").write(n + "\n")
        open(os.path.join(src, "big.bin"), "wb").write(bytes(range(256)) * 80)
        cmd = ["mke2fs", "-q", "-F", "-t", "ext4", "-L", "feat", "-O", feats, "-d", src]
        if "casefold" in feats:
            cmd += ["-E", "encoding=utf8"]
        if "bigalloc" in feats:
            cmd += ["-C", "16384"]
        subprocess.run(cmd + [path], check=True, stdout=subprocess.DEVNULL)
        if "casefold" in feats:
            # A casefolded directory "cf" with enough entries to be indexed.
            cmds = "mkdir cf\nset_inode_field cf flags 0x40080000\n"
            cmds += "".join(f"write /dev/null cf/Seed-File-{i:04d}\n" for i in range(300))
            _quiet(["debugfs", "-w", "-f", "-", path], input=cmds.encode())
            subprocess.run(["e2fsck", "-fyD", path], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        return path
    if kind == "TREE":
        src = os.path.join(root, m.group(3))
        fetch = os.path.join(root, "tools", "fetch-" + os.path.basename(src) + ".sh")
        if not os.path.isdir(src) and os.path.exists(fetch):
            subprocess.run([fetch], check=True, stdout=subprocess.DEVNULL)
        os.environ[f"RUSTOS_DISK{len(scratch)}"] = path
        scratch.append(path)
        with open(path, "wb") as f:
            f.truncate(size * 1024 * 1024)
        subprocess.run(["mke2fs", "-q", "-F", "-t", "ext4", "-d", src, path], check=True)
        return path
    os.environ[f"RUSTOS_DISK{len(scratch)}"] = path
    scratch.append(path)
    with open(path, "wb") as f:
        f.truncate(size * 1024 * 1024)
    if kind in ("EXT2", "EXT4", "EXT4J"):
        fs = "ext2" if kind == "EXT2" else "ext4"
        label = m.group(3) or "data"
        subprocess.run(["mke2fs", "-q", "-F", "-t", fs, "-L", label, path], check=True)
        # Seed a file so read paths are exercised before any write.
        _quiet(["debugfs", "-w", "-R", "mkdir seed", path])
    if kind == "EXT4":
        # A directory big enough to be indexed (e2fsck -D builds the htree).
        cmds = "mkdir big\n" + "".join(f"write /dev/null big/seed-file-{i:04d}\n" for i in range(300))
        _quiet(["debugfs", "-w", "-f", "-", path], input=cmds.encode())
        subprocess.run(["e2fsck", "-fyD", path], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    if kind == "EXT4J":
        d = tempfile.mkdtemp()
        old, new = os.path.join(d, "old"), os.path.join(d, "new")
        open(old, "w").write("OLD-CONTENT\n" * 100)
        open(new, "wb").write(("NEW-CONTENT\n" * 100).encode().ljust(4096, b"\0"))
        _quiet(["debugfs", "-w", "-R", f"write {old} hello.txt", path])
        blk = subprocess.run(["debugfs", "-R", "bmap hello.txt 0", path], capture_output=True,
                             text=True, check=True).stdout.strip()
        _quiet(["debugfs", "-w", "-f", "-", path], input=f"jo\njw -b {blk} {new}\njc\n".encode())
    return path
extra = [re.sub(r"@(EXT2|EXT4J|EXT4F|EXT4|BLANK|TREE):(\d+)(?::([\w/.,^=-]+))?@", _disk, a) for a in extra]
subprocess.run(["cargo", "run", "--quiet", "--", elf, img], cwd=f"{root}/crates/create-image", check=True,
               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
ovmf = next((c for c in ["/usr/share/OVMF/OVMF_CODE_4M.fd", "/usr/share/OVMF/OVMF_CODE.fd",
             "/usr/share/ovmf/OVMF.fd", "/usr/share/edk2/ovmf/OVMF_CODE.fd"] if os.path.exists(c)), None)
if ovmf is None:
    sys.exit("OVMF firmware not found")
cmd = ["qemu-system-x86_64", "-drive", f"if=pflash,format=raw,readonly=on,file={ovmf}",
       "-drive", f"format=raw,file={img}", "-machine", "q35", "-m", "512M", "-cpu", "max", "-smp", os.environ.get("RUSTOS_SMP", "2"),
       "-serial", "stdio", "-display", "none", "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"] + extra
mon_path = tempfile.mktemp(suffix=".mon")
cmd += ["-monitor", f"unix:{mon_path},server,nowait"]
p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
# Never leave QEMU (and its host port forwards) behind, even on a crash.
atexit.register(p.kill)
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
hostprocs = []
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
            # Ask the kernel for a state dump (serial BREAK = SysRq).
            try:
                import socket
                m = socket.socket(socket.AF_UNIX)
                m.connect(mon_path)
                m.sendall(b"chardev-send-break serial0\n")
                time.sleep(0.3)
                m.close()
                buf = b""
                read_until("(?!x)x", 5)  # never matches: collect 5 s of output
                dump = b"\n".join(l for l in buf.split(b"\n") if b"[sysrq]" in l or b"[nmi]" in l)
                print("*** kernel state:\n" + dump.decode(errors="replace"))
            except Exception as e:
                print(f"*** no state dump: {e}")
            ok = False
            break
        log += out
    elif op == "send":
        p.stdin.write(arg.encode().decode('unicode_escape').encode('latin-1') + b"\r")
        p.stdin.flush()
    elif op == "raw":
        p.stdin.write(arg.encode().decode('unicode_escape').encode('latin-1'))
        p.stdin.flush()
    elif op == "monitor":
        # HMP command, e.g. 'monitor sendkey a' or 'monitor device_del u1'.
        import socket
        arg = re.sub(r"@(EXT2|EXT4J|EXT4F|EXT4|BLANK|TREE):(\d+)(?::([\w/.,^=-]+))?@", _disk, arg)
        m = socket.socket(socket.AF_UNIX)
        m.connect(mon_path)
        m.settimeout(2)
        time.sleep(0.2)
        try:
            m.recv(65536)
        except OSError:
            pass
        m.sendall(arg.encode() + b"\n")
        time.sleep(0.5)
        try:
            reply = m.recv(65536).decode(errors="replace")
        except OSError:
            reply = ""
        reply = re.sub(r"\x1b\[[0-9;]*[A-Za-z]", "", reply).split("\n", 1)[-1].replace("(qemu)", "").strip()
        log += f"[monitor] {arg}{': ' + reply if reply else ''}\n".encode()
        m.close()
    elif op == "hostbg":
        # Start a helper process on the host (killed when the test ends).
        hostprocs.append(subprocess.Popen(arg, shell=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        time.sleep(0.5)
    elif op == "hostrun":
        # Run a host command; its output goes to the log and must succeed.
        try:
            r = subprocess.run(arg, shell=True, capture_output=True, timeout=60)
            code, out = r.returncode, r.stdout + r.stderr
        except subprocess.TimeoutExpired as e:
            code, out = "timeout", (e.stdout or b"") + (e.stderr or b"")
        log += f"[host] {arg}\n".encode() + out
        if code != 0:
            sys.stdout.buffer.write(log + buf)
            print(f"\n*** host command failed: {arg!r} (exit {code})")
            ok = False
            break
    elif op == "sleep":
        time.sleep(float(arg))
if ok:
    time.sleep(0.5)
    r, _, _ = select.select([p.stdout], [], [], 0.5)
    if r:
        buf += os.read(p.stdout.fileno(), 65536)
    sys.stdout.buffer.write(log + buf)
p.kill()
for hp in hostprocs:
    hp.kill()
if os.environ.get("RUSTOS_KEEP_DISKS"):
    print("*** kept image", img)
else:
    os.unlink(img)
if os.path.exists(mon_path):
    os.unlink(mon_path)
for f in scratch:
    if os.environ.get("RUSTOS_KEEP_DISKS"):
        print("*** kept disk", f)
    else:
        os.unlink(f)
print("\n*** RESULT:", "PASS" if ok else "FAIL")
sys.exit(0 if ok else 1)
