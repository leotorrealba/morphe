//! morphe-kernel: core geometry kernel for Morphe.
//!
//! Wraps the [manifold3d](https://github.com/elalish/manifold) CSG engine
//! behind the [`Solid`] newtype, and builds primitive tessellation, boolean
//! evaluation, transforms, and STL export on top of it.

use std::path::Path;

pub mod import;
pub mod profile;
pub mod resolve;
pub mod solid;
pub mod stats;
pub mod stl;
pub mod surface;
pub mod tessellate;
pub mod text;

pub use profile::{FillRule, Profile};
pub use resolve::{resolve, resolve_root};
pub use solid::{Solid, SolidError};
pub use stats::{geometry_stats, profile_stats, GeometryStats, ProfileStats};
pub use stl::{to_ascii_stl, to_binary_stl, write_ascii_stl, write_binary_stl};
pub use tessellate::{
    apply_transform, difference, difference_profiles, hull, hull_profiles, intersection,
    intersection_profiles, linear_extrude, minkowski, minkowski_profiles, offset_profile,
    project_solid, rotate_extrude, tessellate_primitive, union, union_profiles, Geometry,
    KernelError,
};

/// Read a whole file for a file-loading construct (`op`), refusing anything
/// but a regular file and anything over `max` bytes, with a loud
/// `KernelError` naming the construct, the path, and the cap (never any file
/// content). ONE tested helper enforces BOTH caps (a declared cap with no
/// enforcement and no test is worthless). Used by
/// [`crate::import::load_stl`] (MAX_STL_BYTES) and the [`crate::surface`]
/// resolve arm (MAX_DAT_BYTES).
///
/// Replaces the old stat-then-`fs::read` pair, which had two holes: a
/// character device reports `len() == 0` (`surface("/dev/zero")` passed the
/// size check, then read until OOM) and the file could change between the stat
/// and the reopen. Now:
///
/// 1. a path `stat` rejects a non-regular target BEFORE opening it (opening
///    a FIFO for reading would block until a writer appears);
/// 2. the file is opened ONCE and the handle's own metadata must again be a
///    regular file (the object actually being read);
/// 3. the read is bounded by `take(max + 1)` — a file that grew past the
///    stat, or lied about its size, fails as over-cap instead of being read
///    unbounded.
pub fn read_capped(path: &Path, op: &str, max: u64) -> Result<Vec<u8>, KernelError> {
    use std::io::Read;
    let not_regular =
        || KernelError::Resolve(format!("`{op}` `{}` is not a regular file", path.display()));
    let over_cap = |len: Option<u64>| {
        KernelError::Resolve(match len {
            Some(len) => format!("`{op}` `{}` is {len} bytes (cap {max})", path.display()),
            None => format!(
                "`{op}` `{}` exceeds {max} bytes (cap {max})",
                path.display()
            ),
        })
    };
    let stat = std::fs::metadata(path).map_err(|e| {
        KernelError::Resolve(format!("`{op}` cannot stat `{}`: {e}", path.display()))
    })?;
    if !stat.is_file() {
        return Err(not_regular());
    }
    let file = std::fs::File::open(path).map_err(|e| {
        KernelError::Resolve(format!("`{op}` cannot read `{}`: {e}", path.display()))
    })?;
    let meta = file.metadata().map_err(|e| {
        KernelError::Resolve(format!("`{op}` cannot stat `{}`: {e}", path.display()))
    })?;
    if !meta.is_file() {
        return Err(not_regular());
    }
    if meta.len() > max {
        return Err(over_cap(Some(meta.len())));
    }
    // `meta.len() <= max` here, so the capacity hint is bounded by the cap.
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    file.take(max.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| {
            KernelError::Resolve(format!("`{op}` cannot read `{}`: {e}", path.display()))
        })?;
    if bytes.len() as u64 > max {
        return Err(over_cap(None));
    }
    Ok(bytes)
}
