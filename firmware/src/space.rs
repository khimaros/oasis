//! how full the flash partitions and the heap are, for the status page.

use esp_idf_svc::sys::{
    MALLOC_CAP_DEFAULT, esp, esp_get_free_heap_size, esp_image_get_metadata, esp_image_metadata_t,
    esp_littlefs_info, esp_ota_get_running_partition, esp_partition_pos_t, heap_caps_get_total_size,
    nvs_get_stats, nvs_stats_t,
};
use oasis_portal::Space;
use std::ffi::CString;
use std::ptr;
use std::sync::OnceLock;

const NVS_ENTRY_BYTES: u64 = 32;

/// bytes of the running firmware image, and of the partition it is in
static FIRMWARE: OnceLock<(u64, u64)> = OnceLock::new();

/// measures the firmware image. this reads its headers from flash, so it
/// is done once at startup instead of on every status request.
pub fn measure_firmware() {
    let running = unsafe { esp_ota_get_running_partition() };
    if running.is_null() {
        return;
    }
    let position = unsafe { esp_partition_pos_t { offset: (*running).address, size: (*running).size } };
    let mut image = esp_image_metadata_t::default();
    if esp!(unsafe { esp_image_get_metadata(&position, &mut image) }).is_ok() {
        let _ = FIRMWARE.set((u64::from(image.image_len), u64::from(position.size)));
    }
}

fn storage() -> Option<Space> {
    let label = CString::new(crate::STORAGE_LABEL).ok()?;
    let (mut total, mut used) = (0usize, 0usize);
    esp!(unsafe { esp_littlefs_info(label.as_ptr(), &mut total, &mut used) }).ok()?;
    Some(Space { name: "storage", used: used as u64, size: total as u64 })
}

/// the key value store that the wifi driver keeps its calibration in.
fn settings() -> Option<Space> {
    let mut stats = nvs_stats_t::default();
    esp!(unsafe { nvs_get_stats(ptr::null(), &mut stats) }).ok()?;
    let bytes = |entries: usize| entries as u64 * NVS_ENTRY_BYTES;
    Some(Space { name: "settings", used: bytes(stats.used_entries), size: bytes(stats.total_entries) })
}

fn memory() -> Space {
    let total = unsafe { heap_caps_get_total_size(MALLOC_CAP_DEFAULT) } as u64;
    let free = u64::from(unsafe { esp_get_free_heap_size() });
    Space { name: "memory", used: total.saturating_sub(free), size: total }
}

/// the partitions of the flash, and the heap.
pub fn space() -> Vec<Space> {
    let firmware = FIRMWARE.get().map(|(used, size)| Space { name: "firmware", used: *used, size: *size });
    [firmware, storage(), settings(), Some(memory())].into_iter().flatten().collect()
}
