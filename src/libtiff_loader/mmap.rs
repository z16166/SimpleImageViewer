// along with this program.  If not, see <https://www.gnu.org/licenses/>.
use libtiff_viewer as lib;
use memmap2::Mmap;
use std::ffi::{c_int, c_void};
use std::sync::Arc;

/// Context passed to libtiff callbacks
pub(crate) struct TiffMmapContext {
    pub(crate) mmap: Arc<Mmap>,
    pub(crate) offset: u64,
}

// --- libtiff Callbacks over memmap2::Mmap ---

pub(crate) unsafe extern "C" fn tiff_read_proc(
    handle: *mut c_void,
    buf: *mut c_void,
    size: lib::tsize_t,
) -> lib::tsize_t {
    let ctx = unsafe { &mut *(handle as *mut TiffMmapContext) };
    let mmap_len = ctx.mmap.len() as u64;

    if ctx.offset >= mmap_len {
        return 0;
    }

    let rem = mmap_len - ctx.offset;
    let to_read = (size as u64).min(rem);

    if to_read > 0 {
        unsafe {
            std::ptr::copy_nonoverlapping(
                ctx.mmap.as_ptr().add(ctx.offset as usize),
                buf as *mut u8,
                to_read as usize,
            );
        }
        ctx.offset += to_read;
    }
    to_read as lib::tsize_t
}

pub(crate) unsafe extern "C" fn tiff_write_proc(
    _: *mut c_void,
    _: *mut c_void,
    _: lib::tsize_t,
) -> lib::tsize_t {
    0
}

pub(crate) unsafe extern "C" fn tiff_seek_proc(
    handle: *mut c_void,
    off: lib::toff_t,
    whence: c_int,
) -> lib::toff_t {
    let ctx = unsafe { &mut *(handle as *mut TiffMmapContext) };
    let mmap_len = ctx.mmap.len() as u64;
    let len_i64 = ctx.mmap.len() as i64;
    match whence {
        0 => ctx.offset = off.min(mmap_len), // SEEK_SET
        1 => {
            // SEEK_CUR: interpret `off` as signed per libtiff conventions (see `toff_t as i64`).
            let next = (ctx.offset as i64)
                .saturating_add(off as i64)
                .clamp(0, len_i64);
            ctx.offset = next as u64;
        }
        2 => {
            // SEEK_END
            let next = len_i64.saturating_add(off as i64).clamp(0, len_i64);
            ctx.offset = next as u64;
        }
        _ => {}
    }
    ctx.offset
}

pub(crate) unsafe extern "C" fn tiff_close_proc(_: *mut c_void) -> c_int {
    0
}

pub(crate) unsafe extern "C" fn tiff_size_proc(handle: *mut c_void) -> lib::toff_t {
    let ctx = unsafe { &*(handle as *const TiffMmapContext) };
    ctx.mmap.len() as u64
}

pub(crate) unsafe extern "C" fn tiff_map_proc(
    handle: *mut c_void,
    base: *mut *mut c_void,
    size: *mut lib::toff_t,
) -> c_int {
    let ctx = unsafe { &*(handle as *const TiffMmapContext) };
    unsafe {
        *base = ctx.mmap.as_ptr() as *mut c_void;
        *size = ctx.mmap.len() as u64;
    }
    1
}

pub(crate) unsafe extern "C" fn tiff_unmap_proc(_: *mut c_void, _: *mut c_void, _: lib::toff_t) {}
