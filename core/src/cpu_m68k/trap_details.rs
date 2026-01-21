use crate::bus::Address;
use crate::cpu_m68k::CpuSized;
use crate::types::{Long, Word};
use num_traits::FromBytes;

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

    /// Value/address on stack
    fn stack<F, T>(regs: &RegisterFile, mut read_mem: F, offset: Address) -> Option<T>
    where
        F: FnMut(Address, usize) -> Option<Vec<u8>>,
        T: CpuSized,
        for<'a> &'a <T as FromBytes>::Bytes: TryFrom<&'a [u8]>,
    {
        let sp = regs.read_a::<Address>(7);
        let bytes = read_mem(sp.wrapping_add(offset), std::mem::size_of::<T>())?;
        let arr_ref: &<T as FromBytes>::Bytes = bytes.as_slice().try_into().ok()?;
        Some(T::from_be_bytes(arr_ref))
    }

    /// Get a formatted string describing the trap arguments
    /// This is called when entering a trap (when the trap instruction is executed)
    pub fn format_arguments<F>(regs: &RegisterFile, trap: Word, mut read_mem: F) -> String
    where
        F: FnMut(Address, usize) -> Option<Vec<u8>>,
    {
        let cleaned_trap = Self::clean_trap(trap);

        // Dispatch to specific trap handlers
        //
        // Types:
        // Size/Ptr = 4 bytes
        match cleaned_trap {
            // Open
            0xA000 => Self::format_open(regs.a[0], &mut read_mem),
            // Close
            0xA001 => Self::format_close(regs.a[0], &mut read_mem),
            // Read
            0xA002 => Self::format_read_write(regs.a[0], "Read", &mut read_mem),
            // Write
            0xA003 => Self::format_read_write(regs.a[0], "Write", &mut read_mem),
            // BlockMove
            0xA02E => {
                format!(
                    "sourcePtr=${:08X} destPtr=${:08X} byteCount={}",
                    regs.a[0], regs.a[1], regs.d[0]
                )
            }
            // VInstall
            0xA033 => Self::format_vbl_task(regs.a[0], &mut read_mem),
            // VRemove
            0xA034 => Self::format_vbl_task(regs.a[0], &mut read_mem),
            // SwapMMUMode
            0xA05D => Self::format_swapmmumode(regs.d[0]),
            // SCSIDispatch
            0xA815 => Self::format_scsi_dispatch_args(regs, trap, &mut read_mem),
            // DrawString
            0xA884 => {
                let Some(addr) = Self::stack(regs, &mut read_mem, 0) else {
                    return "(unreadable from stack)".to_string();
                };
                Self::format_str255(addr, &mut read_mem)
            }
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
            // Open - show refnum assigned
            0xA000 => Self::format_open_return(regs.a[0], &mut _read_mem),
            // Close - show result
            0xA001 => Self::format_file_result(regs.a[0], &mut _read_mem),
            // Read/Write - show actual count transferred
            0xA002 | 0xA003 => {
                let op_name = if cleaned_trap == 0xA002 { "Read" } else { "Write" };
                Self::format_read_write_return(regs.a[0], op_name, &mut _read_mem)
            }
            // BlockMove (0xA02E) default
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
                format!(
                    "D0=${:04X} ({})",
                    d0,
                    match d0 {
                        0 => "noErr",
                        -1 => "qErr",
                        -2 => "vTypErr",
                        _ => "?",
                    }
                )
            }
            // SwapMMUMode
            0xA05D => Self::format_swapmmumode(regs.d[0]),
            // SCSIDispatch
            0xA815 => Self::format_scsi_dispatch_return(regs, trap, &mut _read_mem),
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
            // Open/Close/Read/Write - check ioResult field in param block
            0xA000..=0xA003 => {
                const IO_RESULT_OFFSET: Address = 16;
                if let Some(result_bytes) = _read_mem(regs.a[0].wrapping_add(IO_RESULT_OFFSET), 2) {
                    let result = i16::from_be_bytes([result_bytes[0], result_bytes[1]]);
                    result == 0
                } else {
                    false
                }
            }
            // VInstall and VRemove - success is noErr (0)
            0xA033 | 0xA034 => (regs.d[0] as i16) == 0,
            // SCSIDispatch - success is noErr (0)
            0xA815 => (regs.d[0] as i16) == 0,
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
        format!(
            "D0=${:08X} ({})",
            regs.d[0],
            match regs.d[0] {
                0 => "noErr",
                _ => "?",
            }
        )
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

    /// SwapMMUMode arguments/return value
    fn format_swapmmumode(d0: Long) -> String {
        match d0 {
            0 => "24-bit addressing mode".to_string(),
            1 => "32-bit addressing mode".to_string(),
            _ => "?".to_string(),
        }
    }

    /// Str255 type - 'Pascal string', one byte length + data
    fn format_str255<F>(addr: Address, read_mem: &mut F) -> String
    where
        F: FnMut(Address, usize) -> Option<Vec<u8>>,
    {
        let Some(len) = read_mem(addr, 1).and_then(|v| v.first().copied()) else {
            return format!("(unreadable in format_string @ ${:08X})", addr);
        };
        let Some(bytes) = read_mem(addr.wrapping_add(1), len as usize) else {
            return format!("(unreadable in format_string @ ${:08X})", addr);
        };

        format!("${:08X} {{\"{}\"}}", addr, String::from_utf8_lossy(&bytes))
    }

    /// Format SCSIDispatch arguments based on selector
    fn format_scsi_dispatch_args<F>(regs: &RegisterFile, trap: Word, read_mem: &mut F) -> String
    where
        F: FnMut(Address, usize) -> Option<Vec<u8>>,
    {
        // Two possible calling conventions based on bit 10 (auto-pop bit):
        // 1. 0xA815 (bit 10=0): C inline asm: (SP+0)=selector (2 bytes), (SP+2)=first param
        // 2. 0xAC15 (bit 10=1): Pascal macro: (SP+0)=return addr (4 bytes), (SP+4)=selector, (SP+6)=params

        // Check bit 10 (auto-pop bit) of the original trap number
        let auto_pop = (trap & 0x0400) != 0;
        let (selector_offset, param_offset) = if auto_pop {
            // Pascal convention: selector at SP+4, params at SP+6
            (4, 6)
        } else {
            // C convention: selector at SP+0, params at SP+2
            (0, 2)
        };

        let Some(selector): Option<Word> = Self::stack(regs, &mut *read_mem, selector_offset) else {
            return format!("selector=(unreadable from stack at offset {})", selector_offset);
        };

        let selector_name = match selector {
            0 => "scsiReset",
            1 => "scsiGet",
            2 => "scsiSelect",
            3 => "scsiCmd",
            4 => "scsiComplete",
            5 => "scsiRead",
            6 => "scsiWrite",
            7 => "scsiInstall",
            8 => "scsiRBlind",
            9 => "scsiWBlind",
            10 => "scsiStat",
            11 => "scsiSelAtn",
            12 => "scsiMsgIn",
            13 => "scsiMsgOut",
            _ => "unknown",
        };

        match selector {
            0 => format!("selector={} ({})", selector, selector_name),
            1 => format!("selector={} ({})", selector, selector_name),
            2 | 11 => {
                // scsiSelect, scsiSelAtn: targetID (short) after selector
                let Some(target_id): Option<Word> = Self::stack(regs, &mut *read_mem, param_offset) else {
                    return format!(
                        "selector={} ({}) targetID=(unreadable)",
                        selector, selector_name
                    );
                };
                format!(
                    "selector={} ({}) targetID={}",
                    selector, selector_name, target_id
                )
            }
            3 => {
                // scsiCmd: buffer (Ptr), count (short)
                // Pascal convention: leftmost param pushed first, so farther from SP
                let Some(count): Option<Word> = Self::stack(regs, &mut *read_mem, param_offset) else {
                    return format!(
                        "selector={} ({}) count=(unreadable)",
                        selector, selector_name
                    );
                };
                let Some(buffer): Option<Long> = Self::stack(regs, &mut *read_mem, param_offset + 2) else {
                    return format!(
                        "selector={} ({}) count={} buffer=(unreadable)",
                        selector, selector_name, count
                    );
                };
                format!(
                    "selector={} ({})\n  buffer=${:08X} count={}",
                    selector, selector_name, buffer, count
                )
            }
            4 => {
                // scsiComplete: stat (short*), message (short*), wait (ulong)
                // Pascal convention: pushed left-to-right, so wait is closest to SP
                let Some(wait): Option<Long> = Self::stack(regs, &mut *read_mem, param_offset) else {
                    return format!(
                        "selector={} ({}) wait=(unreadable)",
                        selector, selector_name
                    );
                };
                let Some(msg_ptr): Option<Long> = Self::stack(regs, &mut *read_mem, param_offset + 4) else {
                    return format!(
                        "selector={} ({}) wait={} message=(unreadable)",
                        selector, selector_name, wait
                    );
                };
                let Some(stat_ptr): Option<Long> = Self::stack(regs, &mut *read_mem, param_offset + 8) else {
                    return format!(
                        "selector={} ({}) wait={} message=${:08X} stat=(unreadable)",
                        selector, selector_name, wait, msg_ptr
                    );
                };
                format!(
                    "selector={} ({})\n  stat=${:08X} message=${:08X} wait={}",
                    selector, selector_name, stat_ptr, msg_ptr, wait
                )
            }
            5 | 6 | 8 | 9 => {
                // scsiRead, scsiWrite, scsiRBlind, scsiWBlind: tibPtr (Ptr)
                let Some(tib_ptr): Option<Long> = Self::stack(regs, &mut *read_mem, param_offset) else {
                    return format!(
                        "selector={} ({}) tibPtr=(unreadable)",
                        selector, selector_name
                    );
                };
                let tib_details = Self::format_tib(tib_ptr, &mut *read_mem);
                format!(
                    "selector={} ({}) tibPtr=${:08X}\n{}",
                    selector, selector_name, tib_ptr, tib_details
                )
            }
            10 => format!("selector={} ({})", selector, selector_name),
            12 => {
                // scsiMsgIn: message (short*)
                let Some(msg_ptr): Option<Long> = Self::stack(regs, &mut *read_mem, param_offset) else {
                    return format!(
                        "selector={} ({}) message=(unreadable)",
                        selector, selector_name
                    );
                };
                format!(
                    "selector={} ({}) message=${:08X}",
                    selector, selector_name, msg_ptr
                )
            }
            13 => {
                // scsiMsgOut: message (short)
                let Some(message): Option<Word> = Self::stack(regs, &mut *read_mem, param_offset) else {
                    return format!(
                        "selector={} ({}) message=(unreadable)",
                        selector, selector_name
                    );
                };
                format!(
                    "selector={} ({}) message=${:04X}",
                    selector, selector_name, message
                )
            }
            _ => format!("selector={} (unknown)", selector),
        }
    }

    /// Format SCSIDispatch return value
    fn format_scsi_dispatch_return<F>(regs: &RegisterFile, trap: Word, read_mem: &mut F) -> String
    where
        F: FnMut(Address, usize) -> Option<Vec<u8>>,
    {
        // Check bit 10 (auto-pop bit) of the original trap number
        let auto_pop = (trap & 0x0400) != 0;
        let (selector_offset, param_offset) = if auto_pop {
            (4, 6)
        } else {
            (0, 2)
        };

        let Some(selector): Option<Word> = Self::stack(regs, &mut *read_mem, selector_offset) else {
            return Self::format_scsi_error(regs.d[0] as i16);
        };

        match selector {
            10 => {
                // scsiStat returns status in D0 (not an error code)
                format!("D0=${:04X} (status byte)", regs.d[0] as u16)
            }
            4 => {
                // scsiComplete: read back stat and message values
                // Pascal convention: wait at param_offset, msg at +4, stat at +8
                let Some(msg_ptr): Option<Long> = Self::stack(regs, &mut *read_mem, param_offset + 4) else {
                    return Self::format_scsi_error(regs.d[0] as i16);
                };
                let Some(stat_ptr): Option<Long> = Self::stack(regs, &mut *read_mem, param_offset + 8) else {
                    return Self::format_scsi_error(regs.d[0] as i16);
                };

                let stat = read_mem(stat_ptr, 2)
                    .map(|v| i16::from_be_bytes([v[0], v[1]]))
                    .unwrap_or(-1);
                let msg = read_mem(msg_ptr, 2)
                    .map(|v| i16::from_be_bytes([v[0], v[1]]))
                    .unwrap_or(-1);

                format!(
                    "{}\n  stat=${:04X} message=${:04X}",
                    Self::format_scsi_error(regs.d[0] as i16),
                    stat as u16,
                    msg as u16
                )
            }
            12 => {
                // scsiMsgIn: read back message value
                let Some(msg_ptr): Option<Long> = Self::stack(regs, &mut *read_mem, param_offset) else {
                    return Self::format_scsi_error(regs.d[0] as i16);
                };

                let msg = read_mem(msg_ptr, 2)
                    .map(|v| i16::from_be_bytes([v[0], v[1]]))
                    .unwrap_or(-1);

                format!(
                    "{}\n  message=${:04X}",
                    Self::format_scsi_error(regs.d[0] as i16),
                    msg as u16
                )
            }
            _ => Self::format_scsi_error(regs.d[0] as i16),
        }
    }

    /// Format TIB (Transfer Instruction Block)
    fn format_tib<F>(tib_ptr: Address, read_mem: &mut F) -> String
    where
        F: FnMut(Address, usize) -> Option<Vec<u8>>,
    {
        const TIB_SIZE: usize = 10; // 2 bytes opcode + 4 bytes param1 + 4 bytes param2
        const MAX_INSTRUCTIONS: usize = 8; // Limit how many we display

        let mut result = String::new();
        let mut offset = 0;

        for i in 0..MAX_INSTRUCTIONS {
            let Some(instr_bytes) = read_mem(tib_ptr.wrapping_add(offset), TIB_SIZE) else {
                result.push_str(&format!("  TIB[{}]: (unreadable)\n", i));
                break;
            };

            let opcode = u16::from_be_bytes([instr_bytes[0], instr_bytes[1]]);
            let param1 = u32::from_be_bytes([instr_bytes[2], instr_bytes[3], instr_bytes[4], instr_bytes[5]]);
            let param2 = u32::from_be_bytes([instr_bytes[6], instr_bytes[7], instr_bytes[8], instr_bytes[9]]);

            let opcode_name = match opcode {
                1 => "scInc",
                2 => "scNoInc",
                3 => "scAdd",
                4 => "scMove",
                5 => "scLoop",
                6 => "scNop",
                7 => "scStop",
                8 => "scComp",
                _ => "unknown",
            };

            let detail = match opcode {
                1 => format!("addr=${:08X} count={}", param1, param2),
                2 => format!("addr=${:08X} count={}", param1, param2),
                3 => format!("addr=${:08X} count={}", param1, param2),
                4 => format!("src=${:08X} dst=${:08X}", param1, param2),
                5 => format!("count={} offset={}", param1, param2 as i32),
                6 => "".to_string(),
                7 => "".to_string(),
                8 => format!("addr=${:08X} count={}", param1, param2),
                _ => format!("param1=${:08X} param2=${:08X}", param1, param2),
            };

            if detail.is_empty() {
                result.push_str(&format!("  TIB[{}]: {} ({})\n", i, opcode, opcode_name));
            } else {
                result.push_str(&format!("  TIB[{}]: {} ({}) {}\n", i, opcode, opcode_name, detail));
            }

            offset += TIB_SIZE as u32;

            // Stop if we hit scStop opcode
            if opcode == 7 {
                break;
            }
        }

        result
    }

    /// Format SCSI error code
    fn format_scsi_error(err: i16) -> String {
        let err_name = match err {
            0 => "noErr",
            2 => "scCommErr",
            3 => "scArbNBErr",
            4 => "scBadParmsErr",
            5 => "scPhaseErr",
            6 => "scCompareErr",
            7 => "scMgrBusyErr",
            8 => "scSequenceErr",
            9 => "scBusTOErr",
            10 => "scComplPhaseErr",
            _ => "?",
        };
        format!("D0=${:04X} ({})", err as u16, err_name)
    }

    /// Format Open arguments
    fn format_open<F>(pb_ptr: Address, read_mem: &mut F) -> String
    where
        F: FnMut(Address, usize) -> Option<Vec<u8>>,
    {
        // ParamBlockHeader offsets
        const IO_NAMEPTR_OFFSET: Address = 18;
        const IO_VREFNUM_OFFSET: Address = 22;
        // IOParam offsets (after ParamBlockHeader which is 24 bytes)
        const IO_PERMSSN_OFFSET: Address = 26;

        let Some(name_ptr_bytes) = read_mem(pb_ptr.wrapping_add(IO_NAMEPTR_OFFSET), 4) else {
            return format!("paramBlock=${:08X} (unreadable)", pb_ptr);
        };
        let name_ptr = u32::from_be_bytes([name_ptr_bytes[0], name_ptr_bytes[1], name_ptr_bytes[2], name_ptr_bytes[3]]);

        let Some(vref_num_bytes) = read_mem(pb_ptr.wrapping_add(IO_VREFNUM_OFFSET), 2) else {
            return format!("paramBlock=${:08X} namePtr=${:08X} (vRefNum unreadable)", pb_ptr, name_ptr);
        };
        let vref_num = i16::from_be_bytes([vref_num_bytes[0], vref_num_bytes[1]]);

        let Some(permssn_byte) = read_mem(pb_ptr.wrapping_add(IO_PERMSSN_OFFSET), 1) else {
            return format!(
                "paramBlock=${:08X} namePtr=${:08X} vRefNum={}",
                pb_ptr, name_ptr, vref_num
            );
        };
        let permssn = permssn_byte[0];

        let permssn_str = match permssn {
            0 => "fsCurPerm",
            1 => "fsRdPerm",
            2 => "fsWrPerm",
            3 => "fsRdWrPerm",
            _ => "?",
        };

        // Try to read the filename
        let filename = Self::format_str255(name_ptr, read_mem);

        format!(
            "paramBlock=${:08X}\n  fileName={}\n  vRefNum={} permission={} ({})",
            pb_ptr, filename, vref_num, permssn, permssn_str
        )
    }

    /// Format Close arguments
    fn format_close<F>(pb_ptr: Address, read_mem: &mut F) -> String
    where
        F: FnMut(Address, usize) -> Option<Vec<u8>>,
    {
        // IOParam offsets (after ParamBlockHeader which is 24 bytes)
        const IO_REFNUM_OFFSET: Address = 24;

        let Some(ref_num_bytes) = read_mem(pb_ptr.wrapping_add(IO_REFNUM_OFFSET), 2) else {
            return format!("paramBlock=${:08X} (unreadable)", pb_ptr);
        };
        let ref_num = i16::from_be_bytes([ref_num_bytes[0], ref_num_bytes[1]]);

        format!("paramBlock=${:08X} refNum={}", pb_ptr, ref_num)
    }

    /// Format Read/Write arguments
    fn format_read_write<F>(pb_ptr: Address, _op_name: &str, read_mem: &mut F) -> String
    where
        F: FnMut(Address, usize) -> Option<Vec<u8>>,
    {
        // IOParam structure offsets (after ParamBlockHeader which is 24 bytes)
        const IO_REFNUM_OFFSET: Address = 24;
        const IO_BUFFER_OFFSET: Address = 32;
        const IO_REQCOUNT_OFFSET: Address = 36;
        const IO_POSMODE_OFFSET: Address = 44;
        const IO_POSOFFSET_OFFSET: Address = 46;

        let Some(ref_num_bytes) = read_mem(pb_ptr.wrapping_add(IO_REFNUM_OFFSET), 2) else {
            return format!("paramBlock=${:08X} (unreadable)", pb_ptr);
        };
        let ref_num = i16::from_be_bytes([ref_num_bytes[0], ref_num_bytes[1]]);

        let Some(buffer_bytes) = read_mem(pb_ptr.wrapping_add(IO_BUFFER_OFFSET), 4) else {
            return format!(
                "paramBlock=${:08X} refNum={} (buffer unreadable)",
                pb_ptr, ref_num
            );
        };
        let buffer = u32::from_be_bytes([buffer_bytes[0], buffer_bytes[1], buffer_bytes[2], buffer_bytes[3]]);

        let Some(req_count_bytes) = read_mem(pb_ptr.wrapping_add(IO_REQCOUNT_OFFSET), 4) else {
            return format!(
                "paramBlock=${:08X} refNum={} buffer=${:08X} (count unreadable)",
                pb_ptr, ref_num, buffer
            );
        };
        let req_count = u32::from_be_bytes([req_count_bytes[0], req_count_bytes[1], req_count_bytes[2], req_count_bytes[3]]);

        let Some(pos_mode_bytes) = read_mem(pb_ptr.wrapping_add(IO_POSMODE_OFFSET), 2) else {
            return format!(
                "paramBlock=${:08X}\n  refNum={} buffer=${:08X} count={}",
                pb_ptr, ref_num, buffer, req_count
            );
        };
        let pos_mode = i16::from_be_bytes([pos_mode_bytes[0], pos_mode_bytes[1]]);

        let Some(pos_offset_bytes) = read_mem(pb_ptr.wrapping_add(IO_POSOFFSET_OFFSET), 4) else {
            return format!(
                "paramBlock=${:08X}\n  refNum={} buffer=${:08X} count={} posMode={}",
                pb_ptr, ref_num, buffer, req_count, pos_mode
            );
        };
        let pos_offset = i32::from_be_bytes([pos_offset_bytes[0], pos_offset_bytes[1], pos_offset_bytes[2], pos_offset_bytes[3]]);

        let pos_mode_str = match pos_mode {
            1 => "fsAtMark",
            2 => "fsFromStart",
            3 => "fsFromLEOF",
            4 => "fsFromMark",
            _ => "?",
        };

        format!(
            "paramBlock=${:08X}\n  refNum={} buffer=${:08X} count={}\n  posMode={} ({}) posOffset={}",
            pb_ptr, ref_num, buffer, req_count, pos_mode, pos_mode_str, pos_offset
        )
    }

    /// Format Open return value
    fn format_open_return<F>(pb_ptr: Address, read_mem: &mut F) -> String
    where
        F: FnMut(Address, usize) -> Option<Vec<u8>>,
    {
        // IOParam structure offsets
        const IO_RESULT_OFFSET: Address = 16;
        const IO_REFNUM_OFFSET: Address = 24;

        let Some(result_bytes) = read_mem(pb_ptr.wrapping_add(IO_RESULT_OFFSET), 2) else {
            return format!("paramBlock=${:08X} (result unreadable)", pb_ptr);
        };
        let result = i16::from_be_bytes([result_bytes[0], result_bytes[1]]);

        let Some(ref_num_bytes) = read_mem(pb_ptr.wrapping_add(IO_REFNUM_OFFSET), 2) else {
            return Self::format_file_error(result);
        };
        let ref_num = i16::from_be_bytes([ref_num_bytes[0], ref_num_bytes[1]]);

        format!(
            "{}\n  refNum={}",
            Self::format_file_error(result),
            ref_num
        )
    }

    /// Format generic file operation result (just error code)
    fn format_file_result<F>(pb_ptr: Address, read_mem: &mut F) -> String
    where
        F: FnMut(Address, usize) -> Option<Vec<u8>>,
    {
        const IO_RESULT_OFFSET: Address = 16;

        let Some(result_bytes) = read_mem(pb_ptr.wrapping_add(IO_RESULT_OFFSET), 2) else {
            return format!("paramBlock=${:08X} (result unreadable)", pb_ptr);
        };
        let result = i16::from_be_bytes([result_bytes[0], result_bytes[1]]);

        Self::format_file_error(result)
    }

    /// Format Read/Write return value
    fn format_read_write_return<F>(pb_ptr: Address, _op_name: &str, read_mem: &mut F) -> String
    where
        F: FnMut(Address, usize) -> Option<Vec<u8>>,
    {
        // IOParam structure offsets
        const IO_RESULT_OFFSET: Address = 16;
        const IO_ACTCOUNT_OFFSET: Address = 40;

        let Some(result_bytes) = read_mem(pb_ptr.wrapping_add(IO_RESULT_OFFSET), 2) else {
            return format!("paramBlock=${:08X} (result unreadable)", pb_ptr);
        };
        let result = i16::from_be_bytes([result_bytes[0], result_bytes[1]]);

        let Some(act_count_bytes) = read_mem(pb_ptr.wrapping_add(IO_ACTCOUNT_OFFSET), 4) else {
            return Self::format_file_error(result);
        };
        let act_count = u32::from_be_bytes([act_count_bytes[0], act_count_bytes[1], act_count_bytes[2], act_count_bytes[3]]);

        format!(
            "{}\n  actualCount={}",
            Self::format_file_error(result),
            act_count
        )
    }

    /// Format File Manager error code
    fn format_file_error(err: i16) -> String {
        let err_name = match err {
            0 => "noErr",
            -33 => "dirFulErr",
            -34 => "dskFulErr",
            -35 => "nsvErr",
            -36 => "ioErr",
            -37 => "bdNamErr",
            -38 => "fnOpnErr",
            -39 => "eofErr",
            -40 => "posErr",
            -42 => "tmfoErr",
            -43 => "fnfErr",
            -44 => "wPrErr",
            -45 => "fLckdErr",
            -46 => "vLckdErr",
            -47 => "fBsyErr",
            -48 => "dupFNErr",
            -49 => "opWrErr",
            -50 => "paramErr",
            -51 => "rfNumErr",
            -52 => "gfpErr",
            -53 => "volOffLinErr",
            -54 => "permErr",
            -55 => "volOnLinErr",
            -56 => "nsDrvErr",
            -57 => "noMacDskErr",
            -58 => "extFSErr",
            -59 => "fsRnErr",
            -60 => "badMDBErr",
            -61 => "wrPermErr",
            _ => "?",
        };
        format!("ioResult=${:04X} ({})", err as u16, err_name)
    }
}
