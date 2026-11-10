//! Instruction stream faults on the 68020/68030: the bus fault frame tells
//! them from data faults with the pipe stage bits of the SSW and the stage B
//! address (MC68030UM 8.2, 8.2.1)

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::{
    CpuM68020, CpuM68030, FPU_M68881, FPU_M68882, M68020_ADDRESS_MASK, M68030_ADDRESS_MASK,
};
use crate::types::{Long, Word};

const CODE: Address = 0x1000;
const HANDLER: Address = 0x400;
const STACK: Address = 0x8000;
const BUS_ERROR: Address = 0x0030_0000;

const SSW_FC: Word = 1 << 15;
const SSW_FB: Word = 1 << 14;
const SSW_RC: Word = 1 << 13;
const SSW_RB: Word = 1 << 12;
const SSW_DF: Word = 1 << 8;

/// Runs `code` until the fault handler of `vector`, and returns the frame
/// format word, the SSW and the stage B address
macro_rules! fault_frame {
    ($cpu:ty, $fpu:expr, $mask:expr, $code:expr, $vector:expr) => {{
        let mut cpu = <$cpu>::new(Testbus::new($mask), $fpu);
        cpu.bus.fault_ranges.push(BUS_ERROR..=BUS_ERROR + 0xF);
        let code: &[Word] = $code;
        for (i, w) in code.iter().enumerate() {
            let [h, l] = w.to_be_bytes();
            cpu.bus.mem.insert(CODE + 2 * i as Address, h);
            cpu.bus.mem.insert(CODE + 2 * i as Address + 1, l);
        }
        for (a, b) in (4 * $vector..).zip((HANDLER as Long).to_be_bytes()) {
            cpu.bus.mem.insert(a, b);
        }
        cpu.regs.isp = STACK;
        cpu.regs.sr.set_supervisor(true);
        cpu.regs.a[0] = 0x3001;
        cpu.set_pc(CODE).unwrap();
        cpu.prefetch_refill().unwrap();
        for _ in 0..3 {
            cpu.step().unwrap();
            if cpu.regs.pc == HANDLER {
                break;
            }
        }
        assert_eq!(cpu.regs.pc, HANDLER);
        let ssp = *cpu.regs.ssp();
        let byte = |a: Address| *cpu.bus.mem.get(&a).unwrap_or(&0);
        let word = |a: Address| Word::from_be_bytes([byte(a), byte(a + 1)]);
        let long = |a: Address| (Long::from(word(a)) << 16) | Long::from(word(a + 2));
        (word(ssp + 6), word(ssp + 0x0A), long(ssp + 0x24))
    }};
}

macro_rules! tests {
    ($mod:ident, $cpu:ty, $fpu:expr, $mask:expr) => {
        mod $mod {
            use super::*;

            #[test]
            fn fetch_bus_error_is_a_stage_c_fault() {
                // JMP BUS_ERROR
                let (format, ssw, stage_b) = fault_frame!(
                    $cpu,
                    $fpu,
                    $mask,
                    &[0x4EF9, (BUS_ERROR >> 16) as Word, BUS_ERROR as Word],
                    2
                );
                // Long frame, which has the stage B address
                assert_eq!(format, 0xB008);
                // Fault and rerun on stage C, stage B fetched again, no data
                // fault, supervisor program space
                assert_eq!(
                    ssw & (SSW_FC | SSW_FB | SSW_RC | SSW_RB | SSW_DF),
                    SSW_FC | SSW_RC | SSW_RB
                );
                assert_eq!(ssw & 7, 6);
                // Stage C, the faulted word, is the word before stage B
                assert_eq!(stage_b, BUS_ERROR + 2);
            }

            #[test]
            fn rte_from_address_error_frame_replays() {
                // JMP (A0), A0 odd; the handler returns at once
                let mut cpu = <$cpu>::new(Testbus::new($mask), $fpu);
                for (a, b) in (CODE..).zip(0x4ED0_u16.to_be_bytes()) {
                    cpu.bus.mem.insert(a, b);
                }
                for (a, b) in (HANDLER..).zip(0x4E73_u16.to_be_bytes()) {
                    cpu.bus.mem.insert(a, b);
                }
                for (a, b) in (4 * 3..).zip((HANDLER as Long).to_be_bytes()) {
                    cpu.bus.mem.insert(a, b);
                }
                cpu.regs.isp = STACK;
                cpu.regs.sr.set_supervisor(true);
                cpu.regs.a[0] = 0x3001;
                cpu.set_pc(CODE).unwrap();
                cpu.prefetch_refill().unwrap();
                cpu.step().unwrap();
                assert_eq!(cpu.regs.pc, HANDLER);

                // DF is clear, but no data cycle was completed: RTE goes
                // back to the instruction, it does not resume it
                cpu.step().unwrap();
                assert_eq!(cpu.regs.pc, CODE);
                assert_eq!(*cpu.regs.ssp(), STACK);
            }

            #[test]
            fn odd_fetch_address_error_sets_only_rerun_bits() {
                // JMP (A0), A0 odd
                let (format, ssw, stage_b) = fault_frame!($cpu, $fpu, $mask, &[0x4ED0], 3);
                assert_eq!(format, 0xB00C);
                // No fault bits for an address error (MC68030UM 8.2.1)
                assert_eq!(
                    ssw & (SSW_FC | SSW_FB | SSW_RC | SSW_RB | SSW_DF),
                    SSW_RC | SSW_RB
                );
                assert_eq!(ssw & 7, 6);
                // Stage C is the odd target
                assert_eq!(stage_b, 0x3003);
            }
        }
    };
}

tests!(
    m68020,
    CpuM68020<Testbus<Address, u8, Bus68020>>,
    FPU_M68881,
    M68020_ADDRESS_MASK
);
tests!(
    m68030,
    CpuM68030<Testbus<Address, u8, Bus68020>>,
    FPU_M68882,
    M68030_ADDRESS_MASK
);
