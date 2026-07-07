use super::types::{ImageLoader, TileRequest};

use crate::loader::{DecodeProfile, TileDecodeSource};

impl ImageLoader {
    pub fn request_tile(
        &self,
        index: usize,
        decode_profile: DecodeProfile,
        priority: usize,
        source: TileDecodeSource,
        col: u32,
        row: u32,
    ) -> bool {
        if col >= source.tile_cols() || row >= source.tile_rows() {
            return false;
        }

        let (lock, cvar) = &*self.tile_queue;
        let mut heap = lock.lock();
        heap.push(TileRequest {
            profile_epoch: decode_profile.profile_epoch,
            priority,
            index,
            col,
            row,
            source,
        });
        cvar.notify_one();
        true
    }
}
