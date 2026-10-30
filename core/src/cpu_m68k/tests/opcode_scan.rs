//! Runs all the 65536 opcodes on each CPU: none may make step() fail, whatever
//! the instruction does with its operands.

use crate::bus::Address;
use crate::bus::access::{Bus68000, Bus68020};
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::{
    CpuM68000, CpuM68020Fpu, CpuM68030Fpu, M68000_ADDRESS_MASK, M68020_ADDRESS_MASK,
    M68030_ADDRESS_MASK,
};

const CODE: Address = 0x1000;
/// What follows the opcode, as extension words
const EXTWORDS: [u16; 11] = [
    0x0000, 0xFFFF, 0x0100, 0x0120, 0x8000, 0x2000, 0x4200, 0x6000, 0x6E00, 0xE000, 0x7000,
];

macro_rules! scan {
    ($name:ident, $cpu:ident, $bus:ident, $mask:ident) => {
        #[test]
        fn $name() {
            let mut failed = vec![];
            for (opcode, ext) in (0..=u16::MAX).flat_map(|o| EXTWORDS.iter().map(move |&e| (o, e)))
            {
                let mut cpu = $cpu::<Testbus<Address, u8, $bus>>::new(Testbus::new($mask));
                // Every vector at an even address past the code
                for v in 2..64 {
                    let a = v * 4;
                    cpu.bus.mem.insert(a + 2, 0x40);
                }
                let [h, l] = opcode.to_be_bytes();
                cpu.bus.mem.insert(CODE, h);
                cpu.bus.mem.insert(CODE + 1, l);
                let [h, l] = ext.to_be_bytes();
                for i in (2..16).step_by(2) {
                    cpu.bus.mem.insert(CODE + i, h);
                    cpu.bus.mem.insert(CODE + i + 1, l);
                }
                cpu.regs.isp = 0x8000;
                cpu.regs.sr.set_supervisor(true);
                cpu.regs.sr.set_int_prio_mask(7);
                for a in 0..7 {
                    cpu.regs.a[a] = 0x3000;
                }
                cpu.set_pc(CODE).unwrap();
                cpu.prefetch_refill().unwrap();
                if let Err(e) = cpu.step() {
                    failed.push(format!("{opcode:04X} {ext:04X}: {e}"));
                }
            }
            assert!(
                failed.is_empty(),
                "{} opcodes failed, first ones:\n{}",
                failed.len(),
                failed
                    .iter()
                    .step_by(failed.len().div_ceil(60).max(1))
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        }
    };
}

scan!(scan_68000, CpuM68000, Bus68000, M68000_ADDRESS_MASK);
scan!(scan_68020, CpuM68020Fpu, Bus68020, M68020_ADDRESS_MASK);
scan!(scan_68030, CpuM68030Fpu, Bus68020, M68030_ADDRESS_MASK);

/// An invalid effective address found past an extension word still stacks the
/// address of the instruction (MC68030UM 8.1.5): BFTST A0
#[test]
fn illegal_frame_has_the_instruction_address() {
    let mut cpu =
        CpuM68020Fpu::<Testbus<Address, u8, Bus68020>>::new(Testbus::new(M68020_ADDRESS_MASK));
    cpu.bus.mem.insert(0x12, 0x04);
    cpu.bus.mem.insert(CODE, 0xE8);
    cpu.bus.mem.insert(CODE + 1, 0xC8);
    cpu.regs.isp = 0x8000;
    cpu.regs.sr.set_supervisor(true);
    cpu.set_pc(CODE).unwrap();
    cpu.prefetch_refill().unwrap();
    cpu.step().unwrap();
    assert_eq!(cpu.regs.pc, 0x400);
    let [h, l] = (CODE as u16).to_be_bytes();
    assert_eq!(cpu.bus.mem[&(cpu.regs.isp + 4)], h);
    assert_eq!(cpu.bus.mem[&(cpu.regs.isp + 5)], l);
}
