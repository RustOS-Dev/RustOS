# Shell and Commands

`/bin/sh` (also `rsh`) is the login shell; commands are ordinary programs
in `/bin` — most are applets of two multi-call binaries, `rbox` (core
utilities) and `nettools` (networking). `help` in the shell lists the
built-ins; most commands print a usage line on bad arguments.

## Shell language

* Pipelines `a | b`, lists `a; b`, `a && b`, `a || b`, background `a &`,
  subshells `( ... )`, groups `{ ...; }`, negation `! a`.
* Redirections `<`, `>`, `>>`, `2>`, `2>&1`, `&>`, `<>`, here-documents
  `<<EOF` / `<<-EOF`.
* Variables and parameters: `$VAR`, `${VAR:-default}`, `${VAR:=x}`,
  `${#VAR}`, `$?`, `$$`, `$!`, `$#`, `$@`, `$*`, `$0`–`$9`, `export`,
  `local`, `readonly`, `unset`.
* Command substitution `$(...)` and `` `...` ``; arithmetic `$((...))`
  (integers); globbing `*`, `?`, `[...]`; tilde expansion; quoting.
* Control flow: `if`/`elif`/`else`, `while`, `until`, `for`, `case`,
  functions `name() { ...; }`, `break`, `continue`, `return`.
* Job control: `jobs`, `fg`, `bg`, `wait`, `kill %N`; Ctrl-C (SIGINT) and
  Ctrl-Z (SIGTSTP) go to the foreground job.
* Interactive editing: history (↑/↓, `history`), cursor keys, Tab
  completion of commands and paths.
* Start-up: `/etc/profile`, then `~/.profile`; `source`/`.` runs a file.

Built-ins: `:` `.` `[` `alias` `bg` `break` `cd` (`cd -`) `command`
`continue` `echo` `eval` `exec` `exit` `export` `false` `fg` `help`
`history` `jobs` `kill` `local` `printf` `pwd` `read` `readonly` `return`
`set` `shift` `source` `test` `times` `trap` `true` `type` `umask`
`unalias` `unset` `wait`.

## Files and directories

| Command | Synopsis |
|---------|----------|
| `ls` | `ls [-laAhRdFSrti1] [PATH...]` — long listing with modes, owners, sizes, dates, `total` |
| `cp` | `cp [-rpifvn] SOURCE... DEST` |
| `mv` | `mv [-ifnv] SOURCE... DEST` |
| `rm` | `rm [-rfiIv] FILE...` |
| `mkdir` / `rmdir` | `mkdir [-p] [-m MODE] DIR...` |
| `ln` | `ln [-sf] TARGET LINK` |
| `touch`, `stat`, `readlink`, `realpath`, `basename`, `dirname` | |
| `chmod` | `chmod [-R] MODE FILE...` (octal or symbolic) |
| `find` | `find [PATH] [-name|-iname GLOB] [-type f|d|l] [-maxdepth N] [-exec CMD {} \;] [-print]` |
| `du`, `df` | disk usage |
| `dd` | `dd if= of= bs= count= skip= seek=` |
| `truncate`, `fallocate` | `truncate -s SIZE FILE...`, `fallocate [-n] [-o OFF] -l LEN FILE` (sizes take K/M/G) |
| `mkfifo` | named pipes |
| `md5sum`, `sha256sum`, `cmp`, `diff [-u]` | checksums and comparison |
| `hexdump` / `xxd` | hex dump |

## Text

`cat`, `head`, `tail` (`-n`, `-f`), `wc`, `grep`
(`[-iEFvnclqrwoH] [-e PAT] [-A N]`), `sed` (`s///`, `d`, `p`, addresses,
`-n`, `-i`, `-e`, `-r`), `sort` (`-rnufh`, `-k`, `-t`), `uniq` (`-c -d -u -i`),
`cut` (`-d -f -c`),
`tr`, `tee`, `nl`, `rev`, `tac`, `more`, `seq`, `yes`, `echo`, `printf`,
`xargs`, `test`/`[`.

## Processes and system

`ps` (`aux`), `top`, `kill` (`-SIGNAL`), `sleep`, `time`, `watch`,
`uptime`, `free`/`meminfo`, `uname` (`-a`), `date` (`+FORMAT`), `hostname`,
`id`, `whoami`, `env`, `which`, `dmesg`, `clear`, `sync`,
`reboot`, `poweroff`/`shutdown`, `kapitest` (kernel API self-test).

## Terminals and users

Four virtual consoles run a shell each (`/etc/ttys`); switch with
Alt-F1..F4 or `chvt N`; `fgconsole` prints the visible one, `tty` the
terminal of standard input. Programs can create pseudo-terminals
(`/dev/ptmx`, `/dev/pts/N`).

Logins are off by default (everything runs as root). To require them,
add `login` after a terminal name in `/storage/etc/ttys` (copy of
`/etc/ttys`; `console login` covers the first console) and set a password
with `passwd` (stored as SHA-512 crypt in `/storage/etc/shadow`; accounts
are in `/etc/passwd`, overridable by `/storage/etc/passwd`). `login [-f]
[USER]` can also be run by hand.

## Storage and devices

| Command | Synopsis |
|---------|----------|
| `mount` | `mount [-t TYPE] [-o ro] SOURCE TARGET`; no arguments lists mounts |
| `umount` | `umount TARGET` (refuses while files are open) |
| `mkfs` / `mkfs.fat` / `mkfs.vfat` | `mkfs.vfat [-F 12|16|32] [-n LABEL] DEVICE` |
| `lsblk` | disks, partitions, filesystems and mount points |
| `lspci`, `lsusb` | PCI and USB devices |
| `hwcheck` | `hwcheck [-y] [-o DIR] [--ssid S --pass P] [--open S] [--http URL] [--https URL] [--dns NAME] [--big URL] [--rekey SECS] [SECTION...]` — hardware checklist with logged results ([HARDWARE.md](HARDWARE.md)) |
| `evtest` | `evtest [-c N] [DEVICE]` — print input events from `/dev/input/event0` (or a `/dev/input/js*` device) |
| `bugreport` | `bugreport [-o FILE \| -]` — kernel log, `/proc`, `/sys/class/net` and tool output in one file |

Types for `mount -t`: `vfat`, `ext2`, `ext3`, `ext4`, `tmpfs`,
`proc`, `sysfs`, `devtmpfs`. Partitions are mounted automatically at boot
and on USB hot-plug (`/storage`, `/boot/efi`, `/mnt/<device>`).

## Networking

| Command | Synopsis |
|---------|----------|
| `ip` | `ip [-br] {addr|link|route|neigh} [show|add|del|set] ...` |
| `ifconfig` | `ifconfig [IFACE [ADDR netmask MASK] [up|down]]` |
| `ifup` | `ifup -a | ifup IFACE` (applies `network.conf`) |
| `dhcp` / `dhclient` | `dhcp IFACE [-t SECS]` |
| `route` | `route add|del default gw GW` · `route add -net N netmask M gw GW` |
| `arp` | neighbour cache |
| `ping` | `ping [-c COUNT] [-i INTERVAL] [-W TIMEOUT] [-s SIZE] [-q] HOST` |
| `nslookup` / `host` | `nslookup [-4|-6] NAME [SERVER]` |
| `netstat` | `netstat [-tuln]` |
| `nc` | `nc [-u] [-w SECS] [-z] HOST PORT` · `nc -l [-u] [-p] PORT` |
| `wget` | `wget [-q|-v] [-S] [-k] [-O FILE] [-T SECS] [-U AGENT] [--header 'H: V'] [--post-data DATA|--post-file FILE] [--load-cookies F] [--save-cookies F] [--keep-session-cookies] [--max-redirect N] [--secure-protocol TLSv1_2|TLSv1_3] [--content-on-error] URL` |
| `netcheck` | `netcheck [-q] [-T SECS]` — Internet / captive-portal check |
| `browse` / `lynx` / `www` | `browse [-k] [-dump|-source] [-width N] [--portal] [URL]` — text web browser, see [BROWSER.md](BROWSER.md) |
| `httpd` | `httpd [-p PORT] [-d DIR]` |
| `ntpdate` | `ntpdate [SERVER]` |
| `wifi` | `wifi [status|scan|connect SSID [PASS] [--save] [--no-portal-check]|disconnect|auto|forget SSID] [-i IFACE]` |

See [NETWORKING.md](NETWORKING.md) and [WIFI.md](WIFI.md).

## Exit status

`0` on success, `1` for failures, `2` for usage errors, `126`/`127` when a
command cannot be executed / is not found, `128+N` when killed by signal
N (`$?` in the shell). The dynamic linker exits with 127 when a library or
symbol is missing.
