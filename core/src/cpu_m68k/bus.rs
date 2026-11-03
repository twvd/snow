//! M68k CPU - Bus access functionality

use crate::bus::{Address, Bus, InspectRun, InspectableBus, IrqSource};
use crate::cpu_m68k::FpuM68kType;
use crate::cpu_m68k::bus_transaction::{FaultInfo, bus_transaction};
use crate::cpu_m68k::cache::{CacheTag, CpuCaches};
use crate::cpu_m68k::cpu::{
    Breakpoint, BusBreakpoint, CpuError, CpuM68k, Group0Details, WriteBack040,
};
use crate::cpu_m68k::pmmu::translate::{
    CacheMode, LAST_TRANSLATION_FETCH, LAST_TRANSLATION_READ, LAST_TRANSLATION_WRITE, Translation,
};
use crate::cpu_m68k::{
    CpuM68kType, CpuSized, M68000, M68020, M68040, TORDER_HIGHLOW, TORDER_LOWHIGH,
};
use crate::tickable::Ticks;
use crate::types::Word;
use crate::types::{Byte, Long};

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};

// M68k UM 3.8
pub const FC_UNUSED: u8 = 0;
pub const FC_USER_DATA: u8 = 1;
pub const FC_USER_PROGRAM: u8 = 2;
pub const FC_SUPERVISOR_DATA: u8 = 5;
pub const FC_SUPERVISOR_PROGRAM: u8 = 6;
pub const FC_MASK: u8 = 0b1111;

/// How the host looks at memory
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InspectView {
    /// Physical addresses, as a data read that hits physical tags would see
    Physical,
    /// Logical addresses in the data space of the current mode, as a data
    /// read would see them
    Virtual,
    /// Logical addresses in the program space of the current mode, as an
    /// instruction fetch would see them
    Program,
}

/// What a CPU access is for
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessKind {
    /// Operand read or write, including PC relative operands and MOVES
    Data,
    /// Instruction prefetch
    Fetch,
    /// Access to a physical address: translation table searches and
    /// descriptor updates
    Physical,
}

/// The part of an operand access from logical address `laddr` on, as it is
/// translated and accessed
#[derive(Clone, Copy)]
pub struct CpuAccess {
    pub kind: AccessKind,
    /// The operand, for bus errors
    pub fault: FaultInfo,
    pub laddr: Address,
    /// Bytes left in the operand from `laddr`
    pub size: u32,
    /// Write data, right-aligned to `size`
    pub wdata: Option<Long>,
}

/// Low `bytes` bytes of `val`
pub(in crate::cpu_m68k) const fn low_bytes(val: Long, bytes: u32) -> Long {
    if bytes >= 4 {
        val
    } else {
        val & ((1 << (bytes * 8)) - 1)
    }
}

impl<
    TBus,
    const ADDRESS_MASK: Address,
    const CPU_TYPE: CpuM68kType,
    const FPU_TYPE: FpuM68kType,
    const PMMU: bool,
    TCaches,
> CpuM68k<TBus, ADDRESS_MASK, CPU_TYPE, FPU_TYPE, PMMU, TCaches>
where
    TBus: Bus<Address, u8> + IrqSource,
    TCaches: CpuCaches,
{
    #[inline(always)]
    fn fc_data(&self) -> u8 {
        // The 68020 and up drive their function codes whether there is an
        // MMU or not: they also show in their bus error frames
        if CPU_TYPE >= M68020 {
            if self.regs.sr.supervisor() {
                FC_SUPERVISOR_DATA
            } else {
                FC_USER_DATA
            }
        } else {
            FC_UNUSED
        }
    }

    #[inline(always)]
    pub(in crate::cpu_m68k) fn fc_program(&self) -> u8 {
        if CPU_TYPE >= M68020 {
            if self.regs.sr.supervisor() {
                FC_SUPERVISOR_PROGRAM
            } else {
                FC_USER_PROGRAM
            }
        } else {
            FC_UNUSED
        }
    }

    /// Reads a big-endian word for the host: see inspect_read()
    pub fn inspect_read_word(&mut self, addr: Address, phys: bool) -> Option<u16>
    where
        TBus: InspectableBus<Address, Byte>,
    {
        let hi = self.inspect_read(addr, phys)?;
        let lo = self.inspect_read(addr.wrapping_add(1), phys)?;
        Some(u16::from_be_bytes([hi, lo]))
    }

    /// Writes a big-endian word for the host: see inspect_write()
    pub fn inspect_write_word(&mut self, addr: Address, val: u16, phys: bool) -> Option<()>
    where
        TBus: InspectableBus<Address, Byte>,
    {
        let [hi, lo] = val.to_be_bytes();
        self.inspect_write(addr, hi, phys)?;
        self.inspect_write(addr.wrapping_add(1), lo, phys)
    }

    /// Reads a byte of memory for the host, as a data read of the CPU would
    /// find it: from the caches if they hold it, else from the bus, at the
    /// physical address `addr` if `phys`, else at this logical address in the
    /// data space of the current mode. This leaves no trace: the caches and
    /// the ATC stay as they are, and it takes no CPU time. Returns None if
    /// there is no translation, or nothing to read at the address.
    pub fn inspect_read(&mut self, addr: Address, phys: bool) -> Option<Byte>
    where
        TBus: InspectableBus<Address, Byte>,
    {
        let addr = addr & ADDRESS_MASK;
        let cacr = self.regs.cacr;
        let paddr = if phys {
            let [data, _] = self.caches.inspect_aliases(addr);
            if let Some(alias) = data
                && let Some(byte) =
                    self.caches
                        .inspect_logical_read(cacr, alias.kind, alias.fc, alias.laddr)
                && self.inspect_translate(alias.fc, alias.laddr, false) == Some(addr)
            {
                return Some(byte);
            }
            addr
        } else {
            let fc = self.fc_data();
            if let Some(byte) = self
                .caches
                .inspect_logical_read(cacr, AccessKind::Data, fc, addr)
            {
                return Some(byte);
            }
            self.inspect_translate(fc, addr, false)?
        };
        if let Some(byte) = self.caches.inspect_phys_read(cacr, paddr) {
            return Some(byte);
        }
        self.bus.inspect_read(paddr)
    }

    /// Writes a byte of memory for the host, at the physical address `addr` if
    /// `phys`, else at this logical address in the data space of the current
    /// mode, ignoring protection. Memory is written, and the copies in the
    /// caches get the new value, but for data that memory doesn't have yet:
    /// that only changes in the data cache, and gets to memory with the rest
    /// of it. This leaves no other trace: lines stay as clean or dirty as they
    /// are, no line is loaded, written back or invalidated, the ATC and the
    /// page tables stay as they are, and it takes no CPU time. Returns None if
    /// there is no translation, or the address can't be written.
    pub fn inspect_write(&mut self, addr: Address, val: Byte, phys: bool) -> Option<()>
    where
        TBus: InspectableBus<Address, Byte>,
    {
        let addr = addr & ADDRESS_MASK;
        let paddr = if phys {
            addr
        } else {
            self.inspect_translate(self.fc_data(), addr, true)?
        };
        if !self.caches.inspect_phys_dirty(self.regs.cacr, paddr) {
            self.bus.inspect_write(paddr, val)?;
        }
        if self.watch_active {
            self.watch_write(paddr, 1);
        }
        self.caches.inspect_phys_write(paddr, val);
        for alias in self.caches.inspect_aliases(paddr).into_iter().flatten() {
            if self.inspect_translate(alias.fc, alias.laddr, false) == Some(paddr) {
                self.caches.inspect_alias_write(&alias, val);
            }
        }
        Some(())
    }

    /// Reads from `addr` on into `out` for the host as far as one run goes,
    /// which is at most to the end of a page for logical addresses. See
    /// inspect_read(), with the same absence of any trace; but a long word that
    /// is only in a cache tagged with logical addresses, in a page that has
    /// lost its translation, is a part of a hole here.
    pub fn inspect_read_bulk(
        &mut self,
        addr: Address,
        view: InspectView,
        out: &mut [Byte],
    ) -> InspectRun
    where
        TBus: InspectableBus<Address, Byte>,
    {
        let addr = addr & ADDRESS_MASK;
        let (fc, kind) = match view {
            InspectView::Physical => (None, AccessKind::Data),
            InspectView::Virtual => (Some(self.fc_data()), AccessKind::Data),
            InspectView::Program => (Some(self.fc_program()), AccessKind::Fetch),
        };
        let (paddr, out) = if let Some(fc) = fc {
            let page_mask = self.pmmu_page_mask();
            let len = out
                .len()
                .min(((addr & !page_mask) + page_mask - addr) as usize + 1);
            let Some(paddr) = self.inspect_translate(fc, addr, false) else {
                return InspectRun::Hole(len);
            };
            (paddr, &mut out[..len])
        } else {
            (addr, out)
        };
        let len = match self.bus.inspect_read_bulk(paddr, out) {
            InspectRun::Data(len) => len,
            hole => return hole,
        };

        for cached in self.caches.inspect_cached(self.regs.cacr, kind) {
            // Where the long word is, in the terms of the run
            let long_at = match cached.tag {
                CacheTag::Physical(long_paddr) => long_paddr.wrapping_sub(paddr),
                CacheTag::Logical { fc: long_fc, laddr } => {
                    if let Some(fc) = fc {
                        // Only what this space would hit
                        let same = if kind == AccessKind::Fetch {
                            long_fc & 0b100 == fc & 0b100
                        } else {
                            long_fc == fc
                        };
                        if !same {
                            continue;
                        }
                        laddr.wrapping_sub(addr)
                    } else {
                        let Some(long_paddr) = self.inspect_translate(long_fc, laddr, false) else {
                            continue;
                        };
                        long_paddr.wrapping_sub(paddr)
                    }
                }
            };
            for (i, byte) in cached.long.to_be_bytes().into_iter().enumerate() {
                let at = long_at.wrapping_add(i as Address) as usize;
                if at < len {
                    out[at] = byte;
                }
            }
        }
        InspectRun::Data(len)
    }

    /// Checks if an access needs to fail and raise bus error on alignment errors
    fn verify_access_physical<T: CpuSized>(&self, paddr: Address, read: bool) -> Result<()> {
        if std::mem::size_of::<T>() >= 2 && (paddr & 1) != 0 {
            // Unaligned access
            if CPU_TYPE < M68020 {
                log::warn!("Unaligned access: address {:08X}", paddr);

                // TODO should still happen on 68020+ for PC
                bail!(CpuError::AddressError(Group0Details {
                    function_code: 0,
                    ir: 0,
                    size: std::mem::size_of::<T>(),
                    atc_fault: false,

                    // TODO instruction bit
                    instruction: false,
                    read,
                    address: paddr,

                    // Filled in later
                    start_pc: 0,
                }));
            }
        }
        Ok(())
    }

    /// Reads a value from the bus and spends ticks.
    /// Virtual address version, data FC
    #[inline(never)]
    pub(in crate::cpu_m68k) fn read_ticks<T: CpuSized>(&mut self, vaddr: Address) -> Result<T> {
        self.read_ticks_generic::<T>(AccessKind::Data, self.fc_data(), vaddr)
    }

    /// Reads a value from the bus and spends ticks.
    /// Virtual address version, program FC
    pub(in crate::cpu_m68k) fn read_ticks_program<T: CpuSized>(
        &mut self,
        vaddr: Address,
    ) -> Result<T> {
        self.read_ticks_generic::<T>(AccessKind::Fetch, self.fc_program(), vaddr)
    }

    /// Reads a value from the bus and spends ticks.
    /// Physical address version
    #[inline(never)]
    pub(in crate::cpu_m68k) fn read_ticks_physical<T: CpuSized>(
        &mut self,
        o_paddr: Address,
    ) -> Result<T> {
        // FC unused for physical addressing
        self.read_ticks_generic::<T>(AccessKind::Physical, FC_UNUSED, o_paddr)
    }

    /// Notes a write of `len` bytes at physical address `paddr` if the host
    /// watches any of them
    #[cold]
    #[inline(never)]
    fn watch_write(&mut self, paddr: Address, len: u32) {
        let last = paddr.wrapping_add(len - 1);
        if self
            .watch_ranges
            .iter()
            .any(|&(first_w, last_w)| paddr <= last_w && first_w <= last)
        {
            self.watch_hit = true;
        }
    }

    /// Tells the CPU which ranges the host watches, each an address, a length
    /// and how to look at it. Logical ranges are watched where they translate
    /// to now: call this again when mmu_generation() or the mode change.
    pub fn inspect_watch(&mut self, ranges: &[(Address, u32, InspectView)])
    where
        TBus: InspectableBus<Address, Byte>,
    {
        self.watch_ranges.clear();
        for &(addr, len, view) in ranges {
            if len == 0 {
                continue;
            }
            let fc = match view {
                InspectView::Physical => {
                    let addr = addr & ADDRESS_MASK;
                    self.watch_ranges.push((addr, addr.wrapping_add(len - 1)));
                    continue;
                }
                InspectView::Virtual => self.fc_data(),
                InspectView::Program => self.fc_program(),
            };
            let page_mask = self.pmmu_page_mask();
            let mut done = 0;
            while done < len {
                let vaddr = addr.wrapping_add(done) & ADDRESS_MASK;
                let part = (len - done).min(page_mask - (vaddr & page_mask) + 1);
                if let Some(paddr) = self.inspect_translate(fc, vaddr, false) {
                    self.watch_ranges
                        .push((paddr, paddr.wrapping_add(part - 1)));
                }
                done += part;
            }
        }
        self.watch_active = !self.watch_ranges.is_empty();
    }

    /// Returns whether anything may have changed in the watched ranges since
    /// the last time this was called
    pub fn inspect_take_watch_hit(&mut self) -> bool {
        std::mem::take(&mut self.watch_hit)
    }

    /// Changes whenever translations may have changed: ATC flushes, MMU
    /// register writes, reset
    pub fn mmu_generation(&self) -> u32 {
        self.mmu_generation
    }

    /// Triggers data access breakpoints
    fn data_breakpoint(&mut self, write: bool, addr: Address, val: u8) {
        let kind = if write {
            BusBreakpoint::Write
        } else {
            BusBreakpoint::Read
        };
        if self.breakpoints.iter().any(|bp| {
            *bp == Breakpoint::Bus(kind, addr)
                || *bp == Breakpoint::Bus(BusBreakpoint::ReadWrite, addr)
        }) {
            log::info!(
                "Breakpoint hit (bus {}): ${:08X}, value: ${:02X}, PC: ${:08X}",
                if write { "write" } else { "read" },
                addr,
                val,
                self.regs.pc,
            );
            self.breakpoint_hit.set();
        }
    }

    /// Translates the logical address of an access
    #[inline(always)]
    fn translate(&mut self, acc: &CpuAccess) -> Result<Translation> {
        if acc.kind == AccessKind::Physical || !PMMU {
            return Ok(Translation {
                paddr: acc.laddr,
                cache: CacheMode::WriteThrough,
            });
        }
        // Accesses reuse the last translation of their kind made in the same
        // page. The ATC still holds it, until pmmu_translation_changed(), and
        // for a write with the page marked modified.
        let fc = acc.fault.fc;
        let slot = if acc.fault.write {
            LAST_TRANSLATION_WRITE
        } else if acc.kind == AccessKind::Fetch {
            LAST_TRANSLATION_FETCH
        } else {
            LAST_TRANSLATION_READ
        };
        if let Some(tr) = self.last_translation_find(slot, fc, acc.laddr) {
            return Ok(tr);
        }

        let tr = self
            .pmmu_translate(fc, acc.laddr, acc.fault.write)
            .map_err(|e| match e.downcast_ref() {
                Some(CpuError::BusError(details)) => {
                    anyhow!(CpuError::BusError(Group0Details {
                        size: acc.fault.size,
                        address: acc.laddr,
                        ..*details
                    }))
                }
                _ => e,
            })?;
        self.last_translation_add(slot, fc, acc.laddr, &tr);
        Ok(tr)
    }

    /// Runs a CPU access, up to the end of the long word at most: translates
    /// it, then lets the caches run it, on the bus as needed. Adds its
    /// duration to `ticks` and returns the read data and the number of bytes
    /// transferred.
    #[inline(always)]
    fn cpu_access(&mut self, acc: &CpuAccess, ticks: &mut Ticks) -> Result<(Long, u32)> {
        if acc.wdata.is_none()
            && let Some(res) = self.read_hit(acc.kind, acc.fault.fc, acc.laddr, acc.size, ticks)
        {
            return Ok(res);
        }
        self.cpu_access_translated(acc, ticks)
    }

    /// Runs a read from logically tagged caches, before translation, if
    /// they hold it: see CpuCaches::read_hit(). Data breakpoints need the
    /// physical address: data reads skip this when any is set.
    #[inline(always)]
    fn read_hit(
        &mut self,
        kind: AccessKind,
        fc: u8,
        laddr: Address,
        size: u32,
        ticks: &mut Ticks,
    ) -> Option<(Long, u32)> {
        if kind != AccessKind::Fetch && !self.breakpoints.is_empty() {
            return None;
        }
        if CPU_TYPE >= M68040 {
            return self.phys_read_hit(kind, fc, laddr, size);
        }
        self.caches.read_hit(
            self.regs.cacr,
            kind,
            fc,
            laddr,
            size,
            ticks,
            &mut self.history_current,
        )
    }

    /// read_hit() for physically tagged caches: with the last translation
    /// made in the page, as translate() would use
    #[inline(always)]
    fn phys_read_hit(
        &mut self,
        kind: AccessKind,
        fc: u8,
        laddr: Address,
        size: u32,
    ) -> Option<(Long, u32)> {
        if kind == AccessKind::Physical || !PMMU {
            return None;
        }
        let slot = if kind == AccessKind::Fetch {
            LAST_TRANSLATION_FETCH
        } else {
            LAST_TRANSLATION_READ
        };
        let tr = self.last_translation_find(slot, fc, laddr)?;
        if tr.cache == CacheMode::Inhibited {
            return None;
        }
        let res = self.caches.phys_read_hit(
            self.regs.cacr,
            kind == AccessKind::Fetch,
            fc,
            tr.paddr,
            size,
            &mut self.history_current,
        )?;
        self.write_credit = 0;
        Some(res)
    }

    /// cpu_access() past the caches' read hits
    #[inline(always)]
    fn cpu_access_translated(&mut self, acc: &CpuAccess, ticks: &mut Ticks) -> Result<(Long, u32)> {
        // The bus controller finishes the last write first
        self.write_credit = 0;
        let tr = self.translate(acc)?;
        let paddr = tr.paddr;
        let cacr = self.regs.cacr;
        let (data, len) = self
            .caches
            .access(&mut bus_transaction!(self, ticks), cacr, acc, &tr)?;

        if self.watch_active && acc.wdata.is_some() {
            self.watch_write(paddr, len);
        }

        // Data breakpoints, on the physical address of the access. They
        // are about data: instruction fetches don't trigger them.
        if !self.breakpoints.is_empty() && acc.kind != AccessKind::Fetch {
            for i in 0..len {
                let val = match acc.wdata {
                    Some(d) => d >> (8 * (acc.size - 1 - i)),
                    None => data >> (8 * (len - 1 - i)),
                };
                self.data_breakpoint(acc.fault.write, paddr.wrapping_add(i), val as u8);
            }
        }
        Ok((data, len))
    }

    /// Transfers the 16-byte line holding logical address `laddr` (MOVE16):
    /// reads it, or writes `wline`. The line counterpart of `cpu_access()`:
    /// translates it, lets the caches run it and spends its time. Returns the
    /// read line.
    pub(in crate::cpu_m68k) fn line_ticks(
        &mut self,
        laddr: Address,
        wline: Option<&[Long; 4]>,
    ) -> Result<[Long; 4]> {
        let laddr = laddr & !0xF;
        let acc = CpuAccess {
            kind: AccessKind::Data,
            fault: FaultInfo {
                fc: self.fc_data(),
                o_addr: laddr,
                size: 16,
                write: wline.is_some(),
            },
            laddr,
            size: 16,
            wdata: None,
        };
        let tr = self.translate(&acc)?;
        let paddr = tr.paddr;
        let cacr = self.regs.cacr;
        // The bus controller finishes the last write first
        self.write_credit = 0;
        let mut ticks = 0;
        let line = self.caches.line_access(
            &mut bus_transaction!(self, &mut ticks),
            cacr,
            &acc,
            &tr,
            wline,
        )?;
        self.advance_cycles(ticks)?;

        if self.watch_active && wline.is_some() {
            self.watch_write(paddr, 16);
        }

        // Data breakpoints, on the physical address of the access
        if !self.breakpoints.is_empty() {
            let data = wline.unwrap_or(&line);
            for i in 0..16 {
                let val = (data[i / 4] >> (8 * (3 - i % 4))) as u8;
                self.data_breakpoint(acc.fault.write, paddr.wrapping_add(i as Address), val);
            }
        }
        Ok(line)
    }

    /// Reads a value from the bus and spends ticks.
    #[inline(always)]
    pub(in crate::cpu_m68k) fn read_ticks_generic<T: CpuSized>(
        &mut self,
        kind: AccessKind,
        fc: u8,
        o_addr: Address,
    ) -> Result<T> {
        let len = std::mem::size_of::<T>() as u32;
        let mut result: u64 = 0;
        let fault = FaultInfo {
            fc,
            o_addr,
            size: std::mem::size_of::<T>(),
            write: false,
        };

        if CPU_TYPE < M68020 {
            // The 68000 moves a long operand as two word accesses, with the
            // address error check after the first one
            let addr = if CPU_TYPE == M68000 && len > 1 {
                o_addr & !1
            } else {
                o_addr
            };
            for w in 0..len.div_ceil(2) {
                let mut ticks = 0;
                let acc = CpuAccess {
                    kind,
                    fault,
                    laddr: addr.wrapping_add(2 * w) & ADDRESS_MASK,
                    size: len.min(2),
                    wdata: None,
                };
                let (data, n) = self.cpu_access(&acc, &mut ticks)?;
                result = (result << (8 * n)) | u64::from(data);
                self.advance_cycles(ticks)?;

                if CPU_TYPE == M68000 && w == 0 && len > 1 {
                    // Address errors occur AFTER the first Word was accessed
                    self.verify_access_physical::<T>(o_addr, true)?;
                }
            }
        } else {
            result = u64::from(self.read_operand(kind, fault, len)?);
        }

        Ok(T::chop(result as Long))
    }

    /// Reads an operand of `len` bytes (1-4, a 3-byte one included) with
    /// as many bus cycles as the port sizes need (68020+).
    #[inline(always)]
    fn read_operand(&mut self, kind: AccessKind, fault: FaultInfo, len: u32) -> Result<Long> {
        let o_addr = fault.o_addr;
        if (o_addr & 3) + len <= 4 {
            // The operand is within a long word: a single access. The access
            // is only described for translation, after a cache miss.
            let mut ticks = 0;
            let laddr = o_addr & ADDRESS_MASK;
            let data = match self.read_hit(kind, fault.fc, laddr, len, &mut ticks) {
                Some((data, _)) => data,
                None => {
                    let acc = CpuAccess {
                        kind,
                        fault,
                        laddr,
                        size: len,
                        wdata: None,
                    };
                    self.cpu_access_translated(&acc, &mut ticks)?.0
                }
            };
            self.advance_cycles(ticks)?;
            return Ok(data);
        }

        let mut ticks = 0;
        let mut done = 0;
        let mut result: u64 = 0;
        while done < len {
            // An access goes at most to the end of a long word: the rest
            // of the operand may be on another page, it is translated on
            // its own. We pass cpu_access() the full size left in the
            // operand, not just up to the next long word boundary, as the
            // 68020 and 68030 show it on SIZ.
            let acc = CpuAccess {
                kind,
                fault,
                laddr: o_addr.wrapping_add(done) & ADDRESS_MASK,
                size: len - done,
                wdata: None,
            };
            let (data, n) = self.cpu_access(&acc, &mut ticks)?;
            result = (result << (8 * n)) | u64::from(data);
            done += n;
        }
        self.advance_cycles(ticks)?;
        Ok(result as Long)
    }

    /// Reads a data operand of `len` bytes (1-4) and spends ticks (68020+).
    /// Virtual address version, data FC
    #[inline(never)]
    pub(in crate::cpu_m68k) fn read_ticks_len(&mut self, vaddr: Address, len: u32) -> Result<Long> {
        let fault = FaultInfo {
            fc: self.fc_data(),
            o_addr: vaddr,
            size: len as usize,
            write: false,
        };
        self.read_operand(AccessKind::Data, fault, len)
    }

    /// Writes a value to the bus and spends ticks.
    #[inline(always)]
    pub(in crate::cpu_m68k) fn write_ticks_order_generic<T: CpuSized, const TORDER: usize>(
        &mut self,
        kind: AccessKind,
        fc: u8,
        o_addr: Address,
        value: T,
    ) -> Result<()> {
        let len = std::mem::size_of::<T>() as u32;
        let value = value.expand();
        let fault = FaultInfo {
            fc,
            o_addr,
            size: std::mem::size_of::<T>(),
            write: true,
        };

        if CPU_TYPE < M68020 {
            // The 68000 moves a long operand as two word accesses. Their
            // order and the address error check after the first one belong
            // to the instruction, not to the bus: a long written to -(An)
            // has its low word written first (TORDER_HIGHLOW).
            let addr = if CPU_TYPE == M68000 && len > 1 {
                o_addr & !1
            } else {
                o_addr
            };
            let size = len.min(2);
            let words = len.div_ceil(2);
            for i in 0..words {
                let w = if TORDER == TORDER_HIGHLOW {
                    words - 1 - i
                } else {
                    i
                };
                let mut ticks = 0;
                let acc = CpuAccess {
                    kind,
                    fault,
                    laddr: addr.wrapping_add(2 * w) & ADDRESS_MASK,
                    size,
                    wdata: Some(low_bytes(value >> (8 * (len - size - 2 * w)), size)),
                };
                self.cpu_access(&acc, &mut ticks)?;
                self.advance_cycles(ticks)?;

                if CPU_TYPE == M68000
                    && i == 0
                    && (if TORDER == TORDER_HIGHLOW {
                        len == 4
                    } else {
                        len > 1
                    })
                {
                    // Address errors occur AFTER the first Word was accessed
                    self.verify_access_physical::<T>(o_addr, true)?;
                }
            }
        } else {
            self.write_operand(kind, fault, len, value)?;
        }

        Ok(())
    }

    /// Writes the low `len` bytes (1-4, a 3-byte operand included) of
    /// `value` with as many bus cycles as the port sizes need (68020+).
    #[inline(always)]
    fn write_operand(
        &mut self,
        kind: AccessKind,
        fault: FaultInfo,
        len: u32,
        value: Long,
    ) -> Result<()> {
        if CPU_TYPE >= M68040
            && (fault.o_addr & 3) + len <= 4
            && self.phys_write_hit(kind, fault.fc, fault.o_addr & ADDRESS_MASK, len, value)
        {
            return Ok(());
        }
        self.write_operand_slow(kind, fault, len, value)
    }

    /// A write to a copyback page that hits in physically tagged caches,
    /// with the last write translation made in the page, as translate()
    /// would use. Returns false when the write has to go through
    /// write_operand_slow().
    #[inline(always)]
    fn phys_write_hit(
        &mut self,
        kind: AccessKind,
        fc: u8,
        laddr: Address,
        size: u32,
        value: Long,
    ) -> bool {
        if kind == AccessKind::Physical
            || !PMMU
            || !self.breakpoints.is_empty()
            || self.watch_active
        {
            return false;
        }
        let Some(tr) = self.last_translation_find(LAST_TRANSLATION_WRITE, fc, laddr) else {
            return false;
        };
        if tr.cache != CacheMode::CopyBack {
            return false;
        }
        if !self
            .caches
            .phys_write_hit(self.regs.cacr, fc, tr.paddr, size, value)
        {
            return false;
        }
        self.write_credit = 0;
        true
    }

    /// write_operand() past the cache hits
    #[inline(always)]
    fn write_operand_slow(
        &mut self,
        kind: AccessKind,
        fault: FaultInfo,
        len: u32,
        value: Long,
    ) -> Result<()> {
        let mut ticks = 0;
        let mut done = 0;
        while done < len {
            // A long word at a time, as for reads
            let acc = CpuAccess {
                kind,
                fault,
                laddr: fault.o_addr.wrapping_add(done) & ADDRESS_MASK,
                size: len - done,
                wdata: Some(low_bytes(value, len - done)),
            };
            match self.cpu_access(&acc, &mut ticks) {
                Ok((_, n)) => done += n,
                Err(e) if kind != AccessKind::Physical && self.wb040_defer => {
                    self.wb040_defer_write(e, fault.o_addr, value, done > 0)?;
                    break;
                }
                Err(e) => return Err(e),
            }
        }
        self.advance_cycles(ticks)?;
        self.write_credit = ticks;
        Ok(())
    }

    /// Writes the low `len` bytes (1-4) of `value` as a data operand and
    /// spends ticks (68020+). Virtual address version, data FC
    #[inline(never)]
    pub(in crate::cpu_m68k) fn write_ticks_len(
        &mut self,
        vaddr: Address,
        len: u32,
        value: Long,
    ) -> Result<()> {
        let fault = FaultInfo {
            fc: self.fc_data(),
            o_addr: vaddr,
            size: len as usize,
            write: true,
        };
        self.write_operand(AccessKind::Data, fault, len, value)
    }

    /// Writes a value to the bus (big endian) and spends ticks.
    /// Physical address version
    #[allow(dead_code)]
    pub(in crate::cpu_m68k) fn write_ticks_physical<T: CpuSized>(
        &mut self,
        paddr: Address,
        value: T,
    ) -> Result<()> {
        self.write_ticks_order_physical::<T, TORDER_LOWHIGH>(paddr, value)
    }

    /// Writes a value to the bus (big endian) and spends ticks.
    /// Virtual address version, data FC
    #[inline(never)]
    pub(in crate::cpu_m68k) fn write_ticks<T: CpuSized>(
        &mut self,
        vaddr: Address,
        value: T,
    ) -> Result<()> {
        self.write_ticks_order_generic::<T, TORDER_LOWHIGH>(
            AccessKind::Data,
            self.fc_data(),
            vaddr,
            value,
        )
    }

    /// Writes a value to the bus (big endian) and spends ticks.
    /// Virtual address version
    #[inline(never)]
    pub(in crate::cpu_m68k) fn write_ticks_order<T: CpuSized, const TORDER: usize>(
        &mut self,
        vaddr: Address,
        value: T,
    ) -> Result<()> {
        self.write_ticks_order_generic::<T, TORDER>(AccessKind::Data, self.fc_data(), vaddr, value)
    }

    /// Writes a value to the bus (big endian) and spends ticks, but writes
    /// the word in opposite order if the type is Long.
    /// Virtual address
    pub(in crate::cpu_m68k) fn write_ticks_wflip<T: CpuSized>(
        &mut self,
        addr: Address,
        value: T,
    ) -> Result<()> {
        match std::mem::size_of::<T>() {
            4 => {
                let v: Long = value.expand();
                self.write_ticks_order_generic::<Word, TORDER_LOWHIGH>(
                    AccessKind::Data,
                    self.fc_data(),
                    addr.wrapping_add(2),
                    v as Word,
                )?;
                self.write_ticks_order_generic::<Word, TORDER_LOWHIGH>(
                    AccessKind::Data,
                    self.fc_data(),
                    addr,
                    (v >> 16) as Word,
                )
            }
            _ => self.write_ticks_order_generic::<T, TORDER_LOWHIGH>(
                AccessKind::Data,
                self.fc_data(),
                addr,
                value,
            ),
        }
    }

    /// M68040: a data write of the instruction faulted. Unless the
    /// instruction is restarted on faults, the instruction completes and the
    /// write waits in a write-back slot for the access error handler
    /// (M68040UM 8.4.6). One write can wait that way, a second fault restarts
    /// the instruction.
    fn wb040_defer_write(
        &mut self,
        e: anyhow::Error,
        o_addr: Address,
        value: Long,
        second_page: bool,
    ) -> Result<()> {
        if self.wb040.is_some() {
            return Err(e);
        }
        let Some(CpuError::BusError(details)) = e.downcast_ref() else {
            return Err(e);
        };
        self.wb040 = Some(WriteBack040 {
            details: Group0Details {
                // The fault address is the start of the operand
                address: o_addr,
                ..*details
            },
            data: value,
            ma: second_page && details.atc_fault,
            trace: false,
        });
        Ok(())
    }

    #[inline(never)]
    pub(in crate::cpu_m68k) fn write_ticks_order_physical<T: CpuSized, const TORDER: usize>(
        &mut self,
        o_paddr: Address,
        value: T,
    ) -> Result<()> {
        self.write_ticks_order_generic::<T, TORDER>(AccessKind::Physical, FC_UNUSED, o_paddr, value)
    }
}
