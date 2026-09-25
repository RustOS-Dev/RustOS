//! ELF64 program loader.
//!
//! Loads static executables (ET_EXEC) and static position-independent
//! executables (ET_DYN, relocated by a fixed bias with R_X86_64_RELATIVE
//! relocations applied). PT_LOAD segments are copied into anonymous memory
//! with their requested permissions. The initial stack follows the System V
//! ABI: argc, argv, envp, auxv.

use super::vm::{self, AddressSpace, Area, Backing, PROT_EXEC, PROT_READ, PROT_WRITE};
use crate::errno::*;
use crate::mm::FRAME_SIZE;
use alloc::string::String;
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

struct Phdr {
    kind: u32,
    flags: u32,
    offset: u64,
    vaddr: u64,
    filesz: u64,
    memsz: u64,
}

pub fn load(path: &str, argv: &[String], envp: &[String]) -> KResult<Image> {
    let data = crate::vfs::read_all(path)?;
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
    if data.len() < 64 || &data[..4] != b"\x7fELF" || data[4] != 2 || data[5] != 1 {
        return Err(ENOEXEC);
    }
    let etype = u16_at(&data, 16);
    if u16_at(&data, 18) != EM_X86_64 || (etype != ET_EXEC && etype != ET_DYN) {
        return Err(ENOEXEC);
    }
    let bias = if etype == ET_DYN { PIE_BIAS } else { 0 };
    let entry = u64_at(&data, 24) + bias;
    let phoff = u64_at(&data, 32) as usize;
    let phentsize = u16_at(&data, 54) as usize;
    let phnum = u16_at(&data, 56) as usize;
    if phentsize < 56 || phoff + phentsize * phnum > data.len() {
        return Err(ENOEXEC);
    }
    let phdrs: Vec<Phdr> = (0..phnum)
        .map(|i| {
            let o = phoff + i * phentsize;
            Phdr {
                kind: u32_at(&data, o),
                flags: u32_at(&data, o + 4),
                offset: u64_at(&data, o + 8),
                vaddr: u64_at(&data, o + 16),
                filesz: u64_at(&data, o + 32),
                memsz: u64_at(&data, o + 40),
            }
        })
        .collect();
    if phdrs.iter().any(|p| p.kind == PT_INTERP) {
        // Dynamically linked programs need ld.so, which RustOS does not ship.
        return Err(ENOEXEC);
    }

    let mut space = AddressSpace::new()?;
    let mut max_end = 0u64;
    let mut phdr_addr = 0u64;
    for ph in phdrs.iter() {
        if ph.kind == PT_PHDR {
            phdr_addr = ph.vaddr + bias;
        }
        if ph.kind != PT_LOAD || ph.memsz == 0 {
            continue;
        }
        if ph.offset + ph.filesz > data.len() as u64 || ph.filesz > ph.memsz {
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
        let file = &data[ph.offset as usize..(ph.offset + ph.filesz) as usize];
        write_forced(&mut space, vaddr, file)?;
        if phdr_addr == 0 && ph.offset == 0 {
            phdr_addr = vaddr + phoff as u64;
        }
        max_end = max_end.max(end);
    }
    if max_end == 0 {
        return Err(ENOEXEC);
    }

    // Apply relative relocations for static PIE.
    if bias != 0
        && let Some(dynph) = phdrs.iter().find(|p| p.kind == PT_DYNAMIC)
    {
        apply_relocations(&mut space, &data, dynph, bias)?;
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
    let stack = build_stack(&mut space, argv, envp, path, entry, phdr_addr, phnum as u64)?;
    Ok(Image {
        space,
        entry,
        stack,
    })
}

/// Write into possibly read-only areas during loading.
fn write_forced(space: &mut AddressSpace, addr: u64, data: &[u8]) -> KResult<()> {
    if data.is_empty() {
        return Ok(());
    }
    let start = addr & !(FRAME_SIZE - 1);
    let end = (addr + data.len() as u64).next_multiple_of(FRAME_SIZE);
    let saved: Vec<(u64, u64, u32)> = space
        .areas
        .values()
        .filter(|a| a.start < end && start < a.end && a.prot & PROT_WRITE == 0)
        .map(|a| (a.start, a.end, a.prot))
        .collect();
    for &(s, e, p) in &saved {
        space.protect(s, e, p | PROT_WRITE)?;
    }
    let r = space.write_bytes(addr, data);
    for &(s, e, p) in &saved {
        space.protect(s, e, p)?;
    }
    r
}

fn apply_relocations(
    space: &mut AddressSpace,
    data: &[u8],
    dynph: &Phdr,
    bias: u64,
) -> KResult<()> {
    const DT_NULL: u64 = 0;
    const DT_RELA: u64 = 7;
    const DT_RELASZ: u64 = 8;
    const DT_RELAENT: u64 = 9;
    const R_X86_64_RELATIVE: u32 = 8;
    let dyn_data = data
        .get(dynph.offset as usize..(dynph.offset + dynph.filesz) as usize)
        .ok_or(ENOEXEC)?;
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
    // DT_RELA is a virtual address; find its file offset through PT_LOADs.
    let file_off = vaddr_to_offset(data, rela).ok_or(ENOEXEC)?;
    let table = data
        .get(file_off as usize..(file_off + relasz) as usize)
        .ok_or(ENOEXEC)?;
    for r in table.chunks_exact(relaent as usize) {
        let offset = u64_at(r, 0);
        let info = u64_at(r, 8);
        let addend = u64_at(r, 16);
        if info as u32 == R_X86_64_RELATIVE {
            let v = bias.wrapping_add(addend);
            write_forced(space, bias + offset, &v.to_le_bytes())?;
        } else {
            return Err(ENOEXEC);
        }
    }
    Ok(())
}

fn vaddr_to_offset(data: &[u8], vaddr: u64) -> Option<u64> {
    let phoff = u64_at(data, 32) as usize;
    let phentsize = u16_at(data, 54) as usize;
    let phnum = u16_at(data, 56) as usize;
    for i in 0..phnum {
        let o = phoff + i * phentsize;
        if u32_at(data, o) != PT_LOAD {
            continue;
        }
        let off = u64_at(data, o + 8);
        let va = u64_at(data, o + 16);
        let filesz = u64_at(data, o + 32);
        if vaddr >= va && vaddr < va + filesz {
            return Some(off + (vaddr - va));
        }
    }
    None
}

fn build_stack(
    space: &mut AddressSpace,
    argv: &[String],
    envp: &[String],
    path: &str,
    entry: u64,
    phdr: u64,
    phnum: u64,
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
        (AT_PHDR, phdr),
        (AT_PHENT, 56),
        (AT_PHNUM, phnum),
        (AT_PAGESZ, 4096),
        (AT_BASE, 0),
        (AT_ENTRY, entry),
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
