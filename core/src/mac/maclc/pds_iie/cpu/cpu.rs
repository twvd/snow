//! 6502-family CPU core, adapted from siena's 65816 core.
//!
//! See `mod.rs` for the provenance.  The instruction execution and
//! cycle-counting framework is siena's; the parameterisation into the
//! 6502/65C02/65816 family, the suspend/trap handling and the family-specific semantics
//! quirks are new.

use anyhow::Result;
use arrayvec::ArrayVec;
use num_traits::ToPrimitive;
use serde::{Deserialize, Serialize};

use crate::tickable::Ticks;

use super::alu;
use super::instruction::{AddressingMode, Instruction, InstructionType, MAX_INSTRUCTION_LEN};
use super::regs::{Flag, Register, RegisterFile, RegisterWidth};
use super::{ADDRESS_MASK, Address, Bus6502, ReadKind, Variant};

/// Iterator over the memory bus, wrapping at the bus address mask.
struct BusIterator<'a> {
    bus: &'a dyn Bus6502,
    next: Address,
}

impl<'a> BusIterator<'a> {
    fn new_from(bus: &'a dyn Bus6502, offset: Address) -> Self {
        Self { bus, next: offset }
    }
}

impl<'a> Iterator for BusIterator<'a> {
    type Item = u8;

    fn next(&mut self) -> Option<Self::Item> {
        let curr = self.next;
        self.next = self.next.wrapping_add(1) & self.bus.get_mask();

        Some(self.bus.read(curr, ReadKind::Data))
    }
}

/// 6502-family CPU
#[derive(Serialize, Deserialize)]
pub struct Cpu<TBus: Bus6502> {
    pub verbose: bool,
    pub variant: Variant,
    verbose_wai: bool,
    pub bus: TBus,
    pub regs: RegisterFile,
    pub cycles: Ticks,
    pub wait_for_int: bool,

    /// Halted by STP until reset.
    stopped: bool,

    /// A trapped read suspended the bus mid-instruction: when the bus
    /// resumes the CPU, the whole instruction is re-executed.
    suspended: bool,

    nmi_edge: bool,

    /// Interrupt vector address for COP software interrupt
    pub intvec_cop: Address,

    /// Interrupt vector address for BRK software interrupt
    pub intvec_brk: Address,

    /// Interrupt vector address for non-maskable interrupt
    pub intvec_nmi: Address,

    /// Interrupt vector address for non-maskable interrupt
    pub intvec_int: Address,

    /// Reset vector address
    pub vec_reset: Address,
}

/// Read-modify-write category for abs-indexed address fixups.
#[derive(Copy, Clone, Eq, PartialEq)]
enum Rmw {
    /// Loads, stores and other plain accesses.
    No,
    /// Shifts and rotates (crossing-only fixup on the 6502/65C02).
    Shift,
    /// INC/DEC (the 65C02 always performs the fixup dummy read).
    IncDec,
}

impl<TBus> Cpu<TBus>
where
    TBus: Bus6502,
{
    fn _new(bus: TBus, variant: Variant) -> Self {
        let (intvec_cop, intvec_brk, intvec_nmi, intvec_int, vec_reset) = if variant.is_65816() {
            (0x00FFE4, 0x00FFE6, 0x00FFEA, 0x00FFEE, 0x00FFFC)
        } else {
            (0x000000, 0x00FFFE, 0x00FFFA, 0x00FFFE, 0x00FFFC)
        };
        Self {
            verbose: false,
            variant,
            verbose_wai: false,
            bus,
            regs: RegisterFile::new(),
            cycles: 0,
            wait_for_int: false,
            stopped: false,
            suspended: false,
            nmi_edge: false,
            intvec_cop,
            intvec_brk,
            intvec_nmi,
            intvec_int,
            vec_reset,
        }
    }

    pub fn new(bus: TBus, variant: Variant) -> Self {
        let mut cpu = Self::_new(bus, variant);
        cpu.reset();
        cpu
    }

    pub fn new_pc(bus: TBus, variant: Variant, pc: u16) -> Self {
        let mut cpu = Self::_new(bus, variant);
        cpu.reset_pc(pc);
        cpu
    }

    pub fn reset(&mut self) {
        let pc = self.read16_tick_a16(self.vec_reset);
        self.reset_pc(pc);
    }

    pub fn reset_pc(&mut self, pc: u16) {
        self.regs = RegisterFile::new();
        self.regs.pc = pc;
        self.regs.p = (1 << Flag::M.to_u8().unwrap())
            | (1 << Flag::X.to_u8().unwrap())
            | (1 << Flag::I.to_u8().unwrap());
        self.regs.s = 0x01FF;
        self.regs.emulation = true;
        self.stopped = false;
        self.suspended = false;
        self.wait_for_int = false;
        self.nmi_edge = false;
    }

    pub fn dump_state(&self) -> String {
        format!(
            "{} - {}\n --> {}",
            self.cycles,
            self.regs,
            self.peek_next_instr()
        )
    }

    /// Whether an operation on the register selected by `flag` (M for
    /// the accumulator, X for the index registers) uses 8-bit width.
    ///
    /// On the 65816 the width follows the M/X flags in P; on the 6502
    /// and 65C02 those bits are the hardwired bit 5 and the B flag, and
    /// the registers are always 8-bit.
    fn width8(&self, flag: Flag) -> bool {
        if self.variant.is_65816() {
            self.regs.test_flag(flag)
        } else {
            true
        }
    }

    /// Fetches and decodes the next instruction at PC
    pub fn peek_next_instr(&self) -> Instruction {
        let mut busiter = BusIterator::new_from(&self.bus, self.regs.get_full_pc());
        Instruction::decode(
            &mut busiter,
            self.width8(Flag::M),
            self.width8(Flag::X),
            self.variant,
        )
        .expect("6502-family instruction decode error")
    }

    /// Fetches and decodes the next instruction at PC
    pub fn fetch_next_instr(&mut self) -> Result<Instruction> {
        let mut fetched: ArrayVec<u8, MAX_INSTRUCTION_LEN> = ArrayVec::new();
        let (m, x) = (self.width8(Flag::M), self.width8(Flag::X));
        let mut p = 0;

        let instr = loop {
            let pc = (self.regs.k as Address) << 16 | self.regs.pc.wrapping_add(p) as Address;
            p += 1;

            let kind = if p == 1 {
                ReadKind::Opcode
            } else {
                ReadKind::Data
            };
            match Instruction::decode(&mut fetched.clone().into_iter(), m, x, self.variant) {
                None => fetched.push(self.read_tick_kind(pc, kind)),
                Some(i) => break i,
            }
        };

        Ok(instr)
    }

    /// Executes one CPU step (one instruction).
    pub fn step(&mut self) -> Result<Ticks> {
        let start_cycles = self.cycles;

        // Halted by STP: nothing further happens until a reset.
        if self.stopped {
            return Ok(0);
        }

        // Suspended by the bus (a Gemini trap): do nothing until the
        // host resumes the CPU.
        if self.bus.is_suspended() {
            return Ok(0);
        }

        let nmi_state = self.bus.get_nmi();

        if !nmi_state {
            self.nmi_edge = false;
        }

        if nmi_state && !self.nmi_edge {
            self.nmi_edge = true;
            if self.wait_for_int {
                if self.verbose {
                    println!("{} CPU awoken (NMI)", self.cycles);
                }
                self.tick_bus(2)?;
                self.wait_for_int = false;
                self.verbose_wai = false;
            }
            self.dispatch_interrupt(self.intvec_nmi, false)?;
        } else if self.bus.get_int() {
            if self.wait_for_int {
                if self.verbose {
                    println!("{} CPU awoken (INT)", self.cycles);
                }
                self.tick_bus(2)?;
                self.wait_for_int = false;
                self.verbose_wai = false;
            }
            if !self.regs.test_flag(Flag::I) {
                self.dispatch_interrupt(self.intvec_int, false)?;
            }
        } else if self.wait_for_int {
            if self.verbose && !self.verbose_wai {
                println!("{}", self.dump_state());
                println!("{} CPU waiting for interrupt...", self.cycles);
                self.verbose_wai = true;
            }
            self.tick_bus(1)?;
            return Ok(0);
        }

        if self.verbose {
            println!("{}", self.dump_state());
        }

        // Snapshot for the trap rewind: a trapped read halts the bus
        // mid-instruction, and the instruction re-executes from its
        // start once the bus resumes the CPU.
        let snapshot = self.regs.clone();

        let instr = self.fetch_next_instr()?;

        self.regs.pc = self.regs.pc.wrapping_add(instr.len as u16);
        self.execute_instruction(&instr)?;

        if self.suspended {
            self.regs = snapshot;
            self.cycles = start_cycles;
            self.suspended = false;
            return Ok(0);
        }

        // P bit 5 is hardwired to 1 on the 6502 and 65C02.
        if !self.variant.is_65816() {
            self.regs.p |= 0x20;
        }

        let cycles = self.cycles - start_cycles;

        Ok(cycles)
    }

    /// Tick peripherals
    fn tick_bus(&mut self, cycles: Ticks) -> Result<()> {
        if cycles == 0 {
            return Ok(());
        }

        let cycles_taken = self.bus.tick(cycles)?;
        self.cycles += cycles_taken;
        Ok(())
    }

    /// Reads a memory location while ticking peripherals
    /// for the access time.
    fn read_tick(&mut self, addr: Address) -> u8 {
        self.read_tick_kind(addr, ReadKind::Data)
    }

    /// Reads a memory location, saying what the read is for, while
    /// ticking peripherals for the access time.
    fn read_tick_kind(&mut self, addr: Address, kind: ReadKind) -> u8 {
        // Accesses after a trapped read within the same instruction are
        // suppressed; the instruction will be re-executed on resume.
        if self.suspended {
            return 0;
        }

        let v = self.bus.read(addr, kind);
        self.tick_bus(1).unwrap();
        if self.bus.is_suspended() {
            // The bus latched a trap on this read: halt and rewind the
            // instruction (see step()).
            self.suspended = true;
        }
        v
    }

    /// Reads 16-bits from a memory location while ticking
    /// peripherals for the access time.
    /// Address wraps at 16-bits.
    fn read16_tick_a16(&mut self, addr: Address) -> u16 {
        let mut v = self.bus.read(addr, ReadKind::Data) as u16;
        self.tick_bus(1).unwrap();
        let hi_addr = addr & 0xFFFF0000 | Address::from((addr as u16).wrapping_add(1));
        v |= (self.bus.read(hi_addr, ReadKind::Data) as u16) << 8;
        self.tick_bus(1).unwrap();
        v
    }

    /// Reads 16-bits from a memory location with the NMOS 6502
    /// JMP ($12FF)-style page wrap: the high byte is read from the same
    /// page as the low byte.
    fn read16_tick_page_wrap(&mut self, addr: Address) -> u16 {
        let lo = self.bus.read(addr, ReadKind::Data) as u16;
        self.tick_bus(1).unwrap();
        let hi_addr = (addr & !0xFF) | (addr.wrapping_add(1) & 0xFF);
        let hi = self.bus.read(hi_addr, ReadKind::Data) as u16;
        self.tick_bus(1).unwrap();
        lo | hi << 8
    }

    /// Reads 16-bits from a memory location while ticking
    /// peripherals for the access time.
    /// Address wraps at 24-bits.
    fn read16_tick_a24(&mut self, addr: Address) -> u16 {
        let mut v = self.bus.read(addr, ReadKind::Data) as u16;
        self.tick_bus(1).unwrap();
        let hi_addr = addr.wrapping_add(1);
        v |= (self.bus.read(hi_addr, ReadKind::Data) as u16) << 8;
        self.tick_bus(1).unwrap();
        v
    }

    /// Reads 24-bits from a memory location while ticking
    /// peripherals for the access time.
    /// Address wraps at 16-bits.
    #[cfg(feature = "w65816")]
    fn read24_tick_a16(&mut self, addr: Address) -> u32 {
        let mut v = self.bus.read(addr, ReadKind::Data) as u32;
        self.tick_bus(1).unwrap();
        let mid_addr = addr & 0xFFFF0000 | Address::from((addr as u16).wrapping_add(1));
        v |= (self.bus.read(mid_addr, ReadKind::Data) as u32) << 8;
        self.tick_bus(1).unwrap();
        let hi_addr = addr & 0xFFFF0000 | Address::from((addr as u16).wrapping_add(2));
        v |= (self.bus.read(hi_addr, ReadKind::Data) as u32) << 16;
        self.tick_bus(1).unwrap();
        v
    }

    /// Writes a memory location while ticking peripherals
    /// for the access time.
    fn write_tick(&mut self, addr: Address, val: u8) {
        // Accesses after a trapped read within the same instruction are
        // suppressed; the instruction will be re-executed on resume.
        if self.suspended {
            return;
        }

        self.bus.write(addr, val);
        self.tick_bus(1).unwrap();
    }

    /// Writes 16-bit (LE) to a memory location while ticking
    /// peripherals for the access time.
    /// 16-bit address wrap.
    fn write16_tick_a16(&mut self, addr: Address, val: u16) {
        self.bus.write(addr, (val & 0xFF) as u8);
        self.tick_bus(1).unwrap();
        let hi_addr = addr & 0xFFFF0000 | Address::from((addr as u16).wrapping_add(1));
        self.bus.write(hi_addr, (val >> 8) as u8);
        self.tick_bus(1).unwrap();
    }

    /// Writes 16-bit (LE) to a memory location while ticking
    /// peripherals for the access time.
    /// 16-bit address wrap, descending temporal order.
    fn write16_tick_a16_desc(&mut self, addr: Address, val: u16) {
        let hi_addr = addr & 0xFFFF0000 | Address::from((addr as u16).wrapping_add(1));
        self.bus.write(hi_addr, (val >> 8) as u8);
        self.tick_bus(1).unwrap();
        self.bus.write(addr, (val & 0xFF) as u8);
        self.tick_bus(1).unwrap();
    }

    /// Writes 16-bit (LE) to a memory location while ticking
    /// peripherals for the access time.
    /// 24-bit address wrap.
    fn write16_tick_a24(&mut self, addr: Address, val: u16) {
        self.bus.write(addr, (val & 0xFF) as u8);
        self.tick_bus(1).unwrap();
        let hi_addr = addr.wrapping_add(1);
        self.bus.write(hi_addr, (val >> 8) as u8);
        self.tick_bus(1).unwrap();
    }

    /// Writes 16-bit (LE) to a memory location while ticking
    /// peripherals for the access time.
    /// 24-bit address wrap, descending temporal order.
    fn write16_tick_a24_desc(&mut self, addr: Address, val: u16) {
        let hi_addr = addr.wrapping_add(1);
        self.bus.write(hi_addr, (val >> 8) as u8);
        self.tick_bus(1).unwrap();
        self.bus.write(addr, (val & 0xFF) as u8);
        self.tick_bus(1).unwrap();
    }

    /// Writes 16-bit (LE) to a memory location while ticking
    /// peripherals for the access time.
    /// Selects wrap based on addressing mode.
    fn write16_tick_a(&mut self, instr: &Instruction, addr: Address, value: u16) {
        match instr.def.mode {
            AddressingMode::Direct | AddressingMode::DirectX => {
                self.write16_tick_a16(addr, value);
            }
            #[cfg(feature = "w65816")]
            AddressingMode::StackS => {
                self.write16_tick_a16(addr, value);
            }
            _ => self.write16_tick_a24(addr, value),
        }
    }

    /// Writes 16-bit (LE) to a memory location while ticking
    /// peripherals for the access time.
    /// Selects wrap based on addressing mode, descending temporal order.
    fn write16_tick_a_desc(&mut self, instr: &Instruction, addr: Address, value: u16) {
        match instr.def.mode {
            AddressingMode::Direct | AddressingMode::DirectX => {
                self.write16_tick_a16_desc(addr, value);
            }
            #[cfg(feature = "w65816")]
            AddressingMode::StackS => {
                self.write16_tick_a16_desc(addr, value);
            }
            _ => self.write16_tick_a24_desc(addr, value),
        }
    }

    /// Pushes 8-bits onto the stack
    fn push8(&mut self, val: u8) {
        // The 6502 family has a page-1 stack: the pointer wraps within
        // the low byte, keeping the address in $01xx.
        let addr = if self.variant.is_65816() {
            Address::from(self.regs.read_dec(Register::S))
        } else {
            Address::from(0x0100 | self.regs.read_dec(Register::SL))
        };
        self.write_tick(addr, val);
    }

    /// Pulls 8-bits from the stack
    fn pull8(&mut self) -> u8 {
        let addr = if self.variant.is_65816() {
            Address::from(self.regs.read_inc(Register::S).wrapping_add(1))
        } else {
            Address::from(0x0100 | self.regs.read_inc(Register::SL).wrapping_add(1))
        };
        self.read_tick(addr)
    }

    /// Pushes 16-bits onto the stack, MSB-first
    fn push16(&mut self, val: u16) {
        self.push8((val >> 8) as u8);
        self.push8(val as u8);
    }

    /// Pulls 16-bits from the stack
    fn pull16(&mut self) -> u16 {
        let lo = self.pull8() as u16;
        let hi = self.pull8() as u16;
        lo | hi << 8
    }

    /// Call to an interrupt vector entry.
    fn dispatch_interrupt(&mut self, vector_addr: Address, from_brk: bool) -> Result<()> {
        // The card's Gemini fetches the vector from its alternate
        // firmware bank; a no-op for other buses.
        self.bus.set_int_bank(true);
        if self.variant.is_65816() {
            self.push8(self.regs.read8(Register::K));
            self.push16(self.regs.read(Register::PC));
            self.push8(self.regs.read8(Register::P));

            let addr = self.read16_tick_a16(vector_addr);

            self.regs.write(Register::K, 0);
            self.regs.write(Register::PC, addr);
            self.regs.write_flags(&[(Flag::I, true), (Flag::D, false)]);
        } else {
            // 6502/65C02: push PC (hi, lo) and then P.  The B bit is set
            // on the stack copy for BRK (and PHP) only, and bit 5 is
            // hardwired.
            let mut p = self.regs.read8(Register::P) | 0x20;
            if from_brk {
                p |= 0x10;
            } else {
                p &= !0x10;
            }
            self.push16(self.regs.read(Register::PC));
            self.push8(p);

            let addr = self.read16_tick_a16(vector_addr);

            self.regs.write(Register::PC, addr);
            // The 65C02 clears D on interrupt; the NMOS 6502 does not.
            let mut flags = vec![(Flag::I, true)];
            if self.variant == Variant::M65C02 {
                flags.push((Flag::D, false));
            }
            self.regs.write_flags(&flags);
        }
        Ok(())
    }

    /// Executes an instruction.
    fn execute_instruction(&mut self, instr: &Instruction) -> Result<()> {
        match instr.def.instr_type {
            InstructionType::NOP => self.op_nop(instr),
            #[cfg(feature = "w65816")]
            InstructionType::WDM => Ok(()),
            InstructionType::CLC => self.op_clx(Flag::C),
            InstructionType::CLD => self.op_clx(Flag::D),
            InstructionType::CLI => self.op_clx(Flag::I),
            InstructionType::CLV => self.op_clx(Flag::V),
            InstructionType::SEC => self.op_sex(Flag::C),
            InstructionType::SED => self.op_sex(Flag::D),
            InstructionType::SEI => self.op_sex(Flag::I),
            #[cfg(feature = "w65816")]
            InstructionType::XBA => self.op_xba(),
            #[cfg(feature = "w65816")]
            InstructionType::XCE => self.op_xce(),
            #[cfg(feature = "w65816")]
            InstructionType::TCD => self.op_txx_16b(Register::C, Register::D, true),
            #[cfg(feature = "w65816")]
            InstructionType::TCS => self.op_txx_16b(Register::C, Register::S, false),
            #[cfg(feature = "w65816")]
            InstructionType::TDC => self.op_txx_16b(Register::D, Register::C, true),
            #[cfg(feature = "w65816")]
            InstructionType::TSC => self.op_txx_16b(Register::S, Register::C, true),
            InstructionType::TAX => self.op_txx(Register::C, Register::X, Flag::X),
            InstructionType::TAY => self.op_txx(Register::C, Register::Y, Flag::X),
            InstructionType::TSX => self.op_txx(Register::S, Register::X, Flag::X),
            InstructionType::TXA => self.op_txx(Register::X, Register::C, Flag::M),
            InstructionType::TXS => self.op_txs(),
            #[cfg(feature = "w65816")]
            InstructionType::TXY => self.op_txx(Register::X, Register::Y, Flag::X),
            InstructionType::TYA => self.op_txx(Register::Y, Register::C, Flag::M),
            #[cfg(feature = "w65816")]
            InstructionType::TYX => self.op_txx(Register::Y, Register::X, Flag::X),
            InstructionType::STZ => self.op_stz(instr),
            InstructionType::STA => self.op_stx_reg(instr, Register::C, Flag::M),
            InstructionType::STX => self.op_stx_reg(instr, Register::X, Flag::X),
            InstructionType::STY => self.op_stx_reg(instr, Register::Y, Flag::X),
            InstructionType::LDA => self.op_load(instr, Register::C, Flag::M),
            InstructionType::LDX => self.op_load(instr, Register::X, Flag::X),
            InstructionType::LDY => self.op_load(instr, Register::Y, Flag::X),
            #[cfg(feature = "w65816")]
            InstructionType::REP => self.op_rep(instr),
            #[cfg(feature = "w65816")]
            InstructionType::SEP => self.op_sep(instr),
            InstructionType::ADC => self.op_add(instr),
            InstructionType::SBC => self.op_sbc(instr),
            InstructionType::CMP => self.op_compare(instr, Register::C, Flag::M),
            InstructionType::CPX => self.op_compare(instr, Register::X, Flag::X),
            InstructionType::CPY => self.op_compare(instr, Register::Y, Flag::X),
            InstructionType::DEC if instr.def.mode == AddressingMode::Accumulator => {
                self.op_incdec_reg(Register::C, -1, Flag::M)
            }
            InstructionType::DEX => self.op_incdec_reg(Register::X, -1, Flag::X),
            InstructionType::DEY => self.op_incdec_reg(Register::Y, -1, Flag::X),
            InstructionType::INC if instr.def.mode == AddressingMode::Accumulator => {
                self.op_incdec_reg(Register::C, 1, Flag::M)
            }
            InstructionType::INX => self.op_incdec_reg(Register::X, 1, Flag::X),
            InstructionType::INY => self.op_incdec_reg(Register::Y, 1, Flag::X),
            InstructionType::DEC => self.op_incdec(instr, -1),
            InstructionType::INC => self.op_incdec(instr, 1),
            InstructionType::AND => self.op_and(instr),
            InstructionType::EOR => self.op_eor(instr),
            InstructionType::ORA => self.op_ora(instr),
            InstructionType::BIT => self.op_bit(instr),
            InstructionType::TRB => self.op_trb(instr),
            InstructionType::TSB => self.op_tsb(instr),
            InstructionType::RMB => self.op_bit_mem(instr, false),
            InstructionType::SMB => self.op_bit_mem(instr, true),
            InstructionType::BBR => self.op_bit_branch(instr, false),
            InstructionType::BBS => self.op_bit_branch(instr, true),
            InstructionType::ASL if instr.def.mode == AddressingMode::Accumulator => {
                self.op_asl_acc()
            }
            InstructionType::ASL => self.op_asl(instr),
            InstructionType::LSR if instr.def.mode == AddressingMode::Accumulator => {
                self.op_lsr_acc()
            }
            InstructionType::LSR => self.op_lsr(instr),
            InstructionType::ROL if instr.def.mode == AddressingMode::Accumulator => {
                self.op_rol_acc()
            }
            InstructionType::ROL => self.op_rol(instr),
            InstructionType::ROR if instr.def.mode == AddressingMode::Accumulator => {
                self.op_ror_acc()
            }
            InstructionType::ROR => self.op_ror(instr),
            InstructionType::BCS => self.op_branch(instr, self.regs.test_flag(Flag::C)),
            InstructionType::BCC => self.op_branch(instr, !self.regs.test_flag(Flag::C)),
            InstructionType::BEQ => self.op_branch(instr, self.regs.test_flag(Flag::Z)),
            InstructionType::BNE => self.op_branch(instr, !self.regs.test_flag(Flag::Z)),
            InstructionType::BMI => self.op_branch(instr, self.regs.test_flag(Flag::N)),
            InstructionType::BPL => self.op_branch(instr, !self.regs.test_flag(Flag::N)),
            InstructionType::BVS => self.op_branch(instr, self.regs.test_flag(Flag::V)),
            InstructionType::BVC => self.op_branch(instr, !self.regs.test_flag(Flag::V)),
            InstructionType::BRA => self.op_branch(instr, true),
            #[cfg(feature = "w65816")]
            InstructionType::BRL => self.op_branch_long(instr),
            InstructionType::JMP => self.op_jump(instr),
            InstructionType::PHA => self.op_push_reg_flag(Register::C, Flag::M),
            InstructionType::PHX => self.op_push_reg_flag(Register::X, Flag::X),
            InstructionType::PHY => self.op_push_reg_flag(Register::Y, Flag::X),
            #[cfg(feature = "w65816")]
            InstructionType::PHB => self.op_push_reg(Register::DBR),
            #[cfg(feature = "w65816")]
            InstructionType::PHD => self.op_push_reg(Register::D),
            #[cfg(feature = "w65816")]
            InstructionType::PHK => self.op_push_reg(Register::K),
            InstructionType::PHP => self.op_php(),
            #[cfg(feature = "w65816")]
            InstructionType::PLB => self.op_pull_reg(Register::DBR),
            #[cfg(feature = "w65816")]
            InstructionType::PLD => self.op_pull_reg(Register::D),
            InstructionType::PLP => self.op_plp(),
            InstructionType::PLA => self.op_pull_reg_flag(Register::C, Flag::M),
            InstructionType::PLX => self.op_pull_reg_flag(Register::X, Flag::X),
            InstructionType::PLY => self.op_pull_reg_flag(Register::Y, Flag::X),
            #[cfg(feature = "w65816")]
            InstructionType::JSL => self.op_jsl(instr),
            InstructionType::JSR => self.op_jsr(instr),
            InstructionType::RTS => self.op_rts(),
            #[cfg(feature = "w65816")]
            InstructionType::RTL => self.op_rtl(),
            InstructionType::BRK => self.op_swint(instr, self.intvec_brk),
            #[cfg(feature = "w65816")]
            InstructionType::COP => self.op_swint(instr, self.intvec_cop),
            InstructionType::RTI => self.op_rti(),
            InstructionType::STP => self.op_stp(),
            InstructionType::WAI => self.op_wai(),
            #[cfg(feature = "w65816")]
            InstructionType::PEI => self.op_push(instr),
            #[cfg(feature = "w65816")]
            InstructionType::PEA => self.op_push_imm(instr),
            #[cfg(feature = "w65816")]
            InstructionType::PER => self.op_push_addr(instr),
            #[cfg(feature = "w65816")]
            InstructionType::MVN => self.op_move(instr, false),
            #[cfg(feature = "w65816")]
            InstructionType::MVP => self.op_move(instr, true),

            InstructionType::Undefined => panic!("Undefined instruction encountered"),
        }
    }

    /// Resolves an address from instruction data, registers, etc.
    /// based on the addressing mode and consumes cycles.
    /// Returns the address and whether the access crosses a page.
    ///
    /// `abs_extra_cycle`/`pb_extra_cycle` mirror siena: extra cycles
    /// consumed before/after address resolution.  `rmw` distinguishes
    /// plain accesses from read-modify-write instructions, whose
    /// 6502/65C02 abs-indexed fixup cycles differ from loads/stores.
    fn resolve_address(
        &mut self,
        instr: &Instruction,
        abs_extra_cycle: bool,
        pb_extra_cycle: bool,
        rmw: Rmw,
    ) -> Result<(Address, bool)> {
        // Internal cycles for adding D, if applicable (D > 0).
        if self.regs.read(Register::DL) != 0 {
            match instr.def.mode {
                AddressingMode::Direct
                | AddressingMode::DirectPtr16
                | AddressingMode::DirectPtr16Y
                | AddressingMode::DirectXPtr16
                | AddressingMode::DirectX
                | AddressingMode::DirectY => self.tick_bus(1)?,
                #[cfg(feature = "w65816")]
                AddressingMode::DirectPtr24 | AddressingMode::DirectPtr24Y => self.tick_bus(1)?,
                _ => (),
            };
        }

        // Extra cycles for modes that add a register to the address
        // before resolution.  On the 6502/65C02 the cycle that adds the
        // index register to a zero-page address performs a dummy read
        // of the zp operand address; the 65816 uses an internal cycle.
        match instr.def.mode {
            AddressingMode::DirectX | AddressingMode::DirectXPtr16 | AddressingMode::DirectY => {
                if self.variant.is_65816() {
                    self.tick_bus(1)?;
                } else {
                    self.read_tick(Address::from(
                        self.regs
                            .read(Register::D)
                            .wrapping_add(instr.imm::<u16>()?),
                    ));
                }
            }
            #[cfg(feature = "w65816")]
            AddressingMode::StackS | AddressingMode::StackSPtr16Y => self.tick_bus(1)?,
            AddressingMode::AbsoluteXPtr16 => {
                // JMP (abs,X): dummy re-read of the operand's high byte
                // before the pointer reads. For the 65C02 that is the
                // W65C02S datasheet (table 6-5) and real chip captures
                // (6502Decoder); the 65x02 suite has it at the low byte.
                match self.variant {
                    Variant::M65C02 => {
                        self.read_tick(Address::from(self.regs.pc.wrapping_sub(1)));
                    }
                    #[cfg(feature = "w65816")]
                    Variant::W65816 => {
                        let dummy = (Address::from(self.regs.k) << 16)
                            | Address::from(self.regs.pc.wrapping_sub(1));
                        self.read_tick(dummy);
                    }
                    Variant::M6502 => unreachable!(),
                }
            }
            _ => (),
        }

        // The 6502 and 65C02 wrap addresses at 16 bits; the 65816 wraps
        // at bank boundaries.
        let addr_mask = if self.variant.is_65816() {
            ADDRESS_MASK
        } else {
            0xFFFF
        };

        let idx = |base: Address, idx: Address| {
            (
                base.wrapping_add(idx) & addr_mask,
                base & !0xFF != base.wrapping_add(idx) & addr_mask & !0xFF,
            )
        };
        let noidx = |base: Address| (base, false);

        // The 6502 and 65C02 wrap zero-page indexed addressing within
        // the zero page; the 65816 wraps only at bank boundaries.
        let zp_wrap = |v: u16, variant: Variant| {
            if variant.is_65816() { v } else { v & 0xFF }
        };

        // The pre-fixup address of an indexed access: the low byte has
        // the index added, the high byte not yet corrected.  This is
        // the address the NMOS 6502 puts on the bus for its page-cross
        // dummy read.
        let pre_fixup =
            |base: Address, index: Address| (base & !0xFF) | (base.wrapping_add(index) & 0xFF);

        let (address, crosses_page, wrong_addr) = match instr.def.mode {
            AddressingMode::Direct => {
                let a = noidx(Address::from(
                    self.regs
                        .read(Register::D)
                        .wrapping_add(instr.imm::<u16>()?),
                ));
                (a.0, a.1, None)
            }
            AddressingMode::DirectX => {
                let a = noidx(Address::from(zp_wrap(
                    self.regs
                        .read(Register::D)
                        .wrapping_add(instr.imm::<u16>()?)
                        .wrapping_add(self.regs.read(Register::X)),
                    self.variant,
                )));
                (a.0, a.1, None)
            }
            AddressingMode::DirectY => {
                let a = noidx(Address::from(zp_wrap(
                    self.regs
                        .read(Register::D)
                        .wrapping_add(instr.imm::<u16>()?)
                        .wrapping_add(self.regs.read(Register::Y)),
                    self.variant,
                )));
                (a.0, a.1, None)
            }
            AddressingMode::DirectPtr16 => {
                // The 65C02 reads the (zp) pointer within the zero page.
                let zp = Address::from(
                    self.regs
                        .read(Register::D)
                        .wrapping_add(instr.imm::<u16>()?),
                );
                let ptr = if self.variant.is_65816() {
                    self.read16_tick_a16(zp)
                } else {
                    self.read16_tick_page_wrap(zp)
                };
                let a = noidx(
                    (Address::from(self.regs.read(Register::DBR)) << 16) | Address::from(ptr),
                );
                (a.0, a.1, None)
            }
            #[cfg(feature = "w65816")]
            AddressingMode::DirectPtr24 => {
                let a = noidx(Address::from(
                    self.read24_tick_a16(Address::from(
                        self.regs
                            .read(Register::D)
                            .wrapping_add(instr.imm::<u16>()?),
                    )),
                ));
                (a.0, a.1, None)
            }
            AddressingMode::DirectPtr16Y => {
                // The 6502 and 65C02 read the (zp),Y pointer within
                // the zero page, like JMP ($xxFF).
                let zp = Address::from(
                    self.regs
                        .read(Register::D)
                        .wrapping_add(instr.imm::<u16>()?),
                );
                let ptr = if self.variant.is_65816() {
                    self.read16_tick_a16(zp)
                } else {
                    self.read16_tick_page_wrap(zp)
                };
                let base =
                    (Address::from(self.regs.read(Register::DBR)) << 16) | Address::from(ptr);
                let (addr, crosses) = idx(base, self.regs.read(Register::Y).into());
                (
                    addr,
                    crosses,
                    Some(pre_fixup(base, self.regs.read(Register::Y).into())),
                )
            }
            #[cfg(feature = "w65816")]
            AddressingMode::DirectPtr24Y => {
                let base = Address::from(
                    self.read24_tick_a16(Address::from(
                        self.regs
                            .read(Register::D)
                            .wrapping_add(instr.imm::<u16>()?),
                    )),
                );
                let (addr, crosses) = idx(base, self.regs.read(Register::Y).into());
                (
                    addr,
                    crosses,
                    Some(pre_fixup(base, self.regs.read(Register::Y).into())),
                )
            }
            AddressingMode::DirectXPtr16 => {
                // The 6502 and 65C02 read the (zp,X) pointer within the
                // zero page, like JMP ($xxFF).
                let zp_addr = Address::from(zp_wrap(
                    self.regs
                        .read(Register::D)
                        .wrapping_add(instr.imm::<u16>()?)
                        .wrapping_add(self.regs.read(Register::X)),
                    self.variant,
                ));
                let ptr = if self.variant.is_65816() {
                    self.read16_tick_a16(zp_addr)
                } else {
                    self.read16_tick_page_wrap(zp_addr)
                };
                let a = noidx(
                    (Address::from(self.regs.read(Register::DBR)) << 16) | Address::from(ptr),
                );
                (a.0, a.1, None)
            }
            AddressingMode::Absolute => {
                let a = noidx(
                    Address::from(self.regs.read(Register::DBR)) << 16
                        | Address::from(instr.imm::<u16>()?),
                );
                (a.0, a.1, None)
            }
            AddressingMode::AbsoluteX => {
                let base = Address::from(self.regs.read(Register::DBR)) << 16
                    | Address::from(instr.imm::<u16>()?);
                let (addr, crosses) = idx(base, self.regs.read(Register::X).into());
                (
                    addr,
                    crosses,
                    Some(pre_fixup(base, self.regs.read(Register::X).into())),
                )
            }
            AddressingMode::AbsoluteY => {
                let base = Address::from(self.regs.read(Register::DBR)) << 16
                    | Address::from(instr.imm::<u16>()?);
                let (addr, crosses) = idx(base, self.regs.read(Register::Y).into());
                (
                    addr,
                    crosses,
                    Some(pre_fixup(base, self.regs.read(Register::Y).into())),
                )
            }
            AddressingMode::AbsolutePtr16 => {
                // NMOS 6502: JMP ($12FF) reads the high byte from the
                // same page.
                let ptr = if self.variant == Variant::M6502 {
                    self.read16_tick_page_wrap(Address::from(instr.imm::<u16>()?))
                } else if self.variant == Variant::M65C02 {
                    // W65C02S datasheet table 6-5: a dummy read of the
                    // operand's high byte, then the pointer, without the
                    // page wrap. The 65x02 suite (and MAME) put the dummy
                    // read between the pointer bytes instead, which real
                    // chip captures (6502Decoder) contradict.
                    let pc = self.regs.read(Register::PC);
                    self.read_tick(Address::from(pc.wrapping_sub(1)));
                    self.read16_tick_a16(Address::from(instr.imm::<u16>()?))
                } else {
                    self.read16_tick_a16(Address::from(instr.imm::<u16>()?))
                };
                let a =
                    noidx(Address::from(self.regs.read(Register::DBR)) << 16 | Address::from(ptr));
                (a.0, a.1, None)
            }
            AddressingMode::AbsoluteXPtr16 => {
                let a = noidx(Address::from(
                    self.read16_tick_a16(
                        Address::from(self.regs.read(Register::K)) << 16
                            | Address::from(
                                instr
                                    .imm::<u16>()?
                                    .wrapping_add(self.regs.read(Register::X)),
                            ),
                    ),
                ));
                (a.0, a.1, None)
            }
            #[cfg(feature = "w65816")]
            AddressingMode::AbsolutePtr24 => {
                let a = noidx(Address::from(
                    self.read24_tick_a16(Address::from(instr.imm::<u16>()?)),
                ));
                (a.0, a.1, None)
            }
            #[cfg(feature = "w65816")]
            AddressingMode::StackS => {
                let a = noidx(Address::from(
                    self.regs
                        .read(Register::S)
                        .wrapping_add(instr.imm::<u16>()?),
                ));
                (a.0, a.1, None)
            }
            #[cfg(feature = "w65816")]
            AddressingMode::StackSPtr16Y => {
                let a = noidx(
                    (Address::from(self.regs.read(Register::DBR)) << 16
                        | Address::from(
                            self.read16_tick_a16(Address::from(
                                self.regs
                                    .read(Register::S)
                                    .wrapping_add(instr.imm::<u16>()?),
                            )),
                        ))
                    .wrapping_add(self.regs.read(Register::Y).into())
                        & ADDRESS_MASK,
                );
                (a.0, a.1, None)
            }
            #[cfg(feature = "w65816")]
            AddressingMode::Long => {
                let a = noidx(instr.imm::<u32>()? & ADDRESS_MASK);
                (a.0, a.1, None)
            }
            #[cfg(feature = "w65816")]
            AddressingMode::LongX => {
                let a = noidx(
                    instr
                        .imm::<u32>()?
                        .wrapping_add(self.regs.read(Register::X).into())
                        & ADDRESS_MASK,
                );
                (a.0, a.1, None)
            }
            AddressingMode::Relative8 => {
                let base = self.regs.pc;
                let target = base.wrapping_add_signed(instr.imm::<u8>()? as i8 as i16);
                let crosses = (base ^ target) & 0xFF00 != 0;
                (
                    (Address::from(self.regs.k) << 16) | Address::from(target),
                    crosses,
                    None,
                )
            }
            #[cfg(feature = "w65816")]
            AddressingMode::Relative16 => (
                (Address::from(self.regs.k) << 16)
                    | Address::from(self.regs.pc.wrapping_add_signed(instr.imm::<u16>()? as i16)),
                false,
                None,
            ),

            AddressingMode::Accumulator
            | AddressingMode::Implied
            | AddressingMode::Immediate8
            | AddressingMode::DirectRelative => unreachable!(),
            #[cfg(feature = "w65816")]
            AddressingMode::Immediate16
            | AddressingMode::ImmediateM
            | AddressingMode::ImmediateX
            | AddressingMode::SrcDest => unreachable!(),
        };

        if abs_extra_cycle {
            // Extra cycles for modes that add a register to the
            // address after fetching it.
            match instr.def.mode {
                AddressingMode::AbsoluteX | AddressingMode::AbsoluteY => {
                    if self.variant.is_65816() {
                        self.tick_bus(1)?;
                    } else if rmw == Rmw::Shift && !crosses_page && self.variant == Variant::M6502 {
                        // NMOS RMW without a crossing: internal
                        // index-add cycle (the 65C02 has none).
                        self.tick_bus(1)?;
                    } else if rmw == Rmw::No || rmw == Rmw::IncDec || crosses_page {
                        // 6502/65C02 stores and INC/DEC (always), and
                        // RMW on a page crossing: dummy read.  The
                        // NMOS reads the pre-fixup address, the 65C02
                        // re-reads the last operand byte.
                        match self.variant {
                            Variant::M6502 => {
                                self.read_tick(wrong_addr.expect("indexed address"));
                            }
                            Variant::M65C02 => {
                                self.read_tick(Address::from(self.regs.pc.wrapping_sub(1)));
                            }
                            #[cfg(feature = "w65816")]
                            Variant::W65816 => unreachable!(),
                        }
                    }
                }
                _ => (),
            }
        }

        if pb_extra_cycle {
            // Extra cycle if crossing a page boundary || !X
            match instr.def.mode {
                AddressingMode::AbsoluteX
                | AddressingMode::AbsoluteY
                | AddressingMode::DirectPtr16Y => {
                    if self.variant.is_65816() {
                        if !self.width8(Flag::X) || crosses_page {
                            self.tick_bus(1)?;
                        }
                    } else if crosses_page {
                        // 6502/65C02 loads: dummy read when crossing a
                        // page (NMOS reads the pre-fixup address, the
                        // 65C02 re-reads the last operand byte).
                        match self.variant {
                            Variant::M6502 => {
                                self.read_tick(wrong_addr.expect("indexed address"));
                            }
                            Variant::M65C02 => {
                                self.read_tick(Address::from(self.regs.pc.wrapping_sub(1)));
                            }
                            #[cfg(feature = "w65816")]
                            Variant::W65816 => unreachable!(),
                        }
                    }
                }
                _ => (),
            }
        } else {
            // More cycles for modes that do stuff AFTER address
            // resolution bus activity.  (zp),Y stores: the 65816 takes
            // an internal cycle, the 6502/65C02 a dummy read (NMOS:
            // the pre-fixup address; 65C02: the last operand byte).
            match instr.def.mode {
                AddressingMode::DirectPtr16Y => match self.variant {
                    #[cfg(feature = "w65816")]
                    Variant::W65816 => self.tick_bus(1)?,
                    Variant::M6502 => {
                        self.read_tick(wrong_addr.expect("indexed address"));
                    }
                    Variant::M65C02 => {
                        self.read_tick(Address::from(self.regs.pc.wrapping_sub(1)));
                    }
                },
                _ => (),
            }
        }
        #[cfg(feature = "w65816")]
        if instr.def.mode == AddressingMode::StackSPtr16Y {
            self.tick_bus(1)?;
        }

        Ok((address, crosses_page))
    }

    /// Retrieves address and fetches instruction data.
    fn fetch_data(
        &mut self,
        instr: &Instruction,
        abs_extra_cycle: bool,
        pb_extra_cycle: bool,
        rmw: Rmw,
        width_flag: Flag,
    ) -> Result<(u16, Address)> {
        Ok(match instr.def.mode {
            AddressingMode::Immediate8 => (instr.imm::<u16>()?, Address::MAX),
            #[cfg(feature = "w65816")]
            AddressingMode::Immediate16
            | AddressingMode::ImmediateM
            | AddressingMode::ImmediateX => (instr.imm::<u16>()?, Address::MAX),
            _ => {
                let (addr, _) =
                    self.resolve_address(instr, abs_extra_cycle, pb_extra_cycle, rmw)?;

                (
                    if self.width8(width_flag) {
                        // 8-bit mode
                        self.read_tick(addr).into()
                    } else {
                        // 16-bit mode
                        match instr.def.mode {
                            AddressingMode::Direct | AddressingMode::DirectX => {
                                self.read16_tick_a16(addr)
                            }
                            #[cfg(feature = "w65816")]
                            AddressingMode::StackS => self.read16_tick_a16(addr),
                            _ => self.read16_tick_a24(addr),
                        }
                    },
                    addr,
                )
            }
        })
    }
    /// CLx - Clear x
    fn op_clx(&mut self, f: Flag) -> Result<()> {
        self.regs.write_flags(&[(f, false)]);
        self.tick_bus(1)
    }

    /// SEx - Set x
    fn op_sex(&mut self, f: Flag) -> Result<()> {
        self.regs.write_flags(&[(f, true)]);
        self.tick_bus(1)
    }

    /// XCE - Exchange Carry and Emulation
    #[cfg(feature = "w65816")]
    fn op_xce(&mut self) -> Result<()> {
        let c = self.regs.test_flag(Flag::C);
        self.regs.write_flags(&[(Flag::C, self.regs.emulation)]);
        self.regs.emulation = c;
        if self.regs.emulation {
            self.regs.write_flags(&[(Flag::M, true), (Flag::X, true)]);
            self.regs.write(Register::XH, 0);
            self.regs.write(Register::YH, 0);
            self.regs.write(Register::SH, 1);
        }
        self.tick_bus(1)
    }

    /// XBA - Exchange B and A
    #[cfg(feature = "w65816")]
    fn op_xba(&mut self) -> Result<()> {
        let c = self.regs.read(Register::C);
        let result = c.rotate_left(8);
        self.regs.write(Register::C, result);

        self.regs
            .write_flags(&[(Flag::Z, result & 0xFF == 0), (Flag::N, result & 0x80 != 0)]);
        self.tick_bus(2)
    }

    /// Txx - Transfer some register to another register
    /// (always 16-bit)
    fn op_txx_16b(&mut self, from: Register, to: Register, flags: bool) -> Result<()> {
        let v = self.regs.read(from);
        self.regs.write(to, v);

        if flags {
            self.regs
                .write_flags(&[(Flag::N, v & 0x8000 != 0), (Flag::Z, v == 0)]);
        }
        self.tick_bus(1)
    }

    /// Txx - Transfer some register to another register
    /// (obeying X/M flags)
    fn op_txx(&mut self, from: Register, to: Register, flag: Flag) -> Result<()> {
        let v = self.regs.read(from);
        if self.width8(flag) {
            // 8-bit mode
            self.regs
                .write(to, (self.regs.read(to) & 0xFF00) | (v & 0xFF));
            self.regs
                .write_flags(&[(Flag::N, v & 0x80 != 0), (Flag::Z, v & 0xFF == 0)]);
        } else {
            // 16-bit mode
            self.regs.write(to, v);
            self.regs
                .write_flags(&[(Flag::N, v & 0x8000 != 0), (Flag::Z, v == 0)]);
        }
        self.tick_bus(1)
    }

    /// TXS - Transfer X to Stack pointer
    ///
    /// The 6502 family has a page-1 stack: S = $0100 | X.
    fn op_txs(&mut self) -> Result<()> {
        if self.variant.is_65816() {
            self.op_txx_16b(Register::X, Register::S, false)
        } else {
            self.regs
                .write(Register::S, 0x0100 | (self.regs.read(Register::X) & 0xFF));
            self.tick_bus(1)
        }
    }

    /// STZ - Store zero
    fn op_stz(&mut self, instr: &Instruction) -> Result<()> {
        self.op_store(instr, 0, Flag::M)
    }

    /// STA/STX/STY - Store register
    fn op_stx_reg(&mut self, instr: &Instruction, reg: Register, flag: Flag) -> Result<()> {
        self.op_store(instr, self.regs.read(reg), flag)
    }

    /// Store operations
    fn op_store(&mut self, instr: &Instruction, value: u16, flag: Flag) -> Result<()> {
        let (addr, _) = self.resolve_address(instr, true, false, Rmw::No)?;

        if self.width8(flag) {
            self.write_tick(addr, value as u8);
        } else {
            self.write16_tick_a(instr, addr, value);
        }

        Ok(())
    }

    /// Load operations
    fn op_load(&mut self, instr: &Instruction, destreg: Register, flag: Flag) -> Result<()> {
        let (val, _) = self.fetch_data(instr, false, true, Rmw::No, flag)?;

        if self.width8(flag) {
            // 8-bit mode
            let regval = self.regs.read(destreg);
            self.regs.write(destreg, regval & 0xFF00 | val);
            self.regs
                .write_flags(&[(Flag::N, val & 0x80 != 0), (Flag::Z, val == 0)]);
        } else {
            // 16-bit mode
            self.regs.write(destreg, val);
            self.regs
                .write_flags(&[(Flag::N, val & 0x8000 != 0), (Flag::Z, val == 0)]);
        }

        Ok(())
    }

    /// REP - REset P
    #[cfg(feature = "w65816")]
    fn op_rep(&mut self, instr: &Instruction) -> Result<()> {
        let mask = instr.imm::<u8>()?;
        let p = self.regs.read8(Register::P);
        self.regs.write(Register::P, (p & !mask).into());
        self.tick_bus(1)?;
        Ok(())
    }

    /// SEP - SEt P
    #[cfg(feature = "w65816")]
    fn op_sep(&mut self, instr: &Instruction) -> Result<()> {
        let mask = instr.imm::<u8>()?;
        let p = self.regs.read8(Register::P);
        self.regs.write(Register::P, (p | mask).into());
        self.tick_bus(1)?;
        Ok(())
    }

    /// ADC - Add (accumulator)
    fn op_add(&mut self, instr: &Instruction) -> Result<()> {
        let (data, addr) = self.fetch_data(instr, false, true, Rmw::No, Flag::M)?;

        // The WDC 65C02 takes an extra cycle for decimal ADC (a dummy
        // read of the operand; the immediate modes use an internal
        // cycle).
        if self.variant == Variant::M65C02 && self.regs.test_flag(Flag::D) {
            match instr.def.mode {
                AddressingMode::Immediate8 => self.tick_bus(1)?,
                #[cfg(feature = "w65816")]
                AddressingMode::Immediate16
                | AddressingMode::ImmediateM
                | AddressingMode::ImmediateX => self.tick_bus(1)?,
                _ => {
                    self.read_tick(addr);
                }
            }
        }

        let result = match (self.regs.test_flag(Flag::D), self.width8(Flag::M)) {
            (true, false) => alu::add16_dec(
                self.regs.read(Register::C),
                data,
                self.regs.test_flag(Flag::C),
            ),
            (false, false) => alu::add16(
                self.regs.read(Register::C),
                data,
                self.regs.test_flag(Flag::C),
            ),
            (false, true) => alu::add8(
                self.regs.read(Register::A),
                data,
                self.regs.test_flag(Flag::C),
            ),
            (true, true) => {
                if self.variant == Variant::M6502 {
                    // NMOS 6502: N/V latched before the high-nibble
                    // correction, Z from the binary sum.
                    alu::add8_dec_nmos(
                        self.regs.read(Register::A),
                        data,
                        self.regs.test_flag(Flag::C),
                    )
                } else {
                    alu::add8_dec(
                        self.regs.read(Register::A),
                        data,
                        self.regs.test_flag(Flag::C),
                    )
                }
            }
        };

        if self.width8(Flag::M) {
            // 8-bit
            self.regs.write(Register::A, result.result);
        } else {
            // 16-bit
            self.regs.write(Register::C, result.result);
        }
        self.regs.write_flags(&[
            (Flag::N, result.n),
            (Flag::V, result.v),
            (Flag::Z, result.z),
            (Flag::C, result.c),
        ]);

        Ok(())
    }

    /// SBC - SuBtract (accumulator)
    fn op_sbc(&mut self, instr: &Instruction) -> Result<()> {
        let (data, addr) = self.fetch_data(instr, false, true, Rmw::No, Flag::M)?;

        // The WDC 65C02 takes an extra cycle for decimal SBC (a dummy
        // read of the operand; the immediate modes use an internal
        // cycle).
        if self.variant == Variant::M65C02 && self.regs.test_flag(Flag::D) {
            match instr.def.mode {
                AddressingMode::Immediate8 => self.tick_bus(1)?,
                #[cfg(feature = "w65816")]
                AddressingMode::Immediate16
                | AddressingMode::ImmediateM
                | AddressingMode::ImmediateX => self.tick_bus(1)?,
                _ => {
                    self.read_tick(addr);
                }
            }
        }

        let result = match (self.regs.test_flag(Flag::D), self.width8(Flag::M)) {
            (true, false) => alu::sub16_dec(
                self.regs.read(Register::C),
                data,
                self.regs.test_flag(Flag::C),
            ),
            (false, false) => alu::sub16(
                self.regs.read(Register::C),
                data,
                self.regs.test_flag(Flag::C),
            ),
            (false, true) => alu::sub8(
                self.regs.read(Register::A),
                data,
                self.regs.test_flag(Flag::C),
            ),
            (true, true) => {
                if self.variant == Variant::M6502 {
                    // NMOS 6502: quirky decimal SBC (see alu.rs).
                    alu::sub8_dec_nmos(
                        self.regs.read(Register::A),
                        data,
                        self.regs.test_flag(Flag::C),
                    )
                } else if self.variant == Variant::M65C02 {
                    // WDC 65C02: quirky decimal SBC (see alu.rs).
                    alu::sub8_dec_wdc65c02(
                        self.regs.read(Register::A),
                        data,
                        self.regs.test_flag(Flag::C),
                    )
                } else {
                    alu::sub8_dec(
                        self.regs.read(Register::A),
                        data,
                        self.regs.test_flag(Flag::C),
                    )
                }
            }
        };

        if self.width8(Flag::M) {
            // 8-bit
            self.regs.write(Register::A, result.result);
        } else {
            // 16-bit
            self.regs.write(Register::C, result.result);
        }
        self.regs.write_flags(&[
            (Flag::N, result.n),
            (Flag::V, result.v),
            (Flag::Z, result.z),
            (Flag::C, result.c),
        ]);

        Ok(())
    }

    /// CMP/CPX/CPY - Compare
    fn op_compare(&mut self, instr: &Instruction, to: Register, width_flag: Flag) -> Result<()> {
        let (data, _) = self.fetch_data(instr, false, true, Rmw::No, width_flag)?;
        let val = self.regs.read(to);

        let result = if self.width8(width_flag) {
            alu::sub8(val & 0xFF, data & 0xFF, true)
        } else {
            alu::sub16(val, data, true)
        };

        self.regs.write_flags(&[
            (Flag::N, result.n),
            (Flag::C, result.c),
            (Flag::Z, result.z),
        ]);

        Ok(())
    }

    /// Increment/decrement (register variant)
    fn op_incdec_reg(&mut self, reg: Register, data: i32, width_flag: Flag) -> Result<()> {
        let val = self.regs.read(reg);
        if self.width8(width_flag) {
            // 8-bit
            let result = (((val & 0xFF) as i32 + data) & 0xFF) as u16;
            self.regs.write(reg, val & 0xFF00 | result);
            self.regs
                .write_flags(&[(Flag::Z, result == 0), (Flag::N, result & 0x80 != 0)]);
        } else {
            // 16-bit
            let result = (val as i32 + data) as u16;
            self.regs.write(reg, result);
            self.regs
                .write_flags(&[(Flag::Z, result == 0), (Flag::N, result & 0x8000 != 0)]);
        }

        self.tick_bus(1)
    }

    /// Increment/decrement (memory variant)
    fn op_incdec(&mut self, instr: &Instruction, data: i32) -> Result<()> {
        let (val, addr) = self.fetch_data(instr, true, false, Rmw::IncDec, Flag::M)?;

        // Calculation cycle: the NMOS 6502 re-writes the original
        // value, the 65C02 re-reads it, the 65816 takes an internal
        // cycle.
        match self.variant {
            Variant::M6502 => self.write_tick(addr, val as u8),
            Variant::M65C02 => {
                self.read_tick(addr);
            }
            #[cfg(feature = "w65816")]
            Variant::W65816 => self.tick_bus(1)?,
        }

        if self.width8(Flag::M) {
            // 8-bit
            let result = (((val & 0xFF) as i32 + data) & 0xFF) as u8;
            self.write_tick(addr, result);
            self.regs
                .write_flags(&[(Flag::Z, result == 0), (Flag::N, result & 0x80 != 0)]);
        } else {
            // 16-bit
            let result = (val as i32 + data) as u16;
            self.write16_tick_a_desc(instr, addr, result);
            self.regs
                .write_flags(&[(Flag::Z, result == 0), (Flag::N, result & 0x8000 != 0)]);
        }

        Ok(())
    }

    /// AND - Bitwise AND
    fn op_and(&mut self, instr: &Instruction) -> Result<()> {
        let val = self.regs.read(Register::C);
        let (data, _) = self.fetch_data(instr, false, true, Rmw::No, Flag::M)?;
        if self.width8(Flag::M) {
            // 8-bit
            let result = val & data & 0xFF;
            self.regs.write(Register::A, result);
            self.regs
                .write_flags(&[(Flag::Z, result == 0), (Flag::N, result & 0x80 != 0)]);
        } else {
            // 16-bit
            let result = val & data;
            self.regs.write(Register::C, result);
            self.regs
                .write_flags(&[(Flag::Z, result == 0), (Flag::N, result & 0x8000 != 0)]);
        }

        Ok(())
    }

    /// EOR - Bitwise Exclusive Or
    fn op_eor(&mut self, instr: &Instruction) -> Result<()> {
        let val = self.regs.read(Register::C);
        let (data, _) = self.fetch_data(instr, false, true, Rmw::No, Flag::M)?;
        if self.width8(Flag::M) {
            // 8-bit
            let result = (val ^ data) & 0xFF;
            self.regs.write(Register::A, result);
            self.regs
                .write_flags(&[(Flag::Z, result == 0), (Flag::N, result & 0x80 != 0)]);
        } else {
            // 16-bit
            let result = val ^ data;
            self.regs.write(Register::C, result);
            self.regs
                .write_flags(&[(Flag::Z, result == 0), (Flag::N, result & 0x8000 != 0)]);
        }

        Ok(())
    }

    /// ORA - Bitwise Or
    fn op_ora(&mut self, instr: &Instruction) -> Result<()> {
        let val = self.regs.read(Register::C);
        let (data, _) = self.fetch_data(instr, false, true, Rmw::No, Flag::M)?;
        if self.width8(Flag::M) {
            // 8-bit
            let result = (val | data) & 0xFF;
            self.regs.write(Register::A, result);
            self.regs
                .write_flags(&[(Flag::Z, result == 0), (Flag::N, result & 0x80 != 0)]);
        } else {
            // 16-bit
            let result = val | data;
            self.regs.write(Register::C, result);
            self.regs
                .write_flags(&[(Flag::Z, result == 0), (Flag::N, result & 0x8000 != 0)]);
        }

        Ok(())
    }

    /// BIT - Test bits
    fn op_bit(&mut self, instr: &Instruction) -> Result<()> {
        let val = self.regs.read(Register::C);
        let (data, _) = self.fetch_data(instr, false, true, Rmw::No, Flag::M)?;
        if self.width8(Flag::M) {
            // 8-bit
            let result = val & data & 0xFF;

            match instr.def.mode {
                AddressingMode::Immediate8 => {
                    self.regs.write_flags(&[(Flag::Z, result == 0)]);
                }
                #[cfg(feature = "w65816")]
                AddressingMode::ImmediateM => {
                    self.regs.write_flags(&[(Flag::Z, result == 0)]);
                }
                _ => self.regs.write_flags(&[
                    (Flag::Z, result == 0),
                    (Flag::N, data & 0x80 != 0),
                    (Flag::V, data & 0x40 != 0),
                ]),
            }
        } else {
            // 16-bit
            let result = val & data;
            match instr.def.mode {
                AddressingMode::Immediate8 => {
                    self.regs.write_flags(&[(Flag::Z, result == 0)]);
                }
                #[cfg(feature = "w65816")]
                AddressingMode::ImmediateM => {
                    self.regs.write_flags(&[(Flag::Z, result == 0)]);
                }
                _ => self.regs.write_flags(&[
                    (Flag::Z, result == 0),
                    (Flag::N, data & 0x8000 != 0),
                    (Flag::V, data & 0x4000 != 0),
                ]),
            }
        }

        Ok(())
    }

    /// TRB - Test and Reset bits
    fn op_trb(&mut self, instr: &Instruction) -> Result<()> {
        let val = self.regs.read(Register::C);
        let (data, addr) = self.fetch_data(instr, false, true, Rmw::No, Flag::M)?;

        // Calculation cycle: the 65C02 re-reads the operand, the
        // 65816 takes an internal cycle.
        match self.variant {
            Variant::M65C02 => {
                self.read_tick(addr);
            }
            #[cfg(feature = "w65816")]
            Variant::W65816 => self.tick_bus(1)?,
            Variant::M6502 => unreachable!(),
        }

        if self.width8(Flag::M) {
            // 8-bit
            let result = val as u8 & data as u8;
            self.regs.write_flags(&[(Flag::Z, result == 0)]);
            self.write_tick(addr, data as u8 & !val as u8);
        } else {
            // 16-bit
            let result = val & data;
            self.regs.write_flags(&[(Flag::Z, result == 0)]);
            self.write16_tick_a_desc(instr, addr, data & !val);
        }

        Ok(())
    }

    /// TSB - Test and Set bits
    fn op_tsb(&mut self, instr: &Instruction) -> Result<()> {
        let val = self.regs.read(Register::C);
        let (data, addr) = self.fetch_data(instr, false, true, Rmw::No, Flag::M)?;

        // Calculation cycle: the 65C02 re-reads the operand, the
        // 65816 takes an internal cycle.
        match self.variant {
            Variant::M65C02 => {
                self.read_tick(addr);
            }
            #[cfg(feature = "w65816")]
            Variant::W65816 => self.tick_bus(1)?,
            Variant::M6502 => unreachable!(),
        }

        if self.width8(Flag::M) {
            // 8-bit
            let result = (val as u8) & (data as u8);
            self.regs.write_flags(&[(Flag::Z, result == 0)]);
            self.write_tick(addr, data as u8 | val as u8);
        } else {
            // 16-bit
            let result = val & data;
            self.regs.write_flags(&[(Flag::Z, result == 0)]);
            self.write16_tick_a_desc(instr, addr, data | val);
        }

        Ok(())
    }

    /// RMB/SMB - Reset/Set memory bit (65C02)
    ///
    /// The bit number is encoded in bits 4-6 of the opcode.
    fn op_bit_mem(&mut self, instr: &Instruction, set: bool) -> Result<()> {
        let bit = (instr.raw[0] as usize >> 4) & 7;
        let (val, addr) = self.fetch_data(instr, false, false, Rmw::No, Flag::M)?;

        // Calculation cycle: the 65C02 re-reads the operand.
        self.read_tick(addr);

        let result = if set {
            val | 1 << bit
        } else {
            val & !(1 << bit)
        };
        self.write_tick(addr, result as u8);

        Ok(())
    }

    /// BBR/BBS - Branch on bit reset/set (65C02)
    ///
    /// The bit number is encoded in bits 4-6 of the opcode. The zero page
    /// byte is read twice, with no write, as on the real chips captured by
    /// 6502Decoder: 5 cycles, 6 when taken, 7 across a page.
    fn op_bit_branch(&mut self, instr: &Instruction, set: bool) -> Result<()> {
        let bit = (instr.raw[0] as usize >> 4) & 7;
        let zp = instr.immediate[0] as u16;
        let off = instr.immediate[1] as u8 as i8;

        let val = self.read_tick(Address::from(zp));
        self.read_tick(Address::from(zp));

        let bit_set = (val >> bit) & 1 == 1;
        let taken = if set { bit_set } else { !bit_set };
        if taken {
            // As for the other branches: one cycle more, and another when
            // the target is on another page
            let base = self.regs.read(Register::PC);
            let target = base.wrapping_add_signed(off as i16);
            self.read_tick(Address::from(base));
            if (base ^ target) & 0xFF00 != 0 {
                self.read_tick(Address::from(base));
            }
            self.regs.write(Register::PC, target);
        }

        Ok(())
    }

    /// ASL - Arithmetic Shift Left (register variant)
    fn op_asl_acc(&mut self) -> Result<()> {
        let val = self.regs.read(Register::C);
        if self.width8(Flag::M) {
            // 8-bit
            let result = (val << 1) as u8;
            self.regs.write(Register::A, result.into());
            self.regs.write_flags(&[
                (Flag::Z, result == 0),
                (Flag::N, result & 0x80 != 0),
                (Flag::C, val & 0x80 != 0),
            ]);
        } else {
            // 16-bit
            let result = val << 1;
            self.regs.write(Register::C, result);
            self.regs.write_flags(&[
                (Flag::Z, result == 0),
                (Flag::N, result & 0x8000 != 0),
                (Flag::C, val & 0x8000 != 0),
            ]);
        }

        self.tick_bus(1)
    }

    /// ASL - Arithmetic Shift Left (memory variant)
    fn op_asl(&mut self, instr: &Instruction) -> Result<()> {
        let (val, addr) = self.fetch_data(instr, true, false, Rmw::Shift, Flag::M)?;

        // Calculation cycle: the NMOS 6502 re-writes the original
        // value, the 65C02 re-reads it, the 65816 takes an internal
        // cycle.
        match self.variant {
            Variant::M6502 => self.write_tick(addr, val as u8),
            Variant::M65C02 => {
                self.read_tick(addr);
            }
            #[cfg(feature = "w65816")]
            Variant::W65816 => self.tick_bus(1)?,
        }

        if self.width8(Flag::M) {
            let result = (val << 1) as u8;
            self.write_tick(addr, result);
            self.regs.write_flags(&[
                (Flag::Z, result == 0),
                (Flag::N, result & 0x80 != 0),
                (Flag::C, val & 0x80 != 0),
            ]);
        } else {
            // 16-bit
            let result = val << 1;
            self.write16_tick_a_desc(instr, addr, result);
            self.regs.write_flags(&[
                (Flag::Z, result == 0),
                (Flag::N, result & 0x8000 != 0),
                (Flag::C, val & 0x8000 != 0),
            ]);
        }

        Ok(())
    }

    /// LSR - Logical Shift Right (register variant)
    fn op_lsr_acc(&mut self) -> Result<()> {
        let val = self.regs.read(Register::C);
        if self.width8(Flag::M) {
            // 8-bit
            let result = val as u8 >> 1;
            self.regs.write(Register::A, result.into());
            self.regs.write_flags(&[
                (Flag::Z, result == 0),
                (Flag::N, false),
                (Flag::C, val & 0x01 != 0),
            ]);
        } else {
            // 16-bit
            let result = val >> 1;
            self.regs.write(Register::C, result);
            self.regs.write_flags(&[
                (Flag::Z, result == 0),
                (Flag::N, false),
                (Flag::C, val & 0x01 != 0),
            ]);
        }

        self.tick_bus(1)
    }

    /// LSR - Logical Shift Right (memory variant)
    fn op_lsr(&mut self, instr: &Instruction) -> Result<()> {
        let (val, addr) = self.fetch_data(instr, true, false, Rmw::Shift, Flag::M)?;
        // Calculation cycle
        self.tick_bus(1)?;

        if self.width8(Flag::M) {
            let result = val as u8 >> 1;
            self.write_tick(addr, result);
            self.regs.write_flags(&[
                (Flag::Z, result == 0),
                (Flag::N, false),
                (Flag::C, val & 0x01 != 0),
            ]);
        } else {
            // 16-bit
            let result = val >> 1;
            self.write16_tick_a_desc(instr, addr, result);
            self.regs.write_flags(&[
                (Flag::Z, result == 0),
                (Flag::N, false),
                (Flag::C, val & 0x01 != 0),
            ]);
        }

        Ok(())
    }

    /// ROL - ROtate Left (register variant)
    fn op_rol_acc(&mut self) -> Result<()> {
        let val = self.regs.read(Register::C);
        let c = if self.regs.test_flag(Flag::C) { 1 } else { 0 };
        if self.width8(Flag::M) {
            // 8-bit
            let result = ((val << 1) | c) as u8;
            self.regs.write(Register::A, result.into());
            self.regs.write_flags(&[
                (Flag::Z, result == 0),
                (Flag::N, result & 0x80 != 0),
                (Flag::C, val & 0x80 != 0),
            ]);
        } else {
            // 16-bit
            let result = (val << 1) | c;
            self.regs.write(Register::C, result);
            self.regs.write_flags(&[
                (Flag::Z, result == 0),
                (Flag::N, result & 0x8000 != 0),
                (Flag::C, val & 0x8000 != 0),
            ]);
        }

        self.tick_bus(1)
    }

    /// ROL - ROtate Left (memory variant)
    fn op_rol(&mut self, instr: &Instruction) -> Result<()> {
        let (val, addr) = self.fetch_data(instr, true, false, Rmw::Shift, Flag::M)?;
        let c = if self.regs.test_flag(Flag::C) { 1 } else { 0 };

        // Calculation cycle: the NMOS 6502 re-writes the original
        // value, the 65C02 re-reads it, the 65816 takes an internal
        // cycle.
        match self.variant {
            Variant::M6502 => self.write_tick(addr, val as u8),
            Variant::M65C02 => {
                self.read_tick(addr);
            }
            #[cfg(feature = "w65816")]
            Variant::W65816 => self.tick_bus(1)?,
        }

        if self.width8(Flag::M) {
            let result = ((val << 1) | c) as u8;
            self.write_tick(addr, result);
            self.regs.write_flags(&[
                (Flag::Z, result == 0),
                (Flag::N, result & 0x80 != 0),
                (Flag::C, val & 0x80 != 0),
            ]);
        } else {
            // 16-bit
            let result = (val << 1) | c;
            self.write16_tick_a_desc(instr, addr, result);
            self.regs.write_flags(&[
                (Flag::Z, result == 0),
                (Flag::N, result & 0x8000 != 0),
                (Flag::C, val & 0x8000 != 0),
            ]);
        }

        Ok(())
    }

    /// ROR - ROtate Right (register variant)
    fn op_ror_acc(&mut self) -> Result<()> {
        let val = self.regs.read(Register::C);
        let c = if self.regs.test_flag(Flag::C) { 1 } else { 0 };
        if self.width8(Flag::M) {
            // 8-bit
            let result = (((val & 0xFF) >> 1) | (c << 7)) as u8;
            self.regs.write(Register::A, result.into());
            self.regs.write_flags(&[
                (Flag::Z, result == 0),
                (Flag::N, result & 0x80 != 0),
                (Flag::C, val & 0x01 != 0),
            ]);
        } else {
            // 16-bit
            let result = (val >> 1) | (c << 15);
            self.regs.write(Register::C, result);
            self.regs.write_flags(&[
                (Flag::Z, result == 0),
                (Flag::N, result & 0x8000 != 0),
                (Flag::C, val & 0x01 != 0),
            ]);
        }

        self.tick_bus(1)
    }

    /// ROR - ROtate Right (memory variant)
    fn op_ror(&mut self, instr: &Instruction) -> Result<()> {
        let (val, addr) = self.fetch_data(instr, true, false, Rmw::Shift, Flag::M)?;
        let c = if self.regs.test_flag(Flag::C) { 1 } else { 0 };

        // Calculation cycle: the NMOS 6502 re-writes the original
        // value, the 65C02 re-reads it, the 65816 takes an internal
        // cycle.
        match self.variant {
            Variant::M6502 => self.write_tick(addr, val as u8),
            Variant::M65C02 => {
                self.read_tick(addr);
            }
            #[cfg(feature = "w65816")]
            Variant::W65816 => self.tick_bus(1)?,
        }

        if self.width8(Flag::M) {
            let result = (((val & 0xFF) >> 1) | (c << 7)) as u8;
            self.write_tick(addr, result);
            self.regs.write_flags(&[
                (Flag::Z, result == 0),
                (Flag::N, result & 0x80 != 0),
                (Flag::C, val & 0x01 != 0),
            ]);
        } else {
            // 16-bit
            let result = (val >> 1) | (c << 15);
            self.write16_tick_a_desc(instr, addr, result);
            self.regs.write_flags(&[
                (Flag::Z, result == 0),
                (Flag::N, result & 0x8000 != 0),
                (Flag::C, val & 0x01 != 0),
            ]);
        }

        Ok(())
    }

    /// Branch operations
    fn op_branch(&mut self, instr: &Instruction, cc: bool) -> Result<()> {
        let (addr, crosses) = self.resolve_address(instr, false, false, Rmw::No)?;
        if !cc {
            return Ok(());
        }

        self.regs.write(Register::PC, addr as u16);
        self.tick_bus(1)?;
        // One extra cycle if the branch crosses a page boundary.  The
        // 6502 and 65C02 take this penalty; TomHarte's 65816 test data
        // does not (see tests.rs).
        if crosses && !self.variant.is_65816() {
            self.tick_bus(1)?;
        }
        Ok(())
    }

    /// Branch operations (24-bit)
    #[cfg(feature = "w65816")]
    fn op_branch_long(&mut self, instr: &Instruction) -> Result<()> {
        let (addr, _) = self.resolve_address(instr, false, false, Rmw::No)?;

        self.regs.write(Register::K, ((addr >> 16) as u8).into());
        self.regs.write(Register::PC, addr as u16);
        self.tick_bus(1)
    }

    /// Jump operations
    fn op_jump(&mut self, instr: &Instruction) -> Result<()> {
        let (addr, _) = self.resolve_address(instr, true, false, Rmw::No)?;

        #[cfg(feature = "w65816")]
        if matches!(
            instr.def.mode,
            AddressingMode::Long | AddressingMode::AbsolutePtr24
        ) {
            self.regs.write(Register::K, ((addr >> 16) as u8).into());
        }

        self.regs.write(Register::PC, addr as u16);
        Ok(())
    }

    /// Pull a register from the stack
    #[cfg(feature = "w65816")]
    fn op_pull_reg(&mut self, reg: Register) -> Result<()> {
        // Internal cycles
        self.tick_bus(2)?;

        let (val, himask) = match reg.width() {
            RegisterWidth::EightBit => (self.pull8() as u16, 0x80),
            RegisterWidth::SixteenBit => (self.pull16(), 0x8000),
        };
        self.regs
            .write_flags(&[(Flag::N, (val & himask) != 0), (Flag::Z, val == 0)]);
        self.regs.write(reg, val);

        Ok(())
    }

    /// Pull a register from the stack, depending on width flag
    fn op_pull_reg_flag(&mut self, reg: Register, flag: Flag) -> Result<()> {
        assert_eq!(reg.width(), RegisterWidth::SixteenBit);

        if self.variant.is_65816() {
            // Internal cycles
            self.tick_bus(2)?;
        } else {
            // The 6502/65C02 pull starts with dummy reads of the byte
            // after the opcode and of the current stack location.
            self.read_tick(Address::from(self.regs.pc));
            self.read_tick(Address::from(0x0100 | self.regs.read(Register::SL)));
        }

        if self.width8(flag) {
            let hi = self.regs.read(reg) & 0xFF00;
            let val = self.pull8() as u16;
            self.regs
                .write_flags(&[(Flag::N, (val & 0x80) != 0), (Flag::Z, val == 0)]);
            self.regs.write(reg, hi | val);
        } else {
            let val = self.pull16();
            self.regs
                .write_flags(&[(Flag::N, (val & 0x8000) != 0), (Flag::Z, val == 0)]);
            self.regs.write(reg, val);
        }

        Ok(())
    }

    /// PLP - Pull P
    ///
    /// On the 6502 family the pulled B bit (bit 4) is dropped and bit 5
    /// is hardwired to 1.
    fn op_plp(&mut self) -> Result<()> {
        if self.variant.is_65816() {
            self.tick_bus(2)?;
            let p = self.pull8();
            self.regs.write(Register::P, p.into());
        } else {
            // 6502/65C02: dummy reads of the byte after the opcode and
            // of the current stack location, then the pull.
            self.read_tick(Address::from(self.regs.pc));
            self.read_tick(Address::from(0x0100 | self.regs.read(Register::SL)));
            let p = self.pull8();
            self.regs.write(Register::P, ((p | 0x20) & !0x10).into());
        }
        Ok(())
    }

    /// PHP - Push P
    ///
    /// On the 6502 family the pushed B bit (bit 4) is always set, and
    /// bit 5 is hardwired to 1.
    fn op_php(&mut self) -> Result<()> {
        self.tick_bus(1)?;
        if self.variant.is_65816() {
            Ok(self.push8(self.regs.read8(Register::P)))
        } else {
            Ok(self.push8(self.regs.read8(Register::P) | 0x30))
        }
    }

    /// JSL - Jump Subroutine Long
    #[cfg(feature = "w65816")]
    fn op_jsl(&mut self, instr: &Instruction) -> Result<()> {
        let (data, _) = self.resolve_address(instr, false, false, Rmw::No)?;

        self.push8(self.regs.read8(Register::K));

        // Internal cycle
        self.tick_bus(1)?;

        self.push16(self.regs.read(Register::PC).wrapping_sub(1));
        self.regs
            .write(Register::K, ((data & ADDRESS_MASK) >> 16) as u16);
        self.regs.write(Register::PC, data as u16);

        Ok(())
    }

    /// JSR - Jump Subroutine
    fn op_jsr(&mut self, instr: &Instruction) -> Result<()> {
        let (data, _) = self.resolve_address(instr, false, false, Rmw::No)?;

        // We've already advanced PC at this point past the current
        // instruction, but the address on the stack should be the
        // address of the JSR opcode + 2, which translates into PC - 1.
        let pc = self.regs.read(Register::PC);

        if self.variant.is_65816() {
            // JSR (abs) takes an internal cycle; for JSR (abs,X) the
            // resolve_address call above already consumed the operand
            // re-read and pointer fetches.
            if instr.def.mode == AddressingMode::Absolute {
                self.tick_bus(1)?;
            }
            self.push16(pc.wrapping_sub(1));
            self.regs.write(Register::PC, data as u16);
        } else {
            // The 6502 and 65C02 push the return address before fetching
            // the operand's high byte.  If the stack write overlapped the
            // instruction's last byte, the pushed value is read back as
            // the high byte of the target (the JSR operand fetch order
            // quirk).
            self.push16(pc.wrapping_sub(1));

            let hi = self.read_tick(Address::from(pc.wrapping_sub(1)));
            self.regs
                .write(Register::PC, (hi as u16) << 8 | data as u16 & 0xFF);
        }

        Ok(())
    }

    /// RTL - Return from subroutine long
    #[cfg(feature = "w65816")]
    fn op_rtl(&mut self) -> Result<()> {
        // Internal cycles
        self.tick_bus(2)?;

        let pc = self.pull16().wrapping_add(1);
        let k = self.pull8();
        self.regs.write(Register::PC, pc);
        self.regs.write(Register::K, k.into());

        Ok(())
    }

    /// RTS - Return from subroutine
    fn op_rts(&mut self) -> Result<()> {
        if self.variant.is_65816() {
            // Internal cycles
            self.tick_bus(2)?;

            let pc = self.pull16().wrapping_add(1);
            self.regs.write(Register::PC, pc);

            // Internal cycle
            self.tick_bus(1)?;
        } else {
            // 6502/65C02: dummy reads of the byte after the opcode and
            // of the current stack location, the pulls, and a read of
            // the return address.
            self.read_tick(Address::from(self.regs.pc));
            self.read_tick(Address::from(0x0100 | self.regs.read(Register::SL)));

            let pc = self.pull16().wrapping_add(1);

            self.regs.write(Register::PC, pc);
            // The final read fetches the pulled address (the +1 is
            // applied after the read).
            self.read_tick(Address::from(pc.wrapping_sub(1)));
        }

        Ok(())
    }

    /// BRK/COP - Software interrupts
    fn op_swint(&mut self, instr: &Instruction, vector_addr: Address) -> Result<()> {
        // Actually a read of PC for BRK?
        self.tick_bus((2 - instr.def.len).try_into().unwrap())?;

        // Patch up the PC for BRK, which is 1 byte, but writes the
        // PC to the stack with the same offset as COP.
        let pc = self.regs.read(Register::PC);
        self.regs.write(
            Register::PC,
            pc.wrapping_sub(instr.def.len as u16).wrapping_add(2),
        );

        self.dispatch_interrupt(vector_addr, instr.def.instr_type == InstructionType::BRK)
    }

    /// RTI - ReTurn from Interrupt
    fn op_rti(&mut self) -> Result<()> {
        // Back from the alternate firmware bank (a no-op elsewhere).
        self.bus.set_int_bank(false);
        if self.variant.is_65816() {
            // Internal cycles
            self.tick_bus(2)?;

            let p = self.pull8();
            let pc = self.pull16();
            let k = self.pull8();

            self.regs.write(Register::P, p.into());
            self.regs.write(Register::K, k.into());
            self.regs.write(Register::PC, pc);
        } else {
            // 6502/65C02: dummy reads of the byte after the opcode and of
            // the current stack location, then the pulls.
            self.read_tick(Address::from(self.regs.pc));
            self.read_tick(Address::from(0x0100 | self.regs.read(Register::SL)));

            let p = self.pull8();
            let pc = self.pull16();

            // B is not a real bit (dropped); bit 5 is hardwired.
            self.regs.write(Register::P, ((p | 0x20) & !0x10).into());
            self.regs.write(Register::PC, pc);
        }

        Ok(())
    }

    /// STP - STop the Processor (halt until reset)
    fn op_stp(&mut self) -> Result<()> {
        // 65816: fetch + 2 dummy reads + 1 internal cycle; the 65C02
        // takes one less.
        match self.variant {
            #[cfg(feature = "w65816")]
            Variant::W65816 => self.tick_bus(3)?,
            Variant::M65C02 => self.tick_bus(2)?,
            Variant::M6502 => unreachable!(),
        }
        self.stopped = true;
        Ok(())
    }

    /// WAI - WAit for Interrupt
    fn op_wai(&mut self) -> Result<()> {
        // 65816: fetch + 2 dummy reads + 1 internal cycle; the 65C02
        // takes one less.
        match self.variant {
            #[cfg(feature = "w65816")]
            Variant::W65816 => self.tick_bus(3)?,
            Variant::M65C02 => self.tick_bus(2)?,
            Variant::M6502 => unreachable!(),
        }
        self.wait_for_int = true;
        Ok(())
    }

    /// Instructions that push registers to the stack
    #[cfg(feature = "w65816")]
    fn op_push_reg(&mut self, reg: Register) -> Result<()> {
        self.tick_bus(1)?;
        match reg.width() {
            RegisterWidth::SixteenBit => Ok(self.push16(self.regs.read(reg))),
            RegisterWidth::EightBit => Ok(self.push8(self.regs.read(reg) as u8)),
        }
    }

    /// Instructions that push registers to the stack, depending on
    /// width flag.
    fn op_push_reg_flag(&mut self, reg: Register, flag: Flag) -> Result<()> {
        assert_eq!(reg.width(), RegisterWidth::SixteenBit);

        self.tick_bus(1)?;
        if self.width8(flag) {
            Ok(self.push8(self.regs.read(reg) as u8))
        } else {
            Ok(self.push16(self.regs.read(reg)))
        }
    }

    /// Instructions that push a dereferenced address to the stack
    #[cfg(feature = "w65816")]
    fn op_push(&mut self, instr: &Instruction) -> Result<()> {
        let (addr, _) = self.resolve_address(instr, false, false, Rmw::No)?;
        let value = self.read16_tick_a16(addr);

        Ok(self.push16(value))
    }

    /// Instructions that push an immediate value to the stack
    #[cfg(feature = "w65816")]
    fn op_push_imm(&mut self, instr: &Instruction) -> Result<()> {
        Ok(self.push16(instr.imm::<u16>()?))
    }

    /// Instructions that push an address to the stack
    #[cfg(feature = "w65816")]
    fn op_push_addr(&mut self, instr: &Instruction) -> Result<()> {
        let (addr, _) = self.resolve_address(instr, false, false, Rmw::No)?;

        self.tick_bus(1)?;
        Ok(self.push16(addr as u16))
    }

    /// NOP - No operation
    ///
    /// The 65C02 has NOP variants of several lengths that perform
    /// dummy reads: NOP zp reads zp; NOP zp,X reads zp and zp+X (zp
    /// page-wrapped); NOP abs and NOP abs,X re-read the last operand
    /// byte.
    fn op_nop(&mut self, instr: &Instruction) -> Result<()> {
        match self.variant {
            #[cfg(feature = "w65816")]
            Variant::W65816 => self.tick_bus(1),
            Variant::M6502 => self.tick_bus(1),
            Variant::M65C02 => match instr.def.mode {
                // 2 cycles total: the real NOP (0xEA) has an extra
                // cycle; the undefined 1-byte opcodes take only the
                // fetch.
                AddressingMode::Implied => {
                    if instr.raw[0] == 0xEA {
                        self.tick_bus(1)
                    } else {
                        Ok(())
                    }
                }
                AddressingMode::Immediate8 => Ok(()),
                // 3 cycles total
                AddressingMode::Direct => {
                    let zp = Address::from(
                        self.regs
                            .read(Register::D)
                            .wrapping_add(instr.imm::<u16>()?),
                    );
                    self.read_tick(zp);
                    Ok(())
                }
                // 4 cycles total
                AddressingMode::DirectX => {
                    let zp = Address::from(
                        self.regs
                            .read(Register::D)
                            .wrapping_add(instr.imm::<u16>()?),
                    );
                    self.read_tick(zp);
                    let zp_x = Address::from(
                        (self
                            .regs
                            .read(Register::D)
                            .wrapping_add(instr.imm::<u16>()?)
                            .wrapping_add(self.regs.read(Register::X)))
                            & 0xFF,
                    );
                    self.read_tick(zp_x);
                    Ok(())
                }
                // 4 cycles total (re-read of the last operand byte)
                AddressingMode::Absolute | AddressingMode::AbsoluteX => {
                    self.read_tick(Address::from(self.regs.pc.wrapping_sub(1)));
                    Ok(())
                }
                _ => unreachable!(),
            },
        }
    }

    /// MVN/MVP - Move Negative/Positive
    #[cfg(feature = "w65816")]
    fn op_move(&mut self, instr: &Instruction, up: bool) -> Result<()> {
        let (src_bank, dst_bank) = instr.imm_srcdest::<u8>()?;

        let bytes_left = self.regs.read(Register::C);

        let src_addr = Address::from(match (self.regs.test_flag(Flag::X), up) {
            (false, false) => self.regs.read_inc(Register::X),
            (false, true) => self.regs.read_dec(Register::X),
            (true, true) => self.regs.read_dec(Register::XL),
            (true, false) => self.regs.read_inc(Register::XL),
        });
        let dest_addr = Address::from(match (self.regs.test_flag(Flag::X), up) {
            (false, false) => self.regs.read_inc(Register::Y),
            (false, true) => self.regs.read_dec(Register::Y),
            (true, true) => self.regs.read_dec(Register::YL),
            (true, false) => self.regs.read_inc(Register::YL),
        });

        let src = Address::from(src_bank) << 16 | src_addr;
        let dest = Address::from(dst_bank) << 16 | dest_addr;

        let b = self.read_tick(src);
        self.write_tick(dest, b);

        self.regs.write(Register::DBR, dst_bank.into());
        self.regs.write(Register::C, bytes_left.wrapping_sub(1));

        // Internal cycles
        self.tick_bus(2)?;

        if bytes_left != 0 {
            // To make this interruptable, simply rewind PC so the
            // same instruction is executed again until the process
            // is done.
            self.regs.write(
                Register::PC,
                self.regs
                    .read(Register::PC)
                    .wrapping_sub(instr.def.len as u16),
            );
        }

        Ok(())
    }
}
