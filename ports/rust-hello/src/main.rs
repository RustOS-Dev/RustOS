//! rust-hello: the Rust standard library on RustOS (M43.1).
//!
//! Each check exercises one part of std (or of the crates eDEX-DE is built
//! on) and prints `rust-hello: NAME ok (...)` or `rust-hello: NAME FAILED:
//! ...`; the last line counts them. `rust-hello NAME...` runs only those.
//! The Wayland check needs a compositor (WAYLAND_DISPLAY); without one it
//! is skipped.

use std::{
    collections::HashMap,
    ffi::{CStr, CString},
    fs,
    io::{Read, Seek, SeekFrom, Write},
    net::{TcpListener, TcpStream, UdpSocket},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{fs::PermissionsExt, net::UnixStream, process::CommandExt},
    },
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc, Barrier, Condvar, Mutex, RwLock,
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

type Check = Result<String, String>;
type CheckFn = fn() -> Check;

fn main() {
    let only: Vec<String> = std::env::args().skip(1).collect();
    if only.first().map(String::as_str) == Some("--pty-child") {
        pty_child();
        return;
    }
    if only.first().map(String::as_str) == Some("--spin") {
        spin(Duration::from_millis(400));
        return;
    }
    let checks: [(&str, CheckFn); 14] = [
        ("threads", threads),
        ("fs", files),
        ("process", process),
        ("pty", pty),
        ("calloop", event_loop),
        ("signals", signals),
        ("unwind", unwind),
        ("net", net),
        ("time", time),
        ("env", env),
        ("dlopen-egl", dlopen_egl),
        ("wayland", wayland),
        ("proc", proc_files),
        ("cputime", cpu_time),
    ];
    let (mut passed, mut failed, mut skipped) = (0, 0, 0);
    for (name, f) in checks {
        if !only.is_empty() && !only.iter().any(|o| o == name) {
            continue;
        }
        match f() {
            Ok(detail) if detail.starts_with("skipped") => {
                println!("rust-hello: {name} {detail}");
                skipped += 1;
            }
            Ok(detail) => {
                println!("rust-hello: {name} ok ({detail})");
                passed += 1;
            }
            Err(e) => {
                println!("rust-hello: {name} FAILED: {e}");
                failed += 1;
            }
        }
    }
    println!("rust-hello: {passed} passed, {failed} failed, {skipped} skipped");
    std::process::exit(if failed == 0 { 0 } else { 1 });
}

fn err<E: std::fmt::Display>(what: &str) -> impl FnOnce(E) -> String + '_ {
    move |e| format!("{what}: {e}")
}

fn ensure(cond: bool, what: &str) -> Result<(), String> {
    if cond {
        Ok(())
    } else {
        Err(what.to_string())
    }
}

// ─── Threads ────────────────────────────────────────────────────────────────

thread_local! {
    static TLS: std::cell::Cell<u32> = const { std::cell::Cell::new(7) };
}

fn threads() -> Check {
    const N: usize = 8;
    let counter = Arc::new(Mutex::new(0u64));
    let barrier = Arc::new(Barrier::new(N));
    let (tx, rx) = mpsc::channel();
    let mut handles = Vec::new();
    for i in 0..N {
        let (counter, barrier, tx) = (counter.clone(), barrier.clone(), tx.clone());
        let h = thread::Builder::new()
            .name(format!("worker-{i}"))
            .spawn(move || {
                TLS.with(|t| t.set(i as u32 * 10));
                barrier.wait();
                for _ in 0..10_000 {
                    *counter.lock().unwrap() += 1;
                }
                let name = thread::current().name().map(String::from);
                tx.send((i, name, TLS.with(|t| t.get()))).unwrap();
            })
            .map_err(err("spawn"))?;
        handles.push(h);
    }
    drop(tx);
    for h in handles {
        h.join().map_err(|_| "a worker panicked".to_string())?;
    }
    let mut seen: Vec<_> = rx.iter().collect();
    seen.sort();
    ensure(seen.len() == N, "not every worker reported")?;
    for (i, name, tls) in &seen {
        ensure(
            name.as_deref() == Some(&format!("worker-{i}")[..]),
            "thread names",
        )?;
        ensure(*tls == *i as u32 * 10, "thread-local values")?;
    }
    ensure(TLS.with(|t| t.get()) == 7, "main thread's TLS")?;
    let total = *counter.lock().unwrap();
    ensure(total == (N * 10_000) as u64, "mutex counter")?;
    // Condition variable hand-off and a reader-writer lock.
    let pair = Arc::new((Mutex::new(false), Condvar::new()));
    let p2 = pair.clone();
    let waker = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        *p2.0.lock().unwrap() = true;
        p2.1.notify_all();
    });
    let (lock, cv) = &*pair;
    let guard = cv
        .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(5), |ready| {
            !*ready
        })
        .unwrap();
    ensure(!guard.1.timed_out(), "condvar timed out")?;
    drop(guard);
    waker.join().unwrap();
    let rw = RwLock::new(5);
    thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| assert_eq!(*rw.read().unwrap(), 5));
        }
    });
    *rw.write().unwrap() += 1;
    let cpus = thread::available_parallelism().map_err(err("available_parallelism"))?;
    Ok(format!(
        "{N} threads, counter {total}, {cpus} CPUs, rwlock {}",
        rw.read().unwrap()
    ))
}

// ─── Files ──────────────────────────────────────────────────────────────────

fn files() -> Check {
    let dir = PathBuf::from(format!("/tmp/rust-hello-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("sub/deeper")).map_err(err("create_dir_all"))?;
    let a = dir.join("a.txt");
    fs::write(&a, b"hello rustos\n").map_err(err("write"))?;
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(&a)
        .map_err(err("open append"))?;
    f.write_all(b"second line\n").map_err(err("append"))?;
    f.sync_all().map_err(err("fsync"))?;
    drop(f);
    let text = fs::read_to_string(&a).map_err(err("read"))?;
    ensure(text == "hello rustos\nsecond line\n", "file contents")?;
    let meta = fs::metadata(&a).map_err(err("metadata"))?;
    ensure(meta.len() == 25 && meta.is_file(), "metadata")?;
    meta.modified().map_err(err("mtime"))?;
    let mut perms = meta.permissions();
    perms.set_mode(0o600);
    fs::set_permissions(&a, perms).map_err(err("chmod"))?;
    ensure(
        fs::metadata(&a).unwrap().permissions().mode() & 0o777 == 0o600,
        "chmod",
    )?;
    // Seek, overwrite, truncate.
    let mut f = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&a)
        .map_err(err("open rw"))?;
    f.seek(SeekFrom::Start(6)).map_err(err("seek"))?;
    f.write_all(b"RUSTOS").map_err(err("overwrite"))?;
    f.set_len(12).map_err(err("truncate"))?;
    f.seek(SeekFrom::Start(0)).unwrap();
    let mut s = String::new();
    f.read_to_string(&mut s).map_err(err("read back"))?;
    ensure(s == "hello RUSTOS", "seek/truncate")?;
    drop(f);
    fs::copy(&a, dir.join("b.txt")).map_err(err("copy"))?;
    fs::rename(dir.join("b.txt"), dir.join("sub/c.txt")).map_err(err("rename"))?;
    fs::hard_link(&a, dir.join("hard")).map_err(err("hard_link"))?;
    std::os::unix::fs::symlink("a.txt", dir.join("link")).map_err(err("symlink"))?;
    ensure(
        fs::read_link(dir.join("link")).map_err(err("read_link"))? == std::path::Path::new("a.txt"),
        "read_link",
    )?;
    let canon = fs::canonicalize(dir.join("sub/deeper/../../link")).map_err(err("canonicalize"))?;
    ensure(canon == a, "canonicalize")?;
    let mut names: Vec<String> = fs::read_dir(&dir)
        .map_err(err("read_dir"))?
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    ensure(names == ["a.txt", "hard", "link", "sub"], "read_dir")?;
    ensure(
        fs::read_to_string(dir.join("sub/c.txt")).unwrap() == "hello RUSTOS",
        "copied file",
    )?;
    fs::remove_file(dir.join("hard")).map_err(err("remove_file"))?;
    fs::remove_dir_all(&dir).map_err(err("remove_dir_all"))?;
    ensure(!dir.exists(), "directory removed")?;
    Ok(format!(
        "{} entries, canonical {}",
        names.len(),
        canon.display()
    ))
}

// ─── Processes ──────────────────────────────────────────────────────────────

fn process() -> Check {
    let out = Command::new("/bin/sh")
        .args(["-c", "echo $RUST_HELLO_VAR; pwd; exit 3"])
        .env("RUST_HELLO_VAR", "from-rust")
        .current_dir("/tmp")
        .output()
        .map_err(err("spawn sh"))?;
    ensure(out.status.code() == Some(3), "exit status")?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    ensure(stdout == "from-rust\n/tmp\n", "child stdout")?;
    // Piped stdin and stdout through a child.
    let mut child = Command::new("/bin/cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(err("spawn cat"))?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"round trip\n")
        .map_err(err("write to cat"))?;
    let out = child.wait_with_output().map_err(err("wait cat"))?;
    ensure(out.stdout == b"round trip\n", "cat round trip")?;
    // A missing program fails to spawn with NotFound.
    match Command::new("/no/such/program").status() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        other => return Err(format!("missing program: {other:?}")),
    }
    let exe = std::env::current_exe().map_err(err("current_exe"))?;
    Ok(format!("sh exited 3, cat echoed, exe {}", exe.display()))
}

// ─── Pseudo-terminal ────────────────────────────────────────────────────────

fn pty() -> Check {
    let (mut master, slave) = unsafe {
        let (mut m, mut s) = (0, 0);
        if libc::openpty(
            &mut m,
            &mut s,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        ) != 0
        {
            return Err(format!("openpty: {}", std::io::Error::last_os_error()));
        }
        (fs::File::from_raw_fd(m), OwnedFd::from_raw_fd(s))
    };
    let ws = libc::winsize {
        ws_row: 30,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &ws) };
    let sfd = slave.as_raw_fd();
    let mut cmd = Command::new(std::env::current_exe().map_err(err("current_exe"))?);
    cmd.arg("--pty-child")
        .stdin(Stdio::from(slave.try_clone().map_err(err("dup"))?))
        .stdout(Stdio::from(slave.try_clone().map_err(err("dup"))?))
        .stderr(Stdio::from(slave.try_clone().map_err(err("dup"))?));
    unsafe {
        cmd.pre_exec(move || {
            if libc::setsid() < 0 || libc::ioctl(sfd, libc::TIOCSCTTY, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(err("spawn on pty"))?;
    drop(slave);
    let mut out = Vec::new();
    let mut buf = [0u8; 256];
    let deadline = Instant::now() + Duration::from_secs(10);
    while !String::from_utf8_lossy(&out).contains("pty-done") && Instant::now() < deadline {
        match master.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if e.raw_os_error() == Some(libc::EIO) => break,
            Err(e) => return Err(format!("read master: {e}")),
        }
    }
    let status = child.wait().map_err(err("wait"))?;
    let text = String::from_utf8_lossy(&out).replace('\r', "");
    ensure(
        status.success(),
        &format!("child on the pty failed: {text:?}"),
    )?;
    let tty = text.lines().next().unwrap_or("").to_string();
    ensure(
        tty.starts_with("/dev/pts/"),
        &format!("tty printed {tty:?}"),
    )?;
    ensure(
        text.contains("size 30x100 ctty yes"),
        &format!("child saw {text:?}"),
    )?;
    Ok(format!("{tty}, 100x30, controlling terminal"))
}

/// The pty check's child: its terminal's name, size, and whether it is the controlling one.
fn pty_child() {
    unsafe {
        let name = libc::ttyname(0);
        let name = if name.is_null() {
            String::from("?")
        } else {
            CStr::from_ptr(name).to_string_lossy().into_owned()
        };
        let mut ws: libc::winsize = std::mem::zeroed();
        libc::ioctl(0, libc::TIOCGWINSZ, &mut ws);
        let fd = libc::open(c"/dev/tty".as_ptr(), libc::O_RDWR);
        let ctty = fd >= 0 && libc::tcgetsid(0) == libc::getsid(0);
        println!("{name}");
        println!(
            "size {}x{} ctty {}",
            ws.ws_row,
            ws.ws_col,
            if ctty { "yes" } else { "no" }
        );
        println!("pty-done");
    }
}

// ─── calloop (epoll, timerfd, eventfd, channels) ────────────────────────────

fn event_loop() -> Check {
    use calloop::{
        channel, generic::Generic, ping, timer::TimeoutAction, timer::Timer, EventLoop, Interest,
        Mode, PostAction,
    };
    #[derive(Default)]
    struct State {
        timer: usize,
        msgs: Vec<u32>,
        pipe: Vec<u8>,
        pinged: bool,
    }
    let mut ev: EventLoop<State> = EventLoop::try_new().map_err(err("EventLoop"))?;
    let h = ev.handle();
    h.insert_source(
        Timer::from_duration(Duration::from_millis(30)),
        |_, _, st: &mut State| {
            st.timer += 1;
            if st.timer < 3 {
                TimeoutAction::ToDuration(Duration::from_millis(30))
            } else {
                TimeoutAction::Drop
            }
        },
    )
    .map_err(err("timer"))?;
    let (tx, rx) = channel::channel::<u32>();
    h.insert_source(rx, |e, _, st: &mut State| {
        if let channel::Event::Msg(m) = e {
            st.msgs.push(m);
        }
    })
    .map_err(err("channel"))?;
    let (r, w) = std::io::pipe().map_err(err("pipe"))?;
    let r: OwnedFd = r.into();
    h.insert_source(
        Generic::new(r, Interest::READ, Mode::Level),
        |_, fd, st: &mut State| {
            let mut b = [0u8; 16];
            let n = unsafe { libc::read(fd.as_raw_fd(), b.as_mut_ptr().cast(), b.len()) };
            if n > 0 {
                st.pipe.extend_from_slice(&b[..n as usize]);
            }
            Ok(PostAction::Continue)
        },
    )
    .map_err(err("generic fd"))?;
    let (pinger, ping_src) = ping::make_ping().map_err(err("ping"))?;
    h.insert_source(ping_src, |_, _, st: &mut State| st.pinged = true)
        .map_err(err("ping source"))?;
    let sender = thread::spawn(move || {
        let mut w = w;
        for i in 1..=3 {
            tx.send(i).unwrap();
            thread::sleep(Duration::from_millis(5));
        }
        w.write_all(b"xyz").unwrap();
        pinger.ping();
    });
    let mut st = State::default();
    let deadline = Instant::now() + Duration::from_secs(10);
    while (st.timer < 3 || st.msgs.len() < 3 || st.pipe.len() < 3 || !st.pinged)
        && Instant::now() < deadline
    {
        ev.dispatch(Some(Duration::from_millis(100)), &mut st)
            .map_err(err("dispatch"))?;
    }
    sender.join().unwrap();
    ensure(st.timer == 3, &format!("timer fired {} times", st.timer))?;
    ensure(st.msgs == [1, 2, 3], "channel messages")?;
    ensure(st.pipe == b"xyz", "pipe data")?;
    ensure(st.pinged, "ping")?;
    Ok("timer x3, channel, pipe, ping".into())
}

// ─── Signals ────────────────────────────────────────────────────────────────

static HANDLED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_usr2(_: libc::c_int) {
    HANDLED.store(true, Ordering::SeqCst);
}

fn signals() -> Check {
    use calloop::{
        signals::{Signal, Signals},
        EventLoop,
    };
    // A sigaction handler.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_usr2 as *const () as usize;
        libc::sigemptyset(&mut sa.sa_mask);
        if libc::sigaction(libc::SIGUSR2, &sa, std::ptr::null_mut()) != 0 {
            return Err("sigaction".into());
        }
        libc::raise(libc::SIGUSR2);
    }
    ensure(
        HANDLED.load(Ordering::SeqCst),
        "SIGUSR2 handler did not run",
    )?;
    // signalfd through calloop, as eDEX-DE uses for SIGCHLD/SIGTERM.
    let mut ev: EventLoop<Vec<i32>> = EventLoop::try_new().map_err(err("EventLoop"))?;
    let sigs = Signals::new(&[Signal::SIGUSR1, Signal::SIGCHLD]).map_err(err("Signals"))?;
    ev.handle()
        .insert_source(sigs, |e, _, got: &mut Vec<i32>| got.push(e.signal() as i32))
        .map_err(err("insert signals"))?;
    unsafe { libc::kill(libc::getpid(), libc::SIGUSR1) };
    let mut child = Command::new("/bin/true").spawn().map_err(err("spawn"))?;
    let mut got = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !(got.contains(&libc::SIGUSR1) && got.contains(&libc::SIGCHLD))
        && Instant::now() < deadline
    {
        ev.dispatch(Some(Duration::from_millis(100)), &mut got)
            .map_err(err("dispatch"))?;
    }
    child.wait().map_err(err("wait"))?;
    ensure(got.contains(&libc::SIGUSR1), "SIGUSR1 via signalfd")?;
    ensure(got.contains(&libc::SIGCHLD), "SIGCHLD via signalfd")?;
    // Rust ignores SIGPIPE: writing to a closed pipe is an error, not death.
    let (r, mut w) = std::io::pipe().map_err(err("pipe"))?;
    drop(r);
    match w.write_all(b"x") {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
        other => return Err(format!("write to a closed pipe: {other:?}")),
    }
    Ok("sigaction, signalfd SIGUSR1+SIGCHLD, EPIPE".into())
}

// ─── Unwinding ──────────────────────────────────────────────────────────────

struct DropCount<'a>(&'a AtomicUsize);
impl Drop for DropCount<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[inline(never)]
fn deep_panic(depth: u32, drops: &AtomicUsize) {
    let _guard = DropCount(drops);
    if depth == 0 {
        panic!("rust-hello: expected panic");
    }
    deep_panic(depth - 1, drops);
}

fn unwind() -> Check {
    let drops = AtomicUsize::new(0);
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let r = std::panic::catch_unwind(|| deep_panic(5, &drops));
    std::panic::set_hook(prev);
    ensure(r.is_err(), "panic was not caught")?;
    let msg = r
        .unwrap_err()
        .downcast_ref::<&str>()
        .copied()
        .unwrap_or("")
        .to_string();
    ensure(msg == "rust-hello: expected panic", "panic payload")?;
    ensure(
        drops.load(Ordering::SeqCst) == 6,
        "destructors during unwinding",
    )?;
    // A panicking thread reports through join.
    let h = thread::spawn(|| {
        std::panic::set_hook(Box::new(|_| {}));
        panic!("in a thread")
    });
    ensure(h.join().is_err(), "thread panic")?;
    let _ = std::panic::take_hook();
    let bt = std::backtrace::Backtrace::force_capture();
    let frames = format!("{bt}").lines().count();
    ensure(frames > 2, "backtrace is empty")?;
    Ok(format!("6 frames unwound, backtrace {frames} lines"))
}

// ─── Networking ─────────────────────────────────────────────────────────────

fn net() -> Check {
    let l = TcpListener::bind("127.0.0.1:0").map_err(err("bind"))?;
    let addr = l.local_addr().map_err(err("local_addr"))?;
    let server = thread::spawn(move || -> std::io::Result<()> {
        let (mut s, _) = l.accept()?;
        let mut b = [0u8; 5];
        s.read_exact(&mut b)?;
        s.write_all(&b.map(|c| c.to_ascii_uppercase()))?;
        Ok(())
    });
    let mut c = TcpStream::connect(addr).map_err(err("connect"))?;
    c.set_nodelay(true).map_err(err("nodelay"))?;
    c.write_all(b"hello").map_err(err("send"))?;
    let mut b = [0u8; 5];
    c.read_exact(&mut b).map_err(err("recv"))?;
    server.join().unwrap().map_err(err("server"))?;
    ensure(&b == b"HELLO", "tcp echo")?;
    let u1 = UdpSocket::bind("127.0.0.1:0").map_err(err("udp bind"))?;
    let u2 = UdpSocket::bind("127.0.0.1:0").map_err(err("udp bind"))?;
    u1.send_to(b"dgram", u2.local_addr().unwrap())
        .map_err(err("send_to"))?;
    u2.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut b = [0u8; 16];
    let (n, from) = u2.recv_from(&mut b).map_err(err("recv_from"))?;
    ensure(
        &b[..n] == b"dgram" && from == u1.local_addr().unwrap(),
        "udp",
    )?;
    let (mut a, mut z) = UnixStream::pair().map_err(err("socketpair"))?;
    a.write_all(b"unix").unwrap();
    let mut b = [0u8; 4];
    z.read_exact(&mut b).map_err(err("unix read"))?;
    ensure(&b == b"unix", "unix stream")?;
    Ok(format!("tcp {addr}, udp, unix"))
}

// ─── Time, environment, randomness, /proc ───────────────────────────────────

fn time() -> Check {
    let t0 = Instant::now();
    thread::sleep(Duration::from_millis(20));
    let dt = t0.elapsed();
    ensure(dt >= Duration::from_millis(20), "sleep returned early")?;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(err("system time"))?;
    Ok(format!(
        "slept {} ms, unix time {}",
        dt.as_millis(),
        now.as_secs()
    ))
}

fn env() -> Check {
    std::env::set_var("RUST_HELLO_SET", "1");
    ensure(
        std::env::var("RUST_HELLO_SET").as_deref() == Ok("1"),
        "setenv",
    )?;
    let home = std::env::var("HOME").unwrap_or_default();
    let cwd = std::env::current_dir().map_err(err("getcwd"))?;
    // HashMap's RandomState seeds come from getrandom.
    let a = std::hash::BuildHasher::hash_one(&std::collections::hash_map::RandomState::new(), 1);
    let b = std::hash::BuildHasher::hash_one(&std::collections::hash_map::RandomState::new(), 1);
    let mut m = HashMap::new();
    m.insert("k", 1);
    ensure(m["k"] == 1, "hashmap")?;
    let _ = (a, b);
    let user = unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() {
            String::from("?")
        } else {
            CStr::from_ptr((*pw).pw_name).to_string_lossy().into_owned()
        }
    };
    Ok(format!("HOME={home} cwd={} user={user}", cwd.display()))
}

fn proc_files() -> Check {
    let status = fs::read_to_string("/proc/self/status").map_err(err("/proc/self/status"))?;
    ensure(
        status.contains("Threads:"),
        "/proc/self/status lacks Threads",
    )?;
    let maps = fs::read_to_string("/proc/self/maps").map_err(err("/proc/self/maps"))?;
    ensure(
        maps.contains("rust-hello"),
        "/proc/self/maps lacks the program",
    )?;
    let meminfo = fs::read_to_string("/proc/meminfo").map_err(err("/proc/meminfo"))?;
    ensure(meminfo.starts_with("MemTotal:"), "/proc/meminfo")?;
    let stat = fs::read_to_string("/proc/stat").map_err(err("/proc/stat"))?;
    let cpus = stat.lines().filter(|l| l.starts_with("cpu")).count();
    ensure(cpus >= 2, "/proc/stat has no per-CPU lines")?;
    Ok(format!("status, maps, meminfo, stat ({} cpu lines)", cpus))
}

// ─── dlopen("libEGL.so.1") ──────────────────────────────────────────────────

fn dlopen_egl() -> Check {
    type GetProc = unsafe extern "C" fn(*const libc::c_char) -> *mut libc::c_void;
    type GetPlatformDisplay =
        unsafe extern "C" fn(u32, *mut libc::c_void, *const i32) -> *mut libc::c_void;
    type Initialize = unsafe extern "C" fn(*mut libc::c_void, *mut i32, *mut i32) -> u32;
    type QueryString = unsafe extern "C" fn(*mut libc::c_void, i32) -> *const libc::c_char;
    type Terminate = unsafe extern "C" fn(*mut libc::c_void) -> u32;
    const EGL_PLATFORM_SURFACELESS_MESA: u32 = 0x31DD;
    const EGL_VENDOR: i32 = 0x3053;
    const EGL_VERSION: i32 = 0x3054;
    unsafe {
        let name = CString::new("libEGL.so.1").unwrap();
        let lib = libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
        if lib.is_null() {
            let e = libc::dlerror();
            return Err(if e.is_null() {
                "dlopen failed".into()
            } else {
                CStr::from_ptr(e).to_string_lossy().into_owned()
            });
        }
        let sym = |n: &str| {
            let c = CString::new(n).unwrap();
            libc::dlsym(lib, c.as_ptr())
        };
        let gpa = sym("eglGetProcAddress");
        if gpa.is_null() {
            return Err("no eglGetProcAddress".into());
        }
        let gpa: GetProc = std::mem::transmute(gpa);
        let gpd = gpa(c"eglGetPlatformDisplayEXT".as_ptr());
        let init = sym("eglInitialize");
        let query = sym("eglQueryString");
        let term = sym("eglTerminate");
        if gpd.is_null() || init.is_null() || query.is_null() || term.is_null() {
            return Err("EGL entry points missing".into());
        }
        let gpd: GetPlatformDisplay = std::mem::transmute(gpd);
        let init: Initialize = std::mem::transmute(init);
        let query: QueryString = std::mem::transmute(query);
        let term: Terminate = std::mem::transmute(term);
        let dpy = gpd(
            EGL_PLATFORM_SURFACELESS_MESA,
            std::ptr::null_mut(),
            std::ptr::null(),
        );
        if dpy.is_null() {
            return Err("eglGetPlatformDisplayEXT(surfaceless) failed".into());
        }
        let (mut major, mut minor) = (0, 0);
        if init(dpy, &mut major, &mut minor) == 0 {
            return Err("eglInitialize failed".into());
        }
        let s = |p: *const libc::c_char| {
            if p.is_null() {
                String::new()
            } else {
                CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        };
        let vendor = s(query(dpy, EGL_VENDOR));
        let version = s(query(dpy, EGL_VERSION));
        term(dpy);
        Ok(format!("EGL {major}.{minor} \"{vendor}\" \"{version}\""))
    }
}

// ─── Wayland ────────────────────────────────────────────────────────────────

fn wayland() -> Check {
    use wayland_client::{
        globals::{registry_queue_init, GlobalListContents},
        protocol::wl_registry,
        Connection, Dispatch, QueueHandle,
    };
    struct St;
    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for St {
        fn event(
            _: &mut Self,
            _: &wl_registry::WlRegistry,
            _: wl_registry::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return Ok("skipped (no WAYLAND_DISPLAY)".into());
    }
    let conn = Connection::connect_to_env().map_err(err("connect"))?;
    let (globals, mut queue) = registry_queue_init::<St>(&conn).map_err(err("registry"))?;
    queue.roundtrip(&mut St).map_err(err("roundtrip"))?;
    let names: Vec<String> = globals
        .contents()
        .clone_list()
        .into_iter()
        .map(|g| g.interface)
        .collect();
    for want in ["wl_compositor", "wl_shm", "xdg_wm_base"] {
        ensure(
            names.iter().any(|n| n == want),
            &format!("no {want} global"),
        )?;
    }
    Ok(format!("{} globals incl. wl_compositor", names.len()))
}

// ─── CPU time accounting ────────────────────────────────────────────────────

fn spin(d: Duration) {
    let t0 = Instant::now();
    let mut x = 0u64;
    while t0.elapsed() < d {
        x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
    }
}

fn rusage(who: i32) -> Result<(f64, f64), String> {
    let mut r: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(who, &mut r) } != 0 {
        return Err(format!("getrusage: {}", std::io::Error::last_os_error()));
    }
    let tv = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    Ok((tv(r.ru_utime), tv(r.ru_stime)))
}

fn cpu_time() -> Check {
    // The spin loop reads the clock through a system call (RustOS has no vDSO): count user
    // and system time together.
    let (u0, s0) = rusage(libc::RUSAGE_SELF)?;
    spin(Duration::from_millis(400));
    let (u1, s1) = rusage(libc::RUSAGE_SELF)?;
    let used = (u1 + s1) - (u0 + s0);
    ensure(
        used >= 0.2,
        &format!("CPU time grew {used:.2} s in 0.4 s of spinning"),
    )?;
    // /proc/self/stat fields 14 and 15 (clock ticks).
    let stat = fs::read_to_string("/proc/self/stat").map_err(err("/proc/self/stat"))?;
    let after = stat.rsplit_once(") ").map(|x| x.1).unwrap_or("");
    // Fields after "(comm) ": state ppid pgrp session tty tpgid flags minflt cminflt majflt
    // cmajflt utime stime ...
    let f: Vec<u64> = after
        .split_whitespace()
        .map(|x| x.parse().unwrap_or(0))
        .collect();
    ensure(
        f.len() >= 50 && f[11] + f[12] >= 20,
        &format!("/proc/self/stat utime+stime: {stat:?}"),
    )?;
    let exe = std::env::current_exe().map_err(err("current_exe"))?;
    let st = Command::new(exe)
        .arg("--spin")
        .status()
        .map_err(err("spawn"))?;
    ensure(st.success(), "spinning child failed")?;
    let (cu, cs) = rusage(libc::RUSAGE_CHILDREN)?;
    let cu = cu + cs;
    ensure(cu >= 0.2, &format!("children's CPU time {cu:.2} s"))?;
    Ok(format!("self {u1:.2}+{s1:.2} s, children {cu:.2} s"))
}
