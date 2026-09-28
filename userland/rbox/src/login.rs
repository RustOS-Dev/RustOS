//! login and passwd: user accounts from /etc/passwd and /etc/shadow
//! (SHA-512 crypt). Accounts and password hashes on the storage partition
//! (/storage/etc/passwd, /storage/etc/shadow) take precedence.

use rustos_rt::prelude::*;
use rustos_rt::{env, fs, process, term};

const SHADOW_STORE: &str = "/storage/etc/shadow";

fn read_first(names: &[&str]) -> String {
    names
        .iter()
        .find_map(|p| fs::read_to_string(p).ok())
        .unwrap_or_default()
}

struct Account {
    name: String,
    uid: u32,
    gid: u32,
    home: String,
    shell: String,
}

fn account(user: &str) -> Option<Account> {
    let passwd = read_first(&["/storage/etc/passwd", "/etc/passwd"]);
    passwd.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.len() >= 7 && f[0] == user).then(|| Account {
            name: f[0].into(),
            uid: f[2].parse().unwrap_or(0),
            gid: f[3].parse().unwrap_or(0),
            home: f[5].into(),
            shell: if f[6].is_empty() {
                "/bin/sh".into()
            } else {
                f[6].into()
            },
        })
    })
}

/// The shadow hash for `user`: Some("") for no password, None if locked
/// or unknown.
fn shadow_hash(user: &str) -> Option<String> {
    let shadow = read_first(&[SHADOW_STORE, "/etc/shadow"]);
    let h = shadow.lines().find_map(|l| {
        let mut f = l.split(':');
        (f.next() == Some(user)).then(|| f.next().unwrap_or("").to_string())
    })?;
    (!h.starts_with('!') && !h.starts_with('*')).then_some(h)
}

fn prompt_line(p: &str, echo: bool) -> Option<String> {
    print!("{}", p);
    rustos_rt::io::flush();
    let saved = term::get(0);
    if !echo && let Some(mut t) = saved {
        t.lflag &= !term::ECHO;
        term::set(0, &t);
    }
    let mut line = String::new();
    let mut b = [0u8; 1];
    let ok = loop {
        match rustos_rt::io::read(0, &mut b) {
            Ok(1) if b[0] == b'\n' => break true,
            Ok(1) => line.push(b[0] as char),
            _ => break !line.is_empty(),
        }
    };
    if !echo {
        if let Some(t) = saved {
            term::set(0, &t);
        }
        println!();
    }
    ok.then_some(line)
}

fn start_session(a: &Account) -> i32 {
    // setgid(106) then setuid(105).
    rustos_rt::sys::syscall(106, &[a.gid as usize]);
    rustos_rt::sys::syscall(105, &[a.uid as usize]);
    env::set_var("HOME", &a.home);
    env::set_var("USER", &a.name);
    env::set_var("LOGNAME", &a.name);
    env::set_var("SHELL", &a.shell);
    if env::set_current_dir(&a.home).is_err() {
        let _ = env::set_current_dir("/");
    }
    let argv = vec![format!("-{}", a.shell.rsplit('/').next().unwrap_or("sh"))];
    let e = process::execve(&a.shell, &argv, &env::environ());
    eprintln!("login: cannot run {}: {}", a.shell, e);
    1
}

/// login [-f USER] [USER]
pub fn login(args: &[String]) -> i32 {
    let (force, preset) = match args.get(1).map(|s| s.as_str()) {
        Some("-f") => (true, args.get(2).cloned()),
        Some(u) => (false, Some(u.to_string())),
        None => (false, None),
    };
    let mut failures = 0;
    loop {
        let user = match &preset {
            Some(u) if failures == 0 => u.clone(),
            _ => match prompt_line(&format!("{} login: ", env::hostname()), true) {
                Some(u) if !u.is_empty() => u,
                Some(_) => continue,
                None => return 1,
            },
        };
        let acct = account(&user);
        let ok = force
            || match (&acct, shadow_hash(&user)) {
                (Some(_), Some(h)) if h.is_empty() => true,
                (Some(_), Some(h)) => {
                    let pw = prompt_line("Password: ", false).unwrap_or_default();
                    unixcrypt::verify(pw.as_bytes(), &h)
                }
                _ => {
                    // Ask anyway so unknown names are not revealed.
                    let _ = prompt_line("Password: ", false);
                    false
                }
            };
        if ok && let Some(a) = acct {
            return start_session(&a);
        }
        failures += 1;
        println!("Login incorrect");
        rustos_rt::time::sleep_ms(1000 * failures.min(5));
    }
}

/// passwd [USER]: set a password (stored in /storage/etc/shadow).
pub fn passwd(args: &[String]) -> i32 {
    let user = args
        .get(1)
        .cloned()
        .or_else(|| env::var("USER"))
        .unwrap_or_else(|| "root".into());
    if account(&user).is_none() {
        eprintln!("passwd: unknown user {}", user);
        return 1;
    }
    let (Some(a), Some(b)) = (
        prompt_line("New password: ", false),
        prompt_line("Retype new password: ", false),
    ) else {
        return 1;
    };
    if a != b {
        eprintln!("passwd: passwords do not match");
        return 1;
    }
    let mut rnd = [0u8; 16];
    process::getrandom(&mut rnd);
    let hash = if a.is_empty() {
        String::new()
    } else {
        unixcrypt::sha512_crypt(a.as_bytes(), &unixcrypt::make_salt(&rnd), None)
    };
    let old = read_first(&[SHADOW_STORE, "/etc/shadow"]);
    let mut found = false;
    let mut out = String::new();
    for l in old.lines() {
        let mut f: Vec<String> = l.split(':').map(String::from).collect();
        if f.first().map(|s| s.as_str()) == Some(user.as_str()) {
            if f.len() < 2 {
                f.resize(2, String::new());
            }
            f[1] = hash.clone();
            found = true;
        }
        out.push_str(&f.join(":"));
        out.push('\n');
    }
    if !found {
        out.push_str(&format!("{}:{}:0:0:99999:7:::\n", user, hash));
    }
    let _ = fs::create_dir_all("/storage/etc");
    if let Err(e) = fs::write(SHADOW_STORE, out.as_bytes()) {
        eprintln!("passwd: cannot write {}: {}", SHADOW_STORE, e);
        return 1;
    }
    let _ = fs::set_permissions(SHADOW_STORE, 0o600);
    println!("passwd: password updated for {}", user);
    0
}
