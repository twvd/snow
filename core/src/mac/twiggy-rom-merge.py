#!/usr/bin/env python3
"""Merges the four EPROMs of a Twiggy Mac ROM into the single file Snow loads.

    twiggy-rom-merge.py HIGH0 LOW0 HIGH1 LOW1 OUT

The ROM is four 16K chips: HIGH holds the bytes at even addresses, LOW those at
odd addresses, 0 the first 32K and 1 the second. Adam Goolevitch's machine
(HIGH0B.BIN, LOW0B.BIN, HIGH1B.BIN, LOW1B.BIN) gives a ROM whose SHA-256 is
a41053e89908e0912800d24aa67685ab264fa8735622d50aa718b073cd17522d.

OUT is 64K: a byte of HIGH, then a byte of LOW, for each half in turn.
"""

import sys


def interleave(high, low):
    if len(high) != len(low):
        sys.exit(f"The chips differ in size: {len(high)} and {len(low)} bytes")
    merged = bytearray(2 * len(high))
    merged[0::2] = high
    merged[1::2] = low
    return merged


def main():
    if len(sys.argv) != 6:
        sys.exit(__doc__)
    chips = []
    for path in sys.argv[1:5]:
        with open(path, "rb") as f:
            chips.append(f.read())
    rom = interleave(chips[0], chips[1]) + interleave(chips[2], chips[3])
    with open(sys.argv[5], "wb") as f:
        f.write(rom)


if __name__ == "__main__":
    main()
