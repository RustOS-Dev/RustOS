//! Arguments, environment variables and the working directory.

use crate::sys::{self, cstr, nr};
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::UnsafeCell;

struct Env {
    args: UnsafeCell<Vec<String>>,
    vars: UnsafeCell<Vec<(String, String)>>,
}
unsafe impl Sync for Env {}

static ENV: Env = Env {
    args: UnsafeCell::new(Vec::new()),
    vars: UnsafeCell::new(Vec::new()),
};

unsafe fn cstr_at(p: *const u8) -> String {
    let mut n = 0;
    unsafe {
        while *p.add(n) != 0 {
            n += 1;
        }
        String::from_utf8_lossy(core::slice::from_raw_parts(p, n)).into_owned()
    }
}

pub(crate) fn init_from_stack(sp: *const u64) {
    unsafe {
        let argc = *sp as usize;
        let argv = sp.add(1);
        let args = &mut *ENV.args.get();
        for i in 0..argc {
            args.push(cstr_at(*argv.add(i) as *const u8));
        }
        let mut envp = argv.add(argc + 1);
        let vars = &mut *ENV.vars.get();
        while *envp != 0 {
            let s = cstr_at(*envp as *const u8);
            if let Some((k, v)) = s.split_once('=') {
                vars.push((String::from(k), String::from(v)));
            }
            envp = envp.add(1);
        }
    }
}

pub fn args() -> Vec<String> {
    unsafe { (*ENV.args.get()).clone() }
}

pub fn var(key: &str) -> Option<String> {
    unsafe {
        (*ENV.vars.get())
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }
}

pub fn vars() -> Vec<(String, String)> {
    unsafe { (*ENV.vars.get()).clone() }
}

pub fn set_var(key: &str, val: &str) {
    let vars = unsafe { &mut *ENV.vars.get() };
    match vars.iter_mut().find(|(k, _)| k == key) {
        Some(e) => e.1 = String::from(val),
        None => vars.push((String::from(key), String::from(val))),
    }
}

pub fn remove_var(key: &str) {
    unsafe { (*ENV.vars.get()).retain(|(k, _)| k != key) };
}

/// The environment as "KEY=VALUE" strings (for execve).
pub fn environ() -> Vec<String> {
    vars()
        .into_iter()
        .map(|(k, v)| alloc::format!("{}={}", k, v))
        .collect()
}

pub fn current_dir() -> crate::Result<String> {
    let mut buf = alloc::vec![0u8; 4096];
    let n = sys::check(sys::syscall(
        nr::GETCWD,
        &[buf.as_mut_ptr() as usize, buf.len()],
    ))?;
    buf.truncate(n.saturating_sub(1));
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

pub fn set_current_dir(path: &str) -> crate::Result<()> {
    let p = cstr(path);
    sys::check(sys::syscall(nr::CHDIR, &[p.as_ptr() as usize])).map(|_| ())
}

pub fn hostname() -> String {
    let u = crate::process::uname();
    u.nodename
}

pub fn set_hostname(name: &str) -> crate::Result<()> {
    sys::check(sys::syscall(
        nr::SETHOSTNAME,
        &[name.as_ptr() as usize, name.len()],
    ))
    .map(|_| ())
}
