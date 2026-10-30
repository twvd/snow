#!/usr/bin/env python3
"""Makes ProDOS disk images with known contents, to test the Apple IIe Card's
disk paths, and checks them afterwards.

    make_test_disks.py make DIR     writes the images into DIR
    make_test_disks.py check IMAGE  lists the files of an image and checks
                                    the known ones

Images made (volume name in brackets):
    iie140.po, iie140.do, iie140.2mg    5.25" 140K       [IIE140]
    iie800.po, iie800.2mg               3.5" 800K        [IIE800]
    iie1440.po, iie1440.2mg             3.5" 1.4 MB      [IIE1440]
    iie800-locked.2mg                   800K, write-protected

Each volume holds:
    README     TXT  a line of text
    SMALL      BIN  300 bytes (a seedling file), load address $2000
    PATTERN    BIN  6000 bytes (a sapling file), load address $4000

File contents are pattern(name, i) for byte i, so a copy made on the IIe
(e.g. BLOAD then BSAVE under another name) can be checked with "check".

The disks have no boot blocks: they are data disks. The format follows
"Beneath Apple ProDOS" and the ProDOS 8 Technical Reference Manual,
appendix B.
"""

import struct
import sys
from pathlib import Path

BLOCK = 512
VOLDIR_KEY = 2
VOLDIR_BLOCKS = 4
BITMAP_START = 6
ENTRY_LENGTH = 0x27
ENTRIES_PER_BLOCK = 0x0D

TYPE_TXT = 0x04
TYPE_BIN = 0x06

# ProDOS-order sector -> DOS 3.3-order sector, within a track
PRODOS_TO_DOS = [0x0, 0xE, 0xD, 0xC, 0xB, 0xA, 0x9, 0x8,
                 0x7, 0x6, 0x5, 0x4, 0x3, 0x2, 0x1, 0xF]

# 1 November 2026, 12:00, in ProDOS date/time format
DATE = 26 << 9 | 11 << 5 | 1
TIME = 12 << 8 | 0


def pattern(name, length):
    """The known contents of a file."""
    seed = sum(name.encode())
    return bytes((seed + i * 7 + (i >> 8)) & 0xFF for i in range(length))


FILES = [
    ("README", TYPE_TXT, 0x0000,
     b"SNOW APPLE IIE CARD TEST DISK\r"),
    ("SMALL", TYPE_BIN, 0x2000, pattern("SMALL", 300)),
    ("PATTERN", TYPE_BIN, 0x4000, pattern("PATTERN", 6000)),
]


class Volume:
    def __init__(self, name, blocks):
        self.name = name
        self.blocks = blocks
        self.data = bytearray(blocks * BLOCK)
        bitmap_blocks = (blocks + 4095) // 4096
        self.free = [True] * blocks
        for b in range(BITMAP_START + bitmap_blocks):
            self.free[b] = False
        self.bitmap_blocks = bitmap_blocks
        self.entries = []

    def alloc(self):
        b = self.free.index(True)
        self.free[b] = False
        return b

    def put(self, block, data, offset=0):
        start = block * BLOCK + offset
        self.data[start:start + len(data)] = data

    def add_file(self, name, ftype, aux, contents):
        nblocks = max(1, (len(contents) + BLOCK - 1) // BLOCK)
        if nblocks == 1:
            storage = 1  # seedling
            key = self.alloc()
            self.put(key, contents)
            used = 1
        elif nblocks <= 256:
            storage = 2  # sapling: an index block of block pointers
            key = self.alloc()
            index = bytearray(BLOCK)
            for i in range(nblocks):
                b = self.alloc()
                self.put(b, contents[i * BLOCK:(i + 1) * BLOCK])
                index[i] = b & 0xFF
                index[256 + i] = b >> 8
            self.put(key, index)
            used = nblocks + 1
        else:
            raise ValueError("tree files are not needed here")
        self.entries.append((name, storage, ftype, key, used, len(contents), aux))

    def finish(self):
        # The volume directory: four linked blocks
        for i in range(VOLDIR_BLOCKS):
            b = VOLDIR_KEY + i
            prev = b - 1 if i > 0 else 0
            nxt = b + 1 if i < VOLDIR_BLOCKS - 1 else 0
            self.put(b, struct.pack("<HH", prev, nxt))

        header = bytearray(ENTRY_LENGTH)
        header[0] = 0xF0 | len(self.name)
        header[1:1 + len(self.name)] = self.name.encode()
        header[0x18:0x1C] = struct.pack("<HH", DATE, TIME)
        header[0x1C] = 0  # version
        header[0x1D] = 0  # min_version
        header[0x1E] = 0xC3  # access: destroy, rename, write, read
        header[0x1F] = ENTRY_LENGTH
        header[0x20] = ENTRIES_PER_BLOCK
        header[0x21:0x27] = struct.pack("<HHH", len(self.entries),
                                        BITMAP_START, self.blocks)
        self.put(VOLDIR_KEY, header, 4)

        for n, (name, storage, ftype, key, used, eof, aux) in enumerate(self.entries):
            slot = n + 1  # after the header
            block = VOLDIR_KEY + slot // ENTRIES_PER_BLOCK
            offset = 4 + (slot % ENTRIES_PER_BLOCK) * ENTRY_LENGTH
            e = bytearray(ENTRY_LENGTH)
            e[0] = storage << 4 | len(name)
            e[1:1 + len(name)] = name.encode()
            e[0x10] = ftype
            e[0x11:0x13] = struct.pack("<H", key)
            e[0x13:0x15] = struct.pack("<H", used)
            e[0x15:0x18] = struct.pack("<I", eof)[:3]
            e[0x18:0x1C] = struct.pack("<HH", DATE, TIME)
            e[0x1E] = 0xE3  # access: destroy, rename, backup, write, read
            e[0x1F:0x21] = struct.pack("<H", aux)
            e[0x21:0x25] = struct.pack("<HH", DATE, TIME)
            e[0x25:0x27] = struct.pack("<H", VOLDIR_KEY)
            self.put(block, e, offset)

        # The bitmap: one bit per block, 1 = free, MSB first
        bitmap = bytearray(self.bitmap_blocks * BLOCK)
        for b, free in enumerate(self.free):
            if free:
                bitmap[b >> 3] |= 0x80 >> (b & 7)
        self.put(BITMAP_START, bitmap)
        return bytes(self.data)


def make_volume(name, blocks):
    vol = Volume(name, blocks)
    for fname, ftype, aux, contents in FILES:
        vol.add_file(fname, ftype, aux, contents)
    return vol.finish()


def prodos_to_dos(po):
    """Reorders a 140K ProDOS-order image into DOS 3.3 order."""
    do = bytearray(len(po))
    for track in range(35):
        for sector in range(16):
            src = (track * 16 + sector) * 256
            dst = (track * 16 + PRODOS_TO_DOS[sector]) * 256
            do[dst:dst + 256] = po[src:src + 256]
    return bytes(do)


def twoimg(po, locked=False):
    """Wraps a ProDOS-order image in a 2IMG header."""
    header = bytearray(64)
    header[0:4] = b"2IMG"
    header[4:8] = b"SNOW"
    header[8:16] = struct.pack("<HHI", 64, 1, 1)  # size, version, ProDOS order
    header[16:32] = struct.pack("<IIII", 1 << 31 if locked else 0,
                                len(po) // BLOCK, 64, len(po))
    return bytes(header) + po


def make(outdir):
    out = Path(outdir)
    out.mkdir(parents=True, exist_ok=True)
    images = {}
    p140 = make_volume("IIE140", 280)
    images["iie140.po"] = p140
    images["iie140.do"] = prodos_to_dos(p140)
    images["iie140.2mg"] = twoimg(p140)
    p800 = make_volume("IIE800", 1600)
    images["iie800.po"] = p800
    images["iie800.2mg"] = twoimg(p800)
    images["iie800-locked.2mg"] = twoimg(p800, locked=True)
    p1440 = make_volume("IIE1440", 2880)
    images["iie1440.po"] = p1440
    images["iie1440.2mg"] = twoimg(p1440)
    for name, data in images.items():
        (out / name).write_bytes(data)
        print(f"{out / name}: {len(data)} bytes")


def load_blocks(path):
    """Reads an image as ProDOS-order blocks."""
    data = Path(path).read_bytes()
    if data[:4] == b"2IMG":
        fmt, = struct.unpack_from("<I", data, 0x0C)
        offset, length = struct.unpack_from("<II", data, 0x18)
        data = data[offset:offset + length]
        if fmt == 0:
            path = "x.do"
    if len(data) == 143360 and str(path).lower().endswith((".do", ".dsk")):
        po = bytearray(len(data))
        for track in range(35):
            for sector in range(16):
                src = (track * 16 + PRODOS_TO_DOS[sector]) * 256
                dst = (track * 16 + sector) * 256
                po[dst:dst + 256] = data[src:src + 256]
        data = bytes(po)
    return data


def read_file(data, storage, key, eof):
    def block(b):
        return data[b * BLOCK:(b + 1) * BLOCK]
    if storage == 1:
        return block(key)[:eof]
    if storage == 2:
        index = block(key)
        out = b"".join(block(index[i] | index[256 + i] << 8)
                       for i in range((eof + BLOCK - 1) // BLOCK))
        return out[:eof]
    if storage == 3:
        master = block(key)
        out = b""
        for j in range(128):
            ib = master[j] | master[256 + j] << 8
            if ib == 0 or len(out) >= eof:
                break
            index = block(ib)
            for i in range(256):
                if len(out) >= eof:
                    break
                b = index[i] | index[256 + i] << 8
                out += block(b) if b else bytes(BLOCK)
        return out[:eof]
    return None


def check(path):
    data = load_blocks(path)
    header = data[VOLDIR_KEY * BLOCK + 4:VOLDIR_KEY * BLOCK + 4 + ENTRY_LENGTH]
    name = header[1:1 + (header[0] & 0x0F)].decode()
    total, = struct.unpack_from("<H", header, 0x25)
    print(f"/{name}: {total} blocks")
    known = {f[0]: f[3] for f in FILES}
    ok = True
    block = VOLDIR_KEY
    first = True
    while block:
        b = data[block * BLOCK:(block + 1) * BLOCK]
        nxt, = struct.unpack_from("<H", b, 2)
        for n in range(1 if first else 0, ENTRIES_PER_BLOCK):
            e = b[4 + n * ENTRY_LENGTH:4 + (n + 1) * ENTRY_LENGTH]
            storage = e[0] >> 4
            if storage == 0:
                continue
            fname = e[1:1 + (e[0] & 0x0F)].decode()
            ftype = e[0x10]
            key, = struct.unpack_from("<H", e, 0x11)
            eof = int.from_bytes(e[0x15:0x18], "little")
            aux, = struct.unpack_from("<H", e, 0x1F)
            contents = read_file(data, storage, key, eof)
            if fname in known:
                verdict = "OK" if contents == known[fname] else "BAD"
            elif contents is not None and contents == pattern(fname, eof):
                verdict = "OK (pattern)"
            else:
                verdict = ""
            # A copy of a known file under another name
            for kname, kdata in known.items():
                if fname not in known and contents == kdata:
                    verdict = f"OK (copy of {kname})"
            ok &= verdict != "BAD"
            print(f"  {fname:15} type ${ftype:02X} aux ${aux:04X} "
                  f"{eof:7} bytes {verdict}")
        first = False
        block = nxt
    print("all known files OK" if ok else "MISMATCH")
    return ok


def main():
    if len(sys.argv) == 3 and sys.argv[1] == "make":
        make(sys.argv[2])
    elif len(sys.argv) == 3 and sys.argv[1] == "check":
        sys.exit(0 if check(sys.argv[2]) else 1)
    else:
        print(__doc__)
        sys.exit(2)


if __name__ == "__main__":
    main()
