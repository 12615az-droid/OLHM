//! Read-only SMBIOS/SPD discovery and live memory temperatures/zram counters.
use super::MemoryCollector;
use crate::{error::InterfaceStatus, raw::memory::*};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

fn status(error: io::Error) -> InterfaceStatus {
    match error.kind() {
        io::ErrorKind::NotFound => InterfaceStatus::NotAvailable,
        io::ErrorKind::PermissionDenied => InterfaceStatus::PermissionDenied(error.to_string()),
        _ => InterfaceStatus::ReadError(error.to_string()),
    }
}

fn text(path: &Path) -> Result<String, InterfaceStatus> {
    fs::read_to_string(path).map_err(status)
}

fn directories(path: &Path) -> Result<Vec<PathBuf>, InterfaceStatus> {
    let mut entries = fs::read_dir(path)
        .map_err(status)?
        .map(|entry| entry.map(|entry| entry.path()).map_err(status))
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort();
    Ok(entries)
}

fn binary(path: PathBuf, limit: u64) -> RawMemoryBinary {
    let data = fs::File::open(&path).map_err(status).and_then(|file| {
        let mut bytes = Vec::new();
        file.take(limit).read_to_end(&mut bytes).map_err(status)?;
        Ok(bytes)
    });
    RawMemoryBinary { path, data }
}

// proc escapes whitespace and backslashes in swap filenames as octal.
pub(crate) fn unescape_path(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut output = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            let digits = bytes.get(index + 1..index + 4)?;
            if !digits.iter().all(|b| (b'0'..=b'7').contains(b)) {
                return None;
            }
            let value = (digits[0] - b'0') as u16 * 64
                + (digits[1] - b'0') as u16 * 8
                + (digits[2] - b'0') as u16;
            output.push(u8::try_from(value).ok()?);
            index += 4;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).ok()
}

impl MemoryCollector {
    /// Inventory is read once per session; never scan the SMBus or load drivers.
    pub fn inventory(&mut self) -> RawMemoryInventory {
        let dmi = directories(&self.sys_root.join("firmware/dmi/entries")).map(|paths| {
            paths
                .into_iter()
                .filter(|path| {
                    path.file_name()
                        .and_then(|s| s.to_str())
                        .and_then(|s| s.strip_prefix("17-"))
                        .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                })
                .map(|path| binary(path.join("raw"), 4096))
                .collect()
        });
        let spd = directories(&self.sys_root.join("bus/i2c/devices")).map(|paths| {
            paths
                .into_iter()
                .filter(|path| {
                    text(&path.join("name"))
                        .is_ok_and(|name| matches!(name.trim(), "ee1004" | "spd5118"))
                })
                .map(|path| {
                    let path = fs::canonicalize(&path).unwrap_or(path);
                    binary(path.join("eeprom"), 1024)
                })
                .collect()
        });
        self.temperatures.clear();
        for path in directories(&self.sys_root.join("class/hwmon")).unwrap_or_default() {
            let Ok(name) = text(&path.join("name")) else {
                continue;
            };
            if !matches!(name.trim(), "spd5118" | "jc42") {
                continue;
            }
            let device = fs::canonicalize(path.join("device")).unwrap_or_else(|_| path.clone());
            self.temperatures.push((path, device, name.trim().into()));
        }
        RawMemoryInventory { dmi, spd }
    }

    pub fn sample_details(&self) -> RawMemoryDetails {
        let swaps = text(&self.proc_root.join("swaps"));
        let mut zram = BTreeMap::new();
        for line in swaps.iter().flat_map(|s| s.lines().skip(1)) {
            let parts: Vec<_> = line.split_whitespace().collect();
            if parts.len() != 5 || parts[1] != "partition" {
                continue;
            }
            let Some(filename) = unescape_path(parts[0]) else {
                continue;
            };
            let path = Path::new(&filename);
            let resolved = fs::canonicalize(path).unwrap_or_else(|_| path.into());
            if !resolved.starts_with("/dev") {
                continue;
            }
            let Some(name) = resolved.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if !name
                .strip_prefix("zram")
                .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            {
                continue;
            }
            let base = self.sys_root.join("block").join(name);
            // A kernel-reported block swap plus the matching zram sysfs ABI.
            // Do not classify arbitrary files whose names happen to contain zram.
            let Ok(disksize) = text(&base.join("disksize")) else {
                continue;
            };
            zram.insert(
                filename,
                BTreeMap::from([
                    ("disksize".into(), Ok(disksize)),
                    ("mm_stat".into(), text(&base.join("mm_stat"))),
                    ("comp_algorithm".into(), text(&base.join("comp_algorithm"))),
                ]),
            );
        }
        let temperatures = self
            .temperatures
            .iter()
            .map(|(path, device, driver)| RawMemoryTemperature {
                path: path.clone(),
                device: device.clone(),
                driver: driver.clone(),
                input: text(&path.join("temp1_input")),
                fault: text(&path.join("temp1_fault")),
                enabled: text(&path.join("temp1_enable")),
            })
            .collect();
        RawMemoryDetails {
            swaps,
            zram,
            temperatures,
        }
    }
}
