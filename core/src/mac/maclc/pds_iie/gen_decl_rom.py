#!/usr/bin/env python3
"""Generate the synthesized Apple IIe Card declaration ROM (512 bytes).

All structures follow the standard Macintosh declaration ROM format
(Designing Cards and Drivers for the Macintosh Family): big-endian in
ascending address order.  The ROM occupies the top of PDS slot $E space
on byte lanes 1+3, so one ROM byte = 2 address bytes, and logical byte N
(0 = the top byte at $FEFFFFFF) sits at address 0xFFFFFF - 2N; the
serving code in maclc/bus.rs uses that mapping.  This generator builds
the image with logical 0 = the top.

Format block (the top 20 logical bytes; in ascending address order:
DirOffset, Length, CRC, RevLevel, Format, TestPattern, Reserved,
ByteLanes — so the LAST field, ByteLanes, is the top byte):

    logical 0:  ByteLanes = 0x5A (lanes 1+3, nibble-complement)
    logical 1:  Reserved = 0x00
    logical 2-5: TestPattern = 5A 93 2B C7
    logical 6:  Format = 0x01
    logical 7:  RevLevel = 0x01
    logical 8-11: CRC (skipped by the checksum)
    logical 12-15: Length = 512
    logical 16-19: DirOffset = 0x00FFFF00 (VerifyFormat's rol#8/ext.w
        check forces this shape; the directory ends up 256 ROM bytes
        below the dir-offset field = logical 275)

Checksum (LC ROM VerifyCRC): rol-1-then-add over the bytes in ascending
address order (logical 511 -> 0), skipping the CRC field (logical 8-11).

Directory and sResource entries are longwords [id:8 | offset:24], the
id in the high byte at the entry's own (lowest) address, ids ascending,
0xFF ending the list.  The offset is a signed 24-bit count of ROM bytes
from the entry's own address to the field's data.

Directory (logical 275):
    [0x01 | board sRsrc] [0x80 | IIe sRsrc] [0xFF | 0]

Board sRsrc (category 1 / type 0 — the Slot Manager's own sResource):
    [0x01 sRsrcType  -> 8-byte sType 0001 0000 0000 0000]
    [0x02 sRsrcName  -> C string]
    [0x20 BoardId    = value in the offset field (ReadSlotWord)]
    [0x21 PRAMInitData -> long length + 6 bytes (the sType's
        category/type/drvrSW — read after spSize -= 4)]
    [0xFF | 0]

IIe sRsrc (what the IIe Startup INIT searches: category 10, type 21,
drvrSW 1, drvrHW masked):
    [0x01 sRsrcType -> 8-byte sType 000A 0015 0001 0000]
    [0x02 sRsrcName -> C string]
    [0xFF | 0]
"""

import os
import sys

# Output directory: the first argument, or the current directory.
OUT = sys.argv[1] if len(sys.argv) > 1 else "."

ROM_LEN = 512
rom = bytearray(ROM_LEN)


def put_long(logical_top, value):
    """Big-endian longword, MSB at `logical_top` (the lowest address)."""
    rom[logical_top] = (value >> 24) & 0xFF
    rom[logical_top - 1] = (value >> 16) & 0xFF
    rom[logical_top - 2] = (value >> 8) & 0xFF
    rom[logical_top - 3] = value & 0xFF


def put_entry(logical_top, id_byte, target):
    """A [id:8 | offset:24] longword: the id at the entry's address,
    the signed 24-bit offset = entry - target in ROM bytes (positive
    when the target is below the entry, i.e. at a higher address)."""
    off = (logical_top - target) & 0xFFFFFF
    put_long(logical_top, (id_byte << 24) | off)


def put_bytes(logical_top, data):
    """Raw bytes, data[0] at `logical_top` (ascending address order)."""
    for k, b in enumerate(data):
        rom[logical_top - k] = b


# ---- Format block (the top 20 logical bytes) ---------------------------
rom[0] = 0x5A      # ByteLanes
rom[1] = 0x00      # Reserved
rom[2] = 0xC7      # TestPattern 5A 93 2B C7 (MSB at logical 5)
rom[3] = 0x2B
rom[4] = 0x93
rom[5] = 0x5A
rom[6] = 0x01      # Format
rom[7] = 0x01      # RevLevel
# rom[8..12] = CRC (filled below)
rom[12] = 0x00     # Length = 512
rom[13] = 0x02
rom[14] = 0x00
rom[15] = 0x00
rom[16] = 0x00     # DirOffset = 0x00FFFF00
rom[17] = 0xFF
rom[18] = 0xFF
rom[19] = 0x00

# ---- Directory (logical 275, entries at 275/271/267) -------------------
put_entry(275, 0x01, 263)   # board sRsrc at 263
put_entry(271, 0x80, 219)   # IIe sRsrc at 219
put_entry(267, 0xFF, 267)   # end of list

# ---- Board sRsrc (entries at 263/259/255/251/247) ----------------------
put_entry(263, 0x01, 235)   # sType data at 235
put_entry(259, 0x02, 224)   # name at 224
put_long(255, 0x20000001)   # BoardId = 0x000001 in the offset field
put_entry(251, 0x21, 190)   # PRAMInitData at 190
put_entry(247, 0xFF, 247)   # end of list

# Board sType data: category 1, type 0, drvrSW 0, drvrHW 0 (8 bytes)
put_bytes(235, [0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00])
# Board name (C string)
put_bytes(224, b"Board:Apple IIe Card\x00")
# Board PRAMInitData: the length long counts itself (4 + 6 data
# bytes); the 6 data bytes are the card's initial PRAM contents (the
# slot's PRAM bytes 2-7), zeros for now.
put_bytes(190, [0x00, 0x00, 0x00, 0x0A, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00])

# ---- IIe sRsrc (entries at 219/215/211) --------------------------------
put_entry(219, 0x01, 199)   # sType data at 199
put_entry(215, 0x02, 180)   # name at 180
put_entry(211, 0xFF, 211)   # end of list

# IIe sType data: category 10, type 21, drvrSW 1, drvrHW 0 (8 bytes)
put_bytes(199, [0x00, 0x0A, 0x00, 0x15, 0x00, 0x01, 0x00, 0x00])
# IIe name (C string)
put_bytes(180, b"Apple IIe Card\x00")

# ---- Checksum ----------------------------------------------------------
def rol32(x, n):
    x &= 0xFFFFFFFF
    return ((x << n) | (x >> (32 - n))) & 0xFFFFFFFF


def checksum(skip_set):
    s = 0
    for i in range(ROM_LEN - 1, -1, -1):
        s = rol32(s, 1)
        if i not in skip_set:
            s = (s + rom[i]) & 0xFFFFFFFF
    return s


crc = checksum({8, 9, 10, 11})
rom[11] = (crc >> 24) & 0xFF
rom[10] = (crc >> 16) & 0xFF
rom[9] = (crc >> 8) & 0xFF
rom[8] = crc & 0xFF
assert checksum({8, 9, 10, 11}) == crc

open(os.path.join(OUT, "decl_rom.bin"), "wb").write(bytes(rom))
print("wrote decl_rom.bin, CRC = %08X" % crc)

# Emit Rust
rs = "/// Synthesized Apple IIe Card declaration ROM (see gen_decl_rom.py).\n"
rs += "pub const DECL_ROM: [u8; 512] = [\n"
for i in range(0, ROM_LEN, 16):
    rs += "    " + ", ".join(f"0x{b:02X}" for b in rom[i:i+16]) + ",\n"
rs += "];\n"
open(os.path.join(OUT, "decl_rom.rs"), "w").write(rs)
print("wrote decl_rom.rs")
