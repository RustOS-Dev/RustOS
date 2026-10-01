//! ELF64 program loader.
//!
//! Loads static executables (ET_EXEC), static position-independent
//! executables (ET_DYN, relocated by a fixed bias with R_X86_64_RELATIVE
//! relocations applied) and dynamically linked programs: for those the
//! PT_INTERP interpreter (the RustOS ld.so, a static PIE) is loaded as well
//! and started with AT_BASE / AT_ENTRY / AT_PHDR describing the program; it
//! maps the shared libraries and performs the program's relocations.
//! PT_LOAD segments are copied into anonymous memory with their requested
//! permissions, straight from the file in small chunks: the loader never
//! holds a whole program in the kernel heap (programs can be larger than
//! it). The initial stack follows the System V ABI: argc, argv,
//! envp, auxv.

use super::vm::{self, AddressSpace, Area, Backing, PROT_EXEC, PROT_READ, PROT_WRITE};
use crate::errno::*;
use crate::mm::FRAME_SIZE;
use crate::vfs::{FileType, Inode};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;
const PT_PHDR: u32 = 6;
const PT_GNU_STACK: u32 = 0x6474_e551;
const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;
const EM_X86_64: u16 = 62;
const PIE_BIAS: u64 = 0x0000_5555_5555_0000;
/// Where a position-independent interpreter (ld.so) is placed.
const INTERP_BIAS: u64 = 0x0000_6fff_0000_0000;

const AT_NULL: u64 = 0;
const AT_PHDR: u64 = 3;
const AT_PHENT: u64 = 4;
const AT_PHNUM: u64 = 5;
const AT_PAGESZ: u64 = 6;
const AT_BASE: u64 = 7;
const AT_ENTRY: u64 = 9;
const AT_UID: u64 = 11;
const AT_EUID: u64 = 12;
const AT_GID: u64 = 13;
const AT_EGID: u64 = 14;
const AT_SECURE: u64 = 23;
const AT_RANDOM: u64 = 25;
const AT_EXECFN: u64 = 31;

pub struct Image {
    pub space: AddressSpace,
    pub entry: u64,
    pub stack: u64,
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// Bytes copied per step when filling segments and reading relocations.
const CHUNK: usize = 64 * 1024;
/// Largest program header table accepted (Linux's limit as well).
const MAX_PHDRS_SIZE: usize = 64 * 1024;
/// How much of a script's first line is looked at (Linux's BINPRM_BUF_SIZE).
const SHEBANG_MAX: usize = 256;

/// An executable, read on demand.
struct ElfFile {
    inode: Arc<dyn Inode>,
    size: u64,
}

impl ElfFile {
    fn open(path: &str) -> KResult<Self> {
        let inode = crate::vfs::lookup(path)?;
        let meta = inode.metadata()?;
        if meta.kind == FileType::Directory {
            return Err(EACCES);
        }
        Ok(Self {
            inode,
            size: meta.size,
        })
    }

    /// Up to `len` bytes at `off` (fewer at the end of the file).
    fn read(&self, off: u64, len: usize) -> KResult<Vec<u8>> {
        let len = len.min(self.size.saturating_sub(off) as usize);
        let mut buf: Vec<u8> = Vec::new();
        buf.try_reserve_exact(len).map_err(|_| ENOMEM)?;
        // memset rather than `resize`, which fills byte by byte in debug
        // builds (half a second per exec of a 500 KB program under QEMU).
        // SAFETY: the capacity is at least `len` and every byte is written.
        unsafe {
            core::ptr::write_bytes(buf.as_mut_ptr(), 0, len);
            buf.set_len(len);
        }
        let mut done = 0;
        while done < len {
            let n = self.inode.read_at(off + done as u64, &mut buf[done..])?;
            if n == 0 {
                break;
            }
            done += n;
        }
        buf.truncate(done);
        Ok(buf)
    }

    /// Exactly `len` bytes at `off`, or ENOEXEC.
    fn read_exact(&self, off: u64, len: usize) -> KResult<Vec<u8>> {
        let b = self.read(off, len)?;
        if b.len() != len {
            return Err(ENOEXEC);
        }
        Ok(b)
    }

    /// Copy `len` bytes at file offset `off` to `addr` in `space`.
    fn copy_to(&self, space: &mut AddressSpace, addr: u64, off: u64, len: u64) -> KResult<()> {
        with_writable(space, addr, len, |space| {
            let mut done = 0u64;
            while done < len {
                let n = (len - done).min(CHUNK as u64) as usize;
                let chunk = self.read_exact(off + done, n)?;
                space.write_bytes(addr + done, &chunk)?;
                done += n as u64;
            }
            Ok(())
        })
    }
}

struct Phdr {
    kind: u32,
    flags: u32,
    offset: u64,
    vaddr: u64,
    filesz: u64,
    memsz: u64,
}

pub fn load(path: &str, argv: &[String], envp: &[String]) -> KResult<Image> {
    let file = ElfFile::open(path)?;
    let data = file.read(0, SHEBANG_MAX)?;
    // "#!" scripts: run the interpreter with the script as an argument.
    if data.starts_with(b"#!") {
        let line_end = data.iter().position(|&b| b == b'\n').unwrap_or(data.len());
        let line = core::str::from_utf8(&data[2..line_end])
            .map_err(|_| ENOEXEC)?
            .trim();
        let mut parts = line.splitn(2, ' ');
        let interp = parts.next().ok_or(ENOEXEC)?;
        let mut new_argv: Vec<String> = alloc::vec![String::from(interp)];
        if let Some(arg) = parts.next() {
            new_argv.push(String::from(arg.trim()));
        }
        new_argv.push(String::from(path));
        new_argv.extend(argv.iter().skip(1).cloned());
        return load(interp, &new_argv, envp);
    }
    let (etype, entry0, phoff, phnum, phdrs) = parse_elf(&file)?;
    let bias = if etype == ET_DYN { PIE_BIAS } else { 0 };
    let entry = entry0 + bias;
    let interp = match phdrs.iter().find(|p| p.kind == PT_INTERP) {
        Some(p) => {
            let raw = file.read(p.offset, (p.filesz as usize).min(4096))?;
            let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
            Some(String::from_utf8_lossy(&raw[..end]).into_owned())
        }
        None => None,
    };

    let mut space = AddressSpace::new()?;
    let (max_end, mut phdr_addr) = map_segments(&mut space, &file, &phdrs, bias, phoff)?;
    if interp.is_some() && phdr_addr == 0 {
        return Err(ENOEXEC); // ld.so needs the program headers
    }
    let (run_entry, at_base) = match &interp {
        // Dynamically linked: ld.so relocates the program.
        Some(ipath) => {
            let ifile = ElfFile::open(ipath).map_err(|_| ENOENT)?;
            let (itype, ientry, iphoff, _, iphdrs) = parse_elf(&ifile)?;
            if iphdrs.iter().any(|p| p.kind == PT_INTERP) {
                return Err(ELIBBAD);
            }
            let ibias = if itype == ET_DYN { INTERP_BIAS } else { 0 };
            map_segments(&mut space, &ifile, &iphdrs, ibias, iphoff)?;
            if ibias != 0
                && let Some(dynph) = iphdrs.iter().find(|p| p.kind == PT_DYNAMIC)
            {
                apply_relocations(&mut space, &ifile, &iphdrs, dynph, ibias)?;
            }
            (ientry + ibias, ibias)
        }
        None => {
            // Apply relative relocations for static PIE.
            if bias != 0
                && let Some(dynph) = phdrs.iter().find(|p| p.kind == PT_DYNAMIC)
            {
                apply_relocations(&mut space, &file, &phdrs, dynph, bias)?;
            }
            (entry, 0)
        }
    };
    if phdr_addr < FRAME_SIZE {
        phdr_addr = 0;
    }

    space.brk_start = max_end + FRAME_SIZE;
    space.brk = space.brk_start;
    space.add_area(Area {
        start: space.brk_start,
        end: space.brk_start + FRAME_SIZE,
        prot: PROT_READ | PROT_WRITE,
        flags: vm::MAP_PRIVATE,
        backing: Backing::Anon,
        name: "[heap]",
    })?;

    // Stack.
    let stack_prot = if phdrs
        .iter()
        .any(|p| p.kind == PT_GNU_STACK && p.flags & 1 != 0)
    {
        PROT_READ | PROT_WRITE | PROT_EXEC
    } else {
        PROT_READ | PROT_WRITE
    };
    space.add_area(Area {
        start: vm::STACK_TOP - vm::STACK_SIZE,
        end: vm::STACK_TOP,
        prot: stack_prot,
        flags: vm::MAP_PRIVATE,
        backing: Backing::Anon,
        name: "[stack]",
    })?;
    let aux = AuxInfo {
        entry,
        phdr: phdr_addr,
        phnum: phnum as u64,
        base: at_base,
    };
    let stack = build_stack(&mut space, argv, envp, path, &aux)?;
    Ok(Image {
        space,
        entry: run_entry,
        stack,
    })
}

type ElfInfo = (u16, u64, usize, usize, Vec<Phdr>);

/// Check the ELF header; returns (type, entry, phoff, phnum, program headers).
fn parse_elf(file: &ElfFile) -> KResult<ElfInfo> {
    let data = file.read(0, 64)?;
    if data.len() < 64 || &data[..4] != b"\x7fELF" || data[4] != 2 || data[5] != 1 {
        return Err(ENOEXEC);
    }
    let etype = u16_at(&data, 16);
    if u16_at(&data, 18) != EM_X86_64 || (etype != ET_EXEC && etype != ET_DYN) {
        return Err(ENOEXEC);
    }
    let entry = u64_at(&data, 24);
    let phoff = u64_at(&data, 32) as usize;
    let phentsize = u16_at(&data, 54) as usize;
    let phnum = u16_at(&data, 56) as usize;
    if phentsize < 56 || phentsize * phnum > MAX_PHDRS_SIZE {
        return Err(ENOEXEC);
    }
    let table = file.read_exact(phoff as u64, phentsize * phnum)?;
    let phdrs: Vec<Phdr> = (0..phnum)
        .map(|i| {
            let o = i * phentsize;
            Phdr {
                kind: u32_at(&table, o),
                flags: u32_at(&table, o + 4),
                offset: u64_at(&table, o + 8),
                vaddr: u64_at(&table, o + 16),
                filesz: u64_at(&table, o + 32),
                memsz: u64_at(&table, o + 40),
            }
        })
        .collect();
    Ok((etype, entry, phoff, phnum, phdrs))
}

/// Map and fill every PT_LOAD segment at `bias`. Returns the end of the
/// highest segment and the run-time address of the program headers.
fn map_segments(
    space: &mut AddressSpace,
    file: &ElfFile,
    phdrs: &[Phdr],
    bias: u64,
    phoff: usize,
) -> KResult<(u64, u64)> {
    let mut max_end = 0u64;
    let mut phdr_addr = 0u64;
    for ph in phdrs.iter() {
        if ph.kind == PT_PHDR {
            phdr_addr = ph.vaddr + bias;
        }
        if ph.kind != PT_LOAD || ph.memsz == 0 {
            continue;
        }
        if ph
            .offset
            .checked_add(ph.filesz)
            .is_none_or(|e| e > file.size)
            || ph.filesz > ph.memsz
        {
            return Err(ENOEXEC);
        }
        let vaddr = ph.vaddr + bias;
        let start = vaddr & !(FRAME_SIZE - 1);
        let end = (vaddr + ph.memsz).next_multiple_of(FRAME_SIZE);
        if end > vm::MMAP_TOP || start < FRAME_SIZE {
            return Err(ENOEXEC);
        }
        let mut prot = 0;
        if ph.flags & 4 != 0 {
            prot |= PROT_READ;
        }
        if ph.flags & 2 != 0 {
            prot |= PROT_WRITE;
        }
        if ph.flags & 1 != 0 {
            prot |= PROT_EXEC;
        }
        // Segments may share a boundary page: merge by widening permissions.
        let mut s = start;
        while s < end {
            if let Some(a) = space.find_area(s).cloned() {
                let merged = a.prot | prot;
                if merged != a.prot {
                    space.protect(a.start, a.end, merged)?;
                }
                s = a.end;
                continue;
            }
            let next = space
                .areas
                .range(s..)
                .next()
                .map(|(k, _)| *k)
                .unwrap_or(end)
                .min(end);
            space.add_area(Area {
                start: s,
                end: next,
                prot,
                flags: vm::MAP_PRIVATE,
                backing: Backing::Anon,
                name: "[elf]",
            })?;
            s = next;
        }
        // Temporarily writable while copying.
        file.copy_to(space, vaddr, ph.offset, ph.filesz)?;
        if phdr_addr == 0 && ph.offset == 0 {
            phdr_addr = vaddr + phoff as u64;
        }
        max_end = max_end.max(end);
    }
    if max_end == 0 {
        return Err(ENOEXEC);
    }
    Ok((max_end, phdr_addr))
}

/// Write into possibly read-only areas during loading.
fn write_forced(space: &mut AddressSpace, addr: u64, data: &[u8]) -> KResult<()> {
    with_writable(space, addr, data.len() as u64, |space| {
        space.write_bytes(addr, data)
    })
}

/// Run `f` with the areas covering `addr..addr + len` made writable, then
/// restore their permissions.
fn with_writable(
    space: &mut AddressSpace,
    addr: u64,
    len: u64,
    f: impl FnOnce(&mut AddressSpace) -> KResult<()>,
) -> KResult<()> {
    if len == 0 {
        return Ok(());
    }
    let start = addr & !(FRAME_SIZE - 1);
    let end = (addr + len).next_multiple_of(FRAME_SIZE);
    let saved: Vec<(u64, u64, u32)> = space
        .areas
        .values()
        .filter(|a| a.start < end && start < a.end && a.prot & PROT_WRITE == 0)
        .map(|a| (a.start, a.end, a.prot))
        .collect();
    for &(s, e, p) in &saved {
        space.protect(s, e, p | PROT_WRITE)?;
    }
    let r = f(space);
    for &(s, e, p) in &saved {
        space.protect(s, e, p)?;
    }
    r
}

fn apply_relocations(
    space: &mut AddressSpace,
    file: &ElfFile,
    phdrs: &[Phdr],
    dynph: &Phdr,
    bias: u64,
) -> KResult<()> {
    const DT_NULL: u64 = 0;
    const DT_RELA: u64 = 7;
    const DT_RELASZ: u64 = 8;
    const DT_RELAENT: u64 = 9;
    if dynph.filesz > CHUNK as u64 {
        return Err(ENOEXEC);
    }
    let dyn_data = file.read_exact(dynph.offset, dynph.filesz as usize)?;
    let (mut rela, mut relasz, mut relaent) = (0u64, 0u64, 24u64);
    for e in dyn_data.chunks_exact(16) {
        let tag = u64_at(e, 0);
        let val = u64_at(e, 8);
        match tag {
            DT_NULL => break,
            DT_RELA => rela = val,
            DT_RELASZ => relasz = val,
            DT_RELAENT => relaent = val,
            _ => {}
        }
    }
    if rela == 0 || relasz == 0 {
        return Ok(());
    }
    if relaent < 24 || relaent as usize > CHUNK {
        return Err(ENOEXEC);
    }
    // DT_RELA is a virtual address; find its file offset through PT_LOADs.
    let file_off = vaddr_to_offset(phdrs, rela).ok_or(ENOEXEC)?;
    let per_chunk = (CHUNK as u64 / relaent) * relaent;
    let mut done = 0u64;
    while done < relasz {
        let n = (relasz - done).min(per_chunk);
        let table = file.read_exact(file_off + done, n as usize)?;
        done += n;
        apply_relative(space, &table, relaent as usize, bias)?;
    }
    Ok(())
}

/// Apply the R_X86_64_RELATIVE entries of a slice of a RELA table.
fn apply_relative(
    space: &mut AddressSpace,
    table: &[u8],
    relaent: usize,
    bias: u64,
) -> KResult<()> {
    const R_X86_64_RELATIVE: u32 = 8;
    for r in table.chunks_exact(relaent) {
        let offset = u64_at(r, 0);
        let info = u64_at(r, 8);
        let addend = u64_at(r, 16);
        // Only relative relocations: they make ld-rustos (a static PIE)
        // runnable. Interpreters that relocate themselves (musl's libc.so)
        // redo these (idempotently, RELA addends) and handle the rest.
        if info as u32 == R_X86_64_RELATIVE {
            let v = bias.wrapping_add(addend);
            write_forced(space, bias + offset, &v.to_le_bytes())?;
        }
    }
    Ok(())
}

fn vaddr_to_offset(phdrs: &[Phdr], vaddr: u64) -> Option<u64> {
    phdrs
        .iter()
        .filter(|p| p.kind == PT_LOAD)
        .find(|p| vaddr >= p.vaddr && vaddr - p.vaddr < p.filesz)
        .map(|p| p.offset + (vaddr - p.vaddr))
}

/// Program description passed to the new process in the aux vector.
struct AuxInfo {
    entry: u64,
    phdr: u64,
    phnum: u64,
    base: u64,
}

fn build_stack(
    space: &mut AddressSpace,
    argv: &[String],
    envp: &[String],
    path: &str,
    aux_info: &AuxInfo,
) -> KResult<u64> {
    let mut sp = vm::STACK_TOP;
    let push_bytes = |space: &mut AddressSpace, sp: &mut u64, b: &[u8]| -> KResult<u64> {
        *sp -= b.len() as u64;
        space.write_bytes(*sp, b)?;
        Ok(*sp)
    };
    let str_ptrs = |space: &mut AddressSpace, sp: &mut u64, v: &[String]| -> KResult<Vec<u64>> {
        let mut out = Vec::new();
        for s in v {
            let mut b = s.as_bytes().to_vec();
            b.push(0);
            out.push(push_bytes(space, sp, &b)?);
        }
        Ok(out)
    };
    let argv_p = str_ptrs(space, &mut sp, argv)?;
    let envp_p = str_ptrs(space, &mut sp, envp)?;
    let mut execfn = path.as_bytes().to_vec();
    execfn.push(0);
    sp -= execfn.len() as u64;
    space.write_bytes(sp, &execfn)?;
    let execfn_p = sp;
    // 16 random bytes for AT_RANDOM.
    let mut rnd = [0u8; 16];
    crate::drivers::random::fill(&mut rnd);
    sp -= 16;
    space.write_bytes(sp, &rnd)?;
    let rnd_p = sp;

    let aux: [(u64, u64); 13] = [
        (AT_PHDR, aux_info.phdr),
        (AT_PHENT, 56),
        (AT_PHNUM, aux_info.phnum),
        (AT_PAGESZ, 4096),
        (AT_BASE, aux_info.base),
        (AT_ENTRY, aux_info.entry),
        (AT_UID, 0),
        (AT_EUID, 0),
        (AT_GID, 0),
        (AT_EGID, 0),
        (AT_SECURE, 0),
        (AT_RANDOM, rnd_p),
        (AT_EXECFN, execfn_p),
    ];
    let words = 1 + argv_p.len() + 1 + envp_p.len() + 1 + (aux.len() + 1) * 2;
    sp &= !0xF;
    if words % 2 == 1 {
        sp -= 8;
    }
    let mut vals: Vec<u64> = Vec::with_capacity(words);
    vals.push(argv_p.len() as u64);
    vals.extend(argv_p.iter());
    vals.push(0);
    vals.extend(envp_p.iter());
    vals.push(0);
    for (k, v) in aux.iter() {
        vals.push(*k);
        vals.push(*v);
    }
    vals.push(AT_NULL);
    vals.push(0);
    sp -= (vals.len() * 8) as u64;
    let bytes: Vec<u8> = vals.iter().flat_map(|v| v.to_le_bytes()).collect();
    space.write_bytes(sp, &bytes)?;
    Ok(sp)
}
