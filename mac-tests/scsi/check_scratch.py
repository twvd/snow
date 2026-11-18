#!/usr/bin/python3
# Checks what the final sweep of SCSIExer left on the scratch disk, from the host
import sys
d = open(sys.argv[1], 'rb').read()
def fill(n, seed):
    x = (seed * 2654435761 + 0x9E3779B9) & 0xFFFFFFFF
    o = bytearray(n)
    for i in range(n):
        x ^= (x << 13) & 0xFFFFFFFF; x ^= x >> 17; x ^= (x << 5) & 0xFFFFFFFF
        o[i] = x & 0xFF
    return bytes(o)
bad = 0
for lba in range(0x10000, 0x10000 + 512, 8):
    if d[lba * 512:(lba + 8) * 512] != fill(8 * 512, lba):
        bad += 1; print('BAD', hex(lba))
print('sweep regions bad:', bad)
