//! Instruction times of the MC68040 integer unit (MC68040UM section 10)
//!
//! The 68040 runs instructions through a pipeline: the <ea> calculate and <ea>
//! fetch stages work on the next instructions while the execute stage runs the
//! current one. The tables give the time of each instruction in the execute
//! stage, for accesses that hit in the caches, as a lead time and a base time.
//! The lead time is how long the instruction waits for its operands to come
//! through the earlier stages, which a long enough previous instruction hides.
//! Bus cycles add their own time, like a cache miss does on the 68040 (10.1).

use anyhow::Result;

use crate::bus::{Address, Bus, IrqSource};
use crate::cpu_m68k::cpu::CpuM68k;
use crate::cpu_m68k::{CpuM68kType, FpuM68kType};
use crate::tickable::Ticks;

/// Execute stage time of an instruction
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cpu_m68k) struct Exec040 {
    /// Clocks the instruction can wait for the earlier stages
    pub lead: u16,
    /// Clocks the instruction runs in the execute stage
    pub base: u16,
}

impl Exec040 {
    /// An instruction without an entry in the tables: one clock
    pub const DEFAULT: Self = e(0, 1);
}

impl Default for Exec040 {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Lead time `lead` and base time `base`, written "lead L + base" in the manual
pub(in crate::cpu_m68k) const fn e(lead: u16, base: u16) -> Exec040 {
    Exec040 { lead, base }
}

impl<
    TBus,
    const ADDRESS_MASK: Address,
    const CPU_TYPE: CpuM68kType,
    const FPU_TYPE: FpuM68kType,
    const PMMU: bool,
> CpuM68k<TBus, ADDRESS_MASK, CPU_TYPE, FPU_TYPE, PMMU>
where
    TBus: Bus<Address, u8> + IrqSource,
{
    /// Runs the instruction of this step in the execute stage, for the time
    /// the instruction set in `exec040`, less the lead time the previous
    /// instruction hides: the previous instruction executes for its base
    /// time, during which the earlier stages of this one run, all but the
    /// clock it takes to pass the operands on (MC68040UM 10.2, Figure 10-2).
    pub(in crate::cpu_m68k) fn execute040(&mut self) -> Result<()> {
        let Exec040 { lead, base } = self.exec040;
        let hidden = self.exec040_prev.saturating_sub(1);
        self.exec040_prev = base;
        self.advance_cycles(Ticks::from(base + lead.saturating_sub(hidden)))
    }
}
