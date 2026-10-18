//! The two pages the kernel shows every process read-only (3dbrew,
//! "Configuration Memory"): configuration memory at 0x1FF80000 (versions and
//! memory region sizes) and the shared page at 0x1FF81000 (the clock, the
//! hardware it runs on, the 3D slider, the wireless signal).
//!
//! The versions are those of a recent Old 3DS. The clock starts at a fixed
//! moment, 2020-01-01 00:00 UTC (the low-level machine's MCU clock), and
//! advances with emulated time only, so runs repeat exactly.

use ctr_core::clock::ARM11_HZ;

pub const CONFIG_MEMORY: u32 = 0x1FF8_0000;
pub const SHARED_PAGE: u32 = 0x1FF8_1000;

/// Kernel and firmware 2.52-0: Citra's choice, which titles accept.
const KERNEL_VERSION: [u8; 3] = [0, 0x34, 2];
const CTR_SDK_VERSION: u32 = 0x0000_F297;
/// NS, the title that would have started the program.
const NS_TITLE: u64 = 0x0004_0130_0000_8002;
/// The HOME Menu of the USA region.
const MENU_TITLE: u64 = 0x0004_0030_0000_8F02;
/// FCRAM regions the application's does not take: system, then base.
const FCRAM_LEN: u32 = 0x0800_0000;
const BASE_REGION: u32 = 0x0140_0000;
/// Milliseconds from 1900-01-01 to 2020-01-01: 43,829 days.
pub const RTC_BASE_MS: u64 = 43_829 * 86_400 * 1000;

fn put(page: &mut [u8], at: usize, bytes: &[u8]) {
    page[at..at + bytes.len()].copy_from_slice(bytes);
}

/// Configuration memory for an application in `system_mode` with a region
/// of `app_memory` bytes.
pub fn config_memory(system_mode: u8, app_memory: u32) -> Vec<u8> {
    let mut page = vec![0u8; 0x1000];
    // Kernel version: revision, minor, major at 0x01-0x03.
    put(&mut page, 0x01, &KERNEL_VERSION);
    put(&mut page, 0x08, &NS_TITLE.to_le_bytes());
    // NATIVE_FIRM.
    put(&mut page, 0x10, &2u32.to_le_bytes());
    // A retail unit, started cold.
    page[0x14] = 1;
    page[0x15] = 1;
    page[0x16] = 1;
    put(&mut page, 0x18, &CTR_SDK_VERSION.to_le_bytes());
    put(&mut page, 0x30, &(system_mode as u32).to_le_bytes());
    put(&mut page, 0x40, &app_memory.to_le_bytes());
    let system = FCRAM_LEN.saturating_sub(app_memory + BASE_REGION);
    put(&mut page, 0x44, &system.to_le_bytes());
    put(&mut page, 0x48, &BASE_REGION.to_le_bytes());
    put(&mut page, 0x61, &KERNEL_VERSION);
    put(&mut page, 0x64, &2u32.to_le_bytes());
    put(&mut page, 0x68, &CTR_SDK_VERSION.to_le_bytes());
    page
}

/// The shared page at ARM11 cycle `now`.
pub fn shared_page(now: u64) -> Vec<u8> {
    let mut page = vec![0u8; 0x1000];
    // DATETIME_0 is the one to read.
    put(&mut page, 0x00, &0u32.to_le_bytes());
    // A retail unit.
    page[0x04] = 1;
    update_clock(&mut page, now);
    // No wireless network; the 3D slider down, its light off; the battery
    // light on; PTM has written its hardware information.
    page[0x66] = 0;
    page[0x67] = 7;
    put(&mut page, 0x80, &0u32.to_le_bytes());
    page[0x84] = 0;
    page[0x85] = 1;
    page[0x86] = 1;
    put(&mut page, 0xA0, &MENU_TITLE.to_le_bytes());
    put(&mut page, 0xA8, &MENU_TITLE.to_le_bytes());
    page
}

/// DATETIME_0: milliseconds since 1900 and the tick they were taken at.
pub fn update_clock(page: &mut [u8], now: u64) {
    let ms = RTC_BASE_MS + (now as u128 * 1000 / ARM11_HZ as u128) as u64;
    put(page, 0x20, &ms.to_le_bytes());
    put(page, 0x28, &now.to_le_bytes());
    put(page, 0x30, &0x0FFB_0FF0u32.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_at(page: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(page[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn the_regions_add_up_to_fcram() {
        let page = config_memory(3, 80 << 20);
        assert_eq!(page[0x01..0x04], [0, 0x34, 2]);
        assert_eq!(u32_at(&page, 0x30), 3);
        let regions: u32 = [0x40, 0x44, 0x48].iter().map(|&at| u32_at(&page, at)).sum();
        assert_eq!(regions, FCRAM_LEN);
        assert_eq!(u32_at(&page, 0x40), 80 << 20);
    }

    #[test]
    fn the_clock_starts_in_2020_and_follows_emulated_time() {
        let mut page = shared_page(0);
        let ms = |page: &[u8]| u64::from_le_bytes(page[0x20..0x28].try_into().unwrap());
        assert_eq!(ms(&page), RTC_BASE_MS);
        update_clock(&mut page, ARM11_HZ as u64 * 3);
        assert_eq!(ms(&page), RTC_BASE_MS + 3000);
        // 1900 to 2020 is 120 years with 29 leap days.
        assert_eq!(RTC_BASE_MS / 86_400_000, 120 * 365 + 29);
    }
}
