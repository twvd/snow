use anyhow::Result;
use arrayvec::ArrayVec;
use num_traits::ToPrimitive;
use serde::{Deserialize, Serialize};

use crate::bus::{Address, Bus, BusIterator, ADDRESS_MASK};
use crate::tickable::{Tickable, Ticks};

use super::alu;
use super::instruction::{AddressingMode, Instruction, InstructionType, MAX_INSTRUCTION_LEN};
use super::regs::{Flag, Register, RegisterFile, RegisterWidth};

/// Main SNES CPU (65816)
#[derive(Serialize, Deserialize)]
pub struct Cpu65816<TBus: Bus<Address>> {
    pub verbose: bool,
    verbose_wai: bool,
    pub bus: TBus,
    pub regs: RegisterFile,
    pub cycles: Ticks,
    pub wait_for_int: bool,

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

impl<TBus> Cpu65816<TBus>
where
    TBus: Bus<Address>,
{
    fn _new(bus: TBus) -> Self {
        Self {
            verbose: false,
            verbose_wai: false,
            bus,
            regs: RegisterFile::new(),
            cycles: 0,
            wait_for_int: false,
            nmi_edge: false,
            intvec_cop: 0x00FFE4,
            intvec_brk: 0x00FFE6,
            intvec_nmi: 0x00FFEA,
            intvec_int: 0x00FFEE,
            vec_reset: 0x00FFFC,
        }
    }

    pub fn new(bus: TBus) -> Self {
        let mut cpu = Self::_new(bus);
        cpu.reset();
        cpu
    }

    pub fn new_pc(bus: TBus, pc: u16) -> Self {
        let mut cpu = Self::_new(bus);
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
    }

    pub fn dump_state(&self) -> String {
        format!(
            "{} - {}\n --> {}",
            self.cycles,
            self.regs,
            self.peek_next_instr()
        )
    }

    /// Fetches and decodes the next instruction at PC
    pub fn peek_next_instr(&self) -> Instruction {
        let mut busiter = BusIterator::new_from(&self.bus, self.regs.get_full_pc());
        Instruction::decode(
            &mut busiter,
            self.regs.test_flag(Flag::M),
            self.regs.test_flag(Flag::X),
        )
        .expect("65816 instruction decode error")
    }

    /// Fetches and decodes the next instruction at PC
    pub fn fetch_next_instr(&mut self) -> Result<Instruction> {
        let mut fetched: ArrayVec<u8, MAX_INSTRUCTION_LEN> = ArrayVec::new();
        let (m, x) = (self.regs.test_flag(Flag::M), self.regs.test_flag(Flag::X));
        let mut p = 0;

        let instr = loop {
            let pc = (self.regs.k as Address) << 16 | self.regs.pc.wrapping_add(p) as Address;
            p += 1;

            match Instruction::decode(&mut fetched.clone().into_iter(), m, x) {
                None => fetched.push(self.read_tick(pc)),
                Some(i) => break i,
            }
        };

        Ok(instr)
    }

    /// Executes one CPU step (one instruction).
    pub fn step(&mut self) -> Result<Ticks> {
        let start_cycles = self.cycles;
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
            self.dispatch_interrupt(self.intvec_nmi)?;
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
                self.dispatch_interrupt(self.intvec_int)?;
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

        let instr = self.fetch_next_instr()?;

        self.regs.pc = self.regs.pc.wrapping_add(instr.len as u16);
        self.execute_instruction(&instr)?;
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
        let v = self.bus.read(addr);
        self.tick_bus(1).unwrap();
        v
    }

    /// Reads 16-bits from a memory location while ticking
    /// peripherals for the access time.
    /// Address wraps at 16-bits.
    fn read16_tick_a16(&mut self, addr: Address) -> u16 {
        let mut v = self.bus.read(addr) as u16;
        self.tick_bus(1).unwrap();
        let hi_addr = addr & 0xFFFF0000 | Address::from((addr as u16).wrapping_add(1));
        v |= (self.bus.read(hi_addr) as u16) << 8;
        self.tick_bus(1).unwrap();
        v
    }

    /// Reads 16-bits from a memory location while ticking
    /// peripherals for the access time.
    /// Address wraps at 24-bits.
    fn read16_tick_a24(&mut self, addr: Address) -> u16 {
        let mut v = self.bus.read(addr) as u16;
        self.tick_bus(1).unwrap();
        let hi_addr = addr.wrapping_add(1);
        v |= (self.bus.read(hi_addr) as u16) << 8;
        self.tick_bus(1).unwrap();
        v
    }

    /// Reads 24-bits from a memory location while ticking
    /// peripherals for the access time.
    /// Address wraps at 16-bits.
    fn read24_tick_a16(&mut self, addr: Address) -> u32 {
        let mut v = self.bus.read(addr) as u32;
        self.tick_bus(1).unwrap();
        let mid_addr = addr & 0xFFFF0000 | Address::from((addr as u16).wrapping_add(1));
        v |= (self.bus.read(mid_addr) as u32) << 8;
        self.tick_bus(1).unwrap();
        let hi_addr = addr & 0xFFFF0000 | Address::from((addr as u16).wrapping_add(2));
        v |= (self.bus.read(hi_addr) as u32) << 16;
        self.tick_bus(1).unwrap();
        v
    }

    /// Writes a memory location while ticking peripherals
    /// for the access time.
    fn write_tick(&mut self, addr: Address, val: u8) {
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
            AddressingMode::Direct | AddressingMode::DirectX | AddressingMode::StackS => {
                self.write16_tick_a16(addr, value)
            }
            _ => self.write16_tick_a24(addr, value),
        }
    }

    /// Writes 16-bit (LE) to a memory location while ticking
    /// peripherals for the access time.
    /// Selects wrap based on addressing mode, descending temporal order.
    fn write16_tick_a_desc(&mut self, instr: &Instruction, addr: Address, value: u16) {
        match instr.def.mode {
            AddressingMode::Direct | AddressingMode::DirectX | AddressingMode::StackS => {
                self.write16_tick_a16_desc(addr, value)
            }
            _ => self.write16_tick_a24_desc(addr, value),
        }
    }

    /// Pushes 8-bits onto the stack
    fn push8(&mut self, val: u8) {
        let addr = Address::from(self.regs.read_dec(Register::S));
        self.write_tick(addr, val);
    }

    /// Pulls 8-bits from the stack
    fn pull8(&mut self) -> u8 {
        let addr = Address::from(self.regs.read_inc(Register::S).wrapping_add(1));
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
    fn dispatch_interrupt(&mut self, vector_addr: Address) -> Result<()> {
        self.push8(self.regs.read8(Register::K));
        self.push16(self.regs.read(Register::PC));
        self.push8(self.regs.read8(Register::P));

        let addr = self.read16_tick_a16(vector_addr);

        self.regs.write(Register::K, 0);
        self.regs.write(Register::PC, addr as u16);
        self.regs.write_flags(&[(Flag::I, true), (Flag::D, false)]);
        Ok(())
    }

    /// Executes an instruction.
    fn execute_instruction(&mut self, instr: &Instruction) -> Result<()> {
        match instr.def.instr_type {
            InstructionType::NOP => self.tick_bus(1),
            InstructionType::WDM => Ok(()),
            InstructionType::CLC => self.op_clx(Flag::C),
            InstructionType::CLD => self.op_clx(Flag::D),
            InstructionType::CLI => self.op_clx(Flag::I),
            InstructionType::CLV => self.op_clx(Flag::V),
            InstructionType::SEC => self.op_sex(Flag::C),
            InstructionType::SED => self.op_sex(Flag::D),
            InstructionType::SEI => self.op_sex(Flag::I),
            InstructionType::XBA => self.op_xba(),
            InstructionType::XCE => self.op_xce(),
            InstructionType::TCD => self.op_txx_16b(Register::C, Register::D, true),
            InstructionType::TCS => self.op_txx_16b(Register::C, Register::S, false),
            InstructionType::TDC => self.op_txx_16b(Register::D, Register::C, true),
            InstructionType::TSC => self.op_txx_16b(Register::S, Register::C, true),
            InstructionType::TAX => self.op_txx(Register::C, Register::X, Flag::X),
            InstructionType::TAY => self.op_txx(Register::C, Register::Y, Flag::X),
            InstructionType::TSX => self.op_txx(Register::S, Register::X, Flag::X),
            InstructionType::TXA => self.op_txx(Register::X, Register::C, Flag::M),
            InstructionType::TXS => self.op_txx_16b(Register::X, Register::S, false),
            InstructionType::TXY => self.op_txx(Register::X, Register::Y, Flag::X),
            InstructionType::TYA => self.op_txx(Register::Y, Register::C, Flag::M),
            InstructionType::TYX => self.op_txx(Register::Y, Register::X, Flag::X),
            InstructionType::STZ => self.op_stz(&instr),
            InstructionType::STA => self.op_stx_reg(&instr, Register::C, Flag::M),
            InstructionType::STX => self.op_stx_reg(&instr, Register::X, Flag::X),
            InstructionType::STY => self.op_stx_reg(&instr, Register::Y, Flag::X),
            InstructionType::LDA => self.op_load(&instr, Register::C, Flag::M),
            InstructionType::LDX => self.op_load(&instr, Register::X, Flag::X),
            InstructionType::LDY => self.op_load(&instr, Register::Y, Flag::X),
            InstructionType::REP => self.op_rep(&instr),
            InstructionType::SEP => self.op_sep(&instr),
            InstructionType::ADC => self.op_add(&instr),
            InstructionType::SBC => self.op_sbc(&instr),
            InstructionType::CMP => self.op_compare(&instr, Register::C, Flag::M),
            InstructionType::CPX => self.op_compare(&instr, Register::X, Flag::X),
            InstructionType::CPY => self.op_compare(&instr, Register::Y, Flag::X),
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
            InstructionType::BRL => self.op_branch_long(instr),
            InstructionType::JMP => self.op_jump(instr),
            InstructionType::PHA => self.op_push_reg_flag(Register::C, Flag::M),
            InstructionType::PHX => self.op_push_reg_flag(Register::X, Flag::X),
            InstructionType::PHY => self.op_push_reg_flag(Register::Y, Flag::X),
            InstructionType::PHB => self.op_push_reg(Register::DBR),
            InstructionType::PHD => self.op_push_reg(Register::D),
            InstructionType::PHK => self.op_push_reg(Register::K),
            InstructionType::PHP => self.op_push_reg(Register::P),
            InstructionType::PLB => self.op_pull_reg(Register::DBR),
            InstructionType::PLD => self.op_pull_reg(Register::D),
            InstructionType::PLP => self.op_pull_reg(Register::P),
            InstructionType::PLA => self.op_pull_reg_flag(Register::C, Flag::M),
            InstructionType::PLX => self.op_pull_reg_flag(Register::X, Flag::X),
            InstructionType::PLY => self.op_pull_reg_flag(Register::Y, Flag::X),
            InstructionType::JSL => self.op_jsl(instr),
            InstructionType::JSR => self.op_jsr(instr),
            InstructionType::RTS => self.op_rts(),
            InstructionType::RTL => self.op_rtl(),
            InstructionType::BRK => self.op_swint(instr, self.intvec_brk),
            InstructionType::COP => self.op_swint(instr, self.intvec_cop),
            InstructionType::RTI => self.op_rti(),
            InstructionType::STP => panic!("STP encountered"),
            InstructionType::WAI => {
                self.wait_for_int = true;
                Ok(())
            }
            InstructionType::PEI => self.op_push(instr),
            InstructionType::PEA => self.op_push_imm(instr),
            InstructionType::PER => self.op_push_addr(instr),
            InstructionType::MVN => self.op_move(instr, false),
            InstructionType::MVP => self.op_move(instr, true),

            InstructionType::Undefined => panic!("Undefined instruction encountered"),
        }
    }

    /// Resolves an address from instruction data, registers, etc.
    /// based on the addressing mode and consumes cycles.
    fn resolve_address(
        &mut self,
        instr: &Instruction,
        abs_extra_cycle: bool,
        pb_extra_cycle: bool,
    ) -> Result<Address> {
        // Internal cycles for adding D, if applicable (D > 0).
        if self.regs.read(Register::DL) != 0 {
            match instr.def.mode {
                AddressingMode::Direct
                | AddressingMode::DirectPtr16
                | AddressingMode::DirectPtr16Y
                | AddressingMode::DirectPtr24
                | AddressingMode::DirectPtr24Y
                | AddressingMode::DirectXPtr16
                | AddressingMode::DirectX
                | AddressingMode::DirectY => self.tick_bus(1)?,
                _ => (),
            };
        }

        // Extra internal cycles for modes that add a register
        // to the address before resolution.
        match instr.def.mode {
            AddressingMode::DirectX
            | AddressingMode::DirectXPtr16
            | AddressingMode::DirectY
            | AddressingMode::StackS
            | AddressingMode::StackSPtr16Y => self.tick_bus(1)?,
            AddressingMode::AbsoluteX
            | AddressingMode::AbsoluteXPtr16
            | AddressingMode::AbsoluteY
                if abs_extra_cycle =>
            {
                self.tick_bus(1)?
            }
            _ => (),
        }

        let idx = |base: Address, idx: Address| {
            (
                base.wrapping_add(idx) & ADDRESS_MASK,
                base & !0xFF != base.wrapping_add(idx) & !0xFF & ADDRESS_MASK,
            )
        };
        let noidx = |base: Address| (base, false);
        let (address, crosses_page) = match instr.def.mode {
            AddressingMode::Direct => noidx(Address::from(
                self.regs
                    .read(Register::D)
                    .wrapping_add(instr.imm::<u16>()?),
            )),
            AddressingMode::DirectX => noidx(Address::from(
                self.regs
                    .read(Register::D)
                    .wrapping_add(instr.imm::<u16>()?)
                    .wrapping_add(self.regs.read(Register::X).into()),
            )),
            AddressingMode::DirectPtr16 => noidx(
                (Address::from(self.regs.read(Register::DBR)) << 16)
                    | Address::from(
                        self.read16_tick_a16(Address::from(
                            self.regs
                                .read(Register::D)
                                .wrapping_add(instr.imm::<u16>()?),
                        )),
                    ),
            ),
            AddressingMode::DirectPtr24 => noidx(Address::from(
                self.read24_tick_a16(Address::from(
                    self.regs
                        .read(Register::D)
                        .wrapping_add(instr.imm::<u16>()?),
                )),
            )),
            AddressingMode::DirectPtr16Y => idx(
                (Address::from(self.regs.read(Register::DBR)) << 16)
                    | Address::from(
                        self.read16_tick_a16(Address::from(
                            self.regs
                                .read(Register::D)
                                .wrapping_add(instr.imm::<u16>()?),
                        )),
                    ),
                self.regs.read(Register::Y).into(),
            ),
            AddressingMode::DirectPtr24Y => idx(
                Address::from(
                    self.read24_tick_a16(Address::from(
                        self.regs
                            .read(Register::D)
                            .wrapping_add(instr.imm::<u16>()?),
                    )),
                ),
                self.regs.read(Register::Y).into(),
            ),
            AddressingMode::DirectXPtr16 => noidx(
                (Address::from(self.regs.read(Register::DBR)) << 16)
                    | Address::from(
                        self.read16_tick_a16(Address::from(
                            self.regs
                                .read(Register::D)
                                .wrapping_add(instr.imm::<u16>()?)
                                .wrapping_add(self.regs.read(Register::X)),
                        )),
                    ),
            ),
            AddressingMode::DirectY => noidx(Address::from(
                self.regs
                    .read(Register::D)
                    .wrapping_add(instr.imm::<u16>()?)
                    .wrapping_add(self.regs.read(Register::Y)),
            )),
            AddressingMode::Absolute => noidx(
                Address::from(self.regs.read(Register::DBR)) << 16
                    | Address::from(instr.imm::<u16>()?),
            ),
            AddressingMode::AbsoluteX => idx(
                Address::from(self.regs.read(Register::DBR)) << 16
                    | Address::from(instr.imm::<u16>()?),
                self.regs.read(Register::X).into(),
            ),
            AddressingMode::AbsoluteY => idx(
                Address::from(self.regs.read(Register::DBR)) << 16
                    | Address::from(instr.imm::<u16>()?),
                self.regs.read(Register::Y).into(),
            ),
            AddressingMode::AbsolutePtr16 => noidx(
                Address::from(self.regs.read(Register::DBR)) << 16
                    | Address::from(self.read16_tick_a16(Address::from(instr.imm::<u16>()?))),
            ),
            AddressingMode::AbsoluteXPtr16 => noidx(Address::from(
                self.read16_tick_a16(
                    Address::from(self.regs.read(Register::K)) << 16
                        | Address::from(
                            instr
                                .imm::<u16>()?
                                .wrapping_add(self.regs.read(Register::X)),
                        ),
                ),
            )),
            AddressingMode::AbsolutePtr24 => noidx(Address::from(
                self.read24_tick_a16(Address::from(instr.imm::<u16>()?)),
            )),
            AddressingMode::StackS => noidx(Address::from(
                self.regs
                    .read(Register::S)
                    .wrapping_add(instr.imm::<u16>()?),
            )),
            AddressingMode::StackSPtr16Y => noidx(
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
            ),
            AddressingMode::Long => noidx(instr.imm::<u32>()? & ADDRESS_MASK),
            AddressingMode::LongX => noidx(
                instr
                    .imm::<u32>()?
                    .wrapping_add(self.regs.read(Register::X).into())
                    & ADDRESS_MASK,
            ),
            AddressingMode::Relative8 => (
                (Address::from(self.regs.k) << 16)
                    | Address::from(
                        self.regs
                            .pc
                            .wrapping_add_signed(instr.imm::<u8>()? as i8 as i16),
                    ),
                false,
            ),
            AddressingMode::Relative16 => (
                (Address::from(self.regs.k) << 16)
                    | Address::from(self.regs.pc.wrapping_add_signed(instr.imm::<u16>()? as i16)),
                false,
            ),

            AddressingMode::Accumulator
            | AddressingMode::Implied
            | AddressingMode::Immediate8
            | AddressingMode::Immediate16
            | AddressingMode::ImmediateM
            | AddressingMode::ImmediateX
            | AddressingMode::SrcDest => unreachable!(),
        };

        if pb_extra_cycle {
            // Extra cycle if crossing a page boundary || !X
            match instr.def.mode {
                AddressingMode::AbsoluteX
                | AddressingMode::AbsoluteY
                | AddressingMode::DirectPtr16Y => {
                    if !self.regs.test_flag(Flag::X) || crosses_page {
                        self.tick_bus(1)?
                    }
                }
                _ => (),
            }
        } else {
            // More internal cycles for modes that do stuff AFTER
            // address resolution bus activity.
            match instr.def.mode {
                AddressingMode::DirectPtr16Y => self.tick_bus(1)?,
                _ => (),
            }
        }
        match instr.def.mode {
            AddressingMode::StackSPtr16Y => self.tick_bus(1)?,
            _ => (),
        }

        Ok(address)
    }

    /// Retrieves address and fetches instruction data.
    fn fetch_data(
        &mut self,
        instr: &Instruction,
        abs_extra_cycle: bool,
        pb_extra_cycle: bool,
        width_flag: Flag,
    ) -> Result<(u16, Address)> {
        Ok(match instr.def.mode {
            AddressingMode::Immediate8
            | AddressingMode::Immediate16
            | AddressingMode::ImmediateM
            | AddressingMode::ImmediateX => (instr.imm::<u16>()?, Address::MAX),
            _ => {
                let addr = self.resolve_address(instr, abs_extra_cycle, pb_extra_cycle)?;

                (
                    if self.regs.test_flag(width_flag) {
                        // 8-bit mode
                        self.read_tick(addr).into()
                    } else {
                        // 16-bit mode
                        match instr.def.mode {
                            AddressingMode::Direct
                            | AddressingMode::DirectX
                            | AddressingMode::StackS => self.read16_tick_a16(addr),
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
    fn op_xba(&mut self) -> Result<()> {
        let c = self.regs.read(Register::C);
        let result = c >> 8 | c << 8;
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
        if self.regs.test_flag(flag) {
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
        let addr = self.resolve_address(instr, true, false)?;

        if self.regs.test_flag(flag) {
            self.write_tick(addr, value as u8);
        } else {
            self.write16_tick_a(instr, addr, value);
        }

        Ok(())
    }

    /// Load operations
    fn op_load(&mut self, instr: &Instruction, destreg: Register, flag: Flag) -> Result<()> {
        let (val, _) = self.fetch_data(instr, false, true, flag)?;

        if self.regs.test_flag(flag) {
            // 8-bit mode
            let regval = self.regs.read(destreg);
            self.regs.write(destreg, regval & 0xFF00 | val as u16);
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
    fn op_rep(&mut self, instr: &Instruction) -> Result<()> {
        let mask = instr.imm::<u8>()?;
        let p = self.regs.read8(Register::P);
        self.regs.write(Register::P, (p & !mask).try_into()?);
        self.tick_bus(1)?;
        Ok(())
    }

    /// SEP - SEt P
    fn op_sep(&mut self, instr: &Instruction) -> Result<()> {
        let mask = instr.imm::<u8>()?;
        let p = self.regs.read8(Register::P);
        self.regs.write(Register::P, (p | mask).try_into()?);
        self.tick_bus(1)?;
        Ok(())
    }

    /// ADC - Add (accumulator)
    fn op_add(&mut self, instr: &Instruction) -> Result<()> {
        let (data, _) = self.fetch_data(instr, false, true, Flag::M)?;

        let result = match (self.regs.test_flag(Flag::D), self.regs.test_flag(Flag::M)) {
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
            (true, true) => alu::add8_dec(
                self.regs.read(Register::A),
                data,
                self.regs.test_flag(Flag::C),
            ),
        };

        if self.regs.test_flag(Flag::M) {
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
        let (data, _) = self.fetch_data(instr, false, true, Flag::M)?;

        let result = match (self.regs.test_flag(Flag::D), self.regs.test_flag(Flag::M)) {
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
            (true, true) => alu::sub8_dec(
                self.regs.read(Register::A),
                data,
                self.regs.test_flag(Flag::C),
            ),
        };

        if self.regs.test_flag(Flag::M) {
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
        let (data, _) = self.fetch_data(instr, false, true, width_flag)?;
        let val = self.regs.read(to);

        let result = if self.regs.test_flag(width_flag) {
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
        if self.regs.test_flag(width_flag) {
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
        let (val, addr) = self.fetch_data(instr, true, false, Flag::M)?;

        // Calculation cycle
        self.tick_bus(1)?;

        if self.regs.test_flag(Flag::M) {
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
        let (data, _) = self.fetch_data(instr, false, true, Flag::M)?;
        if self.regs.test_flag(Flag::M) {
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
        let (data, _) = self.fetch_data(instr, false, true, Flag::M)?;
        if self.regs.test_flag(Flag::M) {
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
        let (data, _) = self.fetch_data(instr, false, true, Flag::M)?;
        if self.regs.test_flag(Flag::M) {
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
        let (data, _) = self.fetch_data(instr, false, true, Flag::M)?;
        if self.regs.test_flag(Flag::M) {
            // 8-bit
            let result = val & data & 0xFF;

            match instr.def.mode {
                AddressingMode::ImmediateM => self.regs.write_flags(&[(Flag::Z, result == 0)]),
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
                AddressingMode::ImmediateM => self.regs.write_flags(&[(Flag::Z, result == 0)]),
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
        let (data, addr) = self.fetch_data(instr, false, true, Flag::M)?;

        // Calculation cycle
        self.tick_bus(1)?;

        if self.regs.test_flag(Flag::M) {
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
        let (data, addr) = self.fetch_data(instr, false, true, Flag::M)?;

        // Calculation cycle
        self.tick_bus(1)?;

        if self.regs.test_flag(Flag::M) {
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

    /// ASL - Arithmetic Shift Left (register variant)
    fn op_asl_acc(&mut self) -> Result<()> {
        let val = self.regs.read(Register::C);
        if self.regs.test_flag(Flag::M) {
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
        let (val, addr) = self.fetch_data(instr, true, false, Flag::M)?;

        // Calculation cycle
        self.tick_bus(1)?;

        if self.regs.test_flag(Flag::M) {
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
        if self.regs.test_flag(Flag::M) {
            // 8-bit
            let result = (val as u8 >> 1) as u8;
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
        let (val, addr) = self.fetch_data(instr, true, false, Flag::M)?;
        // Calculation cycle
        self.tick_bus(1)?;

        if self.regs.test_flag(Flag::M) {
            let result = (val as u8 >> 1) as u8;
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
        if self.regs.test_flag(Flag::M) {
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
        let (val, addr) = self.fetch_data(instr, true, false, Flag::M)?;
        let c = if self.regs.test_flag(Flag::C) { 1 } else { 0 };

        // Calculation cycle
        self.tick_bus(1)?;

        if self.regs.test_flag(Flag::M) {
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
        if self.regs.test_flag(Flag::M) {
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
        let (val, addr) = self.fetch_data(instr, true, false, Flag::M)?;
        let c = if self.regs.test_flag(Flag::C) { 1 } else { 0 };

        // Calculation cycle
        self.tick_bus(1)?;

        if self.regs.test_flag(Flag::M) {
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
        let addr = self.resolve_address(instr, false, false)?;
        if !cc {
            return Ok(());
        }

        self.regs.write(Register::PC, addr as u16);
        self.tick_bus(1)
    }

    /// Branch operations (24-bit)
    fn op_branch_long(&mut self, instr: &Instruction) -> Result<()> {
        let addr = self.resolve_address(instr, false, false)?;

        self.regs.write(Register::K, ((addr >> 16) as u8).into());
        self.regs.write(Register::PC, addr as u16);
        self.tick_bus(1)
    }

    /// Jump operations
    fn op_jump(&mut self, instr: &Instruction) -> Result<()> {
        let addr = self.resolve_address(instr, true, false)?;

        match instr.def.mode {
            AddressingMode::Long | AddressingMode::AbsolutePtr24 => {
                self.regs.write(Register::K, ((addr >> 16) as u8).into())
            }
            _ => (),
        }

        self.regs.write(Register::PC, addr as u16);
        Ok(())
    }

    /// Pull a register from the stack
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

        // Internal cycles
        self.tick_bus(2)?;

        if self.regs.test_flag(flag) {
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

    /// JSL - Jump Subroutine Long
    fn op_jsl(&mut self, instr: &Instruction) -> Result<()> {
        let data = self.resolve_address(instr, false, false)?;

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
        let data = self.resolve_address(instr, false, false)?;

        // Internal cycle
        self.tick_bus(1)?;

        // We've already advanced PC at this point past the current
        // instruction, but the address on the stack should be
        // address of JSR opcode + 2, which translates into PC - 1.
        self.push16(self.regs.read(Register::PC).wrapping_sub(1));
        self.regs.write(Register::PC, data as u16);

        Ok(())
    }

    /// RTL - Return from subroutine long
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
        // Internal cycles
        self.tick_bus(2)?;

        let pc = self.pull16().wrapping_add(1);
        self.regs.write(Register::PC, pc);

        // Internal cycle
        self.tick_bus(1)?;

        Ok(())
    }

    /// BRK/COP - Software interrupts
    fn op_swint(&mut self, instr: &Instruction, vector_addr: Address) -> Result<()> {
        // Actually a read of PC for BRK?
        self.tick_bus(2 - instr.def.len)?;

        // Patch up the PC for BRK, which is 1 byte, but writes the
        // PC to the stack with the same offset as COP.
        let pc = self.regs.read(Register::PC);
        self.regs.write(
            Register::PC,
            pc.wrapping_sub(instr.def.len as u16).wrapping_add(2),
        );

        self.dispatch_interrupt(vector_addr)
    }

    /// RTI - ReTurn from Interrupt
    fn op_rti(&mut self) -> Result<()> {
        // Internal cycles
        self.tick_bus(2)?;

        let p = self.pull8();
        let pc = self.pull16();
        let k = self.pull8();

        self.regs.write(Register::P, p.into());
        self.regs.write(Register::K, k.into());
        self.regs.write(Register::PC, pc);

        Ok(())
    }

    /// Instructions that push registers to the stack
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
        if self.regs.test_flag(flag) {
            Ok(self.push8(self.regs.read(reg) as u8))
        } else {
            Ok(self.push16(self.regs.read(reg)))
        }
    }

    /// Instructions that push a dereferenced address to the stack
    fn op_push(&mut self, instr: &Instruction) -> Result<()> {
        let addr = self.resolve_address(instr, false, false)?;
        let value = self.read16_tick_a16(addr);

        Ok(self.push16(value as u16))
    }

    /// Instructions that push an immediate value to the stack
    fn op_push_imm(&mut self, instr: &Instruction) -> Result<()> {
        Ok(self.push16(instr.imm::<u16>()?))
    }

    /// Instructions that push an address to the stack
    fn op_push_addr(&mut self, instr: &Instruction) -> Result<()> {
        let addr = self.resolve_address(instr, false, false)?;

        self.tick_bus(1)?;
        Ok(self.push16(addr as u16))
    }

    /// MVN/MVP - Move Negative/Positive
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

impl<TBus> Tickable for Cpu65816<TBus>
where
    TBus: Bus<Address>,
{
    fn tick(&mut self, _ticks: Ticks) -> Result<Ticks> {
        self.step()
    }
}
