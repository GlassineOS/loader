// SPDX-License-Identifier: GPL-2.0-or-later
//! The firmware watchdog while the loader waits under a parent (loader spec 004
//! FR-012): re-armed for 30 s before each refresh and at least every 10 s, left armed
//! for 30 s when the loader answers, never disabled, so a hang inside the loader still
//! ends in a reset.
use glassine_loader_core::menu::WATCHDOG_S;

/// The watchdog code: above the range UEFI reserves for the firmware (0 to 0xFFFF).
const CODE: u64 = 0x1_0000;

pub fn arm() {
    if let Err(e) = uefi::boot::set_watchdog_timer(WATCHDOG_S, CODE, None) {
        crate::log!("WATCHDOG not armed: {:?}", e.status());
    }
}
