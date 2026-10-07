# Services

`init` (PID 1) supervises long-running programs — daemons and session
programs such as a desktop greeter — and `svc` lists and controls them.
It is a small service manager in the spirit of runit or OpenRC: one file
per service, a list of the ones started at boot, restart policies with
back-off, and no dependency on D-Bus: `svc` talks to init over one
`AF_UNIX` socket.

## Boot order

1. `init` runs `/etc/rc` (network configuration, Wi-Fi autoconnect) and
   waits for it.
2. It starts the **enabled** services, each after the services it names
   in `after=` (when those are enabled too).
3. It starts the shells on the extra consoles from `/etc/ttys` (except
   on a terminal an enabled service runs on) and the console shell.

A service that cannot start — missing program, unknown user, missing
terminal — is marked `failed` and logged; boot goes on.

## Service files

`/etc/svc/NAME.conf` (shipped in the image) or `/storage/etc/svc/NAME.conf`
(on the storage partition, replacing the image's file of the same name).
`NAME` is letters, digits and `-_.@`. The format is `key=value` per line,
`#` starts a comment:

| Key | Meaning |
|-----|---------|
| `description` | one-line description (`svc list`) |
| `exec` | command line (required). It runs through `/bin/sh -c` when it contains shell syntax (quotes, `$`, `;`, `\|`, redirections, globs); otherwise it is split at spaces and the program is looked up in `$PATH` (`/bin:/sbin:/usr/bin:/usr/local/bin`) |
| `tty` | optional `ttyN` (or `/dev/ttyN`): the service runs on that virtual console as its session leader with it as controlling terminal and standard I/O, like the console shells |
| `user` | optional account from `/etc/passwd` (default `root`); sets the uid/gid, `HOME`, `USER`, `LOGNAME` and the working directory |
| `restart` | `no`, `on-failure` (default) or `always` |
| `after` | space-separated service names to start first |
| `env` | `KEY=VALUE` added to the environment; may repeat |

Example:

```
# /storage/etc/svc/web.conf
description=Static web server for /storage/www
exec=httpd -p 80 -d /storage/www
restart=on-failure
```

Every service is a session and process-group leader (`setsid`). Without
`tty=`, standard input is `/dev/null` and standard output and error are
appended to `/var/log/svc/NAME.log`. The variable `SVC_NAME` holds the
service's name. init's own log of starts, exits, restarts and requests
is `/var/log/svc/init.log`; failures are also printed on the console.

## Enabled services

`/storage/etc/svc/enabled` lists the services started at boot, one name
per line; when it does not exist, `/etc/svc/enabled` is used. `svc
enable`/`disable` ask init to edit the storage copy (created from the
image's list the first time), so the choice persists across reboots. Without a
storage partition they edit `/etc/svc/enabled` for this boot only.

The image ships definitions for services that are not ported yet —
`dbus`, `seatd`, `upower`, `rustos-nmd`, `tor` and `edex` (the eDEX-DE
greeter on `tty1`) — plus `httpd` as a working example. None is enabled:
starting one whose program is missing just marks it `failed`. `edex`
uses `tty1`, which also carries the serial console's shell (the console
shell always runs; services on `tty2`..`tty4` replace the shell there).

## States and restarts

| State | Meaning |
|-------|---------|
| `starting` | launched less than a second ago, or waiting to be restarted |
| `running` | has been up for at least a second |
| `stopped` | not running (never started, stopped, or exited cleanly) |
| `failed` | could not be started, exited with an error under `restart=no`, or gave up restarting |

An exit counts as a failure when the status is non-zero or the process
was killed by a signal. `on-failure` restarts after failures, `always`
after every exit (1 s after a clean one). Consecutive failures wait 1 s,
2 s, 4 s, ... up to 30 s; a run of a minute or more resets the delay.
The fifth failure within 60 seconds gives up: the service is `failed`
until it is started again. `svc stop` sends SIGTERM (and SIGCONT) to
the service's process group, then SIGKILL after 5 seconds.

## `svc`

```
svc list [--json]
svc status NAME [--json]
svc start NAME          # start; waits until it runs (up to 5 s)
svc stop NAME           # stop; waits until it has exited
svc restart NAME        # stop and start again (or start if stopped)
svc enable NAME         # start at boot
svc disable NAME
```

Exit status: `0` success, `1` error (message on standard error: the
service failed to start, the service manager is not running, `svc:
permission denied`, ...), `3` unknown service.

Anyone may run `svc list` and `svc status`; `start`, `stop`, `restart`,
`enable` and `disable` are for root only.

`--json` prints one object per service, as an array for `list`:

```json
[{"name":"tor","description":"Tor anonymity daemon","enabled":false,"state":"stopped","pid":null,"tty":null}, ...]
```

| Field | Type |
|-------|------|
| `name`, `description` | string |
| `enabled` | boolean |
| `state` | `"running"`, `"stopped"`, `"starting"` or `"failed"` |
| `pid` | number (main process) or `null` |
| `tty` | `"ttyN"` or `null` |

Keys come in this order on one line; services are sorted by name. The
eDEX-DE Services panel relies on this format.

## Control channel

init listens on the `AF_UNIX` stream socket `/run/svc.sock` (mode 0666,
so anyone may connect) from its main loop, which sleeps in `poll()` on
the socket and the connections waiting for their request and wakes on
SIGCHLD. Every `svc` subcommand is one connection carrying one request:

* `svc` connects, writes one line — `list`, `status NAME` or `COMMAND
  NAME` with `COMMAND` one of `start`, `stop`, `restart`, `enable`,
  `disable` — and reads until init closes the connection;
* init takes the client's uid from `SO_PEERCRED` and applies the policy:
  uid 0 may do everything, other users only `list` and `status`;
* the answer is a status line, `ok`, `unknown` (no such service),
  `denied` (not allowed for this uid) or `error MESSAGE`, followed for
  `list` and `status` by one line per service: `NAME STATE ENABLED PID
  TTY EXEC DESCRIPTION`, separated by tabs, with `yes`/`no`, `-` for no
  process or terminal, and `\\`, `\t`, `\n`, `\r` escaped in the text
  fields.

init re-reads the service files and the enabled list for every request,
so `svc` itself reads no files: the state, the enabled list and the
service definitions all come from init, and `enable`/`disable` are
applied by init, which keeps the access policy in one place. `start`
and `restart` then poll `status` until the service runs (up to 5 s),
`stop` until it has exited. A missing socket or a refused connection
shows up as "the service manager is not running"; a client that sends
no request within 5 seconds is disconnected. Requests that change
something, and refused ones, are logged in `/var/log/svc/init.log`.

The encoding, decoding and the permission decision live in the
host-tested `crates/svcconf` (`Request`, `Response`, `permitted`).
RustOS currently runs every process as root (`setuid` changes nothing),
so the refusal is covered by those unit tests rather than the `svc`
scenario.

## Limitations

* No readiness notification: `after=` orders start-up only, and a
  service counts as `running` after one second.
* No socket activation, timers, resource limits, cgroups or per-service
  namespaces; services run with init's environment plus `env=`.
* init does not watch the service files: changes are picked up on the
  next `svc start`/`stop`/`restart`.
* Starting a service on a terminal that already has a shell (from
  `/etc/ttys`) leaves both on it; disable the terminal in `/etc/ttys`
  or enable the service so init leaves that terminal alone at boot.
