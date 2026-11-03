#!/usr/bin/env python3
"""Merges the two chips of a Lisa boot ROM into the single file Snow loads.

    lisa-rom-merge.py HIGH LOW OUT

The boot ROM is two 8K chips, one per byte lane of the 68000: HIGH holds the
bytes at even addresses, LOW those at odd addresses (MAME's lisa.cpp):

    Rev H     HIGH 341-0175-h    LOW 341-0176-h
    Rev F     HIGH 341-0175-f    LOW 341-0176-f
    Rev 3A    HIGH 341-0347-a    LOW 341-0346-a    (Screen Kit)

OUT is 16K: a byte of HIGH, then a byte of LOW, and so on.
"""

import sys


def main():
    if len(sys.argv) != 4:
        sys.exit(__doc__)
    with open(sys.argv[1], "rb") as f:
        high = f.read()
    with open(sys.argv[2], "rb") as f:
        low = f.read()
    if len(high) != len(low):
        sys.exit(f"The chips differ in size: {len(high)} and {len(low)} bytes")
    merged = bytearray(2 * len(high))
    merged[0::2] = high
    merged[1::2] = low
    with open(sys.argv[3], "wb") as f:
        f.write(merged)


if __name__ == "__main__":
    main()
