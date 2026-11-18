| AppleTalk DDP socket listener
|
| Called by the .MPP driver when a datagram arrives on our socket:
|   d0.b = destination socket, d1.w = data bytes left to read
|   a4 = ReadPacket routine (ReadRest is at a4+2)
| Reads the whole datagram into the buffer given to atalk_listener_init
| and stores the destination socket, the length and a flag.
|
| Register rules (Inside Macintosh: Networking):
|   - until ReadRest: A0-A2, A4 and D1 must be preserved, A5 is free
|     (on entry A5 = the listener address, not the caller's A5)
|   - after ReadRest: A5 must be preserved (ReadRest restores the
|     driver's A5), A0-A3 and D0-D3 are free, D2 survives ReadRest
|   - D4-D7 and A6 must be preserved throughout

	.text
	.globl	atalk_listener_init
atalk_listener_init:
	move.l	4(%sp),%d0
	move.l	%d0,atalk_buf
	move.l	8(%sp),%d0
	move.l	%d0,atalk_flag
	move.l	12(%sp),%d0
	move.l	%d0,atalk_len
	move.l	16(%sp),%d0
	move.l	%d0,atalk_socket
	rts

	.globl	atalk_listener
atalk_listener:
	moveq	#0,%d2
	move.w	%d1,%d2			| data length in the high word of D2
	swap	%d2
	move.b	%d0,%d2			| destination socket in the low byte
	movea.l	atalk_buf,%a3
	move.w	%d1,%d3			| read the whole datagram
	jsr	2(%a4)			| ReadRest (restores the driver's A5)
	movea.l	atalk_socket,%a0
	move.w	%d2,(%a0)
	swap	%d2
	movea.l	atalk_len,%a0
	move.w	%d2,(%a0)
	movea.l	atalk_flag,%a0
	move.w	#1,(%a0)
	rts

| Pointers to the client's buffers, set by atalk_listener_init
atalk_buf:	.long	0
atalk_flag:	.long	0
atalk_len:	.long	0
atalk_socket:	.long	0
