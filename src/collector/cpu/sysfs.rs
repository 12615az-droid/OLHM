//! Shared direct filesystem reading and small kernel-text parsers.
use crate::{
    error::InterfaceStatus,
    raw::cpu::{CpuCapabilities, RawAttributes, RawIndexedAttributes, RawSysfsDevice},
};
use std::{fs, io, path::Path};

fn io_status(error: io::Error) -> InterfaceStatus {
    match error.kind() {
        io::ErrorKind::NotFound => InterfaceStatus::NotAvailable,
        io::ErrorKind::PermissionDenied => InterfaceStatus::PermissionDenied(error.to_string()),
        _ => InterfaceStatus::ReadError(error.to_string()),
    }
}

pub(crate) fn read(path: &Path, caps: &mut CpuCapabilities) -> Option<String> {
    match fs::read_to_string(path) {
        Ok(text) => {
            caps.insert(path.to_owned(), InterfaceStatus::Available);
            Some(text)
        }
        Err(error) => {
            caps.insert(path.to_owned(), io_status(error));
            None
        }
    }
}

pub(crate) fn directories(
    path: &Path,
    prefix: &str,
    caps: &mut CpuCapabilities,
) -> Option<Vec<(u32, std::path::PathBuf)>> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) => {
            caps.insert(path.to_owned(), io_status(error));
            return None;
        }
    };
    caps.insert(path.to_owned(), InterfaceStatus::Available);
    let mut found = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                caps.insert(path.to_owned(), io_status(error));
                continue;
            }
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(suffix) = name.strip_prefix(prefix) else {
            continue;
        };
        if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let id = match suffix.parse::<u32>() {
            Ok(id) => id,
            Err(error) => {
                caps.insert(entry.path(), InterfaceStatus::ParseError(error.to_string()));
                continue;
            }
        };
        match fs::metadata(entry.path()) {
            Ok(metadata) if metadata.is_dir() => found.push((id, entry.path())),
            Ok(_) => {}
            Err(error) => {
                caps.insert(entry.path(), io_status(error));
            }
        }
    }
    found.sort_by_key(|(id, _)| *id);
    Some(found)
}

/// Read immediate regular attributes only. Never recurse into symlink trees
/// or write kernel control files. A directory can be available but empty.
pub(crate) fn attributes(path: &Path, caps: &mut CpuCapabilities) -> Option<RawAttributes> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) => {
            caps.insert(path.to_owned(), io_status(error));
            return None;
        }
    };
    caps.insert(path.to_owned(), InterfaceStatus::Available);
    let mut values = RawAttributes::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                caps.insert(path.to_owned(), io_status(error));
                continue;
            }
        };
        match entry.file_type() {
            Ok(kind) if kind.is_file() => {
                if let Some(text) = read(&entry.path(), caps) {
                    values.insert(entry.file_name().to_string_lossy().into_owned(), text);
                }
            }
            Ok(_) => {}
            Err(error) => {
                caps.insert(entry.path(), io_status(error));
            }
        }
    }
    Some(values)
}

pub(crate) fn indexed(
    path: &Path,
    prefix: &str,
    caps: &mut CpuCapabilities,
) -> Option<Vec<RawIndexedAttributes>> {
    Some(
        directories(path, prefix, caps)?
            .into_iter()
            .map(|(index, path)| RawIndexedAttributes {
                index,
                attributes: attributes(&path, caps),
            })
            .collect(),
    )
}

/// Collect immediate child directories from a sysfs class. Class entries are
/// commonly symlinks, so metadata intentionally follows them.
pub(crate) fn devices(
    path: &Path,
    prefix: Option<&str>,
    caps: &mut CpuCapabilities,
) -> Option<Vec<RawSysfsDevice>> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) => {
            caps.insert(path.to_owned(), io_status(error));
            return None;
        }
    };
    caps.insert(path.to_owned(), InterfaceStatus::Available);
    let mut devices = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                caps.insert(path.to_owned(), io_status(error));
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if prefix.is_some_and(|prefix| !name.starts_with(prefix)) {
            continue;
        }
        match fs::metadata(entry.path()) {
            Ok(metadata) if metadata.is_dir() => devices.push(RawSysfsDevice {
                name,
                canonical_path: fs::canonicalize(entry.path()).ok(),
                attributes: attributes(&entry.path(), caps),
            }),
            Ok(_) => {}
            Err(error) => {
                caps.insert(entry.path(), io_status(error));
            }
        }
    }
    devices.sort_by(|left, right| left.name.cmp(&right.name));
    Some(devices)
}

pub fn parse_unsigned(text: &str) -> Result<u64, String> {
    text.trim().parse::<u64>().map_err(|e| e.to_string())
}

pub fn parse_online(text: &str) -> Result<bool, String> {
    match parse_unsigned(text)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err("online must be 0 or 1".into()),
    }
}

/// Parse the simple CPU list grammar; retain the original string separately.
/// Expansion is bounded to avoid allocating arbitrarily large malformed lists.
pub fn parse_cpu_list(text: &str) -> Result<Vec<u32>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let mut ids = std::collections::BTreeSet::new();
    for part in text.split(',') {
        let parse_id = |value: &str| -> Result<u32, String> {
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(format!("invalid CPU id: {value}"));
            }
            value.parse::<u32>().map_err(|e| e.to_string())
        };
        let (start, end) = match part.split_once('-') {
            Some((start, end)) => (parse_id(start)?, parse_id(end)?),
            None => {
                let id = parse_id(part)?;
                (id, id)
            }
        };
        if start > end {
            return Err("descending CPU range".into());
        }
        if u64::from(end) - u64::from(start) + 1 + ids.len() as u64 > 1_048_576 {
            return Err("CPU list expansion exceeds safety limit".into());
        }
        ids.extend(start..=end);
    }
    Ok(ids.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cpu_lists() {
        assert_eq!(parse_cpu_list("0-7\n").unwrap(), (0..8).collect::<Vec<_>>());
        assert_eq!(
            parse_cpu_list("0-3,8-11").unwrap(),
            vec![0, 1, 2, 3, 8, 9, 10, 11]
        );
        assert_eq!(parse_cpu_list("0,2,4-6").unwrap(), vec![0, 2, 4, 5, 6]);
        assert_eq!(parse_cpu_list("2,0-2").unwrap(), vec![0, 1, 2]);
        assert!(parse_cpu_list("\n").unwrap().is_empty());
        assert_eq!(parse_cpu_list("4294967295").unwrap(), vec![u32::MAX]);
        for input in [
            "3-1",
            "0,",
            "-1",
            "a",
            "0-2-3",
            "0-4294967295",
            "4294967296",
            "0, 1",
        ] {
            assert!(parse_cpu_list(input).is_err(), "{input}");
        }
    }
    #[test]
    fn numeric_and_online() {
        assert_eq!(parse_unsigned(" 4200\n"), Ok(4200));
        assert!(parse_unsigned("-1").is_err());
        assert!(parse_unsigned("18446744073709551616").is_err());
        assert_eq!(parse_online("1\n"), Ok(true));
        assert_eq!(parse_online("0"), Ok(false));
        assert!(parse_online("2").is_err());
    }
    #[test]
    fn availability_categories() {
        assert_eq!(
            io_status(io::Error::from(io::ErrorKind::NotFound)),
            InterfaceStatus::NotAvailable
        );
        assert!(matches!(
            io_status(io::Error::from(io::ErrorKind::PermissionDenied)),
            InterfaceStatus::PermissionDenied(_)
        ));
        assert!(matches!(
            io_status(io::Error::from(io::ErrorKind::InvalidData)),
            InterfaceStatus::ReadError(_)
        ));
    }
}
