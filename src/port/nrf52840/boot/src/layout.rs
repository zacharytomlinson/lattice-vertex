//! Flash geometry shared by the loader, kernel port, and their linker scripts.

pub const PAGE_SIZE: u32 = 4096;
pub const WRITE_SIZE: usize = 4;
pub const FLASH_END: u32 = 0x100000;
pub const RAM_START: u32 = 0x20000000;
pub const RAM_END: u32 = 0x20040000;

/// Page-aligned, disjoint regions in ascending flash order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub loader_start: u32,
    pub state_start: u32,
    pub active_start: u32,
    pub dfu_start: u32,
    pub end: u32,
}

impl Layout {
    pub const fn image_capacity(self) -> u32 {
        self.dfu_start - self.active_start
    }

    pub const fn valid(self) -> bool {
        self.loader_start < self.state_start
            && self.state_start < self.active_start
            && self.active_start < self.dfu_start
            && self.dfu_start < self.end
            && self.end <= FLASH_END
            && self.loader_start.is_multiple_of(PAGE_SIZE)
            && self.state_start.is_multiple_of(PAGE_SIZE)
            && self.active_start.is_multiple_of(PAGE_SIZE)
            && self.dfu_start.is_multiple_of(PAGE_SIZE)
            && self.end.is_multiple_of(PAGE_SIZE)
            && self.end - self.dfu_start >= self.image_capacity() + PAGE_SIZE
            && (2 + 4 * (self.image_capacity() / PAGE_SIZE)) * WRITE_SIZE as u32
                <= self.active_start - self.state_start
    }

    pub fn valid_vectors(self, image_len: u32, vectors: &[u8; 8]) -> bool {
        if !(8..=self.image_capacity()).contains(&image_len) {
            return false;
        }
        let sp = u32::from_le_bytes(vectors[..4].try_into().unwrap());
        let reset = u32::from_le_bytes(vectors[4..].try_into().unwrap());
        sp > RAM_START
            && sp <= RAM_END
            && sp.is_multiple_of(8)
            && reset & 1 == 1
            && (self.active_start + 8..self.active_start + image_len).contains(&(reset & !1))
    }
}

/// Preserve the factory MBR/SoftDevice below 0x27000 and recovery/storage at 0xED000.
/// Loader 32 KiB, boot state 4 KiB, active 376 KiB, DFU 380 KiB.
pub const XIAO: Layout = Layout {
    loader_start: 0x27000,
    state_start: 0x2f000,
    active_start: 0x30000,
    dfu_start: 0x8e000,
    end: 0xed000,
};

/// The factory loader checks this many bytes from 0x27000 on every reset.
/// Only a valid application with a disabled CRC or an immutable-loader-only
/// span may subsequently change the active image and boot state.
pub fn xiao_factory_crc_allows_updates(settings: &[u8; 12]) -> bool {
    let valid = u16::from_le_bytes(settings[..2].try_into().unwrap()) == 1;
    let crc = u16::from_le_bytes(settings[2..4].try_into().unwrap());
    let length = u32::from_le_bytes(settings[8..12].try_into().unwrap());
    valid && (crc == 0 || (8..=XIAO.state_start - XIAO.loader_start).contains(&length))
}

const _: () = assert!(XIAO.valid());
