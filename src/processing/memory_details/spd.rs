//! JEDEC SPD base parameters (DDR4/DDR5); no XMP/EXPO or live controller probing.
//! DDR5 offsets: JESD400-5; DDR4 offsets: JESD21-C SPD annex / decode-dimms.
use super::*;

pub(super) fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0_u16;
    for byte in bytes {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn ascii(bytes: &[u8]) -> Option<String> {
    if !bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        return None;
    }
    let value = std::str::from_utf8(bytes).ok()?.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn module_size(bytes: &[u8], ddr5: bool) -> Option<u64> {
    if bytes[3] & 0xf0 != 0 {
        return None;
    } // Hybrid formats need their own adapter.
    if ddr5 {
        // Only symmetric, single-rank, monolithic organizations are admitted.
        // Do not guess capacities for stacked/asymmetric/multi-rank modules.
        if bytes[4] & 0xe0 != 0 || bytes[8] != 0 || bytes[234] & 0x38 != 0 {
            return None;
        }
        let density = *[0_u64, 4, 8, 12, 16, 24, 32, 48, 64].get((bytes[4] & 0x1f) as usize)?;
        let io_code = bytes[6] >> 5;
        let bus_code = bytes[235] & 7;
        let channels_code = (bytes[235] >> 5) & 3;
        if density == 0 || io_code > 2 || bus_code > 3 || channels_code > 1 {
            return None;
        }
        let io = 4_u64 << io_code;
        let bus = 8_u64 << bus_code;
        if bus < io {
            return None;
        }
        Some(density * 1024 * 1024 * 1024 / 8 * (bus / io) * (1_u64 << channels_code))
    } else {
        if bytes[6] & 0x80 != 0 {
            return None;
        }
        let density_code = bytes[4] & 0x0f;
        let io_code = bytes[12] & 7;
        let bus_code = bytes[13] & 7;
        if density_code > 7 || io_code > 3 || bus_code > 3 {
            return None;
        }
        let io = 4_u64 << io_code;
        let bus = 8_u64 << bus_code;
        if bus < io {
            return None;
        }
        Some(
            (256_u64 << density_code) * 1024 * 1024 / 8
                * (bus / io)
                * u64::from(1 + ((bytes[12] >> 3) & 7)),
        )
    }
}

pub(super) fn parse(raw: &RawMemoryBinary) -> Result<Module, MetricValue> {
    let bytes = raw.data.as_ref().map_err(failed)?;
    let ddr5 = match bytes.get(2) {
        Some(0x12) => true,
        Some(0x0c) => false,
        _ => return Err(MetricValue::NotSupported),
    };
    let (minimum, crc_at, mfg, serial_at, part_at, part_len) = if ddr5 {
        (551, 510, 512, 517, 521, 30)
    } else {
        (349, 126, 320, 325, 329, 20)
    };
    if bytes.len() < minimum {
        return Err(MetricValue::InvalidData);
    }
    if bytes[1] >> 4 != 1 {
        return Err(MetricValue::NotSupported);
    }
    if crc16(&bytes[..crc_at]) != word(bytes, crc_at).ok_or(MetricValue::InvalidData)? {
        return Err(MetricValue::InvalidData);
    }
    let (cycle, timings) = if ddr5 {
        (
            word(bytes, 20).map(i64::from),
            [30, 32, 34, 36].map(|at| word(bytes, at).map(i64::from)),
        )
    } else if bytes[17] == 0 {
        let fine = |coarse, offset| i64::from(bytes[coarse]) * 125 + i64::from(bytes[offset] as i8);
        (
            Some(fine(18, 125)),
            [
                Some(fine(24, 123)),
                Some(fine(25, 122)),
                Some(fine(26, 121)),
                Some((i64::from(bytes[27] & 0x0f) * 256 + i64::from(bytes[28])) * 125),
            ],
        )
    } else {
        (None, [None; 4])
    };
    let cycle = cycle.filter(|ps| *ps > 0);
    let part = ascii(&bytes[part_at..part_at + part_len]);
    let serial_bytes = &bytes[serial_at..serial_at + 4];
    let serial = (!serial_bytes.iter().all(|n| *n == 0)
        && !serial_bytes.iter().all(|n| *n == 0xff))
    .then(|| {
        serial_bytes
            .iter()
            .map(|n| format!("{n:02X}"))
            .collect::<String>()
    });
    let id = &bytes[mfg..mfg + 2];
    let manufacturer = (!id.iter().all(|n| *n == 0) && !id.iter().all(|n| *n == 0xff))
        .then(|| format!("JEDEC {:02X}:{:02X}", id[0], id[1]));
    let timings: Option<Vec<_>> = timings
        .into_iter()
        .map(|n| {
            n.filter(|n| *n > 0)
                .map(|n| format!("{:.3}", n as f64 / 1000.0))
        })
        .collect();
    let source = raw
        .path
        .parent()
        .ok_or(MetricValue::InvalidData)?
        .to_path_buf();
    let address = source.file_name().unwrap_or_default().to_string_lossy();
    let properties = vec![
        property(
            "spd_source",
            "Источник SPD",
            Some(raw.path.display().to_string()),
        ),
        property(
            "spd_integrity",
            "CRC базовых параметров SPD",
            Some("проверена".into()),
        ),
        property(
            "spd_type",
            "Тип памяти (SPD)",
            Some(if ddr5 { "DDR5" } else { "DDR4" }.into()),
        ),
        property(
            "spd_capacity",
            "Объём модуля (поддерживаемая геометрия SPD)",
            module_size(bytes, ddr5).map(capacity),
        ),
        property("spd_manufacturer", "Код производителя (SPD)", manufacturer),
        property("spd_part", "Модель / Part Number (SPD)", part.clone()),
        property("spd_serial", "Серийный номер (SPD)", serial.clone()),
        property(
            "spd_speed",
            "Скорость JEDEC по tCKmin (паспортная, расчёт)",
            cycle.map(|ps| format!("{:.1} MT/s", 2_000_000.0 / ps as f64)),
        ),
        property(
            "spd_clock",
            "Тактовая частота по tCKmin (паспортная, расчёт)",
            cycle.map(|ps| format!("{:.1} МГц", 1_000_000.0 / ps as f64)),
        ),
        property(
            "spd_timings",
            "Минимальные задержки JEDEC tAA/tRCD/tRP/tRAS (не текущие тайминги)",
            timings.map(|n| format!("{} нс", n.join(" / "))),
        ),
        property(
            "spd_configured",
            "Текущая настроенная скорость (SPD её не сообщает)",
            None,
        ),
    ];
    Ok(Module {
        device: Device {
            id: format!("ram/spd/{}", source.display()),
            parent_id: Some("ram".into()),
            kind: DeviceKind::MemoryModule,
            name: format!(
                "Модуль SPD [{address}; {}]",
                part.as_deref().unwrap_or("модель недоступна")
            ),
            properties,
        },
        spd_device: Some(source),
        serial,
        part,
    })
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    pub(crate) fn fixture(ddr5: bool) -> RawMemoryBinary {
        let mut bytes = vec![0; if ddr5 { 1024 } else { 512 }];
        bytes[1] = 0x10;
        bytes[2] = if ddr5 { 0x12 } else { 0x0c };
        bytes[3] = 2;
        let (mfg, sn, pn, len, crc_at) = if ddr5 {
            bytes[4] = 4;
            bytes[6] = 0x20;
            bytes[235] = 0x22;
            bytes[20..22].copy_from_slice(&400_u16.to_le_bytes());
            for offset in [30, 32, 34] {
                bytes[offset..offset + 2].copy_from_slice(&16000_u16.to_le_bytes());
            }
            bytes[36..38].copy_from_slice(&32000_u16.to_le_bytes());
            (512, 517, 521, 30, 510)
        } else {
            bytes[4] = 5;
            bytes[12] = 1;
            bytes[13] = 3;
            bytes[18] = 10;
            bytes[24] = 112;
            bytes[25] = 112;
            bytes[26] = 112;
            bytes[27] = 1;
            (320, 325, 329, 20, 126)
        };
        bytes[mfg..mfg + 2].copy_from_slice(&[0x80, 0x2c]);
        bytes[sn..sn + 4].copy_from_slice(&[0, 1, 2, 3]);
        bytes[pn..pn + len].fill(b' ');
        bytes[pn..pn + 9].copy_from_slice(b"TEST-PART");
        let crc = crc16(&bytes[..crc_at]);
        bytes[crc_at..crc_at + 2].copy_from_slice(&crc.to_le_bytes());
        RawMemoryBinary {
            path: "/fixture/i2c/1-0051/eeprom".into(),
            data: Ok(bytes),
        }
    }

    #[test]
    fn decodes_ddr4_and_ddr5_capabilities_without_claiming_active_timings() {
        for (ddr5, size, speed, timing) in [
            (
                true,
                "16.00 GiB",
                "5000.0 MT/s",
                "16.000 / 16.000 / 16.000 / 32.000 нс",
            ),
            (
                false,
                "8.00 GiB",
                "1600.0 MT/s",
                "14.000 / 14.000 / 14.000 / 32.000 нс",
            ),
        ] {
            let module = parse(&fixture(ddr5)).unwrap();
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
            assert!(value("spd_capacity").unwrap().contains(size));
            assert_eq!(value("spd_speed"), Some(speed));
            assert_eq!(value("spd_timings"), Some(timing));
            assert_eq!(value("spd_configured"), None);
            assert_eq!(module.part.as_deref(), Some("TEST-PART"));
            assert_eq!(module.serial.as_deref(), Some("00010203"));
        }
    }

    #[test]
    fn corrupt_or_unsupported_spd_does_not_generate_characteristics() {
        let mut raw = fixture(true);
        raw.data.as_mut().unwrap()[20] ^= 1;
        assert!(matches!(parse(&raw), Err(MetricValue::InvalidData)));
        raw.data.as_mut().unwrap()[2] = 0xff;
        assert!(matches!(parse(&raw), Err(MetricValue::NotSupported)));
        let mut raw = fixture(false);
        raw.data.as_mut().unwrap().truncate(100);
        assert!(matches!(parse(&raw), Err(MetricValue::InvalidData)));
    }
}
