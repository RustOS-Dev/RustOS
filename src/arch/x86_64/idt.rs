//! Interrupt descriptor table and trap entry.
//!
//! Every vector (0–255) has an assembly stub that pushes a uniform
//! [`TrapFrame`] and calls [`trap_dispatch`]. Coming from ring 3 the stub also
//! executes `swapgs`, so kernel code can always rely on GS pointing at the
//! per-CPU block. The same frame layout is produced by the `syscall` entry, so
//! `fork`, signal delivery and `execve` can all build or edit frames and
//! return through [`trap_return`].

use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

/// Register state saved on kernel entry. Field order matches the push order
/// in `trap_common` (last pushed = lowest address = first field).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct TrapFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub vector: u64,
    pub error_code: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

impl TrapFrame {
    pub fn from_user(&self) -> bool {
        self.cs & 3 == 3
    }
}

// Vectors whose exceptions push an error code themselves.
macro_rules! trap_stubs {
    ($($n:literal),*) => {
        global_asm!(
            $(
                concat!(".hidden trap_stub_", $n),
                concat!("trap_stub_", $n, ":"),
                concat!(".if ", $n, " == 8 || (", $n, " >= 10 && ", $n, " <= 14) || ", $n, " == 17 || ", $n, " == 21 || ", $n, " == 29 || ", $n, " == 30"),
                concat!("    push ", $n),
                ".else",
                "    push 0",
                concat!("    push ", $n),
                ".endif",
                "    jmp trap_common",
            )*
            ".section .data.rel.ro,\"aw\"",
            ".global trap_stub_table",
            ".hidden trap_stub_table",
            ".balign 8",
            "trap_stub_table:",
            $( concat!("    .quad trap_stub_", $n), )*
            ".text",
        );
    };
}

trap_stubs!(
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49,
    50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73,
    74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97,
    98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115, 116,
    117, 118, 119, 120, 121, 122, 123, 124, 125, 126, 127, 128, 129, 130, 131, 132, 133, 134, 135,
    136, 137, 138, 139, 140, 141, 142, 143, 144, 145, 146, 147, 148, 149, 150, 151, 152, 153, 154,
    155, 156, 157, 158, 159, 160, 161, 162, 163, 164, 165, 166, 167, 168, 169, 170, 171, 172, 173,
    174, 175, 176, 177, 178, 179, 180, 181, 182, 183, 184, 185, 186, 187, 188, 189, 190, 191, 192,
    193, 194, 195, 196, 197, 198, 199, 200, 201, 202, 203, 204, 205, 206, 207, 208, 209, 210, 211,
    212, 213, 214, 215, 216, 217, 218, 219, 220, 221, 222, 223, 224, 225, 226, 227, 228, 229, 230,
    231, 232, 233, 234, 235, 236, 237, 238, 239, 240, 241, 242, 243, 244, 245, 246, 247, 248, 249,
    250, 251, 252, 253, 254, 255
);

global_asm!(
    ".global trap_common",
    "trap_common:",
    // Stack: vector, error_code, rip, cs, rflags, rsp, ss
    "    test qword ptr [rsp + 24], 3",
    "    jz 1f",
    "    swapgs",
    "1:",
    "    push rax",
    "    push rbx",
    "    push rcx",
    "    push rdx",
    "    push rsi",
    "    push rdi",
    "    push rbp",
    "    push r8",
    "    push r9",
    "    push r10",
    "    push r11",
    "    push r12",
    "    push r13",
    "    push r14",
    "    push r15",
    "    mov rdi, rsp",
    "    cld",
    "    call {dispatch}",
    ".global trap_return",
    "trap_return:",
    "    pop r15",
    "    pop r14",
    "    pop r13",
    "    pop r12",
    "    pop r11",
    "    pop r10",
    "    pop r9",
    "    pop r8",
    "    pop rbp",
    "    pop rdi",
    "    pop rsi",
    "    pop rdx",
    "    pop rcx",
    "    pop rbx",
    "    pop rax",
    "    test qword ptr [rsp + 24], 3",
    "    jz 2f",
    "    swapgs",
    "2:",
    "    add rsp, 16",
    "    iretq",
    dispatch = sym trap_dispatch,
);

unsafe extern "C" {
    static trap_stub_table: [u64; 256];
    fn trap_return();
}

/// Address of the common trap-return path (pops a [`TrapFrame`] and `iretq`s).
pub fn trap_return_addr() -> u64 {
    trap_return as *const () as u64
}

// ---------------------------------------------------------------------------
// IDT
// ---------------------------------------------------------------------------

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    reserved: u32,
}

impl IdtEntry {
    const EMPTY: IdtEntry = IdtEntry {
        offset_low: 0,
        selector: 0,
        ist: 0,
        type_attr: 0,
        offset_mid: 0,
        offset_high: 0,
        reserved: 0,
    };

    fn new(handler: u64, ist: u8, dpl: u8) -> IdtEntry {
        IdtEntry {
            offset_low: handler as u16,
            selector: super::gdt::KERNEL_CS,
            ist,
            // Present, interrupt gate (IF cleared on entry).
            type_attr: 0x80 | (dpl << 5) | 0xE,
            offset_mid: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            reserved: 0,
        }
    }
}

#[repr(C, align(16))]
struct Idt([IdtEntry; 256]);

static mut IDT: Idt = Idt([IdtEntry::EMPTY; 256]);

pub const VEC_DOUBLE_FAULT: u8 = 8;
pub const VEC_NMI: u8 = 2;
pub const VEC_MACHINE_CHECK: u8 = 18;
pub const VEC_PAGE_FAULT: u8 = 14;
/// Legacy `int 0x80` syscall gate (callable from ring 3).
pub const VEC_SYSCALL: u8 = 0x80;
pub const VEC_TIMER: u8 = 0x20;
pub const VEC_RESCHED_IPI: u8 = 0xF0;
pub const VEC_TLB_IPI: u8 = 0xF1;
pub const VEC_HALT_IPI: u8 = 0xF2;
pub const VEC_APIC_ERROR: u8 = 0xFE;
pub const VEC_SPURIOUS: u8 = 0xFF;

/// Build the IDT (once) and load it on the current CPU.
#[allow(clippy::deref_addrof)]
pub fn init() {
    static BUILT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
    if !BUILT.swap(true, Ordering::SeqCst) {
        let idt: &mut Idt = unsafe { &mut *(&raw mut IDT) };
        let stubs: &[u64; 256] = unsafe { &*(&raw const trap_stub_table) };
        for (v, &handler) in stubs.iter().enumerate() {
            let ist = match v as u8 {
                VEC_DOUBLE_FAULT => super::gdt::IST_DOUBLE_FAULT + 1,
                VEC_NMI => super::gdt::IST_NMI + 1,
                VEC_MACHINE_CHECK => super::gdt::IST_MCE + 1,
                _ => 0,
            };
            let dpl = if v == VEC_SYSCALL as usize || v == 3 {
                3
            } else {
                0
            };
            idt.0[v] = IdtEntry::new(handler, ist, dpl);
        }
    }
    load();
}

pub fn load() {
    #[repr(C, packed)]
    struct Idtr {
        limit: u16,
        base: u64,
    }
    let idtr = Idtr {
        limit: (core::mem::size_of::<Idt>() - 1) as u16,
        base: &raw const IDT as u64,
    };
    unsafe { asm!("lidt [{}]", in(reg) &idtr, options(readonly, nostack, preserves_flags)) };
}

// ---------------------------------------------------------------------------
// Handler registration and vector allocation
// ---------------------------------------------------------------------------

pub type IrqHandler = fn(&mut TrapFrame);
type HandlerBox = alloc::boxed::Box<dyn Fn(&mut TrapFrame) + Send + Sync>;

/// Per-vector handler: a leaked `Box<HandlerBox>` (thin pointer), or null.
/// Replaced handlers are leaked deliberately; vectors are rarely re-bound and
/// another CPU may still be running the old one.
static HANDLERS: [core::sync::atomic::AtomicPtr<HandlerBox>; 256] =
    [const { core::sync::atomic::AtomicPtr::new(core::ptr::null_mut()) }; 256];
static COUNTS: [AtomicUsize; 256] = [const { AtomicUsize::new(0) }; 256];
static ALLOC_LOCK: Mutex<()> = Mutex::new(());

/// First and last vectors handed out to devices (MSI/MSI-X/IOAPIC).
pub const DEVICE_VECTOR_FIRST: u8 = 0x30;
pub const DEVICE_VECTOR_LAST: u8 = 0xEF;

/// Install `handler` for `vector`, replacing any previous handler.
pub fn register<F: Fn(&mut TrapFrame) + Send + Sync + 'static>(vector: u8, handler: F) {
    let b: HandlerBox = alloc::boxed::Box::new(handler);
    let p = alloc::boxed::Box::into_raw(alloc::boxed::Box::new(b));
    HANDLERS[vector as usize].store(p, Ordering::SeqCst);
}

pub fn unregister(vector: u8) {
    HANDLERS[vector as usize].store(core::ptr::null_mut(), Ordering::SeqCst);
}

fn is_free(v: u8) -> bool {
    v != VEC_SYSCALL && HANDLERS[v as usize].load(Ordering::SeqCst).is_null()
}

/// Allocate an unused device vector and install `handler` on it.
pub fn alloc_vector<F: Fn(&mut TrapFrame) + Send + Sync + 'static>(handler: F) -> Option<u8> {
    let _g = ALLOC_LOCK.lock();
    let v = (DEVICE_VECTOR_FIRST..=DEVICE_VECTOR_LAST).find(|&v| is_free(v))?;
    register(v, handler);
    Some(v)
}

/// Allocate `n` consecutive free vectors, aligned to `n` (a power of two), as
/// multi-message MSI requires. Handlers are installed separately.
pub fn alloc_vector_block(n: u8) -> Option<u8> {
    let _g = ALLOC_LOCK.lock();
    let mut v = DEVICE_VECTOR_FIRST.next_multiple_of(n);
    while v as u16 + n as u16 - 1 <= DEVICE_VECTOR_LAST as u16 {
        if (v..v + n).all(is_free) {
            for i in v..v + n {
                // Reserve with a placeholder that just acknowledges.
                register(i, |_f: &mut TrapFrame| super::apic::eoi());
            }
            return Some(v);
        }
        v += n;
    }
    None
}

/// Number of times each vector fired (for `/proc/interrupts`-style output).
pub fn interrupt_count(v: u8) -> usize {
    COUNTS[v as usize].load(Ordering::Relaxed)
}

pub fn interrupt_counts() -> alloc::vec::Vec<(u8, usize)> {
    (0..256)
        .filter_map(|v| {
            let c = COUNTS[v].load(Ordering::Relaxed);
            (c > 0).then_some((v as u8, c))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

const EXCEPTION_NAMES: [&str; 32] = [
    "divide error",
    "debug",
    "NMI",
    "breakpoint",
    "overflow",
    "bound range exceeded",
    "invalid opcode",
    "device not available",
    "double fault",
    "coprocessor segment overrun",
    "invalid TSS",
    "segment not present",
    "stack-segment fault",
    "general protection fault",
    "page fault",
    "reserved",
    "x87 floating-point",
    "alignment check",
    "machine check",
    "SIMD floating-point",
    "virtualization",
    "control protection",
    "reserved",
    "reserved",
    "reserved",
    "reserved",
    "reserved",
    "reserved",
    "hypervisor injection",
    "VMM communication",
    "security",
    "reserved",
];

pub fn exception_name(v: u64) -> &'static str {
    EXCEPTION_NAMES
        .get(v as usize)
        .copied()
        .unwrap_or("interrupt")
}

extern "C" fn trap_dispatch(frame: &mut TrapFrame) {
    let v = frame.vector as usize;
    COUNTS[v].fetch_add(1, Ordering::Relaxed);

    let h = HANDLERS[v].load(Ordering::Acquire);
    if !h.is_null() {
        let handler: &HandlerBox = unsafe { &*h };
        handler(frame);
    } else if v < 32 {
        crate::arch::x86_64::exceptions::handle(frame);
    } else if v == VEC_SPURIOUS as usize {
        // Spurious APIC interrupts need no EOI.
    } else {
        crate::serial_println!("[irq] unhandled vector {}", v);
        super::apic::eoi();
    }

    // Returning to user mode is the safe point for preemption and signals.
    // Interrupts must stay off from here to `iretq` (swapgs window).
    x86_64::instructions::interrupts::disable();
    crate::sched::on_trap_exit(frame);
    x86_64::instructions::interrupts::disable();
}
