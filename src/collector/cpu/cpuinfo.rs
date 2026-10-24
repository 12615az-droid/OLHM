use crate::raw::cpu::RawCpuInfo;

pub fn parse(text: &str) -> Result<RawCpuInfo, String> {
    let mut records = Vec::new();
    let mut record = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            if !record.is_empty() {
                records.push(std::mem::take(&mut record));
            }
        } else {
            let (key, value) = line
                .split_once(':')
                .ok_or_else(|| format!("cpuinfo line {} has no colon", index + 1))?;
            if key.trim().is_empty() {
                return Err(format!("empty key at line {}", index + 1));
            }
            record.push((key.trim().to_owned(), value.trim().to_owned()));
        }
    }
    if !record.is_empty() {
        records.push(record);
    }
    Ok(RawCpuInfo {
        text: text.to_owned(),
        records,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn records_and_unknown_duplicate_fields() {
        let data = "processor : 0\nflags : a b\nfuture : x:y\nfuture : z\n\nprocessor: 9\n";
        let parsed = parse(data).unwrap();
        assert_eq!(parsed.text, data);
        assert_eq!(parsed.records.len(), 2);
        assert_eq!(parsed.records[0][2], ("future".into(), "x:y".into()));
        assert_eq!(parsed.records[0][3].1, "z");
    }
    #[test]
    fn malformed_and_empty() {
        assert!(parse("broken").is_err());
        assert!(parse(": value").is_err());
        assert!(parse("").unwrap().records.is_empty());
    }
}
