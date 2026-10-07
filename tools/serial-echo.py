#!/usr/bin/env python3
"""The far end of a serial line for the `usb-serial` QEMU scenario: connects
to QEMU's chardev (a Unix socket behind -device usb-serial) and sends back
every byte it receives, upper-cased, so a test sees its data cross the
line in both directions. It also logs what arrived.

Usage: serial-echo.py SOCKET LOG
"""

import socket
import sys
import time

path, log = sys.argv[1], sys.argv[2]
s = socket.socket(socket.AF_UNIX)
for _ in range(100):
    try:
        s.connect(path)
        break
    except OSError:
        time.sleep(0.1)
with open(log, "ab", buffering=0) as f:
    while True:
        data = s.recv(4096)
        if not data:
            break
        f.write(data)
        s.sendall(data.upper())
