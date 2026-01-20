use crate::bus::Address;
use crate::types::Word;

use super::regs::RegisterFile;

/// Dissects trap arguments and return values for system trap history
pub struct TrapDetails;

impl TrapDetails {
    /// Extract the cleaned trap number (without flags)
    fn clean_trap(trap: Word) -> Word {
        if trap & (1 << 11) != 0 {
            // OS trap - mask flags and return/save A0 bit
            trap & 0b1111_1000_1111_1111
        } else {
            // Toolbox (ROM) trap - mask auto-pop bit
            trap & 0b1111_1011_1111_1111
        }
    }

    /// Get a formatted string describing the trap arguments
    /// This is called when entering a trap (when the trap instruction is executed)
    pub fn format_arguments<F>(regs: &RegisterFile, trap: Word, mut read_mem: F) -> String
    where
        F: FnMut(Address, usize) -> Option<Vec<u8>>,
    {
        let cleaned_trap = Self::clean_trap(trap);

        // Dispatch to specific trap handlers
        match cleaned_trap {
            // VInstall
            0xA033 => Self::format_vbl_task(regs.a[0], &mut read_mem),
            // VRemove
            0xA034 => Self::format_vbl_task(regs.a[0], &mut read_mem),
            _ => Self::format_generic_arguments(regs),
        }
    }

    /// Get a formatted string describing the trap return value
    /// This is called when leaving a trap (when stack pointer returns to pre-trap level)
    pub fn format_return_value<F>(regs: &RegisterFile, trap: Word, mut _read_mem: F) -> String
    where
        F: FnMut(Address, usize) -> Option<Vec<u8>>,
    {
        let cleaned_trap = Self::clean_trap(trap);

        // Dispatch to specific trap handlers
        match cleaned_trap {
            // VInstall
            0xA033 => {
                let d0 = regs.d[0] as i16;
                format!(
                    "D0=${:04X} ({})",
                    d0,
                    match d0 {
                        0 => "noErr",
                        -2 => "vTypErr",
                        _ => "?",
                    }
                )
            }
            // VRemove
            0xA034 => {
                let d0 = regs.d[0] as i16;
                format!("D0=${:04X} ({})",d0,
                match d0 {
                    0 => "noErr",
                    -1 => "qErr",
                    -2 => "vTypErr",
                    _ => "?"
                })
            }
            _ => Self::format_generic_return_value(regs),
        }
    }

    /// Check if the trap was successful based on return value
    /// This is called when leaving a trap (when stack pointer returns to pre-trap level)
    pub fn check_success<F>(regs: &RegisterFile, trap: Word, mut _read_mem: F) -> bool
    where
        F: FnMut(Address, usize) -> Option<Vec<u8>>,
    {
        let cleaned_trap = Self::clean_trap(trap);

        // Dispatch to specific trap handlers
        match cleaned_trap {
            // VInstall and VRemove - success is noErr (0)
            0xA033 | 0xA034 => (regs.d[0] as i16) == 0,
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

    /// Format VBL task structure
    fn format_vbl_task<F>(vbl_task_ptr: u32, read_mem: &mut F) -> String
    where
        F: FnMut(Address, usize) -> Option<Vec<u8>>,
    {
        if let Some(data) = read_mem(vbl_task_ptr, 14) {
            let q_link = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
            let q_type = i16::from_be_bytes([data[4], data[5]]);
            let vbl_addr = u32::from_be_bytes([data[6], data[7], data[8], data[9]]);
            let vbl_count = i16::from_be_bytes([data[10], data[11]]);
            let vbl_phase = i16::from_be_bytes([data[12], data[13]]);
            format!(
                "vblTaskPtr=${:08X} {{qLink=${:08X} qType=${:04X} vblAddr=${:08X} vblCount=${:04X} vblPhase=${:04X}}}",
                vbl_task_ptr, q_link, q_type as u16, vbl_addr, vbl_count as u16, vbl_phase as u16
            )
        } else {
            format!("vblTaskPtr=${:08X} (unreadable)", vbl_task_ptr)
        }
    }
}
