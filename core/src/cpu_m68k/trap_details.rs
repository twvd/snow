use crate::types::Word;

use super::regs::RegisterFile;

/// Dissects trap arguments and return values for system trap history
pub struct TrapDetails;

impl TrapDetails {
    /// Get a formatted string describing the trap arguments
    /// This is called when entering a trap (when the trap instruction is executed)
    pub fn format_arguments(regs: &RegisterFile, trap: Word) -> String {
        // Extract the cleaned trap number (without flags)
        let cleaned_trap = if trap & (1 << 11) != 0 {
            // OS trap - mask flags and return/save A0 bit
            trap & 0b1111_1000_1111_1111
        } else {
            // Toolbox (ROM) trap - mask auto-pop bit
            trap & 0b1111_1011_1111_1111
        };

        // Dispatch to specific trap handlers
        #[allow(clippy::match_single_binding)]
        match cleaned_trap {
            _ => Self::format_generic_arguments(regs),
        }
    }

    /// Get a formatted string describing the trap return value
    /// This is called when leaving a trap (when stack pointer returns to pre-trap level)
    pub fn format_return_value(regs: &RegisterFile, trap: Word) -> String {
        // Extract the cleaned trap number (without flags)
        let cleaned_trap = if trap & (1 << 11) != 0 {
            // OS trap
            trap & 0b1111_1000_1111_1111
        } else {
            // Toolbox (ROM) trap
            trap & 0b1111_1011_1111_1111
        };

        // Dispatch to specific trap handlers
        #[allow(clippy::match_single_binding)]
        match cleaned_trap {
            _ => Self::format_generic_return_value(regs),
        }
    }

    /// Check if the trap was successful based on return value
    /// This is called when leaving a trap (when stack pointer returns to pre-trap level)
    pub fn check_success(regs: &RegisterFile, trap: Word) -> bool {
        // Extract the cleaned trap number (without flags)
        let cleaned_trap = if trap & (1 << 11) != 0 {
            // OS trap
            trap & 0b1111_1000_1111_1111
        } else {
            // Toolbox (ROM) trap
            trap & 0b1111_1011_1111_1111
        };

        // Dispatch to specific trap handlers
        #[allow(clippy::match_single_binding)]
        match cleaned_trap {
            _ => Self::check_generic_success(regs),
        }
    }

    /// Generic argument formatter
    fn format_generic_arguments(regs: &RegisterFile) -> String {
        format!(
            "D0=${:08X} D1=${:08X} D2=${:08X} A0=${:08X} A1=${:08X} A2=${:08X}",
            regs.d[0], regs.d[1], regs.d[2], regs.a[0], regs.a[1], regs.a[2]
        )
    }

    /// Generic return value formatter
    fn format_generic_return_value(regs: &RegisterFile) -> String {
        format!("D0=${:08X}", regs.d[0])
    }

    /// Generic success
    fn check_generic_success(regs: &RegisterFile) -> bool {
        regs.d[0] == 0
    }
}
