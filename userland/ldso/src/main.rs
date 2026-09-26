//! ld-rustos.so.1: the RustOS dynamic linker.
//!
//! The kernel maps a dynamically linked program together with this
//! interpreter (already relocated) and starts us with the program's initial
//! stack. We map every `DT_NEEDED` library from /lib or /usr/lib (breadth
//! first), resolve symbols in load order (program first) through the GNU or
//! SysV hash tables, apply x86-64 RELA relocations (RELATIVE, 64, GLOB_DAT,
//! JUMP_SLOT with eager binding, COPY), restore segment permissions, run
//! the libraries' initialisers and jump to the program's entry point.
//!
//! No libc, no allocator: raw system calls and fixed-size tables.

#![no_std]
#![no_main]

use core::arch::{asm, global_asm};

global_asm!(
    ".globl _start",
    "_start:",
    "    mov r12, rsp",
    "    mov rdi, rsp",
    "    and rsp, -16",
    "    call {main}",
    "    mov rsp, r12",
    "    xor edx, edx", // no atexit handler
    "    jmp rax",
    main = sym ldso_main,
);

// ---------------------------------------------------------------------------
// System calls
// ---------------------------------------------------------------------------

unsafe fn syscall(n: usize, a: usize, b: usize, c: usize, d: usize, e: usize, f: usize) -> isize {
    let r: isize;
    unsafe {
        asm!("syscall", inlateout("rax") n as isize => r, in("rdi") a, in("rsi") b, in("rdx") c,
             in("r10") d, in("r8") e, in("r9") f, lateout("rcx") _, lateout("r11") _, options(nostack));
    }
    r
}

const SYS_READ: usize = 0;
const SYS_WRITE: usize = 1;
const SYS_OPEN: usize = 2;
const SYS_CLOSE: usize = 3;
const SYS_LSEEK: usize = 8;
const SYS_MMAP: usize = 9;
const SYS_MPROTECT: usize = 10;
const SYS_EXIT: usize = 60;

fn write_err(s: &[u8]) {
    unsafe { syscall(SYS_WRITE, 2, s.as_ptr() as usize, s.len(), 0, 0, 0) };
}

fn fail(msg: &[u8], name: &[u8]) -> ! {
    write_err(b"ld-rustos: ");
    write_err(msg);
    if !name.is_empty() {
        write_err(b": ");
        write_err(name);
    }
    write_err(b"\n");
    unsafe { syscall(SYS_EXIT, 127, 0, 0, 0, 0, 0) };
    loop {}
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    fail(b"internal error", b"")
}

// ---------------------------------------------------------------------------
// ELF structures
// ---------------------------------------------------------------------------

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_PHDR: u32 = 6;

const DT_NULL: i64 = 0;
const DT_NEEDED: i64 = 1;
const DT_PLTRELSZ: i64 = 2;
const DT_HASH: i64 = 4;
const DT_STRTAB: i64 = 5;
const DT_SYMTAB: i64 = 6;
const DT_RELA: i64 = 7;
const DT_RELASZ: i64 = 8;
const DT_INIT: i64 = 12;
const DT_JMPREL: i64 = 23;
const DT_INIT_ARRAY: i64 = 25;
const DT_INIT_ARRAYSZ: i64 = 27;
const DT_GNU_HASH: i64 = 0x6fff_fef5;

const R_X86_64_NONE: u32 = 0;
const R_X86_64_64: u32 = 1;
const R_X86_64_COPY: u32 = 5;
const R_X86_64_GLOB_DAT: u32 = 6;
const R_X86_64_JUMP_SLOT: u32 = 7;
const R_X86_64_RELATIVE: u32 = 8;

#[repr(C)]
#[derive(Clone, Copy)]
struct Phdr {
    p_type: u32,
    p_flags: u32,
    p_offset: u64,
    p_vaddr: u64,
    p_paddr: u64,
    p_filesz: u64,
    p_memsz: u64,
    p_align: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Sym {
    st_name: u32,
    st_info: u8,
    st_other: u8,
    st_shndx: u16,
    st_value: u64,
    st_size: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Rela {
    offset: u64,
    info: u64,
    addend: i64,
}

const MAX_OBJS: usize = 32;
const MAX_SEGS: usize = 8;

#[derive(Clone, Copy)]
struct Obj {
    bias: u64,
    name: [u8; 64],
    name_len: usize,
    strtab: u64,
    symtab: u64,
    gnu_hash: u64,
    sysv_hash: u64,
    rela: u64,
    relasz: u64,
    jmprel: u64,
    pltrelsz: u64,
    init: u64,
    init_array: u64,
    init_arraysz: u64,
    dynamic: u64,
    segs: [(u64, u64, u32); MAX_SEGS],
    nsegs: usize,
}

impl Obj {
    const EMPTY: Obj = Obj {
        bias: 0,
        name: [0; 64],
        name_len: 0,
        strtab: 0,
        symtab: 0,
        gnu_hash: 0,
        sysv_hash: 0,
        rela: 0,
        relasz: 0,
        jmprel: 0,
        pltrelsz: 0,
        init: 0,
        init_array: 0,
        init_arraysz: 0,
        dynamic: 0,
        segs: [(0, 0, 0); MAX_SEGS],
        nsegs: 0,
    };

    fn name(&self) -> &[u8] {
        &self.name[..self.name_len]
    }

    /// Parse the dynamic section (addresses are relative to `bias`).
    fn parse_dynamic(&mut self) {
        let mut d = self.dynamic as *const i64;
        loop {
            let (tag, val) = unsafe { (*d, *d.add(1) as u64) };
            match tag {
                DT_NULL => break,
                DT_STRTAB => self.strtab = self.bias + val,
                DT_SYMTAB => self.symtab = self.bias + val,
                DT_GNU_HASH => self.gnu_hash = self.bias + val,
                DT_HASH => self.sysv_hash = self.bias + val,
                DT_RELA => self.rela = self.bias + val,
                DT_RELASZ => self.relasz = val,
                DT_JMPREL => self.jmprel = self.bias + val,
                DT_PLTRELSZ => self.pltrelsz = val,
                DT_INIT => self.init = self.bias + val,
                DT_INIT_ARRAY => self.init_array = self.bias + val,
                DT_INIT_ARRAYSZ => self.init_arraysz = val,
                _ => {}
            }
            d = unsafe { d.add(2) };
        }
    }

    /// DT_NEEDED names, in order.
    fn needed(&self, mut f: impl FnMut(&[u8])) {
        let mut d = self.dynamic as *const i64;
        loop {
            let (tag, val) = unsafe { (*d, *d.add(1) as u64) };
            if tag == DT_NULL {
                break;
            }
            if tag == DT_NEEDED {
                f(cstr(self.strtab + val));
            }
            d = unsafe { d.add(2) };
        }
    }

    fn sym(&self, idx: u32) -> Sym {
        unsafe { *((self.symtab as *const Sym).add(idx as usize)) }
    }

    /// Look up a defined global/weak symbol by name.
    fn lookup(&self, name: &[u8]) -> Option<Sym> {
        let ok = |s: &Sym| s.st_shndx != 0 && matches!(s.st_info >> 4, 1 | 2) && cstr(self.strtab + s.st_name as u64) == name;
        if self.gnu_hash != 0 {
            let h = self.gnu_hash as *const u32;
            unsafe {
                let nbuckets = *h;
                let symoffset = *h.add(1);
                let bloom_size = *h.add(2);
                let buckets = h.add(4 + bloom_size as usize * 2);
                let chain = buckets.add(nbuckets as usize);
                if nbuckets == 0 {
                    return None;
                }
                let hash = gnu_hash(name);
                let mut idx = *buckets.add((hash % nbuckets) as usize);
                if idx < symoffset {
                    return None;
                }
                loop {
                    let h2 = *chain.add((idx - symoffset) as usize);
                    if (hash | 1) == (h2 | 1) {
                        let s = self.sym(idx);
                        if ok(&s) {
                            return Some(s);
                        }
                    }
                    if h2 & 1 != 0 {
                        return None;
                    }
                    idx += 1;
                }
            }
        }
        if self.sysv_hash != 0 {
            let h = self.sysv_hash as *const u32;
            unsafe {
                let nbucket = *h;
                let buckets = h.add(2);
                let chains = buckets.add(nbucket as usize);
                let mut idx = *buckets.add((elf_hash(name) % nbucket) as usize);
                while idx != 0 {
                    let s = self.sym(idx);
                    if ok(&s) {
                        return Some(s);
                    }
                    idx = *chains.add(idx as usize);
                }
            }
        }
        None
    }
}

fn gnu_hash(name: &[u8]) -> u32 {
    name.iter().fold(5381u32, |h, &c| h.wrapping_mul(33).wrapping_add(c as u32))
}

fn elf_hash(name: &[u8]) -> u32 {
    let mut h = 0u32;
    for &c in name {
        h = (h << 4).wrapping_add(c as u32);
        let g = h & 0xF000_0000;
        if g != 0 {
            h ^= g >> 24;
        }
        h &= !g;
    }
    h
}

fn cstr(addr: u64) -> &'static [u8] {
    let p = addr as *const u8;
    let mut n = 0;
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    unsafe { core::slice::from_raw_parts(p, n) }
}

static mut OBJS: [Obj; MAX_OBJS] = [Obj::EMPTY; MAX_OBJS];
static mut NOBJS: usize = 0;

#[allow(static_mut_refs)]
fn objs() -> &'static mut [Obj] {
    unsafe { &mut OBJS[..NOBJS] }
}

fn prot_of(flags: u32) -> usize {
    ((flags & 4 != 0) as usize) | (((flags & 2 != 0) as usize) << 1) | (((flags & 1 != 0) as usize) << 2)
}

fn page_down(a: u64) -> u64 {
    a & !4095
}
fn page_up(a: u64) -> u64 {
    (a + 4095) & !4095
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

fn read_exact(fd: usize, off: u64, dst: u64, len: u64, name: &[u8]) {
    if unsafe { syscall(SYS_LSEEK, fd, off as usize, 0, 0, 0, 0) } < 0 {
        fail(b"seek failed", name);
    }
    let mut done = 0;
    while done < len {
        let n = unsafe { syscall(SYS_READ, fd, (dst + done) as usize, (len - done) as usize, 0, 0, 0) };
        if n <= 0 {
            fail(b"short read", name);
        }
        done += n as u64;
    }
}

fn already_loaded(name: &[u8]) -> bool {
    objs().iter().any(|o| o.name() == name)
}

/// Map a shared library and add it to the object list.
fn load_library(name: &[u8]) {
    let mut path = [0u8; 256];
    let mut fd = -1isize;
    for dir in [&b"/lib/"[..], &b"/usr/lib/"[..]] {
        if dir.len() + name.len() + 1 > path.len() {
            fail(b"library name too long", name);
        }
        path[..dir.len()].copy_from_slice(dir);
        path[dir.len()..dir.len() + name.len()].copy_from_slice(name);
        path[dir.len() + name.len()] = 0;
        fd = unsafe { syscall(SYS_OPEN, path.as_ptr() as usize, 0, 0, 0, 0, 0) };
        if fd >= 0 {
            break;
        }
    }
    if fd < 0 {
        fail(b"cannot find library", name);
    }
    let fd = fd as usize;
    let mut ehdr = [0u8; 64];
    read_exact(fd, 0, ehdr.as_mut_ptr() as u64, 64, name);
    if &ehdr[..4] != b"\x7fELF" || ehdr[4] != 2 || u16::from_le_bytes([ehdr[16], ehdr[17]]) != 3 {
        fail(b"not an ELF64 shared object", name);
    }
    let phoff = u64::from_le_bytes(ehdr[32..40].try_into().unwrap());
    let phnum = u16::from_le_bytes([ehdr[56], ehdr[57]]) as usize;
    let mut phdrs = [Phdr { p_type: 0, p_flags: 0, p_offset: 0, p_vaddr: 0, p_paddr: 0, p_filesz: 0, p_memsz: 0, p_align: 0 }; 16];
    if phnum > phdrs.len() {
        fail(b"too many program headers", name);
    }
    read_exact(fd, phoff, phdrs.as_mut_ptr() as u64, (phnum * 56) as u64, name);
    let phdrs = &phdrs[..phnum];
    let lo = phdrs.iter().filter(|p| p.p_type == PT_LOAD).map(|p| page_down(p.p_vaddr)).min().unwrap_or(0);
    let hi = phdrs.iter().filter(|p| p.p_type == PT_LOAD).map(|p| page_up(p.p_vaddr + p.p_memsz)).max().unwrap_or(0);
    if hi <= lo {
        fail(b"no loadable segments", name);
    }
    let base = unsafe { syscall(SYS_MMAP, 0, (hi - lo) as usize, 3, 0x22, usize::MAX, 0) };
    if base < 0 {
        fail(b"out of memory", name);
    }
    let bias = base as u64 - lo;
    let mut o = Obj::EMPTY;
    o.bias = bias;
    o.name_len = name.len().min(64);
    o.name[..o.name_len].copy_from_slice(&name[..o.name_len]);
    for p in phdrs {
        match p.p_type {
            PT_LOAD => {
                if p.p_filesz > 0 {
                    read_exact(fd, p.p_offset, bias + p.p_vaddr, p.p_filesz, name);
                }
                if o.nsegs < MAX_SEGS {
                    o.segs[o.nsegs] = (page_down(bias + p.p_vaddr), page_up(bias + p.p_vaddr + p.p_memsz), p.p_flags);
                    o.nsegs += 1;
                }
            }
            PT_DYNAMIC => o.dynamic = bias + p.p_vaddr,
            _ => {}
        }
    }
    unsafe { syscall(SYS_CLOSE, fd, 0, 0, 0, 0, 0) };
    if o.dynamic == 0 {
        fail(b"library has no dynamic section", name);
    }
    o.parse_dynamic();
    push_obj(o);
}

#[allow(static_mut_refs)]
fn push_obj(o: Obj) {
    unsafe {
        if NOBJS == MAX_OBJS {
            fail(b"too many libraries", b"");
        }
        OBJS[NOBJS] = o;
        NOBJS += 1;
    }
}

/// Resolve `name`, searching every object (or every library for COPY).
fn resolve(name: &[u8], skip_first: bool) -> Option<(u64, u64)> {
    objs()
        .iter()
        .skip(skip_first as usize)
        .find_map(|o| o.lookup(name).map(|s| (o.bias + s.st_value, s.st_size)))
}

fn relocate(o: &Obj, is_main: bool) {
    let tables = [(o.rela, o.relasz), (o.jmprel, o.pltrelsz)];
    for (table, size) in tables {
        if table == 0 {
            continue;
        }
        let n = size as usize / core::mem::size_of::<Rela>();
        for i in 0..n {
            let r = unsafe { *((table as *const Rela).add(i)) };
            let ty = r.info as u32;
            let sym_idx = (r.info >> 32) as u32;
            let at = (o.bias + r.offset) as *mut u64;
            let (s, size) = if sym_idx != 0 && ty != R_X86_64_RELATIVE {
                let sym = o.sym(sym_idx);
                let sname = cstr(o.strtab + sym.st_name as u64);
                match resolve(sname, ty == R_X86_64_COPY && is_main) {
                    Some(v) => v,
                    None if sym.st_info >> 4 == 2 => (0, 0), // undefined weak
                    None => fail(b"undefined symbol", sname),
                }
            } else {
                (0, 0)
            };
            unsafe {
                match ty {
                    R_X86_64_NONE => {}
                    R_X86_64_RELATIVE => *at = o.bias.wrapping_add(r.addend as u64),
                    R_X86_64_64 => *at = s.wrapping_add(r.addend as u64),
                    R_X86_64_GLOB_DAT | R_X86_64_JUMP_SLOT => *at = s,
                    R_X86_64_COPY => core::ptr::copy_nonoverlapping(s as *const u8, at as *mut u8, size as usize),
                    _ => fail(b"unsupported relocation type in", o.name()),
                }
            }
        }
    }
}

fn set_prot(o: &Obj, writable: bool) {
    for &(s, e, f) in &o.segs[..o.nsegs] {
        let prot = prot_of(f) | if writable { 2 } else { 0 };
        unsafe { syscall(SYS_MPROTECT, s as usize, (e - s) as usize, prot, 0, 0, 0) };
    }
}

extern "C" fn ldso_main(sp: *const u64) -> u64 {
    // Initial stack: argc, argv..., 0, envp..., 0, auxv pairs.
    let argc = unsafe { *sp } as usize;
    let mut p = unsafe { sp.add(1 + argc + 1) };
    while unsafe { *p } != 0 {
        p = unsafe { p.add(1) };
    }
    p = unsafe { p.add(1) };
    let (mut at_phdr, mut at_phnum, mut at_entry) = (0u64, 0u64, 0u64);
    loop {
        let (k, v) = unsafe { (*p, *p.add(1)) };
        match k {
            0 => break,
            3 => at_phdr = v,
            5 => at_phnum = v,
            9 => at_entry = v,
            _ => {}
        }
        p = unsafe { p.add(2) };
    }
    if at_phdr == 0 || at_entry == 0 {
        fail(b"started without a program (run a dynamically linked binary instead)", b"");
    }

    // The program itself.
    let phdrs = unsafe { core::slice::from_raw_parts(at_phdr as *const Phdr, at_phnum as usize) };
    let bias = match phdrs.iter().find(|p| p.p_type == PT_PHDR) {
        Some(ph) => at_phdr - ph.p_vaddr,
        None => {
            let first = phdrs.iter().find(|p| p.p_type == PT_LOAD).map_or(0, |p| p.p_vaddr);
            at_phdr - 64 - first
        }
    };
    let mut main = Obj::EMPTY;
    main.bias = bias;
    main.name_len = 1;
    main.name[0] = b'-';
    for ph in phdrs {
        match ph.p_type {
            PT_LOAD if main.nsegs < MAX_SEGS => {
                main.segs[main.nsegs] = (page_down(bias + ph.p_vaddr), page_up(bias + ph.p_vaddr + ph.p_memsz), ph.p_flags);
                main.nsegs += 1;
            }
            PT_DYNAMIC => main.dynamic = bias + ph.p_vaddr,
            _ => {}
        }
    }
    if main.dynamic == 0 {
        fail(b"program has no dynamic section", b"");
    }
    main.parse_dynamic();
    push_obj(main);

    // Breadth-first dependency loading.
    let mut i = 0;
    while i < objs().len() {
        let o = objs()[i];
        o.needed(|name| {
            if !already_loaded(name) {
                load_library(name);
            }
        });
        i += 1;
    }

    // Libraries first (a COPY in the program reads relocated library
    // data), then the program.
    let all: &[Obj] = objs();
    for o in all.iter().skip(1) {
        relocate(o, false);
        set_prot(o, false);
    }
    set_prot(&all[0], true);
    relocate(&all[0], true);
    set_prot(&all[0], false);

    // Initialisers, dependencies before dependents.
    for o in all.iter().skip(1).rev() {
        if o.init != 0 {
            let f: extern "C" fn() = unsafe { core::mem::transmute(o.init) };
            f();
        }
        for k in 0..(o.init_arraysz / 8) as usize {
            let fp = unsafe { *((o.init_array as *const u64).add(k)) };
            if fp != 0 && fp != u64::MAX {
                let f: extern "C" fn() = unsafe { core::mem::transmute(fp) };
                f();
            }
        }
    }
    at_entry
}
