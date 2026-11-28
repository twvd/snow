//! The Lisa memory management unit
//!
//! Discrete logic in front of every CPU access: 4 contexts of 128 segments of
//! up to 128 KB, mapped to three physical spaces. Lisa Hardware Manual 2.3, 2.4
//! and 4.4.

use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;

use crate::bus::Address;

const CONTEXTS: usize = 4;
const SEGMENTS: usize = 128;
const REGISTERS: usize = CONTEXTS * SEGMENTS;

/// Logical address bits of the segment number
const SEGMENT_SHIFT: u32 = 17;
/// Logical address bits of the byte in a page
const PAGE_SHIFT: u32 = 9;

/// Physical address spaces
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Space {
    Ram,
    Io,
    /// The boot ROM and the MMU registers
    SpecialIo,
}

/// Access control codes, in SLR bits 11-8
const CODE_RAM_RO_STACK: u16 = 0x4;
const CODE_RAM_RO: u16 = 0x5;
const CODE_RAM_RW_STACK: u16 = 0x6;
const CODE_RAM_RW: u16 = 0x7;
const CODE_IO: u16 = 0x9;
const CODE_SPECIAL_IO: u16 = 0xF;

#[derive(Serialize, Deserialize)]
pub struct LisaMmu {
    /// Segment origin registers: physical page
    #[serde(with = "BigArray")]
    sor: [u16; REGISTERS],
    /// Segment limit registers: access control code, length
    #[serde(with = "BigArray")]
    slr: [u16; REGISTERS],
    /// SEG1 and SEG2 latches: the context outside of supervisor mode, and the
    /// one the registers are accessed in
    context: usize,
    /// START: logical addresses with A14 clear go straight to special I/O space
    setup: bool,
}

impl LisaMmu {
    pub fn new() -> Self {
        Self {
            sor: [0; REGISTERS],
            slr: [0; REGISTERS],
            context: 0,
            setup: true,
        }
    }

    /// The registers keep their contents
    pub fn reset(&mut self) {
        self.context = 0;
        self.setup = true;
    }

    pub fn set_seg1(&mut self, val: bool) {
        self.context = (self.context & !1) | usize::from(val);
    }

    pub fn set_seg2(&mut self, val: bool) {
        self.context = (self.context & !2) | (usize::from(val) << 1);
    }

    pub fn set_setup(&mut self, val: bool) {
        self.setup = val;
    }

    /// Translates a logical address, None if the access is a violation
    #[inline(always)]
    pub fn translate(
        &self,
        addr: Address,
        write: bool,
        supervisor: bool,
    ) -> Option<(Space, Address)> {
        let addr = addr & 0x00FF_FFFF;
        if self.setup && addr & (1 << 14) == 0 {
            return Some((Space::SpecialIo, addr));
        }

        let context = if supervisor { 0 } else { self.context };
        let reg = context * SEGMENTS + (addr >> SEGMENT_SHIFT) as usize;
        let slr = self.slr[reg];
        let (space, stack, read_only) = match (slr >> 8) & 0xF {
            CODE_RAM_RO_STACK => (Space::Ram, true, true),
            CODE_RAM_RO => (Space::Ram, false, true),
            CODE_RAM_RW_STACK => (Space::Ram, true, false),
            CODE_RAM_RW => (Space::Ram, false, false),
            CODE_IO => (Space::Io, false, false),
            CODE_SPECIAL_IO => (Space::SpecialIo, false, false),
            _ => return None,
        };
        if read_only && write {
            return None;
        }

        // The hardware adds the length to the page in the segment, and the
        // carry tells whether the page is past the limit. A stack segment
        // adds a carry in and has the sense inverted: its pages are at the top
        // of the segment.
        let page = (addr >> PAGE_SHIFT) & 0xFF;
        let length = u32::from(slr & 0xFF);
        let in_limit = if stack {
            page + length + 1 > 0xFF
        } else {
            page + length <= 0xFF
        };
        if !in_limit {
            return None;
        }

        let physical_page = (u32::from(self.sor[reg]) + page) & 0xFFF;
        Some((space, (physical_page << PAGE_SHIFT) | (addr & 0x1FF)))
    }

    /// Reads a register through special I/O space, in the current context
    /// whatever the mode of the CPU
    pub fn read_register(&self, addr: Address) -> u16 {
        let reg = self.register(addr);
        if Self::is_sor(addr) {
            self.sor[reg]
        } else {
            self.slr[reg]
        }
    }

    pub fn write_register(&mut self, addr: Address, val: u16) {
        let reg = self.register(addr);
        if Self::is_sor(addr) {
            self.sor[reg] = val & 0xFFF;
        } else {
            self.slr[reg] = val & 0xFFF;
        }
    }

    fn register(&self, addr: Address) -> usize {
        self.context * SEGMENTS + ((addr >> SEGMENT_SHIFT) as usize & (SEGMENTS - 1))
    }

    fn is_sor(addr: Address) -> bool {
        addr & (1 << 3) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEGMENT: Address = 5 << SEGMENT_SHIFT;

    fn map(mmu: &mut LisaMmu, sor: u16, slr: u16) {
        mmu.write_register(SEGMENT | 8, sor);
        mmu.write_register(SEGMENT, slr);
    }

    fn mmu() -> LisaMmu {
        let mut mmu = LisaMmu::new();
        mmu.set_setup(false);
        mmu
    }

    #[test]
    fn setup_bypass_on_a14() {
        let mut mmu = LisaMmu::new();
        assert_eq!(
            mmu.translate(0xFE_0004, false, true),
            Some((Space::SpecialIo, 0xFE_0004))
        );
        // A14 set: normal translation, here of an unmapped segment
        assert_eq!(mmu.translate(0xFE_4004, false, true), None);
        mmu.set_setup(false);
        assert_eq!(mmu.translate(0xFE_0004, false, true), None);
    }

    #[test]
    fn invalid_after_power_on() {
        assert_eq!(mmu().translate(SEGMENT, false, true), None);
    }

    #[test]
    fn normal_segment() {
        let mut mmu = mmu();
        // 2 pages: SLR length is $100 - pages
        map(&mut mmu, 0x400, 0x700 | 0xFE);
        assert_eq!(
            mmu.translate(SEGMENT + 0x123, true, true),
            Some((Space::Ram, 0x8_0123))
        );
        assert_eq!(
            mmu.translate(SEGMENT + 0x3FF, false, true),
            Some((Space::Ram, 0x8_03FF))
        );
        assert_eq!(mmu.translate(SEGMENT + 0x400, false, true), None);
    }

    #[test]
    fn full_segment() {
        let mut mmu = mmu();
        map(&mut mmu, 0, 0x700);
        assert_eq!(
            mmu.translate(SEGMENT + 0x1_FFFF, false, true),
            Some((Space::Ram, 0x1_FFFF))
        );
    }

    #[test]
    fn stack_segment() {
        let mut mmu = mmu();
        // The top 2 pages: SLR length is pages - 1
        map(&mut mmu, 0x400, 0x600 | 0x01);
        assert_eq!(mmu.translate(SEGMENT + 0x1_FBFF, false, true), None);
        assert_eq!(
            mmu.translate(SEGMENT + 0x1_FC00, true, true),
            Some((Space::Ram, (0x400 + 0xFE) << 9))
        );
        assert!(mmu.translate(SEGMENT + 0x1_FFFF, false, true).is_some());
    }

    #[test]
    fn read_only() {
        let mut mmu = mmu();
        map(&mut mmu, 0x400, 0x500);
        assert!(mmu.translate(SEGMENT, false, true).is_some());
        assert_eq!(mmu.translate(SEGMENT, true, true), None);
    }

    #[test]
    fn io_and_special_io() {
        let mut mmu = mmu();
        map(&mut mmu, 0x60, 0x900);
        assert_eq!(
            mmu.translate(SEGMENT + 0x1, false, true),
            Some((Space::Io, 0xC001))
        );
        map(&mut mmu, 0x60, 0xF00);
        assert_eq!(
            mmu.translate(SEGMENT + 0x1, false, true),
            Some((Space::SpecialIo, 0xC001))
        );
    }

    #[test]
    fn supervisor_forces_context_0() {
        let mut mmu = mmu();
        map(&mut mmu, 0x400, 0x700);
        mmu.set_seg1(true);
        mmu.set_seg2(true);
        map(&mut mmu, 0x800, 0x700);

        let page = |t: Option<(Space, Address)>| t.unwrap().1 >> PAGE_SHIFT;
        assert_eq!(page(mmu.translate(SEGMENT, false, true)), 0x400);
        assert_eq!(page(mmu.translate(SEGMENT, false, false)), 0x800);
        // The registers follow the latches, not the mode
        assert_eq!(mmu.read_register(SEGMENT | 8), 0x800);
    }
}
