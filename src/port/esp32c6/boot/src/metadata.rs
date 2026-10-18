// SPDX-License-Identifier: MIT
//! ESP-IDF OTA selection records. CRC covers only the sequence, as specified by
//! bootloader_common_ota_select_crc in ESP-IDF 5.5.4. Invalid/aborted records and
//! torn records must not supersede the metadata for the running confirmed image.

use super::{Error, SECTOR_SIZE};
use embedded_storage::nor_flash::{NorFlash, NorFlashError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum BootState {
    New = 0,
    PendingVerify = 1,
    Valid = 2,
    Invalid = 3,
    Aborted = 4,
    Undefined = u32::MAX,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Record {
    pub sequence: u32,
    pub state: BootState,
}

impl Record {
    pub fn decode(bytes: &[u8; 32]) -> Option<Self> {
        let sequence = u32::from_le_bytes(bytes[..4].try_into().unwrap());
        let state = match u32::from_le_bytes(bytes[24..28].try_into().unwrap()) {
            0 => BootState::New,
            1 => BootState::PendingVerify,
            2 => BootState::Valid,
            3 => BootState::Invalid,
            4 => BootState::Aborted,
            u32::MAX => BootState::Undefined,
            _ => return None,
        };
        let crc = u32::from_le_bytes(bytes[28..].try_into().unwrap());
        if sequence == 0 || sequence == u32::MAX || crc != sequence_crc(sequence) {
            return None;
        }
        Some(Self { sequence, state })
    }

    pub fn encode(self) -> [u8; 32] {
        let mut bytes = [0xff; 32];
        bytes[..4].copy_from_slice(&self.sequence.to_le_bytes());
        bytes[24..28].copy_from_slice(&(self.state as u32).to_le_bytes());
        bytes[28..].copy_from_slice(&sequence_crc(self.sequence).to_le_bytes());
        bytes
    }

    pub fn slot(self) -> usize {
        ((self.sequence - 1) % 2) as usize
    }
    fn bootable(self) -> bool {
        !matches!(self.state, BootState::Invalid | BootState::Aborted)
    }
}

pub(crate) fn sequence_crc(sequence: u32) -> u32 {
    // esp_rom_crc32_le(UINT32_MAX, sequence, 4): reflected CRC, init 0, xorout MAX.
    let mut crc = 0u32;
    for byte in sequence.to_le_bytes() {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

pub(crate) fn read<F: NorFlash>(flash: &mut F, offset: u32) -> Result<[Option<Record>; 2], Error> {
    let mut result = [None; 2];
    for (index, record) in result.iter_mut().enumerate() {
        let mut bytes = super::Aligned([0; 32]);
        flash
            .read(offset + index as u32 * SECTOR_SIZE, &mut bytes.0)
            .map_err(|e| Error::Flash(e.kind()))?;
        *record = Record::decode(&bytes.0);
    }
    Ok(result)
}

pub(crate) fn selected(records: &[Option<Record>; 2]) -> Option<usize> {
    let a = records[0].filter(|r| r.bootable());
    let b = records[1].filter(|r| r.bootable());
    match (a, b) {
        (Some(a), Some(b)) => Some(if a.sequence >= b.sequence { 0 } else { 1 }),
        (Some(_), None) => Some(0),
        (None, Some(_)) => Some(1),
        _ => None,
    }
}

pub(crate) fn next_sequence(records: &[Option<Record>; 2], slot: usize) -> Result<u32, Error> {
    let mut next = records
        .iter()
        .flatten()
        .map(|r| r.sequence)
        .max()
        .unwrap_or(0);
    loop {
        next = next
            .checked_add(1)
            .filter(|n| *n < u32::MAX)
            .ok_or(Error::SequenceExhausted)?;
        // An erased checksum must not accidentally make a partially written record valid.
        if (next - 1) as usize % 2 == slot && sequence_crc(next) != u32::MAX {
            return Ok(next);
        }
    }
}

pub(crate) fn write<F: NorFlash>(flash: &mut F, offset: u32, record: Record) -> Result<(), Error> {
    if sequence_crc(record.sequence) == u32::MAX {
        return Err(Error::SequenceExhausted);
    }
    let bytes = super::Aligned(record.encode());
    flash
        .erase(offset, offset + SECTOR_SIZE)
        .map_err(|e| Error::Flash(e.kind()))?;
    flash
        .write(offset, &bytes.0[..28])
        .map_err(|e| Error::Flash(e.kind()))?;
    // Commit the CRC after the sequence and state. The other metadata sector stays intact.
    flash
        .write(offset + 28, &bytes.0[28..])
        .map_err(|e| Error::Flash(e.kind()))
}
