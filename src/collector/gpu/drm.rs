//! Small read-only DRM queries, matching Linux UAPI (amdgpu_drm/i915_drm/xe_drm).
use super::*;
use std::{fs::OpenOptions, os::fd::AsRawFd};

#[repr(C)]
struct AmdInfo {
    pointer: u64,
    size: u32,
    query: u32,
    parameters: [u32; 4],
}
#[repr(C)]
struct I915Item {
    query: u64,
    length: i32,
    flags: u32,
    pointer: u64,
}
#[repr(C)]
struct I915Query {
    count: u32,
    flags: u32,
    pointer: u64,
}
#[repr(C)]
struct XeQuery {
    extensions: u64,
    query: u32,
    size: u32,
    pointer: u64,
    reserved: [u64; 2],
}

fn ioctl<T>(file: &fs::File, number: u32, write_only: bool, value: &mut T) -> Result<(), GpuError> {
    #[cfg(any(
        target_arch = "x86_64",
        target_arch = "x86",
        target_arch = "aarch64",
        target_arch = "arm",
        target_arch = "riscv64"
    ))]
    {
        let direction = if write_only { 1u32 } else { 3u32 };
        let request = (direction << 30)
            | ((std::mem::size_of::<T>() as u32) << 16)
            | (u32::from(b'd') << 8)
            | number;
        // SAFETY: each caller supplies the exact repr(C) UAPI argument; all pointed-to
        // buffers stay allocated for the call, and the kernel receives their byte lengths.
        if unsafe { libc::ioctl(file.as_raw_fd(), request as libc::c_ulong, value) } < 0 {
            let e = std::io::Error::last_os_error();
            return Err(match e.raw_os_error() {
                Some(libc::ENOTTY | libc::EINVAL | libc::ENODEV) => GpuError::NotSupported,
                _ => e.into(),
            });
        }
        Ok(())
    }
    #[cfg(not(any(
        target_arch = "x86_64",
        target_arch = "x86",
        target_arch = "aarch64",
        target_arch = "arm",
        target_arch = "riscv64"
    )))]
    {
        let _ = (file, number, write_only, value);
        Err(GpuError::NotSupported)
    }
}

fn open(cached: &CachedGpu) -> Result<fs::File, GpuError> {
    let node = cached.render.as_ref().ok_or(GpuError::NotAvailable)?;
    let file = OpenOptions::new()
        .read(true)
        .open(node)
        .map_err(GpuError::from)?;
    // A render node number can be reassigned after unplug. Check the opened fd,
    // not only the pathname, before issuing any vendor-specific query.
    let expected = text(
        &cached
            .path
            .join("drm")
            .join(node.file_name().ok_or(GpuError::InvalidData)?)
            .join("dev"),
    )?;
    let (major, minor) = expected.split_once(':').ok_or(GpuError::InvalidData)?;
    let actual = file.metadata().map_err(GpuError::from)?.rdev();
    if major.parse::<u32>().ok() != Some(libc::major(actual))
        || minor.parse::<u32>().ok() != Some(libc::minor(actual))
    {
        return Err(GpuError::NotAvailable);
    }
    Ok(file)
}
fn size(value: usize) -> Result<(), GpuError> {
    if (8..=65536).contains(&value) {
        Ok(())
    } else {
        Err(GpuError::InvalidData)
    }
}
fn u32_at(bytes: &[u8], at: usize) -> Result<u32, GpuError> {
    Ok(u32::from_ne_bytes(
        bytes
            .get(at..at + 4)
            .ok_or(GpuError::InvalidData)?
            .try_into()
            .unwrap(),
    ))
}
fn u64_at(bytes: &[u8], at: usize) -> Result<u64, GpuError> {
    Ok(u64::from_ne_bytes(
        bytes
            .get(at..at + 8)
            .ok_or(GpuError::InvalidData)?
            .try_into()
            .unwrap(),
    ))
}

pub(super) fn xe_query(cached: &CachedGpu, query: u32) -> Result<Vec<u8>, GpuError> {
    let file = open(cached)?;
    let mut query = XeQuery {
        extensions: 0,
        query,
        size: 0,
        pointer: 0,
        reserved: [0; 2],
    };
    ioctl(&file, 0x40, false, &mut query)?;
    size(query.size as usize)?;
    let mut bytes = vec![0; query.size as usize];
    let capacity = query.size;
    query.pointer = bytes.as_mut_ptr() as u64;
    ioctl(&file, 0x40, false, &mut query)?;
    if query.size > capacity {
        return Err(GpuError::InvalidData);
    }
    bytes.truncate(query.size as usize);
    Ok(bytes)
}
fn i915_regions(cached: &CachedGpu) -> Result<Vec<u8>, GpuError> {
    let file = open(cached)?;
    let mut item = I915Item {
        query: 4,
        length: 0,
        flags: 0,
        pointer: 0,
    };
    let mut query = I915Query {
        count: 1,
        flags: 0,
        pointer: std::ptr::from_mut(&mut item) as u64,
    };
    ioctl(&file, 0x79, false, &mut query)?;
    if item.length < 0 {
        return Err(GpuError::NotSupported);
    }
    size(item.length as usize)?;
    let mut bytes = vec![0; item.length as usize];
    item.pointer = bytes.as_mut_ptr() as u64;
    ioctl(&file, 0x79, false, &mut query)?;
    if item.length < 0 || item.length as usize > bytes.len() {
        return Err(GpuError::InvalidData);
    }
    Ok(bytes)
}
fn amd_apu(cached: &CachedGpu) -> Result<bool, GpuError> {
    let file = open(cached)?;
    let mut data = [0u64; 64];
    let mut query = AmdInfo {
        pointer: data.as_mut_ptr() as u64,
        size: 512,
        query: 0x16,
        parameters: [0; 4],
    };
    ioctl(&file, 0x45, true, &mut query)?;
    // ids_flags is at byte offset 136 in the stable drm_amdgpu_info_device prefix.
    Ok(data[17] & 1 != 0) // AMDGPU_IDS_FLAGS_FUSION
}

fn region_values(bytes: &[u8], xe: bool) -> Result<(bool, u64, u64), GpuError> {
    let (total, used) = region_class(bytes, xe, 1)?;
    Ok((total > 0, total, used))
}

fn region_class(bytes: &[u8], xe: bool, wanted: u16) -> Result<(u64, u64), GpuError> {
    let count = u32_at(bytes, 0)? as usize;
    let header = if xe { 8 } else { 16 };
    if count == 0 || count > 256 || bytes.len() < header + count * 88 {
        return Err(GpuError::InvalidData);
    }
    let mut total = 0u64;
    let mut used = 0u64;
    for entry in bytes[header..header + count * 88].as_chunks::<88>().0 {
        let class = u16::from_ne_bytes(entry[..2].try_into().unwrap());
        if class == wanted {
            let capacity = u64_at(entry, 8)?;
            let busy = u64_at(entry, 16)?;
            if capacity == 0 || busy > capacity {
                return Err(GpuError::InvalidData);
            }
            total = total.checked_add(capacity).ok_or(GpuError::InvalidData)?;
            used = used
                .checked_add(if xe { busy } else { capacity - busy })
                .ok_or(GpuError::InvalidData)?;
        }
    }
    Ok((total, used))
}

pub(super) fn inventory(cached: &mut CachedGpu) {
    let driver = cached
        .raw
        .metadata
        .get("driver")
        .map(String::as_str)
        .unwrap_or_default();
    let result = match driver {
        "amdgpu" => amd_apu(cached).map(|apu| !apu),
        "i915" => i915_regions(cached)
            .and_then(|data| region_values(&data, false))
            .map(|(dedicated, _, _)| dedicated),
        "xe" => xe_query(cached, 1)
            .and_then(|data| region_values(&data, true))
            .map(|(dedicated, _, _)| dedicated),
        _ => return,
    };
    match result {
        Ok(dedicated) => {
            cached.raw.metadata.insert(
                "memory_kind".into(),
                if dedicated { "dedicated" } else { "shared" }.into(),
            );
            cached.raw.metadata.insert(
                "memory_kind_source".into(),
                "DRM device/memory query".into(),
            );
            cached.raw.metadata.remove("drm_status");
        }
        Err(e) => {
            cached
                .raw
                .metadata
                .insert("drm_status".into(), format!("{e:?}"));
        }
    }
}

fn perfmon_capability() -> bool {
    text(Path::new("/proc/self/status"))
        .ok()
        .and_then(|data| {
            data.lines().find_map(|line| {
                line.strip_prefix("CapEff:")
                    .and_then(|s| u64::from_str_radix(s.trim(), 16).ok())
            })
        })
        .is_some_and(|bits| bits & ((1 << 38) | (1 << 21)) != 0)
}

pub(super) fn sample(cached: &mut CachedGpu) {
    let driver = cached
        .raw
        .metadata
        .get("driver")
        .map(String::as_str)
        .unwrap_or_default();
    let xe = driver == "xe";
    if !xe && driver != "i915" {
        return;
    }
    let result = if xe {
        xe_query(cached, 1)
    } else {
        i915_regions(cached)
    };
    match result.and_then(|data| Ok((region_values(&data, xe)?, region_class(&data, xe, 0)?))) {
        Ok(((dedicated, local_total, local_used), (shared_total, shared_used))) => {
            let (total, used) = if dedicated {
                (local_total, local_used)
            } else {
                (shared_total, shared_used)
            };
            cached.raw.metadata.insert(
                "memory_kind".into(),
                if dedicated { "dedicated" } else { "shared" }.into(),
            );
            put(
                &mut cached.raw,
                "memory_total",
                GpuUnit::Byte,
                Ok(total.to_string()),
                "DRM memory regions",
            );
            // i915 returns all-free accounting without CAP_PERFMON; xe reports an estimate directly.
            put(
                &mut cached.raw,
                "memory_used",
                GpuUnit::Byte,
                if xe {
                    Ok(used.to_string())
                } else if !dedicated {
                    Err(GpuError::NotSupported)
                } else if !perfmon_capability() {
                    Err(GpuError::PermissionDenied)
                } else if used == 0 {
                    Err(GpuError::NotAvailable)
                } else {
                    Ok(used.to_string())
                },
                if xe {
                    "DRM xe memory usage estimate"
                } else {
                    "DRM i915 device usage requires CAP_PERFMON; all-free stub not interpreted"
                },
            );
            if dedicated && shared_total > 0 {
                put(
                    &mut cached.raw,
                    "shared_total",
                    GpuUnit::Byte,
                    Ok(shared_total.to_string()),
                    "DRM system-memory region budget",
                );
                if xe {
                    put(
                        &mut cached.raw,
                        "shared_used",
                        GpuUnit::Byte,
                        Ok(shared_used.to_string()),
                        "DRM xe shared-memory usage estimate",
                    );
                }
            }
        }
        Err(e) => {
            put(
                &mut cached.raw,
                "memory_total",
                GpuUnit::Byte,
                Err(e),
                "DRM memory regions",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn drm_layouts_and_memory_regions_are_bounded_and_not_bar_sizes() {
        assert_eq!(std::mem::size_of::<AmdInfo>(), 32);
        assert_eq!(std::mem::size_of::<I915Item>(), 24);
        assert_eq!(std::mem::size_of::<I915Query>(), 16);
        assert_eq!(std::mem::size_of::<XeQuery>(), 40);
        let mut bytes = vec![0; 96];
        bytes[..4].copy_from_slice(&1u32.to_ne_bytes());
        bytes[8..10].copy_from_slice(&1u16.to_ne_bytes());
        bytes[16..24].copy_from_slice(&1024u64.to_ne_bytes());
        bytes[24..32].copy_from_slice(&256u64.to_ne_bytes());
        assert_eq!(region_values(&bytes, true), Ok((true, 1024, 256)));
        assert!(region_values(&bytes[..32], true).is_err());
        bytes[8..10].copy_from_slice(&0u16.to_ne_bytes());
        assert_eq!(region_values(&bytes, true), Ok((false, 0, 0)));
    }
}
