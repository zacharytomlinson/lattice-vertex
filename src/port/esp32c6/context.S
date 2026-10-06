/* SPDX-License-Identifier: MIT
 * Stack-based RV32 context switching for the FreeRTOS-derived task core.
 * The direct software IRQ uses esp-hal's vector table; ordinary peripheral
 * dispatch and reset handling remain with the HAL.
 */
.section .trap.rust, "ax"
.balign 4
.global lattice_c6_switch
.type lattice_c6_switch, @function
lattice_c6_switch:
    /* 36 words: x1..x31 (skip x2), mepc, mstatus, padding. */
    addi sp, sp, -144
    sw ra,   0(sp)
    sw gp,   8(sp)
    sw tp,  12(sp)
    sw t0,  16(sp)
    sw t1,  20(sp)
    sw t2,  24(sp)
    sw s0,  28(sp)
    sw s1,  32(sp)
    sw a0,  36(sp)
    sw a1,  40(sp)
    sw a2,  44(sp)
    sw a3,  48(sp)
    sw a4,  52(sp)
    sw a5,  56(sp)
    sw a6,  60(sp)
    sw a7,  64(sp)
    sw s2,  68(sp)
    sw s3,  72(sp)
    sw s4,  76(sp)
    sw s5,  80(sp)
    sw s6,  84(sp)
    sw s7,  88(sp)
    sw s8,  92(sp)
    sw s9,  96(sp)
    sw s10,100(sp)
    sw s11,104(sp)
    sw t3, 108(sp)
    sw t4, 112(sp)
    sw t5, 116(sp)
    sw t6, 120(sp)
    csrr t0, mepc
    sw t0, 124(sp)
    csrr t0, mstatus
    sw t0, 128(sp)
    la t0, pxCurrentTCB
    lw t0, 0(t0)
    sw sp, 0(t0)
    la sp, lattice_c6_switch_stack_top
    call lattice_c6_select_task
    j lattice_c6_restore
.size lattice_c6_switch, . - lattice_c6_switch

.global lattice_c6_restore
.type lattice_c6_restore, @function
lattice_c6_restore:
    la t0, pxCurrentTCB
    lw t0, 0(t0)
    lw sp, 0(t0)
    lw t0, 124(sp)
    csrw mepc, t0
    lw t0, 128(sp)
    csrw mstatus, t0
    lw ra,   0(sp)
    lw gp,   8(sp)
    lw tp,  12(sp)
    lw t0,  16(sp)
    lw t1,  20(sp)
    lw t2,  24(sp)
    lw s0,  28(sp)
    lw s1,  32(sp)
    lw a0,  36(sp)
    lw a1,  40(sp)
    lw a2,  44(sp)
    lw a3,  48(sp)
    lw a4,  52(sp)
    lw a5,  56(sp)
    lw a6,  60(sp)
    lw a7,  64(sp)
    lw s2,  68(sp)
    lw s3,  72(sp)
    lw s4,  76(sp)
    lw s5,  80(sp)
    lw s6,  84(sp)
    lw s7,  88(sp)
    lw s8,  92(sp)
    lw s9,  96(sp)
    lw s10,100(sp)
    lw s11,104(sp)
    lw t3, 108(sp)
    lw t4, 112(sp)
    lw t5, 116(sp)
    lw t6, 120(sp)
    addi sp, sp, 144
    mret
.size lattice_c6_restore, . - lattice_c6_restore
