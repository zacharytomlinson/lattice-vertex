//! Typed appliance settings share the protected configuration image and flash
//! owner. They are neither program fragments nor externally exposed states.
use super::*;
use crate::ir::{Type, Value};
use lattice_peer_wire::scalar;
const BYTES: usize = 36;
fn setting(bytes: &[u8], e: image::Extent) -> Result<(u64, Type, Option<Value>), Status> {
    let b = &bytes[e.at..e.at + e.span];
    if e.span != BYTES || e.flags != 0 || b[12] > 1 || b[13..PREFIX].iter().any(|b| *b != 0) {
        return Err(Status::Invalid);
    }
    let key = u64::from_le_bytes(b[4..12].try_into().unwrap());
    let ty = scalar::decode_type(b[PREFIX]).ok_or(Status::Invalid)?;
    let (value, used) = if b[12] == 1 {
        let (value, n) = scalar::decode_value(&b[PREFIX..]).ok_or(Status::Invalid)?;
        (Some(value), n)
    } else {
        (None, 1)
    };
    if key == 0 || b[PREFIX + used..].iter().any(|b| *b != 0) {
        return Err(Status::Invalid);
    }
    Ok((key, ty, value))
}
fn record(key: u64, ty: Type, value: Option<Value>) -> [u8; BYTES] {
    let mut b = [0; BYTES];
    b[..2].copy_from_slice(&(BYTES as u16).to_le_bytes());
    b[3] = image::SETTING;
    b[4..12].copy_from_slice(&key.to_le_bytes());
    b[12] = u8::from(value.is_some());
    b[PREFIX] = scalar::type_tag(ty);
    if let Some(value) = value {
        assert_eq!(value.ty(), ty);
        scalar::encode_value(value, &mut b[PREFIX..]);
    }
    b
}
impl Configuration {
    pub(super) fn restore_settings(&self, native: &dyn Native) -> Result<(), Status> {
        let bytes = &self.bytes[..self.used as usize];
        for e in image::extents(bytes) {
            let e = e.map_err(status)?;
            if e.kind != image::SETTING {
                continue;
            }
            let (key, _, _) = setting(bytes, e)?;
            for prior in image::extents(&bytes[..e.at])
                .filter_map(Result::ok)
                .filter(|p| p.kind == image::SETTING)
            {
                if setting(bytes, prior)?.0 == key {
                    return Err(Status::Invalid);
                }
            }
        }
        for e in image::extents(bytes)
            .filter_map(Result::ok)
            .filter(|e| e.kind == image::SETTING)
        {
            let (key, ty, value) = setting(bytes, e)?;
            native
                .restore_setting(key, ty, value)
                .map_err(|_| Status::Binding)?;
        }
        Ok(())
    }
    /// Called only between existing storage operations. Values are independent
    /// settings: a change during the copy leaves the dirty flag set for another
    /// snapshot. No global atomic snapshot or transition history is implied.
    pub(super) fn checkpoint_settings(&mut self, native: &dyn Native) -> Result<(), Status> {
        if self.pending.is_some() || self.checkpoint_pending.is_some() {
            return Err(Status::Busy);
        }
        if !self.configuration_ready() {
            return Err(self.state);
        }
        let capacity = self
            .store
            .as_ref()
            .ok_or(Status::Unsupported)?
            .capacity()
            .min(IMAGE_CAPACITY as u32) as usize;
        let mut missing = 0;
        native.settings(&mut |key, _, _| {
            if !image::extents(&self.bytes[..self.used as usize])
                .filter_map(Result::ok)
                .any(|e| {
                    e.kind == image::SETTING && setting(&self.bytes, e).is_ok_and(|s| s.0 == key)
                })
            {
                missing += 1;
            }
        });
        if self.used as usize + missing * BYTES > capacity {
            return Err(Status::Bounds);
        }
        native.settings(&mut |key, ty, value| {
            let at = image::extents(&self.bytes[..self.used as usize])
                .filter_map(Result::ok)
                .find(|e| {
                    e.kind == image::SETTING && setting(&self.bytes, *e).is_ok_and(|s| s.0 == key)
                })
                .map(|e| e.at);
            let at = at.unwrap_or_else(|| {
                let at = self.used as usize;
                self.used += BYTES as u16;
                at
            });
            self.bytes[at..at + BYTES].copy_from_slice(&record(key, ty, value));
        });
        self.checkpoint(None)
    }
}
