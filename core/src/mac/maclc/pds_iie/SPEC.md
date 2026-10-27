# Apple IIe Card (LC PDS) — how the card works

A hardware and protocol reference for the card: the Gemini host interface,
the card memory and firmware, the trap protocol, the `$02` host calls,
interrupts, disks and the Mac-side display. Everything carries its
evidence level:

- **[ST]** the IIe Startup self-test (`Conf` 1000 = 68k test routine,
  `Conf` 1001 = 65C02 shim, `Conf` 1002-1004 = patches). It checks the
  exact trap code/address/data for each step: Apple's own acceptance
  test of the Gemini, the strongest evidence we have.
- **[FW]** the decrypted card firmware (`Monx` resources, see §4).
- **[DRV]** the IIe Startup application: its CODE 1 (main) and CODE 5
  (A2eDRVR4, the card driver) resources, disassembled; offsets are
  within each resource.
- **[LOG]** bus traces of IIe Startup driving the card (register and
  Slinky traffic, captured under emulation).
- **[MG]** M.G.'s Apple //e Card tech notes
  (<http://apple2.guidero.us/doku.php/mg_notes/iie_card/start>):
  experiments on a real card, plus resource and firmware analysis.
- **[TIL]** Apple Tech Info Library 6571, *Apple IIe Card: Hardware
  Specifications* (7/93), and the Apple IIe Card data sheet (3/91).
- **[WP]** Wikipedia, *Apple IIe Card*; **[TN]** Apple II Technical
  Notes IIe #10 and Misc #7; **[TRM]** *Apple IIe Technical Reference
  Manual*; **[DN]** Apple's LC, LC II and LC III developer notes.
- **[ROM]** the Macintosh LC III ROM, disassembled and booted under
  emulation (its MMU tables read back after boot); **[MAME]** MAME's
  IWM, floppy and LC PDS models.
- **[INF]** inferred, consistent with the above; **[OPEN]** unknown.

## 1. What the card is

- PDS card for the LC family (LC, LC II, LC III, Color Classic, LC
  475/520/550/575 …). Needs **24-bit addressing**; System 6.0.8-7.5.5.
  [WP]
- Parts: **65C02** (1.02 MHz, or 1.9 MHz in the optional fast mode
  [TIL]; Wikipedia gives 2.046 MHz), the
  **Gemini** ASIC (LSI Logic, mostly an Apple IIGS **Mega II**: the IIe
  system functions, minus the CPU, RAM and video generation), an
  **IWM**, **256 KB RAM**, a **17.2349 MHz oscillator** that replaces
  the Mac's video clock in the 560×384 mode, and the 26-pin port
  (Y-cable: two 5.25" drives, a UniDisk 3.5, joystick). [WP]
- Apple's specification [TIL]: 16-bit address bus, 2 banks of 64K
  (128K) for the IIe; 256K on the card; access to up to 1 MB of
  Macintosh RAM (the emulated memory card); the IIe ROM lives in
  write-protected card RAM and contains Applesoft BASIC, the monitor,
  the 80-column firmware and diagnostic routines. Text: 40/80 × 24,
  5×7 matrix, normal/inverse/MouseText. Graphics: lo-res 40×48 16
  colours, hi-res 280×192 6 colours (and monochrome), double hi-res
  140×192 16 colours and 560×192 monochrome. OS: ProDOS, DOS 3.3,
  Pascal. Drives: two Apple 5.25 Drives (the platinum A9M0107 is the
  only 5.25" drive supported), one UniDisk 3.5, the internal
  SuperDrive. **Monitors: all LC monitors (12" and 13").** Order no.
  M0444LL/A; dealer installation.
- **No ROM and no video generator on the card.** The Mac loads the
  firmware into card RAM through the Slinky. The Mac draws the Apple II
  screen with QuickDraw, from display-store traps and the soft-switch
  state, into its own framebuffer, full-screen. While the card runs,
  the Mac application is the card's service processor: keyboard, mouse,
  speaker, serial ports, clock, SmartPort/ProDOS volumes, AppleShare
  and the memory card are all host-emulated. [WP][MG][DRV]
- Mac applications are suspended while the IIe runs. [WP]

## 2. Bus attachment

- Pseudo-slot $E (the LC "processor-direct slot", addressed as NuBus
  slot $E). [DN]
- **How the app finds the register block** (CODE 2 0x766-0x7D8,
  0x95A):
  1. `_SNextTypeSRsrc` for category 10, type 21. For the slot s it
     returns, the base is `((($F0|s) << 4) | s) ror 12`, which is
     **`$FEE00000`** for slot E. With no slot found, the base is
     `$FEE00000` on the LC (Gestalt machine type 19) and 0 elsewhere
     (error 5).
  2. The base becomes **`$50FC0000`** instead in two cases: on the LC
     when SysEnvirons does not report a 68020 (processor field ≠ 3);
     and on other machines when Gestalt `'sltc'` succeeds and returns
     0. A Gestalt error keeps `$FEE00000`. [DRV]
  3. All register access goes through that base (global
     `a5(-4578)`). [DRV]
- **What the card sees.** The LC's 96-pin connector carries A0-A27,
  A31 and /FC3; A28-A30 are absent [DN]. The card must answer at:
  - `$FEE00000` from 32-bit code;
  - the 24-bit `$E00000`, which the LC passes as card address
    `$E00000`;
  - the LC II's and LC III's MMU remaps of 24-bit addresses (below),
    `$FE000000` and `$FEE00000`.

  The only decode that fits all of these is that the card decodes
  **A19-A0 only** within its slot select. The register block is then
  at the bottom of every 1 MB of the slot space and the declaration
  ROM at the top of every 1 MB. [DN][DRV][INF]
- **LC II**: /FC3 is always driven high; the MMU remaps the 24-bit
  `$E00000`-`$EFFFFF` to `$FE000000`-`$FE0FFFFF`. [DN]
- **LC III**:
  - The PDS is selected (/PDS.AS) only for: /FC3 low (24-bit) with
    A23-A20 = $E, or /FC3 high (32-bit) with `$FExxxxxx` or
    `$Exxxxxxx` (the super slot space). /FC3 is low only for 24-bit
    accesses to slot $E space. [DN]
  - The ROM's 24-bit MMU table (TC `$80D86500`, initial shift 8) maps:

    | 24-bit logical | physical |
    |---|---|
    | `$E00000`-`$EFFFFF` | `$FE000000`-`$FE0FFFFF` |
    | `$F00000` | `$50F00000` (I/O) |
    | `$FC0000`-`$FFFFFF` | **`$FEE00000`-`$FEEFFFFF`** |

    The 32-bit table makes `$80000000`-`$FFFFFFFF` transparent through
    TT1 (`$807F8507`). So in 24-bit mode the app's `$50FC0000` base
    reaches the card at `$FEE00000`, where A23-A20 = $E: the same
    cycle the card sees on an LC. [ROM]
  - `SwapMMUMode` is picked by `MMUType` (table at ROM `$3E50`). Only
    the HMMU variant (`$3F86`) touches hardware: it flips bit 3 of the
    VIA2/pseudo-VIA data register B (`($0CEC)`), the V8's FC3 output.
    The 68030 and 68040 variants only reload CRP/TC/TT. So on the LC
    III no software bit tells Sonora the addressing mode. On the LC
    III, bit 3 of that register is a serial data line, most likely to
    the Omega clock chip (ROM `$8C734`). [ROM] [OPEN] what drives
    /FC3.
  - `$50FC0000` is not the PDS (/PDS.AS). It is the second entry of
    the ROM's base-address table (ROM `$D2E2C`), probably the
    diagnostic ROM probe, which reads 8 bytes there right after reset.
    MAME has a VIA1 mirror there (`mirror(0x00fc0000)`). [ROM][DN]
  - MMU templates (ROM `$813C8`) map logical `$50FC0000` and 24-bit
    `$FC0000` (256 KB each) to `$FEE00000`. [ROM]
- The card does not work in 32-bit mode, even on real hardware. [WP]
- **16-bit port**: the declaration ROM sits on the odd bytes (lanes
  1+3), and the Slinky RAM test needs one transfer per word cycle.
  [LOG][ST]
- Declaration ROM: the standard format. It must hold a Board sResource
  (category 1) plus the card's sResource (category 10, type 21,
  drvrSW 1) that the INIT searches for. The real ROM has not been
  dumped publicly; a ROM with these two sResources satisfies the LC
  boot ROM's Slot Manager and the INIT. [LOG]

## 3. Host register block

Offsets from the base; "hi" = even byte (D15-8), "lo" = odd byte. [MG]
was the first published table; it is refined here.

| Off | R/W | Meaning | Evidence |
|---|---|---|---|
| `$00` | r | high byte of the trap-code word; never read by IIe Startup | [DRV] |
| `$01` | r | **Trap code**, 0 = none. Wait loops read it once and discard the value, then poll until nonzero (1000 tries). | [ST][DRV] |
| `$04/$05` | r | **Trap address**. The reset vector fetch reports `$FFFC`; slot traps report the compacted `(slot << 4) \| reg`. Reading it releases a motor-on display trap (§6.1). | [ST][DRV] |
| `$08` | r | **Video soft-switch byte**: bit 7 ALTCHAR, 6 TEXT, 5 HIRES, 4 AN3 (1 = DHIRES off), 3 80COL, 2 MIXED, 1 PAGE2, 0 80STORE (all set = `$FF`). Also read, value discarded, before every `$1C` write. | [ST]; bit order [DRV] (§6.3) |
| `$09` | r | bit 7 = **keyboard strobe**. | [ST] |
| `$0C` | r | Trap data (the byte a trapped write stored). | [ST] |
| `$0C` | w | **Ack/resume**. Also completes a trapped read with the current `$19`. | [ST][DRV] |
| `$0D` | r/w | Control register, §3.1. | [ST][DRV] |
| `$10`/`$11` | r/w | Slinky main / aux lane, §5. | [ST][LOG] |
| `$14` | — | unknown ("none?"); never accessed by IIe Startup | [MG][DRV] |
| `$18` | w | Keyboard: key code, bit 7 = strobe. | [ST][MG] |
| `$19` | w | **Read-trap answer**. Writing it resumes the trapped read. | [ST][DRV] |
| `$1C/$1D` | r/w | Slinky address (big-endian; reads return the incremented value). | [ST][DRV] |
| `$20` | r/w | Control 2, §3.2. | [ST][DRV] |
| `$21` | r/w | Control 3: **display-trap enable mask** (bits 7:2), §6.3. Bit 1: **game-button read traps** (`$C061`-`$C063`, §6.2); the driver keeps it set (`$22`, `$2A`). Bit 0 [OPEN]. | [ST][DRV] |

### 3.1 `$0D`

| Bit | Meaning | Evidence |
|---|---|---|
| 7 | **AKD** (any key down): the bit 7 that `$C010` returns. | [ST] |
| 6 | **VBL**, host-driven: what `$C019` bit 7 reads. The app's VBL task clears it (code_1 0x48D0). The card has no VBL timer of its own. | [ST][DRV] |
| 5 | unknown; never touched by IIe Startup | [DRV] |
| 4 | **ROM write enable** (65C02 writes land in the ROM image). The driver sets it when the firmware writes `$5A` to `$C072` and clears it on `$00` (code_5 0x1686). | [ST][DRV][MG] |
| 3 | **Slinky upper-128K select**. The VBL task saves and restores it together with `$1C` around its channel handlers. | [ST][DRV] |
| 2 | While set, zero-page/stack stores under ALTZP land in main memory, i.e. the aux bank is disabled or write-protected [ST]. While set, the driver also masks `$21` to bits 3:2 (§6.3) [DRV]. Exact meaning [OPEN]. | [ST][DRV] |
| 1:0 | **Display-mode tag**, echoed in **bits 7:6 of every trap code** (`$0D=3` → `$E0`/`$E1`). The host chooses it per display mode (§6.3) and uses the code's top bits to index its handler tables; nothing shows that the Gemini interprets it. | [ST][DRV] |

### 3.2 `$20`

| Bit | Meaning | Evidence |
|---|---|---|
| 7 | **Run**. With bit 3 clear, the first trap is the reset vector fetch (`$FFFC`). | [ST] |
| 5 | **IRQ** to the 65C02. | [ST] |
| 4 | **Drive motor on** (host-maintained): set on the `$38` trap, cleared on `$28`. While set, display stores report odd codes, which the driver only records as dirty cells (§6.1) and redraws at motor-off: *the screen freezes while the motor runs*, which [MG] observed on real hardware. | [ST][DRV][MG] |
| 3 | **CPU run/hold** (inferred): clear = the 65C02 is held at its next bus cycle; set = running. At the run edge (`$80`) the reset vector fetch traps first (`$FFFC`). Otherwise the hold latches nothing: the driver clears it, rewrites `$21`, re-reads `$01`, then sets it again (code_5 0x81A-0x8A0), and it saves the `$01` it read and dispatches it afterwards as a pending trap (code_5 0x82C, 0x175E). So `$01` must read 0 during a hold with nothing pending, and a trap latched before the hold stays pending across it. | [ST][DRV] [INF] |

`$20/$21 = $0802` stops the CPU. The 1000 reads of `$20` that follow
are a **delay loop**, not a poll. [DRV]

## 4. Card memory and firmware

### 4.1 RAM layout (256 KB)

| Card offset | Contents | Evidence |
|---|---|---|
| `0x00000` | IIe main 64K | [ST] |
| `0x10000` | IIe aux 64K | [ST] |
| `0x20000`-`0x2FFFF` | upper-main 64K ("firmware"), §4.2 | [FW][MG] |
| `0x30000`-`0x3FFFF` | upper-aux 64K: host buffers? (the `$C800` images are in upper-main, §12b) | [OPEN] |

The Slinky reaches the upper half with `$0D` bit 3 set (`$10` → upper
main, `$11` → upper aux). [ST]

### 4.2 Firmware (`Monx` resources)

`Monx` resources are XOR-obfuscated with the repeating key **"DoubleX"**
(the card's codename was "Double Exposure"). A 6-byte little-endian
header gives `[65C02 addr][firmware offset][length]`; the firmware
offset is relative to the upper-main 64K. [MG], verified by decrypting
all of them. [FW]

| Resource | CPU addr | FW offset | Size | What |
|---|---|---|---|---|
| 2606 A2EMonitorROM | `$C000` | `$0000` | 16K | the patched enhanced IIe ROM (`$C000`-`$FFFF`) |
| 2607 D1 NMI-IRQ Rcvr | `$D000` | `$5000` | 102 | §8 |
| 2608 D2 NMI-IRQ Rcvr | `$D000` | `$9000` | 102 | same for D2 |
| 2609 D1 INTCX | `$C3FA` | `$43FA` | 423 | interrupt entry (`$C3FA` = the enhanced IIe IRQ entry) |
| 2610 D2 INTCX | `$C3FA` | `$83FA` | 423 | |
| 2611 D1 Vectors | `$FFFA` | `$7FFA` | 6 | NMI=`$D000`, RESET=`$FA62`, IRQ=`$D003` |
| 2612 D2 Vectors | `$FFFA` | `$BFFA` | 6 | the same |
| 5120 PFI 0 | `$D400` | `$5400` | 9472 | D1 bank `$D400`-`$F6FF` |
| 5121 PFI 1 | `$D400` | `$9400` | 9472 | D2 bank |
| 5632 5.25" Disk | `$C800` | `$F000` | 2K | the Disk II `$C800` code |

So the upper-main 64K is: **main ROM `$0000`**, **D1 `$4000`**, **D2
`$8000`**, slot/`$C800` area `$C000`-`$FFFF` (CPU address − `$C000` +
bank base). The banks map the 65C02's `$C000`-`$FFFF`. [FW]

- **Main** (A2EMonitorROM) is the normal IIe ROM view. [FW][MG]
- **D1/D2** ("alternate firmware") hold only the interrupt receivers,
  the INTCX entry, PFI and their own vectors; everything else in them
  is empty. `$C02B` bit 3 picks D1 or D2. [FW][MG]
- `$C028` = switch to main firmware, `$C029` = switch to the alternate
  bank (read or write). [MG]. The IRQ path ends with `BIT $C028`
  (INTCX `$C410`). [FW]
- **Interrupts run in the alternate bank**: the D1/D2 vector
  resources exist and point into D1/D2-only code, and the self-test
  puts its NMI handler only in D1 while giving the main ROM and D1 the
  same vectors. So the Gemini must switch to the alternate bank when
  the CPU takes an NMI/IRQ (at the vector fetch at the latest). The
  NMI receiver returns with a bare `RTI`, no `$C028`, so the switch
  back on that path is either RTI, a host action, or a later `$C028`.
  [INF] [OPEN] exact trigger. The main ROM's `$C3FA`-`$C412` is
  byte-identical to D1/D2 INTCX, so the IRQ path's `JMP $C410`
  works whichever bank `$C100`-`$C7FF` comes from at that moment; the
  window that must come from the alternate bank is at least the
  vectors and the receiver (`$D000`-`$D065`). [FW]
- Shim `$2143` ([ST]): `LDY $FFFA; LDA $C02B; ORA #$80; STA $C028;
  CPY $FFFA` must see `$FFFA` *change*, and it then clears `$C02B`
  bit 3. In the self-test every RAM bank's `$FFFA` is `$00`, so the
  post-write view must be non-RAM (reads non-zero). A `$C028` *write*
  with bit 7 set therefore selects something other than the main/D1/D2
  RAM. [ST] [OPEN] what.
- `$C02B` bits ([MG], from TN IIe #10 and the firmware): 7 set in
  IRQINTCX; 6 cleared at `$C41C`, set at `$C573` (INTCX); 5 enable
  `$C8xx` writes?; 4 second aux bank / second zero page?; 3 alt bank
  D1/D2; 2 CPU speed (documented); 1 HDSEL; 0 /3.5DISK. The PFI code
  sets bits 4+5 after writing `$5A` to `$C072` and clears them after
  writing `$00`. While they are set it uses `$C800`-`$CFFF` as RAM
  (`STZ`/`INC $C889`, the zero-page save `$80`-`$93` → `$CF94,X`,
  `$CF73`), so bit 5 enables C8-space writes. [FW]. The PFI is entered
  from its device's `$C800` code (`BIT $C08A / STA $C029 / JSR $D400 /
  STA $C028`) with that slot selected, so its C8 RAM is the slot's
  `$C8xx` image; the host's card-memory accessor maps CPU
  `$C800`-`$CFFF` to fw `$C000 + n×$800` for that slot. [FW][DRV]
- Bit 4 (with ALTZP: a second zero page, §4.3). No host code touches
  the upper-aux 64K beyond clearing it at startup and the RAM test,
  so it is presumably the 6502-private "second aux bank" that bit 4
  selects; whether page 1 and the aux LC area move too is [OPEN]. The
  main ROM's IRQ exit re-selects `$C08A`/`$C082` (LC read ROM,
  write-protect) only when bit 4 was set, which ties bit 4 to the
  language-card area as well. [FW][DRV][INF]
- `$C072` writes: INTCX `$C4AB`=`$00`, `$C579`=`$5A`; PFI 0 `$D445`/
  `$F60B`=`$5A`, `$D470`/`$F5EF`=`$00`; PFI 1 `$D4BC`/`$F178`=`$5A`,
  `$D519`/`$F205`=`$00`. [MG]. The host turns `$5A`/`$00` into `$0D`
  bit 4 (ROM write enable). [DRV]
- ID bytes: `$FBB3=$06`, `$FBC0=$E0` (enhanced IIe), **`$FBDD=$02`**
  marks the card (it is the `02 01` bell call, §7), `$FBBE` = version.
  [TN][MG]
- Other firmware differences: the 5.25" boot fails over to the next
  slot (like the IIc/IIgs); the title screen carries a copyright notice
  (host-drawn, §7). [MG]

### 4.3 Language card and aux

- LC RAM: the standard IIe behaviour (bank 1/2, prewrite, ALTZP
  selects main/aux), as exercised by shim `$2198`. [ST]. Its physical
  placement inside the 64K banks (what the Slinky sees) is [OPEN]; see
  §14 for the Mega II-based speculation.
- `$C02B` bit 4 + ALTZP: a second zero page (shim `$2198`). [ST]

### 4.4 IIe soft switches the Gemini implements itself

The Gemini implements the enhanced IIe's memory and ROM switches in
hardware: none of them trap (§6.2) [ST][DRV], and the card's own
firmware relies on them exactly [FW]. The semantics are the standard
ones (*Apple IIe Technical Reference Manual*, [TRM]).

Writes (any value):

| Addr | Switch | Effect |
|---|---|---|
| `$C000`/`$C001` | 80STORE off/on | on: PAGE2 selects the bank for the display pages (below); also trapped as a display switch (§6.2) |
| `$C002`/`$C003` | RAMRD off/on | `$0200`-`$BFFF` reads main/aux, except the 80STORE ranges |
| `$C004`/`$C005` | RAMWRT off/on | `$0200`-`$BFFF` writes main/aux, except the 80STORE ranges |
| **`$C006`** | **SETSLOTCXROM** | `$C100`-`$CFFF` = the **slot** ROMs (`$C300` per SLOTC3ROM, `$C800` per INTC8ROM) |
| **`$C007`** | **SETINTCXROM** | `$C100`-`$CFFF` = the **internal** ROM (main bank image) |
| `$C008`/`$C009` | ALTZP off/on | zero page, stack and the LC RAM in main/aux |
| `$C00A` | SETINTC3ROM | `$C300`-`$C3FF` = internal (80-column firmware) |
| `$C00B` | SETSLOTC3ROM | `$C300`-`$C3FF` = slot 3 ROM |
| `$C00C`-`$C00F` | 80COL, ALTCHARSET | (trapped as display switches) |

**Bank selection for `$0200`-`$BFFF`** [TRM]:

- 80STORE **off**: reads follow RAMRD, writes follow RAMWRT.
- 80STORE **on**: `$0400`-`$07FF` is aux when PAGE2 is set and main
  otherwise (reads and writes alike); `$2000`-`$3FFF` likewise, but only
  when HIRES is also on. PAGE2 then no longer selects the displayed
  page. All other addresses keep following RAMRD/RAMWRT; in
  particular `$0800`-`$0BFF` and `$4000`-`$5FFF` do **not** move to aux.
- `$0000`-`$01FF` follows ALTZP, independently of all of the above.

The 80-column firmware relies on this: it writes the even columns with
`STA $C055 / STA $04xx` (aux) and the odd columns with `STA $C054 /
STA $04xx` (main) while 80STORE is on (main ROM `$CCD5`-`$CCE2`) [FW][LOG].
The display-store trap code reports the bank the store actually lands
in (`$8A` vs `$82` for those two stores, with the 80-column tag) [ST].

Reads (bit 7 = state; bits 6:0 = the keyboard latch):

| Addr | Read | Bit 7 = 1 when |
|---|---|---|
| `$C010` | AKD + clear strobe | a key is down (from `$0D` bit 7 on the card) |
| `$C011` | RDLCBNK2 | LC bank 2 selected |
| `$C012` | RDLCRAM | LC RAM read-enabled |
| `$C013`/`$C014` | RDRAMRD/RDRAMWRT | aux selected |
| **`$C015`** | **RDCXROM** | **INTCXROM on (internal ROM)**. Confirmed by the card ROM: the `$FBB4` thunk saves it with `BIT $C015 / PHP`, and the exit at `$C20A` does `PLP / BMI keep / STA $C006`. [FW] |
| `$C016` | RDALTZP | ALTZP on |
| `$C017` | RDC3ROM | SLOTC3ROM on (slot 3 ROM) |
| `$C018` | RD80STORE | 80STORE on |
| `$C019` | RDVBL | (from `$0D` bit 6 on the card) |
| `$C01A`-`$C01F` | TEXT, MIXED, PAGE2, HIRES, ALTCHAR, 80COL | on |

INTC8ROM (`$C800`-`$CFFF` when INTCXROM is off): set by any access to
`$C300`-`$C3FF` while SLOTC3ROM is off (internal C3), cleared by any
access to `$CFFF`. While set, `$C800`-`$CFFF` reads the internal ROM;
otherwise the selected slot's `$C8xx` ROM. [TRM]. On the card, `$CFFF`
also traps (§6.2) and the per-slot `$C8xx` images live in the slot-ROM
bank (§12b), one per slot. Every card's firmware uses the standard
protocol (`BIT $CFFF`, then code in its own `$Cn00` page, then `JSR`/
`JMP $C8xx`), so the Gemini must select the image of the slot whose
`$Cn00` page was last accessed. [FW]. Whether a second slot-page access
before `$CFFF` re-selects (last wins) or not (first wins, as with real
cards fighting on the bus) is [OPEN]; the card's firmware never does it.

Reset state: all memory switches off, **SLOTCXROM (slot ROMs)**,
**INTC3ROM (internal C3)**, INTC8ROM clear, LC = read ROM / write
RAM bank 2 (prewrite armed by the standard rules). [TRM]

The enhanced IIe monitor calls internal-ROM code constantly through
the `$FBB4` thunk (`STA $C007 / JMP $C100`), e.g. from INIT/SETWND,
VTAB, SETVID and SETKBD; RESET reaches the internal `$C100`-`$C2FF`
code before anything else. [FW]

### 4.5 65C02 timing

- The 65C02 runs cycle-exact at **1.02 MHz** (normal) or **1.9 MHz**
  (fast, `$C02B` bit 2; RESET copies `$C05C` straight into `$C02B`,
  `$FECE`, so the option-panel speed is `$C05C` bit 2). [TIL][TN][FW].
  (One M.G. table gives bit 4 for `$C05C`; the ROM's copy into `$C02B`
  settles it as bit 2.)
- The host protocol relies on real instruction timing. For example,
  after the self-test's `$FFFC` ack the 65C02 needs ≥ 9 µs (vector
  fetch, `JMP $200B`, `LDA $C07D`) before its next trap, while the 68k
  writes `$0D=3` about 1-2 µs after the ack. The test expects that
  write to be reflected in the next code (`$E1`). [ST]

## 5. Slinky

- One transfer per bus cycle: a word at `$10` moves main (hi) + aux
  (lo) at the same address; a byte moves one lane; one increment
  either way. [ST][LOG]
- Loader formats: `Conf` = `[cpu LE][slinky LE][len LE] data`;
  `Monx` = the same header, XOR "DoubleX". Streamed byte-wise via `$10`
  (or `$11` for aux) after `tst.b $08; move.w addr,$1C`. [ST][FW]

## 6. Traps

### 6.1 Codes (`$01`; bits 7:6 = `$0D` bits 1:0; table = `code & $3F`)

| Code | Trigger | Host handling | Evidence |
|---|---|---|---|
| `$02`/`$0A`, `$22`/`$2A` | text page 1/2 store, main/aux | screen update | [ST] |
| `$04`/`$0C`, `$24`/`$2C` | hires page 1/2 store, main/aux | screen update | [ST] |
| `+$01` | the same while `$20` bit 4 (motor) is set | the driver reads `$04`, marks the cell dirty and never acks (code_5 0x1F5E, 0x2106); the dirty cells are redrawn at motor-off. **The read of `$04/$05` releases the 65C02, and an ack (`$0C`) does not**: the self-test only passes that way (§12 step 7). | [ST][DRV][INF] |
| `$08`/`$09` | slot I/O `$C090`-`$C0FF` write/read (except the IWM) | per-slot handler, compacted address | [ST][DRV] |
| `$20` | `$C0xx` switch write; `$04 & $7F` indexes the handler table; data in `$0C` | | [ST][DRV] |
| `$21` | `$C0xx` switch read; answered via `$19` | | [ST][DRV] |
| `$38` / `$28` | motor on `$C0E9` / off `$C0E8`, read or write | `$20` bit 4 | [ST][DRV] |
| `$00 $01 $06 $07 $0E $0F $26 $27 $2E-$37` | — | driver fatal errors | [DRV] |
| `$10`-`$1F` | — | nop/ack stubs | [DRV] [OPEN] |
| ? | reset vector fetch (`$FFFC`) | ack + `$20=$88` | [ST] [OPEN] code |

### 6.2 Which `$C0xx` switches trap (driver tables at code_5 0x1382/0x17DE)

- Writes: `$C000/$C001`, `$C00C`-`$C00F`, `$C050`-`$C05F`, and **all of
  `$C070`-`$C07F`**. The ones with real handlers: `$C070` PDLTRIG,
  `$C071`, **`$C072`** (the `$5A` handshake → `$0D` bit 4), `$C073`,
  `$C074`-`$C077` (stored per index), **`$C078`-`$C07C`** (register
  mailbox: A, X, Y, P, S), **`$C07D`** (service call number, §7),
  `$C07E`/`$C07F` (ack only: IOUDIS is not implemented [TN]).
  [DRV][ST]
- Reads: the same set; `$C074`-`$C07D` are answered from the stored
  values via `$19`; `$C05C` bit 2 = option-panel speed [MG]. [DRV]
- **Game buttons** `$C061`-`$C063`: reads trap (code `$21`) while `$21`
  bit 1 is set. The host answers from the Mac's modifier keys
  (`$C061` Open-Apple, `$C062` Closed-Apple, `$C063` shift as
  `$FF`/`$7F`; code_5 0x1BB8, 0x1BE0, 0x1BF6). **Restart IIe** uses
  this to cold-start the IIe:
  1. stop the 65C02;
  2. fill the text page with spaces;
  3. set a one-shot flag;
  4. start with `$20 = $80` (code_5 0x47A-0x4CA).

  The `$C061` handler then answers `$FF` once, so the RESET code sees
  Open-Apple down (Control-Open-Apple-Reset) and cold-starts, and the
  handler clears `$21` bit 1. Without the button trap, RESET finds a
  valid power-up byte and warm-starts into BASIC. [DRV][LOG]
- **`$CFFF`** (C8-space release) traps both ways; the driver just acks.
  [DRV][LOG]
- Not trapped (card-internal): keyboard `$C000`/`$C010`, `$C019`
  (from `$0D` bit 6), the memory switches, the LC switches,
  `$C028`/`$C029`/`$C02B`, the IWM `$C0E0`-`$C0EF` except the motor
  pair. [ST][MG]
- Screen holes (`$xx78`-`7F`, `$xxF8`-`FF`) don't trap. [TN]
- Undefined `$C000`-`$C07F` reads return the last key (no floating
  bus). [MG][TN]

### 6.3 Display-trap filtering (`$21`, `$0D` bits 1:0)

The host decides which display stores trap, from the IIe display
mode. The driver keeps a mirror of `$08` (seeded from `$08` at init,
code_5 0x2D4; the `$C0xx` switch handlers set/clear its bits, which
gives the `$08` bit order in §3). On every display-switch change it:

1. builds the effective mode: the mirror with PAGE2 cleared when
   80STORE is set (PAGE2 then selects aux memory, not the displayed
   page) (code_5 0x7AE);
2. looks up a per-mode long at code_5 0xB5C (256 entries, indexed by
   the switch byte): `[00][mode × 2][handler block][$0D tag]`. There
   are 24 distinct modes. The tag (`$0D` bits 1:0, echoed in the trap
   code) is 1 for 40-column text, lo-res and full-screen double
   hi-res; 2 for 80-column text, double lo-res and mixed modes with
   80-column text; 3 for full-screen hi-res and mixed double hi-res;
   0 for mixed hi-res with 40-column text;
3. holds the CPU (`$20` bit 3 clear), rewrites **`$21`**, re-reads
   `$01`, releases the CPU (code_5 0x81A-0x8A0).

`$21` (from the table at code_5 0x8F4, indexed by effective switch
bits 6:1) is a **display-trap enable mask**: the Gemini traps a
display store only if the area's bit is set:

| `$21` bit | Area (enabled when it is on screen) |
|---|---|
| 7 | hi-res page 1 (`$2000`) |
| 6 | hi-res page 2 (`$4000`) |
| 5 | text/lo-res page 1 (`$0400`) |
| 4 | text/lo-res page 2 (`$0800`) |
| 3 | aux text/lo-res (80-column text, double lo-res) |
| 2 | aux hi-res (double hi-res: 80COL + HIRES + DHIRES on) |
| 1:0 | preserved by the driver, meaning open |

Examples: 40-column text page 1 = `$20`; 80-column text = `$28`;
hi-res page 1 = `$80`; mixed hi-res = `$A0`; double hi-res = `$84`;
mixed double hi-res = `$AC`. On a page flip (only PAGE2 changed) the
driver also enables both pages of the visible kind (`|= $C0` or
`|= $30`), so stores to either page keep trapping. While `$0D` bit 2
is set the mask is reduced to bits 3:2. The self-test sets `$21=$FC`
(every area) for its display-store steps and `$21=0` before the slot
test. [ST][DRV]

This is how the Technical Note's rules (aux text traps only in 80
columns, aux hi-res only in double hi-res, nothing that isn't shown)
come about: the host programs them through `$21`. [TN][DRV]

## 7. `$02 nn`: host service calls (the "weird opcode")

On the card, `$02 nn` (a 2-byte NOP on the 65C02) is a **call into the
host**. [MG] observed this on real hardware; [FW] shows the mechanism:

1. The Gemini turns the `$02` fetch into an **NMI**. The NMI is taken
   in the alternate bank: vector `$D000`.
2. The receiver (D1/D2 `$D01C`) stores **A → `$C078`, X → `$C079`,
   Y → `$C07A`, S → `$C07C`, P (from the stack) → `$C07B`**. All are
   trapped writes, so the host now holds the CPU state.
3. It takes the stacked return address − 1 (the operand byte), loads
   `nn` and **writes it to `$C07D`**. The host dispatches `nn` (code_5
   0x1C9A: `nn < 8` → table at 0x1CB4).
4. It reads S, P (back onto the stack), Y, X, A from `$C07C`/`$C07B`/
   `$C07A`/`$C079`/`$C078`. These are trapped reads that the host
   answers via `$19`, so **the host can return any register and flag
   values**. Then `RTI`.

Known calls [MG] (driver handlers in parentheses):

| `nn` | Where | Function |
|---|---|---|
| `00` | self-test | no-op (0x1CD4 `rts`) |
| `01` | `$FBDD` BELL1 | play the Mac system beep (0x1D28) |
| `02` | `$FAB4` PWRUP | replaces `LDA #$C8`: A = `$Cn+1` (startup slot n) or `$C8` (scan) (0x1D66) |
| `03` | `$FAC0` PWRUP | replaces `CMP #$C0`: sets the flags as `CMP #$Cn-1`; shows "UNABLE TO BOOT FROM STARTUP SLOT" (0x1D8A) |
| `04` | `$FB63` APPLEII | draws the copyright message (host-drawn; it disappears when the screen scrolls) (0x1EBC) |
| `05` | ? | unknown, presumably exists (0x1EE8) |
| `06` | `$FD78` GETLN1 | key translation: DELETE → ← in A |
| `07` | ? | |

The register mailbox is essential: the host only sees and returns
the CPU state through the `$C078`-`$C07C` traps. Without them `02 03`
cannot set the flags and the PWRUP slot scan never ends.

## 8. Interrupts

- **NMI**: from `$02` (§7). Vector `$D000` in D1/D2. [FW][MG][ST]
- **IRQ**: host-asserted via `$20` bit 5 [ST]. Vector `$D003` in
  D1/D2 → `BIT $C015 / STA $C007` (save SLOTCXROM, force INTCXROM) →
  the enhanced IIe register push → `TRB $C02B #$80` → `JMP $C410` →
  `BIT $C028` (back to main firmware) → the IIe ROM's IRQ handler.
  The INTCX entry `$C3FA` does the same with `TSB $C02B #$80`, which
  explains "bit 7 set in IRQINTCX". [FW]

## 9. Disks

- **5.25"**: the card's IWM drives real Apple 5.25" drives on the
  Y-cable; slot 6 = Disk II with the `$C800` code from `Monx` 5632.
  The IWM is not trapped except the motor pair; the screen freezes
  while the motor is on. [WP][MG][DRV]
- **UniDisk 3.5** on the Y-cable; other Apple II floppy drives are not
  supported; the platinum Apple 5.25 Drive (A9M0107) is the only
  5.25" drive supported. The internal SuperDrive is also usable.
  [WP][TIL]
- The Y-cable splits disk from joystick (a 19-pin disk connector and a
  9-pin game connector). Several drives share the disk connector by
  daisy-chaining: a UniDisk 3.5 passes through to an Apple 5.25 Drive.
  [INF]

### 9.0 The IWM

- At `$C0E0`-`$C0EF` (slot 6's DEVSEL space). Every switch acts on
  **any access**, read or write. The Disk II firmware only reads,
  indexed with X = `$60`: `LDA $C08C,X` is `$C0EC`. [FW]
  - `$C0E0`-`$C0E7`: the four phase lines. They are shared by both
    drives; the enabled drive steps.
  - `$C0E8`/`$C0E9`: motor; these trap (`$28`/`$38`) as well as
    switching.
  - `$C0EA`/`$C0EB`: drive 1/2.
  - `$C0EC`-`$C0EF`: Q6 and Q7, cleared by the even address and set
    by the odd one.
- **Registers** (as MAME's `iwm.cpp`; [MAME]):
  - A read returns the register that Q7/Q6 select: 00 data,
    01 status, 10 handshake, 11 `$FF`.
  - **Status**: bit 7 = the sense line, bit 5 = enabled, bits 4:0 =
    the mode.
  - **Handshake**: bit 7 = write buffer empty (ready), bit 6 = no
    underrun. It resets to `$BF`.
  - A write with Q6 = Q7 = 1 loads the **mode** register while the
    drive is disabled, and the write data register while it is
    enabled.
  - The self-test's `$1F`/`$00` round trip (shim `$2163`) is a mode
    write read back through the status register. [ST]
- **Mode bits**: 0 latch, 1 asynchronous, 2 immediate motor-off (timer
  disabled), 3 fast (2 µs cells), 4 8 MHz clock.
  - The **motor-off timer** has a fixed length, 2^23 FCLK periods
    (about 1.2 s), and the mode can only enable or disable it. The
    drive stays enabled that long after the motor switch goes off.
  - Loaders rely on it: Lode Runner's switches the motor off and goes
    on reading the disk. [MAME][LOG]
- **Read data**: a byte completes when the shift register's bit 7
  sets; the 10-bit self-sync bytes keep the framing.
  - **Synchronous mode**: the completed byte is visible for only
    about 2 bit cells, then the data register follows the shift
    register (bit 7 clear). The boot ROM depends on it: after `AA` it
    re-reads `$C08C,X` about 12 cycles later (`$C667`-`$C671`) and
    must not see the same byte again.
  - **Asynchronous mode**: the byte is held until read, then cleared.

  [MAME][FW]
- **Write**: in asynchronous mode the IWM takes a byte every 8 bit
  cells.
  - If no new byte has been loaded by then, it underruns: handshake
    bit 6 clears and writing stops.
  - Entering write mode sets bit 6; entering read mode clears the data
    register.
  - The SmartPort packet send writes most bytes **without polling**
    the handshake, exactly 32 CPU cycles apart (slot 5 `$CA25`-`$CAB8`).
    So a slow-mode bit cell must be **4 CPU cycles** (a 32-cycle byte,
    as on the Apple IIc). The same cell gives the Disk II's classic 32
    cycles per nibble. [FW][MAME][INF]
- **Sense line**: for a 5.25" drive it is write-protect, **forced high
  while phase 1 is on** (MAME `floppy.cpp`, "phase 1 can force it to
  1 for drive detection"). For SmartPort it is `/ACK`. [MAME]

### 9.0.1 The UniDisk probe

- Before booting the IIe, the app runs a host-loaded program at
  `$2000`: a copy of the slot 5 SmartPort send (`$C9B3`-`$CAF4`).
  1. Bus reset (PH0+PH2), then enable (PH1+PH3).
  2. Mode `$07` (async, latched, immediate off), drive 1, motor on.
  3. Wait for `/ACK` high: `$C0EE` bit 7, 50 tries. Phase 1 makes it
     high even with only a 5.25" drive.
  4. REQ (PH1 on), then the packet: sync `$FF $3F $CF $F3 $FC $FF`,
     the `$C3` packet-begin mark, the command bytes and the checksum.
  5. Wait for the underrun (`AND #$40`, no time limit).
  6. Wait for `/ACK` low, 10 tries.

  Waits 3 and 6 have limits; the waits for ready (`ASL $C0EC`/`BCC`,
  `LDY $C0EC`/`BPL`) and wait 5 have none, so the IWM's handshake must
  work even with no device on the bus.
- With no UniDisk, the send ends with C = 1, and the program stores
  the device count, 0, in `$01` for the host. [LOG][FW]
- The IIe's reset restarts the IWM in mode 0, so the probe's mode `$07`
  does not carry over into the IIe's own disk boot. [LOG][INF]
- **ProDOS hard disk / Mac 3.5" / AppleShare**: host-side block
  devices behind the slot traps (`$08/$09`), served by the Mac
  drivers (`CNmc` SmartPort etc.). The volumes are partition files,
  real ProDOS partitions, or 800K ProDOS disks in the SuperDrive (with
  the ProDOS File System extension). [WP][DRV]
- Accessing `$C0xD` (x = 8 + slot of the AppleTalk card) crashes the
  card and its driver. [MG]

### 9.1 Virtual slots and host-side devices

- Slot ROM images loaded by IIe Startup (§12a): 1 serial (printer/
  modem), 2 AppleClock, 3 the internal 80-column firmware ("video
  only"), 4 MouseCard, 5 SmartPort, 6 Disk II (via the IWM), 7 memory
  card (Slinky; RamFactor-compatible firmware can replace it). The
  slot assignment is configurable in the control panel. [LOG][MG]
- **Slot 5 is dual**: its SmartPort firmware serves the host's block
  devices through slot 5 traps (the C5 entry's `STA`/`LDA $C0DE`, then
  `$C0DD`, `$C0D1`, `$C0DB`), such as a ProDOS partition on the Mac's
  disk, and a UniDisk 3.5 on the external port through the IWM.
  [FW][LOG]
- The **startup slot** comes from the host's answer to `02 02`: A =
  `$Cn+1` boots slot n (§7). It is read when the IIe starts or
  restarts. [LOG][DRV]
- [MG] lists slot 4's I/O (`$C0C0`-`$C0CF`) as absent on the card,
  although the mouse firmware sits at `$C400` (the mouse is
  host-emulated). [OPEN] how it reaches the host.
- SmartPort: no device names for partitions, and no type/subtype/
  version values. [MG]
- AppleTalk: emulated Workstation Card; shares the Mac's node
  address (the IIe can't see NBP names registered by the Mac). The
  boot blocks (ProDOS 1.9 + Logon 1.3) come from the app's `BBLK` 5120
  resource rather than the network. [MG]
- The aux firmware banks also host slot-card extensions (e.g. a
  RamFactor's partition manager and diagnostics). The card has no
  `$C8xx` bank switching for slot firmware. [MG]
- Accessing `$C029` (IIgs NEWVIDEO, used by Bitsy Bye in ProDOS
  2.4.0/2.4.1 and Apple II Desktop 1.1) switches in the alternate
  firmware bank, and the next monitor call crashes. [MG]

### 9.2 Behaviour visible to software

- **No floating bus anywhere** and no Apple II video circuitry:
  unused I/O reads return the last key pressed, and "vapor lock" video
  synchronisation fails. [MG]
- Double lo-res colours are wrong in every other column (the host
  renderer's palette). [MG]
- Software that leaves the Disk II motor on shows no display updates
  while it runs (§3.2 bit 4). [MG]
- Control-Open-Apple-Reset reboots the IIe and queues a reboot of the
  Mac on quit; Restart IIe from the option panel avoids it [MG]. Restart
  IIe works by answering the Open-Apple button read once (§6.2). [DRV]

## 10. Display (Mac side)

- The **Mac renders** the Apple II screen in software with QuickDraw,
  driven by the display-store traps and the `$08` soft-switch byte (and
  the trapped `$C050`-`$C05F`/`$C00C`-`$C00F` switches). Full-screen
  only. The card has no video output, so there is no overlay and no
  RAMDAC key. [WP][DRV]
- On LC/LC II hosts the app enters the **V8 Apple II mode** itself
  (CODE 1 0x4202, machine types 19 and 37):
  - `bset #0, $50F26001`: pseudo-VIA register `$01` (the V8
    expansion register) bit 0 = the **Apple II 560×384 mode**.
  - then **`V8MonP` (`$50F26010`) bits 2:0 += 1**; leaving the mode
    clears bit 0 and subtracts 1.
  - On other hosts (LC III/Sonora, Color Classic…) it issues a private
    video-driver `_Control`, **csCode `$81`**, instead. [DRV]
- Depth: since the app adds one to the V8 depth field on entry, the
  real pixel depth in A2 mode is the field minus one, which should
  equal the depth in the Ariel RAMDAC's control register (bits 2:0).
  [DRV] [INF]: to confirm in the LC ROM's video driver.
- Monitors: Apple lists all LC monitors (12" and 13") as supported
  [TIL]. Whether the 13" uses the 560×384 mode (centred) or another
  layout is [OPEN].
- Timing: the card's 17.2349 MHz clock replaces the Mac dot clock
  (15.6672 MHz); 704×407 total (560+144 × 384+23) = 60.15 Hz, the same
  scan rate as the 512×384 mode (640×407 at 15.6672 MHz), about 10%
  denser horizontally. [WP][DN]
- VRAM: the normal framebuffer at `$50F40000`, fixed row bytes 512
  (256K VRAM) or 1024 (512K); 560 px at 8 bpp needs 1024. [DN][INF]
- The Color Classic's 512×384 built-in display has a switchable
  560×384 mode, mostly for this card. [WP]
- **LC III (Sonora)**: the developer note lists 560×384 on the 12" RGB
  monitor at 17.2340 MHz, with depth "16" (possibly a typo for 4). The
  Omega clock chip generates the Apple II dot clock, so the LC's
  AIICLOCK pin (B31) became 16MASTER. The app selects the mode with
  the video driver's private `_Control` `$81`. The Sonora register
  that the driver programs for it is [OPEN]. [DN][DRV]
- MAME (current tree): V8/Sonora/Spice render 1-8 bpp at the
  monitor-ID resolution only; **no A2 mode, no IIe card**; the Ariel
  key colour is "never used by Mac OS" (it needs a second slave Ariel).
  There is no other reference implementation of the card.

## 11. Boot protocol (normal path, driver code_5 0x450 / self-test)

1. Stop (`$20/$21 = $0802`), ack, delay; `bsr` to the loaders
   (Slinky, `$0D` bit 3 per target half): RAM, firmware (`Monx`), slot
   ROMs (`CNxx`/`C8xx`), vectors.
2. Text page fill: `$1C=$0400`, word writes `$A0A0` skipping the screen
   holes (main + aux spaces). [DRV]
3. `$20=$80` (run): ack traps until the trap address is `$FFFC`, then
   `$20=$88`, ack → the 65C02 boots at `$FA62` (enhanced IIe RESET).
   Timeout → error 42. [DRV]
4. Then the steady state: the VBL task (code_1 0x48C2, per-channel
   handlers, `$0D` bit 6 VBL) plus the trap dispatcher (code_5
   0x16D8).

## 12. The self-test (`Conf` 1000), step by step

The error number in parentheses is what each step returns on mismatch.

1. `$0D=0`, `$20=$0802`, read `$04`, ack; RAM-test the upper then
   the lower half.
2. Load the shim: main `$2000` via `$10`, aux `$2000` via `$11`. Upper:
   NMI handler (`LDA #2 / STA $C07D / RTI`) at D1 `$D000` (fw `$5000`);
   vectors at D1 `$FFFA` (fw `$7FFA`) and main `$FFFA` (fw `$3FFA`):
   NMI `$D000`, RESET `$2000`, IRQ `$2003`.
3. `$21=$FC`, `$20=$80`: ack until address `$FFFC`; `$20=$88`, ack.
4. `$0D=3`: `LDA $C07D` → `$E1`, answer `$19=0`; `STA $C07D` → `$E0`.
   (7)
5. `$0D=0`: 8 display stores → `$0A $2A $0C $2C $02 $22 $04 $24`. (3)
6. `STA/LDA $C0E9` → `$38 $38`. (5)
7. `$20=$98`: 8 stores → `$0B $2B $0D $2D $03 $23 $05 $25`; `$20=$88`.
   (6) The 65C02 program puts a `STA $C07D` after each store. Per
   store, the 68k:
   1. acks;
   2. waits for the code;
   3. reads `$04`;
   4. waits again, unchecked, which catches the `$C07D` trap.

   The next ack releases that `$C07D` trap. This only lines up if the
   `$04` read releases the odd display trap and the ack does not
   (§6.1). [ST][LOG]
8. `STA/LDA $C0E8` → `$28 $28`. (4)
9. `$21=0`: `STA/LDA $C0A0` → `$08 $09`. (8)
10. Keyboard: `$18=$A0`, `$09` bit 7 set; `$0D` bit 7=1: `LDA $C000`
    → `$A0`, `LDA $C010` → bit 7 set; `$0D` bit 7=0, `$09` bit 7
    clear; `LDA $C000` → `$20`; `LDA $C010` → bit 7 clear. (9)
11. VBL: `$0D` bit 6 = 1/0 → `$C019` bit 7 = 1/0. (10)
12. `02 00` → NMI → mailbox `2` (11); `$20` bit 5 → the IRQ handler
    (12) → the `STA $C07D` after the IRQ wait loop.
13. `$0D` bit 2 aux disable: the last mailbox byte is 0. (13)
14. `$0D` bit 4 ROM write enable: `$E000` written through, then
    ignored. (14)
15. 16 switch writes: `$08` = `$FF`, then `$00`. (15)
16. The final `STX $C07D` (X = LC/aux, `$C028` bank and IWM results).
17. `$20=$0802`.

Note: the self-test runs with test-only vectors (RESET=`$2000`) in
**both** main and D1. It cannot tell which bank is live at reset;
only the firmware layout (§4.2) can.

## 12a. Firmware facts used by the boot (from the decrypted main ROM)

- Main ROM vectors (A2EMonitorROM `$FFFA`): NMI `$03FB`, RESET `$FA62`,
  IRQ `$C3FA`, i.e. the standard enhanced IIe ones. The D1/D2 vectors
  (NMI `$D000`, IRQ `$D003`) are the ones the Gemini must use for
  interrupts (§8). [FW]
- RESET `$FA62`: `CLD`, `JSR $FE84` (SETNORM), `JSR $FB2F` (INIT: reads
  `$C056`/`$C054`/`$C051`), `JSR $FE93` (SETVID), `JSR $FE89`
  (SETKBD), `LDA $C058/$C05A`, `JSR $FECE` (`LDA $C05C / STA $C02B /
  JMP $FBB4`: the option-panel speed becomes the CPU speed), `BIT
  $CFFF`, `BIT $C010`, `JSR $FF3A`, the warm/cold check on
  `$03F2`-`$03F4`, then PWRUP `$FAA6`: `JSR $FB60` (HOME + `02 04`
  copyright), the slot scan at `$FAB4` with `02 02`/`02 03`. [FW]
- **`$FBB4` is the enhanced IIe's internal-ROM thunk**: `BIT $C015 /
  PHP / STA $C007 / JMP $C100` (Y = function index; the `$C100`
  dispatcher is at `$C213`). The exit at `$C20A` is `PLP / BMI +3 /
  JMP $FEC5` (`STA $C006 / RTS`). So **`$C015` bit 7 = 1 means
  INTCXROM (internal) is on**, and after `STA $C007` the `$C100`-`$CFFF`
  range must read the internal ROM. SETWND, VTAB, SETVID/SETKBD and
  others all go through it. [FW]
- The per-slot ROM images the app loads into the slot-ROM bank (fw
  `$C100`-`$C7FF`): slot 1 = serial (Printer/Modem `CNxx`), 2 =
  AppleClock, 3 = (internal 80-column), 4 = MouseCard, 5 = SmartPort
  (`$Cn01/03/05/07` = `20 00 03 00`, IWM-driven 3.5" via the
  `$C0Ex` IWM), 6 = 5.25" Disk II (`$Cn07` = `$3C`), 7 = a second
  SmartPort block device (`20 00 03 00`, the host hard disk path).
  The per-slot `$C8xx` images go to fw `$C000 + n×$800` (§12b).
  [LOG][FW]
- The slot pages are served at `$C100`-`$C7FF` when INTCXROM is off
  (`$C300` per SLOTC3ROM); with INTCXROM on, the internal (main-ROM)
  pages show. The main ROM's C8-CF pages (16 KB image, offsets
  `$0800`-`$0FFF`) are the internal C8 (80-column firmware), shown
  only with INTCXROM or INTC8ROM. [FW][LOG]
- The card patches the enhanced monitor's WAIT sites with `$02`
  driver calls: `$FBDD` = `02 01 60` (stock `A9 40 20`) and `$CB21` =
  `02 01 60 EA EA` (stock `A9 40 20 34 CB`).  fn 01 = the WAIT
  service. [FW]

## 12b. Slot `$C800` expansion ROMs

- The app loads, with `$0D` bit 3 set and through the main lane
  (`$10`), each slot's 256-byte page to fw `$Cn00` and each slot's
  2 KB `$C800`-`$CFFF` image to fw **`$C000 + n×$800`** (slot 1 `$C800`,
  2 `$D000`, 3 `$D800`, 4 `$E000`, 5 `$E800`, 6 `$F000`, 7 `$F800`),
  i.e. card `0x2C000 + n×0x800`. The 5.25" image's `Monx` header
  (`$C800` → fw `$F000`) agrees with slot 6. [LOG][FW]
- Every slot page follows the standard protocol before using its
  `$C8xx` code: C1 `STA/BIT $CFFF` then `JSR $C805`/`JMP $CF00`/…; C2
  `BIT $CFFF / JSR $C800`; C4 `LDA $CFFF / JMP $C800`; C5 `LDA $CFFF`
  at `$C508` and `$C51C`, then `JSR $CE4D`, `JSR $CE0D`, `JMP $C800`,
  `JMP $C870`/`$C956`; C6 `BIT $CFFF / JSR $C800`; C7 `BIT $CFFF /
  JSR $C9CC` and `JSR $C800`. [FW]
- The images match their slot pages: the slot 5 image at `$C800`
  reads the SmartPort command list through `($54),Y` (set up by the C5
  page), and its `$CE0D` drives the `$C0E8`-`$C0EF` IWM; the slot 7
  image's `$C9CC` is `STX $0778 / LDX $C05C / STX $C02B / …`, the same
  sequence as the C7 page's caller. [FW]
- Hence the selection rule (§4.4): `$CFFF` releases the C8 space, an
  access to `$Cn00`-`$CnFF` with INTCXROM off (for slot 3: SLOTC3ROM on)
  selects slot n's image, and an access to `$C3xx` with SLOTC3ROM off
  sets INTC8ROM (internal C8). If the internal C8 is served instead,
  the slot 5 boot code runs the 80-column firmware's `$CE0D`/`$CE4D`;
  the IIe then stops in a keyboard wait, and a keypress returns through
  a 4-byte exit over the slot code's 30-byte zero-page save (`$40`-`$5D`
  pushed at `$C57B`-`$C581`), so `RTS` pops saved zero-page bytes and
  lands in the Monitor through a BRK. [FW][LOG]
- With nothing selected and INTC8ROM off, `$C800`-`$CFFE` has no
  owner (a stock IIe reads the floating bus). What the Gemini returns
  is [OPEN].

## 13. Open questions

- `$20` bit 3 (inferred: CPU hold, §3.2); `$21` bit 0; `$0D` bit 2's
  exact meaning (aux disable vs protect, and why the driver masks
  `$21` to bits 3:2 with it).
- How the Gemini releases a motor-on (odd) display trap. The read of
  `$04/$05` releasing it is inferred from the driver and the
  self-test (§6.1).
- The IWM's FCLK and its ratio to the 65C02 clock. 4 CPU cycles per
  slow-mode bit cell (7 FCLK per CPU cycle, as on the IIc) is
  inferred from the SmartPort send's unpolled writes (§9.0).
- Whether the card gates its registers on /FC3, which would explain
  why it fails in 32-bit mode, and what makes Sonora drive /FC3 low
  in 24-bit mode (§2).
- What `'sltc'` is, and which machines take the `$50FC0000` base
  (§2).
- The code of the `$FFFC` trap; the bank-switch rules around
  interrupts (entry trigger, exit); what a `$C028` write with bit 7
  selects (shim `$2143`).
- Trap codes `$10`-`$1F`; the `02 05`/`02 07` calls.
- The LC RAM physical placement; `$C02B` bits 4/5/6.
- The video switches' reset state. The driver seeds its switch mirror
  from `$08` at startup but leaves its displayed-mode state at 0 (GR)
  until a switch trap changes the mirror, so `$08` must read `$00`
  (TEXT off) before the 65C02 runs, or the RESET code's `$C051` is
  never seen as a change and full-screen redraws come out as lores.
  [DRV][INF]
- How a trapped read inside a read-modify-write (the 65C02 INC/DEC
  reads the operand twice) is resumed: the slot 7 memory-card probe
  does `DEC $C0F0`. Whatever the mechanism, both reads must return the
  host's answer. [FW][INF]
- The `$C800` slot selection details (§12b): last-wins vs first-wins,
  and what an unowned `$C800`-`$CFFE` reads.
- The depth relationship in A2 mode and the 13" monitor layout (the
  LC ROM's video driver).
- How the host reaches the emulated slot 4 mouse (no `$C0Cx` I/O).

## 14. Speculation: what the Gemini may inherit from the Mega II

**Everything in this section is speculation.** The Gemini is described
as a heavily modified Mega II [WP]; nothing below has been observed on
the card itself. Facts about the Mega II come from the Apple IIGS
(MAME's IIGS driver and secondary sources) and may not carry over.

- **Language-card RAM placement.** MAME's IIGS model of the Mega II's
  128K places, in each of main and aux: LC **bank 2 `$D000` at
  physical `$C000`-`$CFFF`**, **bank 1 `$D000` at `$D000`-`$DFFF`**,
  and `$E000`-`$FFFF` at `$E000`-`$FFFF`, selected by ALTZP; reset
  state bank 2. If the Gemini does the same, a Slinky read of main
  `$C000` returns LC bank 2. (Speculation; the card's software never
  reads LC RAM through the Slinky in the traces seen so far.)
- **1 MHz timing.** In the IIGS, the Mega II's 1 MHz side runs 64
  cycles of 14 × 14.318 MHz clocks and stretches every 65th to 16, the
  IIe "long cycle": 14.31818 MHz × 65 / 912 = **1.0205 MHz** on
  average. That matches Apple's "1.02 MHz" [TIL], so the Gemini may
  clock the 65C02 the same way. The card's clock source isn't known.
  (Speculation.)
- **Keyboard, mouse, character/video ROMs.** The Mega II has a keyboard
  encoder, a mouse controller and video/character ROMs [WP]; the
  Gemini omits video generation [WP]. The card firmware never touches
  the IIGS Mega II registers (`$C041` INTEN, `$C044`/`$C045` mouse
  deltas, `$C046` INTFLAG, `$C047`/`$C048` interrupt clears, `$C068`
  STATEREG, `$C035`/`$C036`): a scan of the decrypted firmware and slot
  ROMs finds only the IIe set plus `$C028`/`$C029`/`$C02B`/
  `$C072`-`$C07D` (a fact about the firmware [FW]). Whether the Gemini
  still decodes them is unknown; software has no reason to use them.
  (Speculation.)
- **Slots.** The IIGS Mega II has no IOSEL/DEVSEL slot selects (the
  IIGS's slots come from a separate ASIC). That fits the card, where
  all slot I/O except the IWM traps to the host (§6). (Speculation
  about the lineage; the trapping itself is a fact, §6.)
- **What is new in the Gemini.** None of the following exists in the
  IIGS Mega II, so they are presumably Gemini additions: the PDS host
  register block and the trap engine (§3, §6), the Slinky (§5), the
  `$02` opcode → NMI (§7), the main/D1/D2 firmware banking with
  interrupt redirection (§4.2), and the host-driven VBL/AKD inputs
  (§3.1). (Speculation.)
- The Mega II also served as support circuitry on Apple's Apple II
  Video Overlay Card [WP]. Nothing ties that card's overlay to the IIe
  Card, which has no video output.

## Sources

- M.G.'s Apple //e Card notes: <http://apple2.guidero.us/doku.php/mg_notes/iie_card/start>
  (pages: firmware, iie_startup_resources, io_addresses, io_page,
  mac_side_interface, weird_stuff, things_go_boom, ctrl_oa_reset,
  appletalk, problem_software, smartport) and projects (NetBoot LC,
  RamFactor LC, ProDOS 2.4.1 for the card).
- Apple Tech Info Library 6571, *Apple IIe Card: Hardware
  Specifications* (rev. 7/12/93), and the *Apple IIe Card* data sheet
  (Apple, March 1991).
- Wikipedia, Apple IIe Card: <https://en.wikipedia.org/wiki/Apple_IIe_Card>
- Wikipedia, Mega II: <https://en.wikipedia.org/wiki/Mega_II>
- MAME `src/mame/apple/apple2gs.cpp` (the Mega II side of the IIGS:
  language-card placement, 1 MHz long-cycle timing).
- baldengineer, *Mega IIe* (a single-chip IIe on a Mega
  II): <https://github.com/baldengineer/mega-iie>
- Low End Mac, *Apple IIe Card: A Tool for Getting Macs into Schools*:
  <https://lowendmac.com/2018/apple-iie-card-a-tool-for-getting-macs-into-schools/>
- Apple II Technical Notes index (IIe #10, Misc #7):
  <https://downloads.reactivemicro.com/Apple%20II%20Items/Documentation/Manuals/Apple%20II%20Technical%20Notes/Apple%20II%20Technical%20Notes%20Index.htm>
- [TRM] *Apple IIe Technical Reference Manual* (Apple, enhanced IIe
  edition): the soft-switch and INTC8ROM rules in §4.4.
- [DN] Apple, *Macintosh LC Computer Developer Note* (the 68020 Direct
  Slot: pinout, pseudo-slot addressing), *Macintosh LC II* and
  *Macintosh LC III Developer Notes* (FC3, /PDS.AS, the 24/32-bit
  maps, video modes including 560×384).
- MAME `src/mame/apple/v8.cpp`, `sonora.cpp`,
  `src/devices/video/ariel.cpp`; `src/devices/machine/iwm.cpp` (IWM
  registers, write shifter, motor-off timer); `src/devices/imagedev/
  floppy.cpp` (the 5.25" sense line and phase 1); `src/devices/bus/
  nubus/nubus.cpp` and `8lc.cpp` (the LC PDS address mapping);
  `src/lib/formats/ap2_dsk.cpp` (the 16-sector track layout).
- [ROM] The Macintosh LC III ROM (1993, checksum `ECBBC41C`):
  `SwapMMUMode` variants, the base-address table, MMU templates, and
  the MMU tables built at boot.
