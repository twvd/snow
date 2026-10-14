//! 68020 dynamic bus sizing

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::{CpuM68020Fpu, M68020_ADDRESS_MASK};
use crate::types::Long;

type TestCpu = CpuM68020Fpu<Testbus<Address, u8, Bus68020>>;

const MOVE_L_A0_D0: u16 = 0x2010;
const MOVE_L_D0_A0: u16 = 0x2080;
const DATA: Address = 0x2000;

/// Runs one instruction with A0 pointing to `addr`, the data area behind a
/// port of `width` bytes
fn run(opcode: u16, width: u32, addr: Address, d0: Long) -> TestCpu {
    const PC: Address = 0x1000;
    let mut bus = Testbus::new(M68020_ADDRESS_MASK);
    bus.port_widths.push((DATA..=DATA + 0xFF, width));
    let mut cpu = TestCpu::new(bus);

    for (i, word) in [opcode, 0x4E71, 0x4E71].into_iter().enumerate() {
        let a = PC + (i as Address * 2);
        cpu.bus.mem.insert(a, (word >> 8) as u8);
        cpu.bus.mem.insert(a + 1, word as u8);
    }
    for i in 0..8 {
        cpu.bus.mem.insert(DATA + i, 0x11 * (i as u8 + 1));
    }

    cpu.regs.isp = 0x800;
    cpu.regs.sr.set_supervisor(true);
    cpu.regs.sr.set_int_prio_mask(7);
    cpu.set_pc(PC).unwrap();
    cpu.prefetch_refill().unwrap();
    cpu.regs.write_a(0, addr);
    cpu.regs.write_d(0, d0);
    cpu.cycles = 0;
    cpu.step().unwrap();
    cpu
}

#[test]
fn long_read_port_widths() {
    let base = run(MOVE_L_A0_D0, 4, DATA, 0);
    assert_eq!(base.regs.read_d::<Long>(0), 0x1122_3344);

    for (width, cycles) in [(2, 1), (1, 3)] {
        let cpu = run(MOVE_L_A0_D0, width, DATA, 0);
        assert_eq!(cpu.regs.read_d::<Long>(0), 0x1122_3344);
        assert_eq!(cpu.cycles, base.cycles + cycles * 3);
    }
}

#[test]
fn misaligned_long_read() {
    let base = run(MOVE_L_A0_D0, 4, DATA, 0);

    // 3 + 1 bytes on a 32-bit port
    let cpu = run(MOVE_L_A0_D0, 4, DATA + 1, 0);
    assert_eq!(cpu.regs.read_d::<Long>(0), 0x2233_4455);
    assert_eq!(cpu.cycles, base.cycles + 3);

    // 1 + 2 + 1 bytes on a 16-bit port
    let cpu = run(MOVE_L_A0_D0, 2, DATA + 1, 0);
    assert_eq!(cpu.regs.read_d::<Long>(0), 0x2233_4455);
    assert_eq!(cpu.cycles, base.cycles + 6);
}

#[test]
fn long_write_port_widths() {
    for width in [4, 2, 1] {
        for offset in 0..4 {
            let cpu = run(MOVE_L_D0_A0, width, DATA + offset, 0xAABB_CCDD);
            let a = DATA + offset;
            let bytes: Vec<u8> = (a..a + 4).map(|a| cpu.bus.mem[&a]).collect();
            assert_eq!(
                bytes,
                [0xAA, 0xBB, 0xCC, 0xDD],
                "width {width} offset {offset}"
            );
        }
    }
}
