//! Stand-ins for the boot ROM routines that bare-metal software calls.
//!
//! Homebrew treats a few boot ROM functions as a library: GodMode9's
//! `common/bfn.h` lists their addresses and AAPCS signatures for both
//! processors. Without the ROMs the shim puts routines of its own at those
//! addresses. They are written from the documented signatures, not from the
//! ROMs: barriers and cache maintenance return at once (nothing is cached
//! here), the critical-section pair masks and restores IRQs, and the delay
//! loop burns roughly the requested number of cycles.

const BX_LR: u32 = 0xE12F_FF1E;
const UDF: u32 = 0xE7F0_00F0;

/// `void f(...)`: nothing to do.
const RETURN: &[u32] = &[BX_LR];

/// `bool f(...)`: report that the cache was off.
const RETURN_FALSE: &[u32] = &[
    0xE3A0_0000, // mov r0, #0
    BX_LR,
];

/// Not provided: fail loudly rather than return garbage.
const MISSING: &[u32] = &[UDF];

/// `void waitCycles(u32 cycles)`: four cycles a turn in this core's model.
const WAIT_CYCLES: &[u32] = &[
    0xE250_0004, // subs r0, r0, #4
    0x8AFF_FFFD, // bhi  back
    BX_LR,
];

/// `u32 enterCriticalSection()`: mask IRQs, return the old mask bit.
const ENTER_CRITICAL: &[u32] = &[
    0xE10F_0000, // mrs r0, cpsr
    0xE380_1080, // orr r1, r0, #0x80
    0xE121_F001, // msr cpsr_c, r1
    0xE200_0080, // and r0, r0, #0x80
    BX_LR,
];

/// `void leaveCriticalSection(u32 state)`: restore the mask bit.
const LEAVE_CRITICAL: &[u32] = &[
    0xE10F_1000, // mrs r1, cpsr
    0xE3C1_1080, // bic r1, r1, #0x80
    0xE200_0080, // and r0, r0, #0x80
    0xE181_1000, // orr r1, r1, r0
    0xE121_F001, // msr cpsr_c, r1
    BX_LR,
];

const ENABLE_MPU: &[u32] = &[
    0xEE11_0F10, // mrc p15, 0, r0, c1, c0, 0
    0xE380_0001, // orr r0, r0, #1
    0xEE01_0F10, // mcr p15, 0, r0, c1, c0, 0
    BX_LR,
];

const DISABLE_MPU: &[u32] = &[
    0xEE11_0F10, // mrc p15, 0, r0, c1, c0, 0
    0xE3C0_0001, // bic r0, r0, #1
    0xEE01_0F10, // mcr p15, 0, r0, c1, c0, 0
    BX_LR,
];

/// `void resetControlRegisters()`: protection unit and caches off.
const RESET_CONTROL: &[u32] = &[
    0xEE11_0F10, // mrc p15, 0, r0, c1, c0, 0
    0xE3C0_0A01, // bic r0, r0, #0x1000
    0xE3C0_0005, // bic r0, r0, #5
    0xEE01_0F10, // mcr p15, 0, r0, c1, c0, 0
    BX_LR,
];

/// Where a secondary ARM11 core waits: sleep until an interrupt, then jump
/// to the entry point at 0x1FFFFFDC once one is there.
const SECONDARY_WAIT: &[u32] = &[
    0xE320_F003, // wfi
    0xE59F_000C, // ldr r0, =0x1FFFFFDC
    0xE590_0000, // ldr r0, [r0]
    0xE350_0000, // cmp r0, #0
    0x0AFF_FFFA, // beq back to the wfi
    0xE12F_FF10, // bx  r0
    0x1FFF_FFDC,
];

/// ARM11 address of [`SECONDARY_WAIT`] (3dbrew: "waits for IPI + branches to
/// word @ 0x1FFFFFDC").
pub const SECONDARY_WAIT_ADDR: u32 = 0x0001_004C;

/// ARM9 routines, by offset into the ROM at 0xFFFF0000.
pub const ARM9: &[(usize, &[u32])] = &[
    (0x0198, WAIT_CYCLES),
    (0x03A4, MISSING),
    (0x03F0, MISSING),
    (0x06EC, ENTER_CRITICAL),
    (0x0700, LEAVE_CRITICAL),
    (0x0798, RETURN_FALSE),
    (0x07B0, RETURN_FALSE),
    (0x07C8, RETURN_FALSE),
    (0x07F0, RETURN),
    (0x07FC, RETURN),
    (0x0830, RETURN),
    (0x0868, RETURN),
    (0x0884, RETURN),
    (0x08A8, RETURN),
    (0x096C, RETURN),
    (0x0A5C, RETURN_FALSE),
    (0x0A74, RETURN_FALSE),
    (0x0A8C, RETURN_FALSE),
    (0x0AB4, RETURN),
    (0x0AC0, RETURN),
    (0x0C38, ENABLE_MPU),
    (0x0C48, DISABLE_MPU),
    (0x0C58, RESET_CONTROL),
];

/// ARM11 routines, by offset into the ROM (which appears at 0 and 0x10000).
pub const ARM11: &[(usize, &[u32])] = &[
    (0x004C, SECONDARY_WAIT),
    (0x1288, RETURN_FALSE),
    (0x12A0, RETURN_FALSE),
    (0x12B8, RETURN_FALSE),
    (0x12E0, RETURN),
    (0x12EC, RETURN),
    (0x1320, RETURN),
    (0x1358, RETURN),
    (0x1374, RETURN),
    (0x1398, RETURN),
    (0x13C0, RETURN),
    (0x13E8, RETURN),
    (0x13F4, RETURN_FALSE),
    (0x140C, RETURN_FALSE),
    (0x1424, RETURN_FALSE),
    (0x144C, RETURN),
    (0x1458, RETURN),
    (0x1490, RETURN),
    (0x14F4, RETURN),
    (0x16E4, MISSING),
    (0x1730, MISSING),
    (0x1A38, WAIT_CYCLES),
    (0x1AC4, ENTER_CRITICAL),
    (0x1AD8, LEAVE_CRITICAL),
];

/// Copy a routine table into a ROM image.
pub fn install(rom: &mut [u8], table: &[(usize, &[u32])]) {
    for (offset, code) in table {
        for (n, word) in code.iter().enumerate() {
            let at = offset + n * 4;
            rom[at..at + 4].copy_from_slice(&word.to_le_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routines_do_not_overlap() {
        for table in [ARM9, ARM11] {
            let mut ends: Vec<(usize, usize)> = table
                .iter()
                .map(|(offset, code)| (*offset, offset + code.len() * 4))
                .collect();
            ends.sort_unstable();
            for pair in ends.windows(2) {
                assert!(
                    pair[0].1 <= pair[1].0,
                    "{:#x} runs into {:#x}",
                    pair[0].0,
                    pair[1].0
                );
            }
        }
    }
}
