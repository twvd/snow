//! Tests for the M68040 access error stack frame

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::regs::Register;
use crate::cpu_m68k::{CpuM68040Fpu, M68040_ADDRESS_MASK};
use crate::types::{Long, Word};

type TestCpu = CpuM68040Fpu<Testbus<Address, u8, Bus68020>>;

const CODE: Address = 0x1000;
const ACCESS_ERROR_HANDLER: Address = 0x2000;
const TRACE_HANDLER: Address = 0x2100;
const STACK: Address = 0x8000;
/// Access error frame on the stack
const FRAME: Address = STACK - 0x3C;

// Mapped by the page tables below, the next page is not resident
const ROOT_TABLE: Address = 0x0001_0000;
const PTR_TABLE: Address = 0x0001_1000;
const PAGE_TABLE: Address = 0x0001_2000;
const MAPPED: Address = 0x1084_2000;
const NOT_RESIDENT: Address = MAPPED + 0x2000;

const BUS_ERROR: Address = 0x0030_0000;

fn write_word(cpu: &mut TestCpu, addr: Address, value: Word) {
    cpu.bus.mem.insert(addr, (value >> 8) as u8);
    cpu.bus.mem.insert(addr + 1, value as u8);
}

fn write_long(cpu: &mut TestCpu, addr: Address, value: Long) {
    write_word(cpu, addr, (value >> 16) as Word);
    write_word(cpu, addr + 2, value as Word);
}

fn read_word(cpu: &TestCpu, addr: Address) -> Word {
    let byte = |a: Address| Word::from(*cpu.bus.mem.get(&a).unwrap_or(&0));
    (byte(addr) << 8) | byte(addr + 1)
}

fn read_long(cpu: &TestCpu, addr: Address) -> Long {
    (Long::from(read_word(cpu, addr)) << 16) | Long::from(read_word(cpu, addr + 2))
}

/// A CPU running `code` in supervisor mode, with the MMU on: code, stack and
/// vectors (the low 16MB) transparently translated, MAPPED paged.
fn testcpu(code: &[Word]) -> TestCpu {
    let mut cpu = TestCpu::new(Testbus::new(M68040_ADDRESS_MASK));
    cpu.bus.fault_ranges.push(BUS_ERROR..=BUS_ERROR + 0xF);

    for (i, &w) in code.iter().enumerate() {
        write_word(&mut cpu, CODE + 2 * i as Address, w);
    }
    // RTE
    write_word(&mut cpu, ACCESS_ERROR_HANDLER, 0x4E73);
    write_long(&mut cpu, 0x08, ACCESS_ERROR_HANDLER);
    write_long(&mut cpu, 0x24, TRACE_HANDLER);

    write_long(&mut cpu, ROOT_TABLE + 8 * 4, PTR_TABLE | 2);
    write_long(&mut cpu, PTR_TABLE + 33 * 4, PAGE_TABLE | 2);
    write_long(&mut cpu, PAGE_TABLE + 4, 0x0020_0000 | 1);
    cpu.regs.write(Register::SRP040, ROOT_TABLE);
    cpu.regs.write(Register::URP040, ROOT_TABLE);
    // Base $00, mask $00, enabled, user and supervisor
    cpu.regs.write(Register::ITT0, 0x0000_C000_u32);
    cpu.regs.write(Register::DTT0, 0x0000_C000_u32);
    // Enabled, 8KB pages
    cpu.regs.write(Register::TC040, 0xC000_u32);
    cpu.pmmu_cache_ensure();

    cpu.regs.isp = STACK;
    cpu.regs.sr.set_supervisor(true);
    cpu.regs.sr.set_int_prio_mask(7);
    cpu.set_pc(CODE).unwrap();
    cpu.prefetch_refill().unwrap();
    cpu
}

#[test]
fn page_fault_write_completes_instruction() {
    // MOVE.L D0,(A0)+ ; NOP
    let mut cpu = testcpu(&[0x20C0, 0x4E71]);
    cpu.regs.write_d(0, 0x1234_5678_u32);
    cpu.regs.write_a(0, NOT_RESIDENT);

    cpu.step().unwrap();

    // The instruction completed
    assert_eq!(cpu.regs.read_a::<Address>(0), NOT_RESIDENT + 4);
    assert_eq!(cpu.regs.pc, ACCESS_ERROR_HANDLER);
    assert_eq!(*cpu.regs.ssp(), FRAME);

    // Returns after the instruction
    assert_eq!(read_long(&cpu, FRAME + 0x02), CODE + 2);
    assert_eq!(read_word(&cpu, FRAME + 0x06), 0x7008);
    // SSW: ATC, write, long, supervisor data
    assert_eq!(read_word(&cpu, FRAME + 0x0C), 0x0405);
    // The write waits in WB2, WB1 and WB3 are empty
    assert_eq!(read_word(&cpu, FRAME + 0x0E), 0);
    assert_eq!(read_word(&cpu, FRAME + 0x10), 0x85);
    assert_eq!(read_word(&cpu, FRAME + 0x12), 0);
    assert_eq!(read_long(&cpu, FRAME + 0x14), NOT_RESIDENT);
    assert_eq!(read_long(&cpu, FRAME + 0x20), NOT_RESIDENT);
    assert_eq!(read_long(&cpu, FRAME + 0x24), 0x1234_5678);

    // RTE goes on with the next instruction
    cpu.step().unwrap();
    assert_eq!(cpu.regs.pc, CODE + 2);
    assert_eq!(*cpu.regs.ssp(), STACK);
}

#[test]
fn bus_error_write_in_wb1() {
    // MOVE.W D0,(A0) ; NOP
    let mut cpu = testcpu(&[0x3080, 0x4E71]);
    cpu.regs.write_d(0, 0xABCD_u32);
    cpu.regs.write_a(0, BUS_ERROR + 1);

    cpu.step().unwrap();

    assert_eq!(cpu.regs.pc, ACCESS_ERROR_HANDLER);
    assert_eq!(read_long(&cpu, FRAME + 0x02), CODE + 2);
    // SSW: write, word, supervisor data
    assert_eq!(read_word(&cpu, FRAME + 0x0C), 0x0045);
    assert_eq!(read_word(&cpu, FRAME + 0x10), 0);
    assert_eq!(read_word(&cpu, FRAME + 0x12), 0xC5);
    assert_eq!(read_long(&cpu, FRAME + 0x14), BUS_ERROR + 1);
    assert_eq!(read_long(&cpu, FRAME + 0x28), BUS_ERROR + 1);
    // On the byte lanes of the address (table 8-5)
    assert_eq!(read_long(&cpu, FRAME + 0x2C), 0x00AB_CD00);
}

#[test]
fn read_fault_restarts_instruction() {
    // MOVE.L (A0)+,D1
    let mut cpu = testcpu(&[0x2218]);
    cpu.regs.write_a(0, NOT_RESIDENT);

    cpu.step().unwrap();

    assert_eq!(cpu.regs.read_a::<Address>(0), NOT_RESIDENT);
    assert_eq!(read_long(&cpu, FRAME + 0x02), CODE);
    // SSW: ATC, read, long, supervisor data
    assert_eq!(read_word(&cpu, FRAME + 0x0C), 0x0505);
    assert_eq!(read_word(&cpu, FRAME + 0x10), 0);
    assert_eq!(read_word(&cpu, FRAME + 0x12), 0);
}

#[test]
fn movem_write_fault_restarts_instruction() {
    // MOVEM.L D0-D1,-(A0)
    let mut cpu = testcpu(&[0x48E0, 0xC000]);
    cpu.regs.write_a(0, NOT_RESIDENT + 8);

    cpu.step().unwrap();

    assert_eq!(cpu.regs.read_a::<Address>(0), NOT_RESIDENT + 8);
    assert_eq!(read_long(&cpu, FRAME + 0x02), CODE);
    assert_eq!(read_word(&cpu, FRAME + 0x10), 0);
}

#[test]
fn write_fault_with_pending_trace() {
    // MOVE.L D0,(A0)
    let mut cpu = testcpu(&[0x2080]);
    cpu.regs.write_a(0, NOT_RESIDENT);
    cpu.regs.sr.set_trace(true);

    cpu.step().unwrap();

    assert_eq!(cpu.regs.pc, ACCESS_ERROR_HANDLER);
    // CT set, the effective address is the traced instruction
    assert_eq!(read_word(&cpu, FRAME + 0x0C), 0x2405);
    assert_eq!(read_long(&cpu, FRAME + 0x08), CODE);

    // RTE turns the frame into the trace exception frame
    cpu.step().unwrap();
    assert_eq!(cpu.regs.pc, TRACE_HANDLER);
    let sp = *cpu.regs.ssp();
    assert_eq!(sp, STACK - 12);
    assert_eq!(read_long(&cpu, sp + 2), CODE + 2);
    assert_eq!(read_word(&cpu, sp + 6), 0x2024);
    assert_eq!(read_long(&cpu, sp + 8), CODE);
}
