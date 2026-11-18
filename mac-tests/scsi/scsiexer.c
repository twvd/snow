/*
 * SCSIExer: exercises the ways a Mac OS program can move data on SCSI.
 *
 * File Manager part: runs on the boot volume, in a file it creates.
 * SCSI Manager part: raw commands, ONLY on a scratch disk at SCSI ID 1 whose
 * block 0 is all zeros or starts with the SCSIEXER signature.
 *
 * Results go to the file "SCSIExer.log" at the root of the boot volume, which
 * is flushed after each test. The machine powers off at the end if the file
 * "SCSIExer.auto" exists there.
 */
#include <stdint.h>
#include <stdio.h>
#include <stdarg.h>
#include <string.h>
#include <Files.h>
#include <Memory.h>
#include <OSUtils.h>
#include <Traps.h>
#include <Gestalt.h>

#define SCRATCH_ID 1
#define NOCACHE 0x20
#define PLEASECACHE 0x10
#define BLK 512L
#define MINBUF 8192L
#define MAXBUF (512L * 1024L)
/* Left to the Toolbox and the stack */
#define RESERVE (64L * 1024L)

/* The SCSI Manager, trap $A815 with a selector */
pascal int16_t SGet(void) M68K_INLINE(0x3F3C, 1, 0xA815);
pascal int16_t SReset(void) M68K_INLINE(0x3F3C, 0, 0xA815);
pascal int16_t SSelect(int16_t id) M68K_INLINE(0x3F3C, 2, 0xA815);
pascal int16_t SCmd(void *cdb, int16_t n) M68K_INLINE(0x3F3C, 3, 0xA815);
pascal int16_t SComplete(int16_t *st, int16_t *msg, int32_t wait) M68K_INLINE(0x3F3C, 4, 0xA815);
pascal int16_t SRead(void *tib) M68K_INLINE(0x3F3C, 5, 0xA815);
pascal int16_t SWrite(void *tib) M68K_INLINE(0x3F3C, 6, 0xA815);
pascal int16_t SRBlind(void *tib) M68K_INLINE(0x3F3C, 8, 0xA815);
pascal int16_t SWBlind(void *tib) M68K_INLINE(0x3F3C, 9, 0xA815);

enum { scInc = 1, scLoop = 5, scStop = 7 };
typedef struct __attribute__((packed)) { int16_t op; int32_t p1, p2; } TIB;

static int16_t logRef, vRef;
static long fails, passes;
static uint8_t *rawbuf, *cmpbuf;
static long bufsz;
static int scratch;
static TIB tib[4];

/* The memcpy of older Retro68 toolchains takes a misaligned 64 KB as 0 bytes */
static void copy(uint8_t *d, const uint8_t *s, long n)
{
	long i;
	for (i = 0; i < n; i++)
		d[i] = s[i];
}

static void set(uint8_t *d, uint8_t v, long n)
{
	long i;
	for (i = 0; i < n; i++)
		d[i] = v;
}

static void lg(const char *fmt, ...)
{
	char s[200];
	va_list ap;
	long n;
	va_start(ap, fmt);
	n = vsnprintf(s, sizeof s, fmt, ap);
	va_end(ap);
	FSWrite(logRef, &n, s);
}

static void flushlog(void)
{
	ParamBlockRec pb;
	memset(&pb, 0, sizeof pb);
	pb.ioParam.ioVRefNum = vRef;
	PBFlushVolSync(&pb);
}

static void genfill(uint8_t *p, long n, uint32_t seed)
{
	uint32_t x = seed * 2654435761UL + 0x9E3779B9UL;
	long i;
	for (i = 0; i < n; i++) {
		x ^= x << 13; x ^= x >> 17; x ^= x << 5;
		p[i] = (uint8_t)x;
	}
}

static void result(const char *what, long a, long b, long c, int bad, long at)
{
	if (bad) {
		fails++;
		lg("FAIL %s %ld %ld %ld err/at %d %ld\r", what, a, b, c, bad, at);
	} else {
		passes++;
	}
}

static long diff(const uint8_t *a, const uint8_t *b, long n)
{
	long i;
	for (i = 0; i < n; i++)
		if (a[i] != b[i])
			return i;
	return -1;
}

/* ------- File Manager ------- */

static OSErr fileio(int16_t ref, int write, void *buf, long n, long pos, int mode, int async)
{
	ParamBlockRec pb;
	OSErr e;
	memset(&pb, 0, sizeof pb);
	pb.ioParam.ioRefNum = ref;
	pb.ioParam.ioBuffer = buf;
	pb.ioParam.ioReqCount = n;
	pb.ioParam.ioPosMode = fsFromStart | mode;
	pb.ioParam.ioPosOffset = pos;
	if (async) {
		e = write ? PBWriteAsync(&pb) : PBReadAsync(&pb);
		while (pb.ioParam.ioResult > 0)
			;
		e = pb.ioParam.ioResult;
	} else {
		e = write ? PBWriteSync(&pb) : PBReadSync(&pb);
	}
	if (!e && pb.ioParam.ioActCount != n)
		e = -9999;
	return e;
}

static void filetests(int mode, const char *name)
{
#ifdef QUICK
	static const long sizes[] = { 32768, 65536 };
#else
	static const long sizes[] = { 1, 2, 3, 4, 17, 255, 256, 511, 512, 513, 1023, 1024, 1025,
		4095, 4096, 4097, 32768, 65536, 131072, 262144, 524288, 1048576 };
#endif
#ifdef QUICK
	static const long poss[] = { 0 };
#else
	static const long poss[] = { 0, 1, 2, 3, 511, 512, 513, 4095, 100000 };
#endif
	static const unsigned char fn[] = "\pSCSIExer.tmp";
	int16_t ref;
	unsigned s, p, off, async;
	uint32_t seed = 1;
	long before = fails;

	FSDelete(fn, vRef);
	if (Create(fn, vRef, '????', 'DATA') || FSOpen(fn, vRef, &ref)) {
		lg("FAIL create\r");
		fails++;
		return;
	}
	for (s = 0; s < sizeof sizes / sizeof *sizes; s++)
		for (p = 0; p < sizeof poss / sizeof *poss; p++)
			for (off = 0; off < 4; off++)
				for (async = 0; async < 2; async++) {
					long n = sizes[s], pos = poss[p], at;
					int e;
					if (n > bufsz)
						continue;
					genfill(cmpbuf, n, ++seed);
					copy(rawbuf + off, cmpbuf, n);
					at = diff(rawbuf + off, cmpbuf, n);
					if (at >= 0) {
						result("memcpy", n, (long)rawbuf, off, 1, at);
						continue;
					}
					e = fileio(ref, 1, rawbuf + off, n, pos, mode, async);
					if (e) {
						result(name, n, pos, off, e, -1);
						continue;
					}
					set(rawbuf, 0xA5, n + 8);
					e = fileio(ref, 0, rawbuf + off, n, pos, mode, async);
					at = e ? -1 : diff(rawbuf + off, cmpbuf, n);
					result(name, n, pos, off, e ? e : (at >= 0 ? 1 : 0), at);
					if (!e && at >= 0) {
						/* Which of the write and the read is wrong: compare
						 * to the file read at another alignment */
						long last = at, i, bad = 0;
						const uint8_t *r = rawbuf + off;
						for (i = at; i < n; i++)
							if (r[i] != cmpbuf[i]) {
								bad++;
								last = i;
							}
						lg(" %ld bytes differ, last %ld; got %02x%02x%02x%02x expected %02x%02x%02x%02x; before %02x\r",
							bad, last, r[at], r[at + 1], r[at + 2], r[at + 3], cmpbuf[at],
							cmpbuf[at + 1], cmpbuf[at + 2], cmpbuf[at + 3], rawbuf[off ? off - 1 : 0]);
						set(rawbuf, 0xA5, n + 8);
						e = fileio(ref, 0, rawbuf, n, pos, mode, 0);
						at = e ? -1 : diff(rawbuf, cmpbuf, n);
						lg(" read again at offset 0: err %d first diff %ld\r", e, at);
					}
				}
	FSClose(ref);
	FSDelete(fn, vRef);
	lg("file %s: %ld failures\r", name, fails - before);
	flushlog();
}

/* A file larger than any cache of the System, in chunks of varied sizes,
 * written then read back, which also leaves it for the host to check */
static void bigfile(long maxsize)
{
	static const unsigned char fn[] = "\pSCSIExer.big";
	static const long chunks[] = { 512, 3072, 65536, 7168, 131072, 1024, 32768 };
	HParamBlockRec h;
	int16_t ref;
	long total, pos, n, at, before = fails;
	unsigned c;
	int e, pass;

	memset(&h, 0, sizeof h);
	h.volumeParam.ioVRefNum = vRef;
	PBHGetVInfoSync(&h);
	total = (long)(unsigned short)h.volumeParam.ioVFrBlk * h.volumeParam.ioVAlBlkSiz / 2;
	if (total > maxsize)
		total = maxsize;
	total &= ~511L;
	if (total < 1024L * 1024L)
		return;
	FSDelete(fn, vRef);
	if (Create(fn, vRef, '????', 'DATA') || FSOpen(fn, vRef, &ref)) {
		lg("FAIL big file create\r");
		fails++;
		return;
	}
	for (pass = 0; pass < 2; pass++) {
		for (pos = 0, c = 0; pos < total; pos += n, c = (c + 1) % 7) {
			n = chunks[c];
			if (n > bufsz)
				n = bufsz;
			if (n > total - pos)
				n = total - pos;
			genfill(cmpbuf, n, (uint32_t)pos + 77);
			if (pass == 0) {
				e = fileio(ref, 1, cmpbuf, n, pos, 0, 0);
				at = -1;
			} else {
				e = fileio(ref, 0, rawbuf, n, pos, 0, 0);
				at = e ? -1 : diff(rawbuf, cmpbuf, n);
			}
			result(pass ? "bigread" : "bigwrite", pos, n, 0, e ? e : (at >= 0 ? 1 : 0), at);
		}
		flushlog();
		/* Takes what was read from the caches */
		if (pass == 0) {
			FSClose(ref);
			flushlog();
			FSOpen(fn, vRef, &ref);
		}
	}
	FSClose(ref);
	lg("big file of %ld bytes: %ld failures\r", total, fails - before);
	flushlog();
}

/* ------- SCSI Manager on the scratch disk ------- */

static int start(uint8_t *cdb, int n)
{
	int16_t e;
	if ((e = SGet()) != 0)
		return e;
	if ((e = SSelect(SCRATCH_ID)) != 0)
		return e;
	return SCmd(cdb, n);
}

static int finish(void)
{
	int16_t st = -1, msg = -1, e;
	e = SComplete(&st, &msg, 300);
	return e ? e : st;
}

static void rw10(uint8_t *c, uint8_t op, long lba, int blocks)
{
	memset(c, 0, 10);
	c[0] = op; c[2] = lba >> 24; c[3] = lba >> 16; c[4] = lba >> 8; c[5] = lba;
	c[7] = blocks >> 8; c[8] = blocks;
}

static void rw6(uint8_t *c, uint8_t op, long lba, int blocks)
{
	memset(c, 0, 6);
	c[0] = op; c[1] = lba >> 16; c[2] = lba >> 8; c[3] = lba; c[4] = blocks;
}

/* A TIB moving n bytes in pieces of chunk bytes */
static void maketib(void *buf, long n, long chunk)
{
	tib[0].op = scInc; tib[0].p1 = (long)buf; tib[0].p2 = chunk;
	tib[1].op = scLoop; tib[1].p1 = -10; tib[1].p2 = n / chunk;
	tib[2].op = scStop; tib[2].p1 = 0; tib[2].p2 = 0;
}

static int xfer(int method, uint8_t *c, int clen, void *buf, long n, long chunk)
{
	int e, e2;
	if ((e = start(c, clen)) != 0)
		return e;
	maketib(buf, n, chunk);
	switch (method) {
	case 0: e = SRead(tib); break;
	case 1: e = SRBlind(tib); break;
	case 2: e = SWrite(tib); break;
	default: e = SWBlind(tib); break;
	}
	e2 = finish();
	return e ? e : e2;
}

static void rawmatrix(void)
{
	static const int counts[] = { 1, 2, 8, 64 };
	static const long chunks[] = { 512, 1, 2, 4, 16, 17, 256 };
	unsigned k, ch, off, m, cdb;
	long lba = 0x1000;
	uint32_t seed = 0x1000;
	long before = fails;
	uint8_t c[10];

	for (k = 0; k < 4; k++)
		for (ch = 0; ch < 7; ch++)
			for (off = 0; off < 4; off++)
				for (m = 0; m < 2; m++)
					for (cdb = 0; cdb < 2; cdb++) {
						long n = counts[k] * BLK, chunk = chunks[ch], at;
						int e;
						if (n > bufsz || n % chunk) continue;
						if (chunk < 16 && n > 4096) continue;
						genfill(cmpbuf, n, ++seed);
						copy(rawbuf + off, cmpbuf, n);
						if (cdb) rw10(c, 0x2A, lba, counts[k]);
						else rw6(c, 0x0A, lba, counts[k]);
						e = xfer(2 + m, c, cdb ? 10 : 6, rawbuf + off, n, chunk);
						if (e) { result("rawwrite", n, chunk, off, e, -1); continue; }
						set(rawbuf, 0xA5, n + 8);
						if (cdb) rw10(c, 0x28, lba, counts[k]);
						else rw6(c, 0x08, lba, counts[k]);
						e = xfer(m, c, cdb ? 10 : 6, rawbuf + off, n, chunk);
						at = e ? -1 : diff(rawbuf + off, cmpbuf, n);
						result(m ? "rawblind" : "rawpoll", n, chunk, off, e ? e : (at >= 0 ? 1 : 0), at);
						lba = 0x1000 + (lba - 0x1000 + counts[k]) % 0x4000;
					}
	lg("raw matrix: %ld failures\r", fails - before);
	flushlog();
}

/* Reads back blocks and checks them; what a failed command leaves */
static void checkblocks(const char *what, long lba, int blocks, uint32_t seed)
{
	uint8_t c[10];
	long at;
	int e;
	genfill(cmpbuf, blocks * BLK, seed);
	rw10(c, 0x28, lba, blocks);
	e = xfer(1, c, 10, rawbuf, blocks * BLK, BLK);
	at = e ? -1 : diff(rawbuf, cmpbuf, blocks * BLK);
	lg("  %s: readback err %d, first diff %ld\r", what, e, at);
}

static void prepare(long lba, int blocks, uint32_t seed)
{
	uint8_t c[10];
	genfill(rawbuf, blocks * BLK, seed);
	rw10(c, 0x2A, lba, blocks);
	xfer(3, c, 10, rawbuf, blocks * BLK, BLK);
}

/* Commands whose data phase doesn't match: only logged, as there's no right
 * answer to compare with. What matters is what the following commands see. */
static void abnormal(void)
{
	uint8_t c[10];
	int e, e2;
	const long lba = 0x8000;

	lg("abnormal transfers\r");
	/* Short read: asks 4 blocks, takes 1 and half a block */
	prepare(lba, 4, 77);
	rw10(c, 0x28, lba, 4);
	e = xfer(1, c, 10, rawbuf, BLK, BLK);
	lg(" short read 1/4: err %d\r", e);
	checkblocks("after short read", lba, 4, 77);
	rw10(c, 0x28, lba, 1);
	e = xfer(0, c, 10, rawbuf, 256, 256);
	lg(" short read half block: err %d\r", e);
	checkblocks("after short read 2", lba, 4, 77);

	/* Long read: asks 1 block, offers a TIB of 4 */
	rw10(c, 0x28, lba, 1);
	e = xfer(0, c, 10, rawbuf, 4 * BLK, BLK);
	lg(" long read polled: err %d\r", e);
	rw10(c, 0x28, lba, 1);
	e = xfer(1, c, 10, rawbuf, 4 * BLK, BLK);
	lg(" long read blind: err %d\r", e);
	checkblocks("after long reads", lba, 4, 77);

	/* Short write: asks 4 blocks, gives 1. The disk waits for the rest. */
	genfill(rawbuf, 4 * BLK, 78);
	rw10(c, 0x2A, lba, 4);
	e = xfer(2, c, 10, rawbuf, BLK, BLK);
	lg(" short write polled: err %d\r", e);
	SReset();
	checkblocks("after short write polled and reset", lba, 4, 77);
	prepare(lba, 4, 77);
	rw10(c, 0x2A, lba, 4);
	e = xfer(3, c, 10, rawbuf, 256, 256);
	lg(" short write blind: err %d\r", e);
	SReset();
	checkblocks("after short write blind and reset", lba, 4, 77);

	/* Long write: asks 1 block, offers 4 */
	prepare(lba, 4, 77);
	genfill(rawbuf, 4 * BLK, 79);
	rw10(c, 0x2A, lba, 1);
	e = xfer(2, c, 10, rawbuf, 4 * BLK, BLK);
	lg(" long write polled: err %d\r", e);
	rw10(c, 0x2A, lba, 1);
	e = xfer(3, c, 10, rawbuf, 4 * BLK, BLK);
	lg(" long write blind: err %d\r", e);
	SReset();

	/* No data phase taken at all, reset in the middle of a command */
	rw10(c, 0x28, lba, 2);
	e = start(c, 10);
	e2 = finish();
	lg(" read, no data phase taken: %d %d\r", e, e2);
	SReset();
	rw10(c, 0x28, lba, 2);
	e = start(c, 10);
	SReset();
	lg(" reset in read: %d\r", e);
	rw10(c, 0x2A, lba + 8, 2);
	e = start(c, 10);
	SReset();
	lg(" reset in write: %d\r", e);

	/* Out of range and unknown commands, then sense */
	rw10(c, 0x28, 0x7FFFFFF0L, 1);
	e = start(c, 10);
	e2 = finish();
	lg(" read out of range: %d status %d\r", e, e2);
	memset(c, 0, 10); c[0] = 3; c[4] = 18;
	e = xfer(1, c, 6, rawbuf, 18, 18);
	lg(" sense: %d\r", e);
	memset(c, 0, 10); c[0] = 0x12; c[4] = 36;
	e = xfer(0, c, 6, rawbuf, 36, 36);
	lg(" inquiry: %d\r", e);
	memset(c, 0, 10); c[0] = 0x25;
	e = xfer(0, c, 10, rawbuf, 8, 8);
	lg(" read capacity: %d\r", e);
	memset(c, 0, 10); c[0] = 0x1A; c[2] = 0x3F; c[4] = 255;
	e = xfer(0, c, 6, rawbuf, 64, 64);
	lg(" mode sense, partly taken: %d\r", e);
	flushlog();
}

/* All the memory this program can get, and transfers across every megabyte of
 * it: buffers that straddle the banks of the machine */
static void ramtests(uint8_t *big, long size)
{
	const long window = 65536L, mb = 1024L * 1024L;
	uint32_t *p = (uint32_t *)((long)(big + 3) & ~3L);
	long n = (size - 4) / 4, i, bad = 0, first = -1, b, lastbad = 0;
	long before = fails;
	uint8_t c[10];
	int pass;

	for (pass = 0; pass < 2; pass++) {
		uint32_t mix = pass ? 0xA5A55A5AUL : 0;
		for (i = 0; i < n; i++)
			p[i] = (uint32_t)(long)&p[i] ^ mix;
		for (i = 0; i < n; i++)
			if (p[i] != ((uint32_t)(long)&p[i] ^ mix)) {
				if (first < 0)
					first = (long)&p[i];
				lastbad = (long)&p[i];
				bad++;
			}
	}
	lg("memory walk %08lx +%ld: %ld bad, first %08lx last %08lx\r", (long)big, size, bad, first, lastbad);
	if (bad)
		fails++;
	if (!scratch)
		return;
	flushlog();
	/* The first megabyte boundary in the block, then all the others */
	for (b = ((long)big + window + mb - 1) & ~(mb - 1); b + window < (long)big + size; b += mb) {
		uint8_t *q = (uint8_t *)(b - window / 2);
		long at;
		int e;
		genfill(q, window, (uint32_t)b);
		copy(cmpbuf, q, window > bufsz ? bufsz : window);
		rw10(c, 0x2A, 0x18000L, window / BLK);
		e = xfer(3, c, 10, q, window, BLK);
		set(q, 0xA5, window);
		if (!e) {
			rw10(c, 0x28, 0x18000L, window / BLK);
			e = xfer(1, c, 10, q, window, BLK);
		}
		at = e ? -1 : diff(q, cmpbuf, window > bufsz ? bufsz : window);
		result("across", b, window, 0, e ? e : (at >= 0 ? 1 : 0), at);
	}
	lg("across the megabytes: %ld failures\r", fails - before);
	flushlog();
}

/* What's left on the disk, for the host to check against the same generator */
static void finalsweep(void)
{
	uint8_t c[10];
	long lba;
	int e, off = 0;
	long bad = 0;
	for (lba = 0x10000; lba < 0x10000 + 512; lba += 8, off = (off + 1) & 3) {
		genfill(rawbuf + off, 8 * BLK, (uint32_t)lba);
		rw10(c, 0x2A, lba, 8);
		e = xfer(3, c, 10, rawbuf + off, 8 * BLK, BLK);
		if (e) bad++;
	}
	lg("final sweep: %ld errors\r", bad);
	flushlog();
}

static int scratch_ok(void)
{
	uint8_t c[10];
	long i;
	int e;
	rw10(c, 0x28, 0, 1);
	e = xfer(0, c, 10, rawbuf, BLK, BLK);
	if (e) {
		lg("no scratch disk at ID %d (err %d)\r", SCRATCH_ID, e);
		return 0;
	}
	if (!memcmp(rawbuf, "SCSIEXER", 8))
		return 1;
	for (i = 0; i < BLK; i++)
		if (rawbuf[i]) {
			lg("disk at ID %d is not a scratch disk, raw tests skipped\r", SCRATCH_ID);
			return 0;
		}
	memcpy(rawbuf, "SCSIEXER", 8);
	rw10(c, 0x2A, 0, 1);
	return xfer(3, c, 10, rawbuf, BLK, BLK) == 0;
}

int main(void)
{
	static const unsigned char logn[] = "\pSCSIExer.log", autof[] = "\pSCSIExer.auto";
	int16_t r;
	ParamBlockRec pb;

	memset(&pb, 0, sizeof pb);
	/* The boot volume, not the folder of this program */
	{
		HParamBlockRec h;
		memset(&h, 0, sizeof h);
		h.volumeParam.ioVolIndex = 1;
		PBHGetVInfoSync(&h);
		vRef = h.volumeParam.ioVRefNum;
	}
	FSDelete(logn, vRef);
	Create(logn, vRef, 'ttxt', 'TEXT');
	if (FSOpen(logn, vRef, &logRef))
		return 1;
	MaxApplZone();
	bufsz = (MaxBlock() - RESERVE) / 2 - 16;
	lg("memory for this program: %ld bytes\r", (long)MaxBlock());
	if (bufsz > MAXBUF)
		bufsz = MAXBUF;
	if (bufsz < MINBUF) {
		lg("not enough memory: %ld bytes\r", (long)MaxBlock());
		goto out;
	}
	rawbuf = (uint8_t *)NewPtr(bufsz + 16);
	cmpbuf = (uint8_t *)NewPtr(bufsz + 16);
	if (!rawbuf || !cmpbuf) {
		lg("no memory for 2 buffers of %ld bytes\r", bufsz);
		goto out;
	}
	lg("buffers of %ld bytes\r", bufsz);
	lg("SCSIExer start, trap $A089 %s\r",
		GetOSTrapAddress(0xA089) == GetToolTrapAddress(0xA89F) ? "absent" : "present");
	flushlog();

#ifdef QUICK
	filetests(NOCACHE, "noCache");
	goto done;
#endif
#ifdef ACROSS
	scratch = scratch_ok();
	{
		long left = MaxBlock() - RESERVE;
		uint8_t *big = (uint8_t *)NewPtr(left);
		if (big)
			ramtests(big, left);
	}
	goto done;
#endif
	filetests(0, "default");
	filetests(PLEASECACHE, "pleaseCache");
	filetests(NOCACHE, "noCache");
	bigfile(24L * 1024L * 1024L);
	{
		long aux = 0;
		Gestalt('a/ux', &aux);
		if (aux)
			lg("A/UX $%lx\r", aux);
		scratch = scratch_ok();
		/* A bus reset would hit the disks of the kernel too */
		if (scratch) {
			rawmatrix();
			if (!aux)
				abnormal();
			rawmatrix();
			finalsweep();
		}
	}
	{
		long left = MaxBlock() - RESERVE;
		if (left > 256L * 1024L) {
			uint8_t *big = (uint8_t *)NewPtr(left);
			if (big)
				ramtests(big, left);
		}
	}
#if defined(QUICK) || defined(ACROSS)
done:
#endif
	lg("SCSIExer done: %ld passed, %ld failed\r", passes, fails);
out:
	FSClose(logRef);
	flushlog();
	if (FSOpen(autof, vRef, &r) == 0) {
		FSClose(r);
		ShutDwnPower();
	}
	return 0;
}
