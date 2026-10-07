//! Processes: address space, file descriptors, credentials, process groups,
//! parent/child relationships and exit status.

pub mod elf;
pub mod fd;
pub mod futex;
pub mod itimer;
pub mod signal;
pub mod uaccess;
pub mod vm;

use crate::arch::x86_64::{gdt, idt::TrapFrame};
use crate::errno::*;
use crate::sched::{self, Thread, WaitQueue};
use crate::sync::Mutex;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};

pub type Pid = u32;

pub struct Process {
    pub pid: Pid,
    pub ppid: AtomicU32,
    pub pgid: AtomicU32,
    pub sid: AtomicU32,
    pub name: Mutex<String>,
    pub cmdline: Mutex<Vec<String>>,
    pub exe: Mutex<String>,
    pub vm: Mutex<Option<vm::Vm>>,
    /// The address space after exit, until the last thread is gone: other
    /// threads may still run on it (in user mode or in a syscall) when one
    /// thread ends the process.
    retired_vm: Mutex<Option<vm::Vm>>,
    pub files: Mutex<fd::FdTable>,
    pub cwd: Mutex<String>,
    pub umask: AtomicU32,
    pub uid: AtomicU32,
    pub gid: AtomicU32,
    pub threads: Mutex<Vec<Weak<Thread>>>,
    pub children: Mutex<Vec<Arc<Process>>>,
    /// Encoded wait status once the process has exited.
    pub exit_status: Mutex<Option<i32>>,
    pub zombie: AtomicBool,
    /// Stopped by a job-control signal (reported once to wait4).
    pub stopped: AtomicBool,
    pub stop_reported: AtomicBool,
    pub continued: AtomicBool,
    pub stop_sig: AtomicI32,
    /// Parent waits here for child state changes.
    pub child_wq: WaitQueue,
    pub signals: signal::SignalState,
    /// Monotonic time (ns since boot) when the process was created.
    pub start_ns: u64,
    /// CPU time (timer ticks) of exited threads, and of reaped children
    /// (with their own reaped children): see `sched::cputime`.
    pub dead_utime: AtomicU64,
    pub dead_stime: AtomicU64,
    pub cutime: AtomicU64,
    pub cstime: AtomicU64,
    /// Controlling terminal of the process's session.
    pub ctty: Mutex<Option<Arc<crate::tty::Tty>>>,
}

static PROCESSES: Mutex<BTreeMap<Pid, Weak<Process>>> = Mutex::new(BTreeMap::new());
static NEXT_PID: AtomicU32 = AtomicU32::new(1);

impl Process {
    fn new(parent: Option<&Arc<Process>>) -> Arc<Process> {
        let pid = NEXT_PID.fetch_add(1, Ordering::SeqCst);
        let (ppid, pgid, sid, cwd, umask, name) = match parent {
            Some(p) => (
                p.pid,
                p.pgid.load(Ordering::SeqCst),
                p.sid.load(Ordering::SeqCst),
                p.cwd.lock().clone(),
                p.umask.load(Ordering::SeqCst),
                p.name.lock().clone(),
            ),
            None => (0, pid, pid, String::from("/"), 0o022, String::from("init")),
        };
        let p = Arc::new(Process {
            pid,
            ppid: AtomicU32::new(ppid),
            pgid: AtomicU32::new(pgid),
            sid: AtomicU32::new(sid),
            name: Mutex::new(name),
            cmdline: Mutex::new(Vec::new()),
            exe: Mutex::new(String::new()),
            vm: Mutex::new(None),
            retired_vm: Mutex::new(None),
            files: Mutex::new(fd::FdTable::new()),
            cwd: Mutex::new(cwd),
            umask: AtomicU32::new(umask),
            uid: AtomicU32::new(parent.map_or(0, |p| p.uid.load(Ordering::SeqCst))),
            gid: AtomicU32::new(parent.map_or(0, |p| p.gid.load(Ordering::SeqCst))),
            threads: Mutex::new(Vec::new()),
            children: Mutex::new(Vec::new()),
            exit_status: Mutex::new(None),
            zombie: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            stop_reported: AtomicBool::new(false),
            continued: AtomicBool::new(false),
            stop_sig: AtomicI32::new(0),
            child_wq: WaitQueue::new(),
            signals: signal::SignalState::new(),
            start_ns: crate::time::nanos(),
            dead_utime: AtomicU64::new(0),
            dead_stime: AtomicU64::new(0),
            cutime: AtomicU64::new(0),
            cstime: AtomicU64::new(0),
            ctty: Mutex::new(match parent {
                Some(p) => p.ctty.lock().clone(),
                None => Some(crate::tty::console()),
            }),
        });
        PROCESSES.lock().insert(pid, Arc::downgrade(&p));
        p
    }

    pub fn vm(&self) -> Option<vm::Vm> {
        self.vm.lock().clone()
    }

    pub fn parent(&self) -> Option<Arc<Process>> {
        find(self.ppid.load(Ordering::SeqCst))
    }

    pub fn main_thread(&self) -> Option<Arc<Thread>> {
        self.threads.lock().iter().find_map(|w| w.upgrade())
    }

    /// Free the address space of an exited process once all its threads
    /// are gone (called by the scheduler's worker as each thread is
    /// dropped).
    pub fn release_retired_vm(&self) {
        if self.threads.lock().iter().all(|w| w.strong_count() == 0) {
            drop(self.retired_vm.lock().take());
        }
    }

    pub fn live_threads(&self) -> Vec<Arc<Thread>> {
        self.threads
            .lock()
            .iter()
            .filter_map(|w| w.upgrade())
            .collect()
    }

    /// (user, system) CPU ticks of the process: exited threads plus the
    /// live ones.
    pub fn cpu_times(&self) -> (u64, u64) {
        let (mut u, mut s) = (
            self.dead_utime.load(Ordering::Relaxed),
            self.dead_stime.load(Ordering::Relaxed),
        );
        for t in self.live_threads() {
            if !t.acct_folded() {
                let (tu, ts) = t.cpu_times();
                u += tu;
                s += ts;
            }
        }
        (u, s)
    }

    /// (user, system) CPU ticks of reaped children.
    pub fn children_cpu_times(&self) -> (u64, u64) {
        (
            self.cutime.load(Ordering::Relaxed),
            self.cstime.load(Ordering::Relaxed),
        )
    }

    /// Resolve a user path against the working directory.
    pub fn abs_path(&self, path: &str) -> String {
        crate::vfs::absolute(&self.cwd.lock(), path)
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        PROCESSES.lock().remove(&self.pid);
    }
}

pub fn find(pid: Pid) -> Option<Arc<Process>> {
    PROCESSES.lock().get(&pid).and_then(|w| w.upgrade())
}

/// Like [`all`] but gives up if the table is locked (state dumps).
pub fn try_all() -> Option<Vec<Arc<Process>>> {
    PROCESSES
        .try_lock()
        .map(|t| t.values().filter_map(|w| w.upgrade()).collect())
}

pub fn all() -> Vec<Arc<Process>> {
    PROCESSES
        .lock()
        .values()
        .filter_map(|w| w.upgrade())
        .collect()
}

/// The process owning the current thread.
pub fn current() -> Option<Arc<Process>> {
    sched::try_current().and_then(|t| t.process.lock().clone())
}

pub fn current_pid() -> Pid {
    current().map_or(0, |p| p.pid)
}

// ---------------------------------------------------------------------------
// Page faults from the trap handler
// ---------------------------------------------------------------------------

/// Resolve a page fault on a user address via demand paging / COW.
pub fn handle_page_fault(frame: &mut TrapFrame, addr: u64) -> bool {
    if addr >= crate::mm::USER_END {
        return false;
    }
    let Some(p) = current() else {
        crate::serial_println!("[vm] fault at {:#x}: no current process", addr);
        return false;
    };
    let Some(vm) = p.vm() else {
        crate::serial_println!(
            "[vm] fault at {:#x}: pid {} has no address space",
            addr,
            p.pid
        );
        return false;
    };
    let write = frame.error_code & 2 != 0;
    let exec = frame.error_code & 16 != 0;
    // Faults are handled with interrupts on (another CPU may need the lock).

    vm.lock().handle_fault(addr, write, exec)
}

/// A user page fault that could not be resolved: SIGSEGV with SEGV_ACCERR
/// when a mapping covers `addr` (the access was not permitted), else
/// SEGV_MAPERR, as Linux.
pub fn user_page_fault(frame: &mut TrapFrame, addr: u64) {
    let mapped = addr < crate::mm::USER_END
        && current()
            .and_then(|p| p.vm())
            .is_some_and(|vm| vm.lock().find_area(addr).is_some());
    let code = if mapped {
        signal::SEGV_ACCERR
    } else {
        signal::SEGV_MAPERR
    };
    user_fault(frame, signal::SIGSEGV, code, addr);
}

/// A fault in user mode that could not be resolved: signal the faulting
/// thread with `si_code` `code` and `si_addr` `addr`.
pub fn user_fault(frame: &mut TrapFrame, sig: u32, code: i32, addr: u64) {
    if let Some(p) = current() {
        let area = p.vm().and_then(|vm| {
            vm.lock().find_area(addr).map(|a| {
                alloc::format!(" in {} {:#x}-{:#x} prot {}", a.name, a.start, a.end, a.prot)
            })
        });
        crate::serial_println!(
            "[proc] pid {} ({}) {} at rip {:#x} addr {:#x} err {:#x}{}",
            p.pid,
            p.name.lock(),
            signal::signal_name(sig),
            frame.rip,
            addr,
            frame.error_code,
            area.as_deref().unwrap_or(" (no mapping)")
        );
        signal::force_fault(&p, sig, signal::SigInfo::fault(code, addr));
    } else {
        panic!("user fault without a process");
    }
}

// ---------------------------------------------------------------------------
// Creation
// ---------------------------------------------------------------------------

fn user_frame(entry: u64, stack: u64) -> TrapFrame {
    TrapFrame {
        rip: entry,
        cs: gdt::USER_CS as u64,
        rflags: 0x202,
        rsp: stack,
        ss: gdt::USER_DS as u64,
        ..Default::default()
    }
}

/// Start the first user process from `path`.
pub fn spawn_init(path: &str, argv: &[&str], envp: &[&str]) -> KResult<Arc<Process>> {
    let p = Process::new(None);
    // Descriptors 0-2 are the console.
    {
        let tty = crate::tty::console_file()?;
        let mut files = p.files.lock();
        files.install_at(0, tty.clone(), false);
        files.install_at(1, tty.clone(), false);
        files.install_at(2, tty, false);
    }
    let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
    let envp: Vec<String> = envp.iter().map(|s| s.to_string()).collect();
    let img = elf::load(path, &argv, &envp)?;
    *p.vm.lock() = Some(Arc::new(Mutex::new(img.space)));
    *p.name.lock() = basename(path);
    *p.exe.lock() = path.to_string();
    *p.cmdline.lock() = argv;
    let pml4 = p.vm().unwrap().lock().pml4;
    let t = sched::new_user_thread(&basename(path), &user_frame(img.entry, img.stack), pml4);
    *t.process.lock() = Some(p.clone());
    p.threads.lock().push(Arc::downgrade(&t));
    crate::tty::set_foreground(p.pgid.load(Ordering::SeqCst));
    sched::make_ready(t);
    Ok(p)
}

pub fn basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// fork(): duplicate the current process. Returns the child's pid. The
/// child's thread inherits the caller's signal mask and, with
/// `keep_altstack`, its alternate signal stack.
pub fn fork(frame: &TrapFrame, keep_altstack: bool) -> KResult<Pid> {
    let parent = current().ok_or(ESRCH)?;
    let child = Process::new(Some(&parent));
    let space = parent.vm().ok_or(EFAULT)?.lock().fork()?;
    let pml4 = space.pml4;
    *child.vm.lock() = Some(Arc::new(Mutex::new(space)));
    *child.files.lock() = parent.files.lock().clone();
    *child.cmdline.lock() = parent.cmdline.lock().clone();
    *child.exe.lock() = parent.exe.lock().clone();
    child.signals.inherit_from(&parent.signals);
    parent.children.lock().push(child.clone());

    let mut cf = *frame;
    cf.rax = 0;
    let cur = sched::current();
    let t = sched::new_user_thread(&child.name.lock(), &cf, pml4);
    t.fs_base.store(
        x86_64::registers::model_specific::FsBase::read().as_u64(),
        Ordering::SeqCst,
    );
    unsafe {
        // Inherit the FPU/SSE state.
        core::arch::asm!("fxsave64 [{}]", in(reg) t.fpu.get(), options(nostack));
    }
    t.sig.inherit(&cur.sig, keep_altstack);
    *t.process.lock() = Some(child.clone());
    child.threads.lock().push(Arc::downgrade(&t));
    sched::make_ready(t);
    Ok(child.pid)
}

/// clone() with CLONE_VM|CLONE_THREAD: a new thread in this process.
pub fn clone_thread(
    frame: &TrapFrame,
    stack: u64,
    tls: Option<u64>,
    child_tid: u64,
    clear_tid: u64,
) -> KResult<u64> {
    let p = current().ok_or(ESRCH)?;
    let pml4 = p.vm().ok_or(EFAULT)?.lock().pml4;
    let mut cf = *frame;
    cf.rax = 0;
    if stack != 0 {
        cf.rsp = stack;
    }
    let t = sched::new_user_thread(&p.name.lock(), &cf, pml4);
    // Same mask as the creator, no alternate stack.
    t.sig.inherit(&sched::current().sig, false);
    t.fs_base.store(
        tls.unwrap_or_else(|| x86_64::registers::model_specific::FsBase::read().as_u64()),
        Ordering::SeqCst,
    );
    *t.process.lock() = Some(p.clone());
    p.threads.lock().push(Arc::downgrade(&t));
    let tid = t.tid;
    if child_tid != 0 {
        let _ = uaccess::write_user(child_tid, &(tid as u32));
    }
    t.clear_child_tid.store(clear_tid, Ordering::SeqCst);
    sched::make_ready(t);
    Ok(tid)
}

/// exit(2) of one thread: the process ends with its last thread.
pub fn exit_thread(status: i32) -> ! {
    {
        let me = sched::current();
        let p = current().expect("exit without a process");
        let others = p
            .live_threads()
            .iter()
            .any(|t| !Arc::ptr_eq(t, &me) && t.state() != sched::State::Dead);
        if !others {
            drop(me);
            drop(p);
            exit_current(status);
        }
        futex::clear_tid_and_wake(me.clear_child_tid.swap(0, Ordering::SeqCst));
        me.fold_cpu_times(&p);
        p.threads
            .lock()
            .retain(|w| w.upgrade().is_some_and(|t| !Arc::ptr_eq(&t, &me)));
    }
    sched::exit_current();
}

/// execve(): replace the current process image. On success `frame` is
/// rewritten to enter the new program.
pub fn exec(
    frame: &mut TrapFrame,
    path: &str,
    argv: Vec<String>,
    envp: Vec<String>,
) -> KResult<()> {
    let p = current().ok_or(ESRCH)?;
    let abs = p.abs_path(path);
    let img = elf::load(&abs, &argv, &envp)?;
    let pml4 = img.space.pml4;
    let old = p.vm.lock().replace(Arc::new(Mutex::new(img.space)));
    let t = sched::current();
    t.cr3.store(pml4, Ordering::SeqCst);
    unsafe {
        x86_64::registers::control::Cr3::write(
            x86_64::structures::paging::PhysFrame::containing_address(x86_64::PhysAddr::new(pml4)),
            x86_64::registers::control::Cr3Flags::empty(),
        );
    }
    drop(old);
    t.fs_base.store(0, Ordering::SeqCst);
    x86_64::registers::model_specific::FsBase::write(x86_64::VirtAddr::new(0));
    unsafe {
        let fpu = &mut *t.fpu.get();
        fpu.0 = [0; 512];
        fpu.0[0] = 0x7F;
        fpu.0[1] = 0x03;
        fpu.0[24] = 0x80;
        fpu.0[25] = 0x1F;
        core::arch::asm!("fxrstor64 [{}]", in(reg) t.fpu.get(), options(nostack));
    }
    p.files.lock().close_on_exec();
    p.signals.reset_on_exec();
    t.sig.reset_on_exec();
    let name = basename(&abs);
    *p.name.lock() = name.clone();
    *t.name.lock() = name;
    *p.exe.lock() = abs;
    *p.cmdline.lock() = argv;
    *frame = user_frame(img.entry, img.stack);
    Ok(())
}

// ---------------------------------------------------------------------------
// Exit and wait
// ---------------------------------------------------------------------------

/// Terminate the current process with an encoded wait status.
pub fn exit_current(status: i32) -> ! {
    // Every reference must be dropped before the final switch-away: this
    // function never returns, so its locals would otherwise leak.
    {
        let p = current().expect("exit without a process");
        do_exit(&p, status);
    }
    sched::exit_current();
}

/// Tear down `p` (called on its own thread).
fn do_exit(p: &Arc<Process>, status: i32) {
    if p.zombie.swap(true, Ordering::SeqCst) {
        return;
    }
    // Other threads of the process are told to die.
    let me = sched::current();
    for t in p.live_threads() {
        if !Arc::ptr_eq(&t, &me) {
            t.interrupted.store(true, Ordering::SeqCst);
            signal::kill_thread(&t);
        }
    }
    p.files.lock().close_all();
    // Switch to the kernel page table before dropping the address space.
    me.cr3.store(
        crate::mm::KERNEL_PML4.load(Ordering::SeqCst),
        Ordering::SeqCst,
    );
    unsafe {
        x86_64::registers::control::Cr3::write(
            x86_64::structures::paging::PhysFrame::containing_address(x86_64::PhysAddr::new(
                crate::mm::KERNEL_PML4.load(Ordering::SeqCst),
            )),
            x86_64::registers::control::Cr3Flags::empty(),
        );
    }
    me.is_user.store(false, Ordering::SeqCst);
    me.fold_cpu_times(p);
    // Gone for everyone looking the process up; freed once no thread of it
    // runs any more (`release_retired_vm`).
    let vm = p.vm.lock().take();
    *p.retired_vm.lock() = vm;
    *p.exit_status.lock() = Some(status);

    // Re-parent children to init.
    let children: Vec<Arc<Process>> = core::mem::take(&mut *p.children.lock());
    if let Some(init) = find(1).filter(|i| i.pid != p.pid) {
        for c in children {
            c.ppid.store(1, Ordering::SeqCst);
            init.children.lock().push(c);
        }
        init.child_wq.wake_all();
    }

    if let Some(parent) = p.parent() {
        let (code, st) = if status & 0x7f == 0 {
            (signal::CLD_EXITED, (status >> 8) & 0xff)
        } else if status & 0x80 != 0 {
            (signal::CLD_DUMPED, status & 0x7f)
        } else {
            (signal::CLD_KILLED, status & 0x7f)
        };
        let info = signal::SigInfo::child(code, st, p);
        signal::send_info(&parent, signal::SIGCHLD, info);
        parent.child_wq.wake_all();
    }
    crate::tty::process_exited(p);
    crate::syscall::fdobj::EXIT_WQ.wake_all();
    itimer::remove(p.pid);
    if p.pid == 1 {
        crate::println!("[init] init exited with status {:#x}", status);
    }
}

pub const WNOHANG: u32 = 1;
pub const WUNTRACED: u32 = 2;
pub const WCONTINUED: u32 = 8;

/// wait4(): returns (pid, status, (utime, stime) of the reaped child in
/// ticks, children included) or pid 0 with WNOHANG when nothing changed.
pub fn wait(pid: i32, options: u32) -> KResult<(Pid, i32, (u64, u64))> {
    let p = current().ok_or(ESRCH)?;
    loop {
        let result = {
            let children = p.children.lock();
            let matching: Vec<&Arc<Process>> = children
                .iter()
                .filter(|c| match pid {
                    -1 => true,
                    0 => c.pgid.load(Ordering::SeqCst) == p.pgid.load(Ordering::SeqCst),
                    n if n < -1 => c.pgid.load(Ordering::SeqCst) == (-n) as u32,
                    n => c.pid == n as u32,
                })
                .collect();
            if matching.is_empty() {
                return Err(ECHILD);
            }
            let mut found = None;
            for c in matching {
                if c.zombie.load(Ordering::SeqCst) && c.exit_status.lock().is_some() {
                    let ((u, s), (cu, cs)) = (c.cpu_times(), c.children_cpu_times());
                    found = Some((c.pid, c.exit_status.lock().unwrap(), Some((u + cu, s + cs))));
                    break;
                }
                if options & WUNTRACED != 0
                    && c.stopped.load(Ordering::SeqCst)
                    && !c.stop_reported.swap(true, Ordering::SeqCst)
                {
                    let sig = c.stop_sig.load(Ordering::SeqCst);
                    found = Some((c.pid, (sig << 8) | 0x7f, None));
                    break;
                }
                if options & WCONTINUED != 0 && c.continued.swap(false, Ordering::SeqCst) {
                    found = Some((c.pid, 0xffff, None));
                    break;
                }
            }
            found
        };
        if let Some((cpid, status, reaped)) = result {
            let mut times = (0, 0);
            if let Some((u, s)) = reaped {
                p.children.lock().retain(|c| c.pid != cpid);
                p.cutime.fetch_add(u, Ordering::Relaxed);
                p.cstime.fetch_add(s, Ordering::Relaxed);
                times = (u, s);
            }
            return Ok((cpid, status, times));
        }
        if options & WNOHANG != 0 {
            return Ok((0, 0, (0, 0)));
        }
        let pp = p.clone();
        let ok = p.child_wq.wait_interruptible(|| {
            pp.children.lock().iter().any(|c| {
                c.zombie.load(Ordering::SeqCst)
                    || (options & WUNTRACED != 0
                        && c.stopped.load(Ordering::SeqCst)
                        && !c.stop_reported.load(Ordering::SeqCst))
                    || (options & WCONTINUED != 0 && c.continued.load(Ordering::SeqCst))
            })
        });
        if !ok && signal::has_pending() {
            return Err(EINTR);
        }
    }
}

/// Encode a normal exit status for wait4.
pub fn exit_code_status(code: i32) -> i32 {
    (code & 0xff) << 8
}

/// Most recently assigned process id.
pub fn last_pid() -> Pid {
    NEXT_PID.load(Ordering::SeqCst).saturating_sub(1)
}

/// Number of live (non-zombie) processes.
pub fn count() -> usize {
    all()
        .iter()
        .filter(|p| !p.zombie.load(Ordering::SeqCst))
        .count()
}

/// Members of a process group.
pub fn group_members(pgid: Pid) -> Vec<Arc<Process>> {
    all()
        .into_iter()
        .filter(|p| p.pgid.load(Ordering::SeqCst) == pgid && !p.zombie.load(Ordering::SeqCst))
        .collect()
}
