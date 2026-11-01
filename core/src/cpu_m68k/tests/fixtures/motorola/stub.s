| Runs Motorola's itest.sa (see ../README.md): the call-outs it needs, printing
| on the test device. The image follows the 128 bytes of call-outs at the end of
| this stub, loaded there by the test. Not part of Motorola's package.
|
| Build: m68k-apple-macos-as -m68040 stub.s -o stub.o
|        m68k-apple-macos-ld -Ttext=0x10000 -e _start stub.o -o stub.elf
|        m68k-apple-macos-objcopy -O binary stub.elf stub.bin
	.set	DEV_FAIL, 0x100000
	.set	DEV_PASS, 0x100004
	.set	DEV_OUT, 0x100014
	.text
	.globl	_start
_start:
	movea.l	#0x0F0000,%sp
	jsr	callouts+128
	move.l	#1,DEV_PASS
1:	stop	#0x2700
	bra	1b

| The test passes a string on the stack, and returns with it there
print_str:
	movem.l	%d0/%a0,-(%sp)
	movea.l	12(%sp),%a0
1:	move.b	(%a0)+,%d0
	beq.s	2f
	move.b	%d0,DEV_OUT
	bra.s	1b
2:	movem.l	(%sp)+,%d0/%a0
	rts

| The test only prints a number, of the failed case, on a failure
print_num:
	movem.l	%d0-%d2,-(%sp)
	move.l	16(%sp),%d0
	moveq	#7,%d2
1:	rol.l	#4,%d0
	move.b	%d0,%d1
	and.b	#15,%d1
	add.b	#'0',%d1
	cmp.b	#'9',%d1
	bls.s	2f
	add.b	#7,%d1
2:	move.b	%d1,DEV_OUT
	dbra	%d2,1b
	move.l	#1,DEV_FAIL
	movem.l	(%sp)+,%d0-%d2
	rts

	.balign	4
callouts:
	.long	print_str-callouts
	.long	print_num-callouts
	.space	120
