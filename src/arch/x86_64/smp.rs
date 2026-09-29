//! Application-processor (AP) bring-up and cross-CPU operations.
//!
//! Each AP is started with INIT-SIPI-SIPI at a trampoline copied into a
//! page below 1 MiB. The trampoline switches real mode → protected mode →
//! long mode on temporary page tables (the kernel's upper-level entries plus
//! an identity map of the first 2 MiB), then jumps to [`ap_entry`] on a
//! freshly allocated kernel stack. The AP loads the kernel page tables,
//! installs its own GDT/TSS/per-CPU block, enables its local APIC timer and
//! joins the shared run queue.
//!
//! Also here: TLB shootdown, reschedule kicks and halting other CPUs.

use super::{apic, cpu, gdt, idt};
use crate::mm;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use x86_64::registers::control::{Cr0, Cr3, Cr4};
use x86_64::registers::model_specific::Msr;

// Offsets of the parameter block at the end of the trampoline page.
const P_GDTR: usize = 0xF00; // limit u16, base u32
const P_FAR32: usize = 0xF08; // offset u32, selector u16
const P_FAR64: usize = 0xF10; // offset u32, selector u16
const P_CR3: usize = 0xF18; // temporary PML4 (below 4 GiB)
const P_CR4: usize = 0xF1C;
const P_STACK: usize = 0xF20;
const P_ENTRY: usize = 0xF28;
const P_CPU: usize = 0xF30;
const P_KCR3: usize = 0xF38;
const P_GDT: usize = 0xF40; // 4 descriptors

core::arch::global_asm!(
    ".section .text.ap_trampoline, \"ax\"",
    ".global ap_trampoline_start",
    ".global ap_trampoline_end",
    ".code16",
    "ap_trampoline_start:",
    "    cli",
    "    cld",
    "    movw %cs, %ax",
    "    movw %ax, %ds",
    "    movzwl %ax, %esi",
    "    shll $4, %esi", // esi = trampoline physical base
    "    lgdtl 0xF00",
    "    movl %cr0, %eax",
    "    orl $1, %eax",
    "    movl %eax, %cr0",
    "    ljmpl *0xF08",
    ".code32",
    ".global ap_trampoline_pm32",
    "ap_trampoline_pm32:",
    "    movw $0x10, %ax",
    "    movw %ax, %ds",
    "    movw %ax, %es",
    "    movw %ax, %ss",
    "    movl 0xF1C(%esi), %eax", // CR4 as on the BSP (PAE set)
    "    orl $0x20, %eax",
    "    movl %eax, %cr4",
    "    movl 0xF18(%esi), %eax",
    "    movl %eax, %cr3",
    "    movl $0xC0000080, %ecx", // EFER: LME | NXE | SCE
    "    rdmsr",
    "    orl $0x901, %eax",
    "    wrmsr",
    "    movl %cr0, %eax",
    "    orl $0x80010001, %eax", // PG | WP | PE
    "    movl %eax, %cr0",
    "    ljmpl *0xF10(%esi)",
    ".code64",
    ".global ap_trampoline_lm64",
    "ap_trampoline_lm64:",
    "    movl %esi, %esi",
    "    movq 0xF20(%rsi), %rsp",
    "    movq 0xF30(%rsi), %rdi", // arg 0: CPU index
    "    movq 0xF28(%rsi), %rax", // ap_entry (runtime address)
    "    movq 0xF38(%rsi), %rsi", // arg 1: kernel CR3
    "    xorq %rbp, %rbp",
    "    pushq $0",
    "    jmpq *%rax",
    "ap_trampoline_end:",
    ".text",
    options(att_syntax)
);

unsafe extern "C" {
    static ap_trampoline_start: u8;
    static ap_trampoline_end: u8;
    static ap_trampoline_pm32: u8;
    static ap_trampoline_lm64: u8;
}

static AP_STARTED: AtomicBool = AtomicBool::new(false);
static ONLINE: AtomicU32 = AtomicU32::new(1);
static BSP_PAT: AtomicU64 = AtomicU64::new(0);

/// Number of CPUs running the scheduler.
pub fn online() -> u32 {
    ONLINE.load(Ordering::SeqCst)
}

fn wr<T: Copy>(base_virt: u64, off: usize, v: T) {
    unsafe { core::ptr::write_unaligned((base_virt + off as u64) as *mut T, v) }
}

/// Build temporary page tables below 4 GiB: the kernel's PML4 with the
/// first 2 MiB identity mapped. Returns the PML4 physical address.
fn temp_page_tables() -> Option<u64> {
    let kernel = mm::KERNEL_PML4.load(Ordering::SeqCst);
    let (pml4, pdpt, pd) = mm::with_frames(|f| {
        Some((
            f.alloc_contiguous(1, 4096, 1 << 32)?,
            f.alloc_contiguous(1, 4096, 1 << 32)?,
            f.alloc_contiguous(1, 4096, 1 << 32)?,
        ))
    })?;
    unsafe {
        let src = mm::phys_ptr::<u64>(kernel);
        let dst = mm::phys_ptr::<u64>(pml4);
        core::ptr::copy_nonoverlapping(src, dst, 512);
        // Keep whatever the kernel maps under PML4[0] / PDPT[0] except the
        // first 2 MiB, which becomes an identity map.
        let e0 = *src;
        let pdpt_ptr = mm::phys_ptr::<u64>(pdpt);
        core::ptr::write_bytes(pdpt_ptr, 0, 512);
        if e0 & 1 != 0 {
            core::ptr::copy_nonoverlapping(
                mm::phys_ptr::<u64>(e0 & 0x000F_FFFF_FFFF_F000),
                pdpt_ptr,
                512,
            );
        }
        let pd_ptr = mm::phys_ptr::<u64>(pd);
        core::ptr::write_bytes(pd_ptr, 0, 512);
        let old_pdpt0 = *pdpt_ptr;
        if old_pdpt0 & 1 != 0 && old_pdpt0 & 0x80 == 0 {
            core::ptr::copy_nonoverlapping(
                mm::phys_ptr::<u64>(old_pdpt0 & 0x000F_FFFF_FFFF_F000),
                pd_ptr,
                512,
            );
        }
        *pd_ptr = 0x83; // 2 MiB page at 0: present, writable, huge
        *pdpt_ptr = pd | 0x3;
        *dst = pdpt | 0x3;
    }
    Some(pml4)
}

/// Start every processor listed in the MADT.
pub fn start_aps() {
    if option_env!("RUSTOS_NOSMP").is_some() {
        return;
    }
    let cpus = super::acpi::platform().cpus.clone();
    if cpus.len() <= 1 {
        return;
    }
    let bsp = apic::id();
    let tramp_len =
        ((&raw const ap_trampoline_end as u64) - (&raw const ap_trampoline_start as u64)) as usize;
    let Some(page) = mm::alloc_low_frame() else {
        crate::println!("[smp] no page below 1 MiB for the AP trampoline; staying uniprocessor");
        return;
    };
    let Some(pml4) = temp_page_tables() else {
        crate::println!("[smp] cannot build AP page tables");
        return;
    };
    let virt = mm::phys_to_virt(page);
    unsafe {
        core::ptr::copy_nonoverlapping(&raw const ap_trampoline_start, virt as *mut u8, tramp_len);
    }
    let off = |sym: *const u8| unsafe { sym.offset_from(&raw const ap_trampoline_start) } as u32;
    // Temporary GDT: null, 32-bit code, data, 64-bit code.
    wr(virt, P_GDT, 0u64);
    wr(virt, P_GDT + 8, 0x00CF_9A00_0000_FFFFu64);
    wr(virt, P_GDT + 16, 0x00CF_9200_0000_FFFFu64);
    wr(virt, P_GDT + 24, 0x00AF_9A00_0000_FFFFu64);
    wr(virt, P_GDTR, 31u16);
    wr(virt, P_GDTR + 2, page as u32 + P_GDT as u32);
    wr(
        virt,
        P_FAR32,
        page as u32 + off(&raw const ap_trampoline_pm32),
    );
    wr(virt, P_FAR32 + 4, 0x08u16);
    wr(
        virt,
        P_FAR64,
        page as u32 + off(&raw const ap_trampoline_lm64),
    );
    wr(virt, P_FAR64 + 4, 0x18u16);
    wr(virt, P_CR3, pml4 as u32);
    wr(virt, P_CR4, Cr4::read_raw() as u32 & !(1 << 17)); // no PCIDE before long mode
    wr(virt, P_KCR3, Cr3::read_raw().0.start_address().as_u64());
    BSP_PAT.store(unsafe { Msr::new(0x277).read() }, Ordering::SeqCst);

    let mut next_id = 1u32;
    for &lapic in cpus.iter().filter(|&&id| id != bsp) {
        if next_id as usize >= cpu::MAX_CPUS {
            break;
        }
        let Some(stack) = mm::KernelStack::new(64 * 1024) else {
            break;
        };
        wr(virt, P_STACK, stack.top() - 16);
        core::mem::forget(stack);
        wr(virt, P_ENTRY, ap_entry as *const () as u64);
        wr(virt, P_CPU, next_id as u64);
        AP_STARTED.store(false, Ordering::SeqCst);
        core::sync::atomic::fence(Ordering::SeqCst);
        apic::send_init(lapic);
        crate::time::delay_us(10_000);
        let vector = (page >> 12) as u8;
        apic::send_sipi(lapic, vector);
        // Answer shootdowns from already-running APs while waiting (the
        // BSP still has interrupts off here).
        let started = || {
            tlb_service();
            AP_STARTED.load(Ordering::SeqCst)
        };
        let mut ok = crate::time::wait_until(5, started);
        if !ok {
            apic::send_sipi(lapic, vector);
            ok = crate::time::wait_until(200, started);
        }
        if ok {
            next_id += 1;
        } else {
            crate::println!("[smp] CPU with APIC id {} did not start", lapic);
        }
    }
    crate::println!(
        "[smp] {} CPUs online ({})",
        online(),
        if apic::is_x2apic() { "x2APIC" } else { "xAPIC" }
    );
}

/// First Rust code on an AP (long mode, kernel page tables, own stack).
extern "C" fn ap_entry(cpu_id: u64, kernel_cr3: u64) -> ! {
    unsafe {
        // Leave the temporary tables (this code is mapped in both).
        core::arch::asm!("mov cr3, {}", in(reg) kernel_cr3, options(nostack));
        // Same control-register setup as the BSP.
        Msr::new(0x277).write(BSP_PAT.load(Ordering::SeqCst));
        Cr0::write_raw(Cr0::read_raw() | (1 << 16));
    }
    idt::load();
    let tss = gdt::init_cpu();
    apic::init_local();
    cpu::init(cpu_id as u32, apic::id(), tss);
    super::syscall_entry::init();
    crate::sched::init_cpu(&alloc::format!("cpu{}", cpu_id));
    crate::time::start_tick();
    x86_64::instructions::interrupts::enable();
    ONLINE.fetch_add(1, Ordering::SeqCst);
    AP_STARTED.store(true, Ordering::SeqCst);
    crate::sched::exit_current();
}

// ---------------------------------------------------------------------------
// Inter-processor interrupts
// ---------------------------------------------------------------------------

static TLB_LOCK: crate::sync::Mutex<()> = crate::sync::Mutex::new(());
static TLB_TARGET: AtomicU64 = AtomicU64::new(0);
static TLB_PENDING: AtomicU32 = AtomicU32::new(0);
/// Shootdown generation; each CPU acknowledges every generation once.
static TLB_GEN: AtomicU64 = AtomicU64::new(0);
static TLB_SEEN: [AtomicU64; cpu::MAX_CPUS] = [const { AtomicU64::new(0) }; cpu::MAX_CPUS];

/// Debug: ask the next NMI to print where the CPU was.
pub static NMI_DUMP: AtomicBool = AtomicBool::new(false);

pub fn init_ipis() {
    idt::register(idt::VEC_TLB_IPI, tlb_ipi);
    idt::register(idt::VEC_RESCHED_IPI, resched_ipi);
    idt::register(idt::VEC_HALT_IPI, halt_ipi);
}

/// Acknowledge an outstanding shootdown request (from the IPI, or while
/// spinning so two CPUs shooting down at once cannot deadlock).
fn tlb_service() {
    let me = cpu::this().cpu_id as usize;
    let generation = TLB_GEN.load(Ordering::SeqCst);
    if TLB_SEEN[me].load(Ordering::SeqCst) >= generation {
        return;
    }
    TLB_SEEN[me].store(generation, Ordering::SeqCst);
    let target = TLB_TARGET.load(Ordering::SeqCst);
    let (cur, _) = Cr3::read_raw();
    if target == 0 || cur.start_address().as_u64() == target {
        x86_64::instructions::tlb::flush_all();
    }
    TLB_PENDING.fetch_sub(1, Ordering::SeqCst);
}

fn tlb_ipi(_f: &mut idt::TrapFrame) {
    tlb_service();
    apic::eoi();
}

fn resched_ipi(_f: &mut idt::TrapFrame) {
    cpu::this().need_resched.store(1, Ordering::SeqCst);
    apic::eoi();
}

fn halt_ipi(_f: &mut idt::TrapFrame) {
    x86_64::instructions::interrupts::disable();
    loop {
        x86_64::instructions::hlt();
    }
}

/// Answer a pending shootdown from code that spins with interrupts off.
#[inline]
pub fn poll() {
    if ONLINE.load(Ordering::Relaxed) > 1
        && TLB_PENDING.load(Ordering::Relaxed) != 0
        && cpu::is_initialized()
    {
        tlb_service();
    }
}

/// Make every other CPU drop TLB entries for address space `pml4` (0 = all
/// address spaces, for kernel mappings). The caller has already flushed
/// its own TLB.
pub fn tlb_shootdown(pml4: u64) {
    // Stay on this CPU for the whole request (and flush it here too): a
    // thread that migrated after its own flush would mark the wrong CPU as
    // done, and neither CPU would flush.
    x86_64::instructions::interrupts::without_interrupts(|| {
        let (cur, _) = Cr3::read_raw();
        if pml4 == 0 || cur.start_address().as_u64() == pml4 {
            x86_64::instructions::tlb::flush_all();
        }
        tlb_shootdown_others(pml4)
    })
}

fn tlb_shootdown_others(pml4: u64) {
    let n = online();
    if n <= 1 || !cpu::is_initialized() {
        return;
    }
    let _g = loop {
        if let Some(g) = TLB_LOCK.try_lock() {
            break g;
        }
        tlb_service();
        core::hint::spin_loop();
    };
    let me = cpu::this().cpu_id as usize;
    TLB_TARGET.store(pml4, Ordering::SeqCst);
    TLB_PENDING.store(n - 1, Ordering::SeqCst);
    let generation = TLB_GEN.load(Ordering::SeqCst) + 1;
    TLB_SEEN[me].store(generation, Ordering::SeqCst);
    TLB_GEN.store(generation, Ordering::SeqCst);
    apic::send_ipi_all_but_self(idt::VEC_TLB_IPI);
    // Bounded wait: a CPU spinning with interrupts off cannot answer.
    let t = crate::time::Deadline::after_ms(500);
    // A CPU that never answers is halted or wedged: stop waiting.
    let giveup = crate::time::Deadline::after_ms(10_000);
    let mut warned = false;
    let start = crate::time::nanos();
    let mut samples = 0u32;
    while TLB_PENDING.load(Ordering::SeqCst) != 0 {
        if option_env!("RUSTOS_SMP_DEBUG").is_some() {
            let ms = (crate::time::nanos() - start) / 1_000_000;
            let due = [50u64, 150, 300];
            if (samples as usize) < due.len() && ms >= due[samples as usize] {
                samples += 1;
                for id in 0..cpu::cpu_count() {
                    if TLB_SEEN[id as usize].load(Ordering::SeqCst) < generation
                        && let Some(c) = cpu::cpu(id)
                    {
                        crate::serial_println!("[smp] cpu{} late {} ms", id, ms);
                        NMI_DUMP.store(true, Ordering::SeqCst);
                        apic::send_ipi_raw(c.lapic_id, (4 << 8) | (1 << 14));
                    }
                }
            }
        }
        if t.expired() && !warned {
            // Keep waiting: going on without the other CPU's flush would
            // let it use stale translations of pages about to be freed.
            warned = true;
            let mut late = alloc::string::String::new();
            for (id, seen) in TLB_SEEN.iter().enumerate().take(cpu::cpu_count() as usize) {
                if seen.load(Ordering::SeqCst) < generation {
                    late.push_str(&alloc::format!(" cpu{}", id));
                }
            }
            crate::serial_println!(
                "[smp] TLB shootdown from cpu{} slow (waiting for{})",
                me,
                late
            );
            // In case the IPI was lost.
            apic::send_ipi_all_but_self(idt::VEC_TLB_IPI);
        }
        if giveup.expired() {
            crate::serial_println!("[smp] TLB shootdown from cpu{} abandoned", me);
            break;
        }
        core::hint::spin_loop();
    }
}

/// Make every other CPU print where it is (state dumps).
pub fn nmi_dump_others() {
    if online() <= 1 || !cpu::is_initialized() {
        return;
    }
    let me = cpu::this().cpu_id;
    NMI_DUMP.store(true, Ordering::SeqCst);
    for id in 0..cpu::cpu_count() {
        if id != me
            && let Some(c) = cpu::cpu(id)
        {
            apic::send_ipi_raw(c.lapic_id, (4 << 8) | (1 << 14));
            crate::time::delay_us(20_000);
        }
    }
}

/// Ask CPU `id` to reschedule (it has new work in its run queue).
pub fn kick_cpu(id: u32) {
    if online() > 1
        && cpu::is_initialized()
        && id != cpu::this().cpu_id
        && let Some(c) = cpu::cpu(id)
    {
        apic::send_ipi(c.lapic_id, idt::VEC_RESCHED_IPI);
    }
}

/// Stop all other CPUs (panic, power-off, reboot).
pub fn halt_others() {
    if online() > 1 && apic::is_enabled() {
        apic::send_ipi_all_but_self(idt::VEC_HALT_IPI);
    }
}
