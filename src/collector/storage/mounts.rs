use super::*;
use std::{ffi::CString, mem::MaybeUninit, os::unix::ffi::OsStrExt};

fn unescape(text: &str) -> Result<String, InterfaceStatus> {
    // Same octal pathname encoding as /proc/swaps; the helper performs no I/O.
    crate::collector::memory::unescape_path(text)
        .ok_or_else(|| InterfaceStatus::ParseError("invalid mountinfo path escape".into()))
}

fn filesystem_stats(path: &Path) -> Result<RawFsStat, InterfaceStatus> {
    // libc field widths vary between 32-bit and 64-bit Linux ABIs.
    fn widen(value: impl Into<u64>) -> u64 {
        value.into()
    }
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|error| InterfaceStatus::ParseError(error.to_string()))?;
    let mut output = MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: path is NUL terminated and output points to writable statvfs storage.
    if unsafe { libc::statvfs(path.as_ptr(), output.as_mut_ptr()) } != 0 {
        return Err(status(io::Error::last_os_error()));
    }
    // SAFETY: statvfs initialized output after returning success.
    let output = unsafe { output.assume_init() };
    Ok(RawFsStat {
        fragment_size: widen(output.f_frsize),
        blocks: widen(output.f_blocks),
        free: widen(output.f_bfree),
        available: widen(output.f_bavail),
    })
}

fn data_fs(kind: &str) -> bool {
    !matches!(
        kind,
        "proc"
            | "sysfs"
            | "devtmpfs"
            | "tmpfs"
            | "ramfs"
            | "devpts"
            | "cgroup"
            | "cgroup2"
            | "debugfs"
            | "tracefs"
            | "securityfs"
            | "pstore"
            | "mqueue"
            | "hugetlbfs"
            | "fusectl"
            | "configfs"
            | "efivarfs"
            | "nsfs"
            | "autofs"
            | "binfmt_misc"
            | "rpc_pipefs"
            | "bpf"
    )
}

pub(super) fn parse(text: &str) -> Result<Vec<RawMount>, InterfaceStatus> {
    let mut mounts = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let parts: Vec<_> = line.split_whitespace().collect();
        let delimiter = parts
            .iter()
            .position(|p| *p == "-")
            .ok_or_else(|| InterfaceStatus::ParseError("mountinfo: missing separator".into()))?;
        if delimiter < 6
            || parts.len() != delimiter + 4
            || parts[0].parse::<u64>().is_err()
            || parts[1].parse::<u64>().is_err()
        {
            return Err(InterfaceStatus::ParseError(
                "invalid mountinfo record".into(),
            ));
        }
        let Some((major, minor)) = parts[2].split_once(':') else {
            return Err(InterfaceStatus::ParseError("invalid mount device".into()));
        };
        if major.parse::<u32>().is_err() || minor.parse::<u32>().is_err() {
            return Err(InterfaceStatus::ParseError("invalid mount device".into()));
        }
        let kind = parts[delimiter + 1];
        if !data_fs(kind) {
            continue;
        }
        mounts.push(RawMount {
            id: parts[0].into(),
            major_minor: parts[2].into(),
            root: unescape(parts[3])?,
            target: unescape(parts[4])?,
            options: parts[5].into(),
            fs_type: kind.into(),
            source: unescape(parts[delimiter + 2])?,
            stats: Err(InterfaceStatus::NotAvailable),
        });
    }
    Ok(mounts)
}

pub(super) fn collect(path: &Path) -> Result<Vec<RawMount>, InterfaceStatus> {
    let mut mounts = parse(&read(path)?)?;
    let mut cache: BTreeMap<(String, String, String, String), RawFsStat> = BTreeMap::new();
    for mount in &mut mounts {
        // Listing remote mounts is safe; do not risk an uninterruptible NFS/FUSE
        // capacity call just because the remote server has stopped responding.
        if matches!(
            mount.fs_type.as_str(),
            "nfs" | "nfs4" | "cifs" | "smb3" | "9p" | "ceph"
        ) || mount.fs_type.starts_with("fuse.")
        {
            mount.stats = Err(InterfaceStatus::NotAvailable);
            continue;
        }
        let key = (
            mount.major_minor.clone(),
            mount.root.clone(),
            mount.source.clone(),
            mount.fs_type.clone(),
        );
        mount.stats = if let Some(stats) = cache.get(&key) {
            Ok(stats.clone())
        } else {
            filesystem_stats(Path::new(&mount.target))
        };
        if let Ok(stats) = &mount.stats {
            cache.insert(key, stats.clone());
        }
    }
    Ok(mounts)
}
