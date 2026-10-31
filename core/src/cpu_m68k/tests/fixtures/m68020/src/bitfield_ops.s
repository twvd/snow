.include "entry.s"
/* Test: Bitfield Operations (68020+) */
/* BFCHG, BFCLR, BFEXTS, BFEXTU, BFFFO, BFINS, BFSET, BFTST */

.set SRC_LOC, STACK2_BASE
.set BF_LOC1, SRC_LOC+0
.set BF_LOC2, SRC_LOC+4

run_test:
    /* =================================================================== */
    /* BFTST - Test Bit Field */
    /* =================================================================== */
    mov.l #0x01234567, BF_LOC1
    bftst BF_LOC1{#12:#8}        | Test bits 12-19 (0x34)
    beq TEST_FAIL               | Z should be 0 (field not zero)
    bmi TEST_FAIL               | N should be 0 (MSB of field is 0)
    
    mov.l #0x00800000, BF_LOC1
    bftst BF_LOC1{#8:#8}        | Test bits 8-15 (0x80)
    beq TEST_FAIL               | Z should be 0
    bpl TEST_FAIL               | N should be 1 (MSB is 1)
    
    mov.l #0x00000000, BF_LOC1
    bftst BF_LOC1{#0:#32}       | Test all bits (0)
    bne TEST_FAIL               | Z should be 1 (field is zero)
    
    /* =================================================================== */
    /* BFEXTU - Extract Bit Field Unsigned */
    /* =================================================================== */
    mov.l #0x01234567, BF_LOC1
    bfextu BF_LOC1{#12:#8}, %d0  | Extract 0x34
    cmp.l #0x34, %d0
    bne TEST_FAIL
    
    mov.l #0xF0000000, BF_LOC1
    bfextu BF_LOC1{#0:#4}, %d0   | Extract 0xF
    cmp.l #0x0F, %d0             | Unsigned: 0x0F
    bne TEST_FAIL
    
    /* =================================================================== */
    /* BFEXTS - Extract Bit Field Signed */
    /* =================================================================== */
    mov.l #0x01234567, BF_LOC1
    bfexts BF_LOC1{#12:#8}, %d0  | Extract 0x34 (sign-extended)
    cmp.l #0x34, %d0             | Positive value unchanged
    bne TEST_FAIL
    
    mov.l #0xF0000000, BF_LOC1
    bfexts BF_LOC1{#0:#4}, %d0   | Extract 0xF (sign-extended)
    cmp.l #0xFFFFFFFF, %d0       | Signed: -1
    bne TEST_FAIL
    
    /* =================================================================== */
    /* BFCLR - Clear Bit Field */
    /* =================================================================== */
    mov.l #0xFFFFFFFF, BF_LOC1
    bfclr BF_LOC1{#8:#8}        | Clear bits 8-15
    cmp.l #0xFF00FFFF, BF_LOC1
    bne TEST_FAIL
    
    /* =================================================================== */
    /* BFSET - Set Bit Field */
    /* =================================================================== */
    mov.l #0x00000000, BF_LOC1
    bfset BF_LOC1{#8:#8}        | Set bits 8-15
    cmp.l #0x00FF0000, BF_LOC1
    bne TEST_FAIL
    
    /* =================================================================== */
    /* BFCHG - Change (complement) Bit Field */
    /* =================================================================== */
    mov.l #0x0F0F0F0F, BF_LOC1
    bfchg BF_LOC1{#8:#8}        | Complement bits 8-15 (0x0F -> 0xF0)
    cmp.l #0x0FF00F0F, BF_LOC1
    bne TEST_FAIL
    
    /* =================================================================== */
    /* BFINS - Insert Bit Field */
    /* =================================================================== */
    mov.l #0x00000000, BF_LOC1
    mov.l #0xAB, %d0
    bfins %d0, BF_LOC1{#12:#8}  | Insert 0xAB at bits 12-19
    cmp.l #0x000AB000, BF_LOC1
    bne TEST_FAIL
    
    /* =================================================================== */
    /* BFFFO - Find First One in Bit Field */
    /* =================================================================== */
    mov.l #0x00010000, BF_LOC1
    bfffo BF_LOC1{#0:#32}, %d0  | Find first 1 bit
    cmp.l #15, %d0               | Bit 15 is the first 1
    bne TEST_FAIL
    
    mov.l #0x80000000, BF_LOC1
    bfffo BF_LOC1{#0:#32}, %d0  | Find first 1 bit
    cmp.l #0, %d0                | Bit 0 is the first 1
    bne TEST_FAIL
    
    /* =================================================================== */
    /* Bitfield spanning multiple bytes (wrap-around in register) */
    /* =================================================================== */
    mov.l #0x01234567, %d1
    bfextu %d1{#28:#8}, %d0     | Extract 4 bits at end + 4 bits at start
    cmp.l #0x70, %d0             | Should be 0x70 (7 << 4 | 0)
    bne TEST_FAIL
    
    /* =================================================================== */
    /* Bitfield spanning multiple memory words */
    /* =================================================================== */
    mov.l #0x01234567, BF_LOC1
    mov.l #0x89ABCDEF, BF_LOC2
    bfextu BF_LOC1{#24:#16}, %d0 | Extract 8 bits from LOC1, 8 from LOC2
    cmp.l #0x6789, %d0
    bne TEST_FAIL
    
    rts
