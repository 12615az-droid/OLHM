//! SMBIOS 3.8, DSP0134, Memory Device (type 17), little-endian fields.
use super::*;

fn dword(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn speed(bytes: &[u8], offset: usize, extended: usize) -> Option<u32> {
    match word(bytes, offset)? {
        0 => None,
        0xffff => {
            dword(bytes, extended).filter(|value| *value >= 65535 && value & 0x8000_0000 == 0)
        }
        value => Some(value.into()),
    }
}

fn clean(text: &str) -> Option<String> {
    let text = text.trim();
    if text.chars().all(|c| c == '0') || text.chars().all(|c| c == 'f' || c == 'F') {
        return None;
    }
    (!text.is_empty()
        && !matches!(
            text.to_ascii_lowercase().as_str(),
            "unknown"
                | "not specified"
                | "none"
                | "to be filled by o.e.m."
                | "00000000"
                | "ffffffff"
        ))
    .then(|| text.to_owned())
}

pub(super) fn parse(raw: &RawMemoryBinary) -> Result<Option<Module>, MetricValue> {
    let raw_bytes = raw.data.as_ref().map_err(failed)?;
    if raw_bytes.first() != Some(&17) {
        return Err(MetricValue::InvalidData);
    }
    let length = *raw_bytes.get(1).ok_or(MetricValue::InvalidData)? as usize;
    if length < 0x15 {
        return Err(MetricValue::InvalidData);
    }
    let bytes = raw_bytes.get(..length).ok_or(MetricValue::InvalidData)?;
    let size = word(bytes, 0x0c).ok_or(MetricValue::InvalidData)?;
    if size == 0 {
        return Ok(None);
    }
    if size == 0xffff {
        return Err(MetricValue::NotAvailable);
    }
    let size = match size {
        0x7fff => dword(bytes, 0x1c)
            .filter(|n| *n > 0 && n & 0x8000_0000 == 0)
            .map(|n| u64::from(n) * 1024 * 1024),
        n if n & 0x8000 != 0 => Some(u64::from(n & 0x7fff) * 1024),
        n => Some(u64::from(n) * 1024 * 1024),
    }
    .ok_or(MetricValue::InvalidData)?;
    let strings = raw_bytes.get(length..).ok_or(MetricValue::InvalidData)?;
    if !strings.windows(2).any(|bytes| bytes == [0, 0]) {
        return Err(MetricValue::InvalidData);
    }
    let string = |offset: usize| -> Option<String> {
        let index = usize::from(*bytes.get(offset)?);
        if index == 0 {
            return None;
        }
        let string = strings
            .split(|byte| *byte == 0)
            .take_while(|s| !s.is_empty())
            .nth(index - 1)?;
        clean(std::str::from_utf8(string).ok()?)
    };
    let configured = speed(bytes, 0x20, 0x58);
    let memory_type = match bytes[0x12] {
        0x18 => Some("DDR3"),
        0x1a => Some("DDR4"),
        0x22 => Some("DDR5"),
        0x1b => Some("LPDDR"),
        0x1c => Some("LPDDR2"),
        0x1d => Some("LPDDR3"),
        0x1e => Some("LPDDR4"),
        0x23 => Some("LPDDR5"),
        _ => None,
    };
    let serial = string(0x18);
    let part = string(0x1a);
    let slot = string(0x10);
    let properties = vec![
        property(
            "source",
            "Источник",
            Some("SMBIOS Type 17; данные прошивки при запуске".into()),
        ),
        property("slot", "Слот", slot.clone()),
        property("bank", "Банк", string(0x11)),
        property("capacity", "Объём модуля", Some(capacity(size))),
        property("memory_type", "Тип памяти", memory_type.map(str::to_owned)),
        property("manufacturer", "Производитель", string(0x17)),
        property("part", "Модель / Part Number", part.clone()),
        property("serial", "Серийный номер", serial.clone()),
        property(
            "rated_speed",
            "Предельная скорость (SMBIOS)",
            speed(bytes, 0x15, 0x54).map(|n| format!("{n} MT/s")),
        ),
        property(
            "configured_speed",
            "Настроенная скорость (SMBIOS; при запуске)",
            configured.map(|n| format!("{n} MT/s")),
        ),
        property(
            "clock",
            "Тактовая частота DDR (расчёт по настроенной скорости)",
            configured
                .filter(|_| matches!(memory_type, Some("DDR3" | "DDR4" | "DDR5")))
                .map(|n| format!("{:.1} МГц", n as f64 / 2.0)),
        ),
        property(
            "ranks",
            "Ранги",
            bytes
                .get(0x1b)
                .map(|n| n & 0x0f)
                .filter(|n| *n > 0)
                .map(|n| n.to_string()),
        ),
    ];
    Ok(Some(Module {
        device: Device {
            id: format!("ram/dmi/{}", raw.path.display()),
            parent_id: Some("ram".into()),
            kind: DeviceKind::MemoryModule,
            name: format!(
                "Модуль памяти [{}]",
                slot.as_deref().unwrap_or("слот не указан")
            ),
            properties,
        },
        spd_device: None,
        serial,
        part,
    }))
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    pub(crate) fn fixture(size: u16) -> RawMemoryBinary {
        let mut bytes = vec![0; 0x5c];
        bytes[0] = 17;
        bytes[1] = 0x5c;
        bytes[0x0c..0x0e].copy_from_slice(&size.to_le_bytes());
        bytes[0x10] = 1;
        bytes[0x11] = 2;
        bytes[0x12] = 0x22;
        bytes[0x15..0x17].copy_from_slice(&6000_u16.to_le_bytes());
        bytes[0x17] = 3;
        bytes[0x18] = 4;
        bytes[0x1a] = 5;
        bytes[0x20..0x22].copy_from_slice(&5600_u16.to_le_bytes());
        bytes.extend_from_slice(b"DIMM_A2\0BANK 0\0Vendor\x0000010203\0TEST-PART\0\0");
        RawMemoryBinary {
            path: "/fixture/17-0/raw".into(),
            data: Ok(bytes),
        }
    }

    #[test]
    fn distinguishes_empty_slots_size_and_configured_ddr_speed() {
        let module = parse(&fixture(16384)).unwrap().unwrap();
        let value = |id: &str| {
            module
                .device
                .properties
                .iter()
                .find(|p| p.id == id)
                .unwrap()
                .value
                .as_deref()
        };
        assert_eq!(value("slot"), Some("DIMM_A2"));
        assert_eq!(value("configured_speed"), Some("5600 MT/s"));
        assert_eq!(value("clock"), Some("2800.0 МГц"));
        assert_eq!(value("manufacturer"), Some("Vendor"));
        assert_eq!(value("part"), Some("TEST-PART"));
        assert!(value("capacity").unwrap().contains("16.00 GiB"));
        assert!(parse(&fixture(0)).unwrap().is_none());
        assert!(matches!(
            parse(&fixture(0xffff)),
            Err(MetricValue::NotAvailable)
        ));
        let mut lpddr = fixture(16384);
        lpddr.data.as_mut().unwrap()[0x12] = 0x23;
        assert!(
            parse(&lpddr)
                .unwrap()
                .unwrap()
                .device
                .properties
                .iter()
                .any(|p| p.id == "clock" && p.value.is_none())
        );
    }

    #[test]
    fn validates_extended_fields_and_rejects_truncated_records() {
        let mut raw = fixture(0x7fff);
        let bytes = raw.data.as_mut().unwrap();
        bytes[0x1c..0x20].copy_from_slice(&65536_u32.to_le_bytes());
        bytes[0x20..0x22].copy_from_slice(&0xffff_u16.to_le_bytes());
        bytes[0x58..0x5c].copy_from_slice(&70000_u32.to_le_bytes());
        let module = parse(&raw).unwrap().unwrap();
        assert!(
            module
                .device
                .properties
                .iter()
                .any(|p| p.id == "capacity" && p.value.as_ref().unwrap().contains("64.00 GiB"))
        );
        assert!(
            module
                .device
                .properties
                .iter()
                .any(|p| p.id == "configured_speed" && p.value.as_deref() == Some("70000 MT/s"))
        );
        raw.data.as_mut().unwrap()[0x58..0x5c].copy_from_slice(&6000_u32.to_le_bytes());
        assert!(
            parse(&raw)
                .unwrap()
                .unwrap()
                .device
                .properties
                .iter()
                .any(|p| p.id == "configured_speed" && p.value.is_none())
        );
        raw.data.as_mut().unwrap().truncate(20);
        assert!(matches!(parse(&raw), Err(MetricValue::InvalidData)));
        assert!(clean("0000000000000000").is_none());
        assert!(clean("FFFFFFFFFFFFFFFF").is_none());
    }
}
