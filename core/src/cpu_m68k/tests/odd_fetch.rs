//! Instruction fetches at an odd address on the 68020/68030: every way to get
//! there takes the address error exception (MC68030UM 7.1, 8.1.3)

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::{CpuM68020Fpu, CpuM68030Fpu, M68020_ADDRESS_MASK, M68030_ADDRESS_MASK};
use crate::types::Word;

const CODE: Address = 0x1000;
const HANDLER: Address = 0x400;
const STACK: Address = 0x8000;

macro_rules! run {
    ($cpu:ty, $mask:expr, $code:expr, $stack:expr) => {{
        let mut cpu = <$cpu>::new(Testbus::new($mask));
        let code: &[Word] = $code;
        for (i, w) in code.iter().enumerate() {
            let [h, l] = w.to_be_bytes();
            cpu.bus.mem.insert(CODE + 2 * i as Address, h);
            cpu.bus.mem.insert(CODE + 2 * i as Address + 1, l);
        }
        let stack: &[Word] = $stack;
        let sp = STACK - 2 * stack.len() as Address;
        for (i, w) in stack.iter().enumerate() {
            let [h, l] = w.to_be_bytes();
            cpu.bus.mem.insert(sp + 2 * i as Address, h);
            cpu.bus.mem.insert(sp + 2 * i as Address + 1, l);
        }
        // Vector 3
        let [h, l] = (HANDLER as Word).to_be_bytes();
        cpu.bus.mem.insert(0x0E, h);
        cpu.bus.mem.insert(0x0F, l);
        cpu.regs.isp = sp;
        cpu.regs.sr.set_supervisor(true);
        cpu.regs.a[0] = 0x3001;
        cpu.regs.d[0] = 1;
        cpu.set_pc(CODE).unwrap();
        cpu.prefetch_refill().unwrap();
        cpu.step().unwrap();
        assert_eq!(cpu.regs.pc, HANDLER);
        // Whatever the stack had, the frame is below where it started, at
        // the instruction
        let ssp = *cpu.regs.ssp();
        assert!(ssp < sp);
        let word = |a: Address| Word::from_be_bytes([cpu.bus.mem[&a], cpu.bus.mem[&(a + 1)]]);
        assert_eq!(word(ssp + 4), CODE as Word);
        assert_eq!(word(ssp + 6) & 0x0FFF, 3 * 4);
    }};
}

macro_rules! tests {
    ($mod:ident, $cpu:ty, $mask:expr) => {
        mod $mod {
            use super::*;
            type Cpu = $cpu;

            #[test]
            fn jmp() {
                run!(Cpu, $mask, &[0x4ED0], &[]); // JMP (A0)
            }
            #[test]
            fn jsr() {
                run!(Cpu, $mask, &[0x4E90], &[]); // JSR (A0)
            }
            #[test]
            fn bra() {
                run!(Cpu, $mask, &[0x6001], &[]); // BRA.B *+3
            }
            #[test]
            fn bsr() {
                run!(Cpu, $mask, &[0x6101], &[]); // BSR.B *+3
            }
            #[test]
            fn dbra() {
                run!(Cpu, $mask, &[0x51C8, 0x0001], &[]); // DBRA D0,*+3
            }
            #[test]
            fn rts() {
                run!(Cpu, $mask, &[0x4E75], &[0, 0x3001]);
            }
            #[test]
            fn rtd() {
                run!(Cpu, $mask, &[0x4E74, 0], &[0, 0x3001]);
            }
            #[test]
            fn rtr() {
                run!(Cpu, $mask, &[0x4E77], &[0, 0, 0x3001]);
            }
            #[test]
            fn rte() {
                run!(Cpu, $mask, &[0x4E73], &[0x2000, 0, 0x3001, 0]);
            }
            #[test]
            fn fbt() {
                run!(Cpu, $mask, &[0xF28F, 0x0001], &[]); // FBT *+3
            }
        }
    };
}

tests!(
    m68020,
    CpuM68020Fpu<Testbus<Address, u8, Bus68020>>,
    M68020_ADDRESS_MASK
);
tests!(
    m68030,
    CpuM68030Fpu<Testbus<Address, u8, Bus68020>>,
    M68030_ADDRESS_MASK
);
